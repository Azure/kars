// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    credentials, live, privacy_tests::admission_ready, reconcile, report_tests::grant_fixture,
    tests::registration,
};
use crate::sre_registration::{AGENT_SECRET, PRIVATE_SECRET, ROUTER_SA, RUNTIME_NAMESPACE};
use serde_json::json;

const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";

#[tokio::test]
async fn publication_failures_preserve_ready_identity_and_retry_complete_reconciliation() {
    for code in [409, 403, 503] {
        let (_server, client, state, _) = grant_fixture().await;
        let mut reg = registration();
        admission_ready(&state);
        {
            let mut state = state.lock().unwrap();
            state.objects.insert(
                "/api/v1/namespaces/kars-sre/pods".into(),
                json!({
                    "apiVersion":"v1","kind":"PodList","metadata":{},"items":[]
                }),
            );
            state.objects.insert(
                "/api/v1/secrets".into(),
                json!({
                    "apiVersion":"v1","kind":"SecretList","metadata":{},"items":[]
                }),
            );
        }
        let authority = live::verify(&client, &reg).await.unwrap();
        let sa = credentials::ensure_service_account(&client, &reg, &authority)
            .await
            .unwrap();
        reg.status = Some(
            serde_json::from_value(json!({
                "phase":"Ready","observedGeneration":1,
                "routerServiceAccountUid":sa.metadata.uid,
                "legacySecretAccessDenied":true,"privacyRevision":crate::sre_privacy::REVISION
            }))
            .unwrap(),
        );
        reg.status.as_mut().unwrap().privacy_epoch = Some(reg.epoch());
        state
            .lock()
            .unwrap()
            .objects
            .insert(REG.into(), serde_json::to_value(&reg).unwrap());
        credentials::ensure_for_test(&client, &reg, &authority, &sa)
            .await
            .unwrap();
        reconcile(&client, &reg).await.unwrap();
        reg = serde_json::from_value(state.lock().unwrap().objects[REG].clone()).unwrap();
        let before = state.lock().unwrap().objects.clone();
        {
            let mut state = state.lock().unwrap();
            state.calls.clear();
            state.status_error = Some(code);
        }
        assert_eq!(
            reconcile(&client, &reg).await.unwrap_err(),
            format!("Publish SRE authority status: Kubernetes status {code}")
        );
        {
            let mut state = state.lock().unwrap();
            assert_eq!(
                state.objects, before,
                "publication failure {code} mutated identity or grants"
            );
            assert!(state.calls.iter().all(|(method, _, _)| method != "DELETE"));
            let writes: Vec<_> = state
                .calls
                .iter()
                .filter(|(method, path, _)| method == "PATCH" && path.ends_with("/status"))
                .collect();
            assert_eq!(
                writes.len(),
                1,
                "publication failure must not publish a false Blocked status"
            );
            assert_eq!(
                writes[0].2["metadata"]["uid"],
                reg.metadata.uid.as_deref().unwrap()
            );
            assert_eq!(
                writes[0].2["metadata"]["resourceVersion"],
                reg.metadata.resource_version.as_deref().unwrap()
            );
            assert_eq!(writes[0].2["status"]["phase"], "Ready");
            assert!(
                writes[0].2["status"]
                    .as_object()
                    .unwrap()
                    .contains_key("detail")
            );
            assert!(writes[0].2["status"]["detail"].is_null());
            state.status_error = None;
            state.calls.clear();
        }
        reconcile(&client, &reg).await.unwrap();
        {
            let state = state.lock().unwrap();
            assert_eq!(state.objects[REG]["status"]["phase"], "Ready");
            assert!(
                state
                    .calls
                    .iter()
                    .any(|(_, path, _)| path.ends_with("/subjectaccessreviews"))
            );
            assert!(state.calls.iter().any(|(method, path, _)| method == "GET"
                && path.contains("/validatingadmissionpolicies/")));
            for (kind, name) in [
                ("serviceaccounts", ROUTER_SA),
                ("secrets", PRIVATE_SECRET),
                ("secrets", AGENT_SECRET),
            ] {
                let path = format!("/api/v1/namespaces/{RUNTIME_NAMESPACE}/{kind}/{name}");
                assert_eq!(state.objects[&path], before[&path]);
            }
            assert!(state.calls.iter().all(|(method, _, _)| method != "DELETE"));
        }
        // A real privacy failure on the retry must still retire owned identity,
        // even when publishing the resulting Blocked status also conflicts.
        {
            let mut state = state.lock().unwrap();
            state.allowed = true;
            state.status_error = Some(409);
            state.calls.clear();
        }
        assert!(reconcile(&client, &reg).await.is_err());
        let state = state.lock().unwrap();
        assert!(!state.objects.contains_key(&format!(
            "/api/v1/namespaces/{RUNTIME_NAMESPACE}/serviceaccounts/{ROUTER_SA}"
        )));
        assert!(!state.objects.contains_key(&format!(
            "/api/v1/namespaces/{RUNTIME_NAMESPACE}/secrets/{PRIVATE_SECRET}"
        )));
    }
}

#[tokio::test]
async fn provisioning_publication_conflict_does_not_delete_the_new_identity() {
    let (_server, client, state, _) = grant_fixture().await;
    let reg = registration();
    admission_ready(&state);
    {
        let mut state = state.lock().unwrap();
        state.objects.insert(
            "/api/v1/namespaces/kars-sre/pods".into(),
            json!({
                "apiVersion":"v1","kind":"PodList","metadata":{},"items":[]
            }),
        );
        state.status_error = Some(409);
    }
    assert_eq!(
        reconcile(&client, &reg).await.unwrap_err(),
        "Publish SRE authority status: Kubernetes status 409"
    );
    let state = state.lock().unwrap();
    assert!(state.objects.contains_key(&format!(
        "/api/v1/namespaces/{RUNTIME_NAMESPACE}/serviceaccounts/{ROUTER_SA}"
    )));
    assert!(state.calls.iter().all(|(method, _, _)| method != "DELETE"));
    assert!(!state.objects.contains_key(&format!(
        "/api/v1/namespaces/{RUNTIME_NAMESPACE}/secrets/{PRIVATE_SECRET}"
    )));
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|(method, path, _)| method == "PATCH" && path.ends_with("/status"))
            .count(),
        1
    );
}
