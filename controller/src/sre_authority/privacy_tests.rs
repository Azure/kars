// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::tests::{State, fixture, registration};
use super::*;
use crate::sre_registration::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const SECRETS: &str = "/api/v1/namespaces/kars-sre/secrets";
const SA: &str = "/api/v1/namespaces/kars-sre/serviceaccounts/sre-api-router";
const CRBS: &str = "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings";

fn ready() -> KarsSRERegistration {
    let mut reg = registration();
    reg.status = Some(
        serde_json::from_value(json!({
            "phase":"Ready","observedGeneration":1,"privacyEpoch":reg.epoch(),
            "routerServiceAccountUid":"router-sa","legacySecretAccessDenied":true,
            "privacyRevision":crate::sre_privacy::REVISION,
        }))
        .unwrap(),
    );
    reg
}

fn admission_ready(state: &Arc<Mutex<State>>) {
    let mut state = state.lock().unwrap();
    for name in admission::POLICIES {
        state.objects.insert(format!("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/{name}"),json!({
            "metadata":{"name":name,"generation":1},"spec":{"failurePolicy":"Fail","validations":[]},
            "status":{"observedGeneration":1,"typeChecking":{}}}));
        state.objects.insert(
            format!(
                "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/{name}"
            ),
            json!({
            "metadata":{"name":name},"spec":{"policyName":name,"validationActions":["Deny"]}}),
        );
    }
}

fn alias(name: &str, account_name: &str, account_uid: Option<&str>) -> Value {
    let mut secret = json!({"apiVersion":"v1","kind":"Secret","type":"kubernetes.io/service-account-token",
        "metadata":{"name":name,"namespace":"kars-sre","uid":format!("uid-{name}"),"resourceVersion":"1",
            "annotations":{"kubernetes.io/service-account.name":account_name}},
        "data":{"token":"PRIVATE_TOKEN_SENTINEL"}});
    if let Some(uid) = account_uid {
        secret["metadata"]["annotations"]["kubernetes.io/service-account.uid"] = uid.into();
    }
    secret
}

#[tokio::test]
async fn prestaged_aliases_and_recreated_uids_never_receive_a_service_account_or_credentials() {
    for (name, uid) in [
        ("sre-api-router", None),
        ("sre-api-router", Some("obsolete-sa")),
        ("renamed-before-policy", Some("router-sa")),
    ] {
        let (_server, client, state) = fixture().await;
        let reg = ready();
        state.lock().unwrap().objects.insert(
            format!("{SECRETS}/arbitrary-alias"),
            alias("arbitrary-alias", name, uid),
        );
        let authority = live::verify(&client, &reg).await.unwrap();
        let error = credentials::ensure_service_account(&client, &reg, &authority)
            .await
            .unwrap_err();
        assert!(!error.contains("PRIVATE_TOKEN_SENTINEL"));
        let state = state.lock().unwrap();
        assert!(state.calls.iter().all(|(method, _, _)| method == "GET"));
        assert!(
            state
                .objects
                .contains_key(&format!("{SECRETS}/arbitrary-alias"))
        );
        assert!(
            state
                .metadata_requests
                .iter()
                .all(|accept| accept.contains("PartialObjectMetadataList"))
        );
    }
}

#[tokio::test]
async fn unsafe_alias_after_ready_revokes_owned_grants_but_preserves_foreign_and_unrelated_subjects()
 {
    let (_server, client, state) = fixture().await;
    let reg = ready();
    admission_ready(&state);
    {
        let mut state = state.lock().unwrap();
        state
            .objects
            .insert(REG.into(), serde_json::to_value(&reg).unwrap());
        state.objects.insert(
            format!("{SECRETS}/alias"),
            alias("alias", "sre-api-router", Some("router-sa")),
        );
        state.objects.insert(
            SA.into(),
            json!({"metadata":{"name":ROUTER_SA,"namespace":RUNTIME_NAMESPACE,
            "uid":"router-sa","resourceVersion":"1"}}),
        );
        for (name, role, owner) in [
            (
                "kars-sre-private-reader",
                "kars-sre-private-diagnostics",
                "registration",
            ),
            (
                "kars-sre-private-author",
                "kars-sre-action-author",
                "foreign",
            ),
            (
                "kars-sre-private-renew",
                "kars-sre-router-renew",
                "registration",
            ),
        ] {
            let mut binding = json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRoleBinding",
                "metadata":{"name":name,"uid":format!("uid-{name}"),"resourceVersion":"1",
                    "annotations":{OWNER:owner,"kars.azure.com/namespace-uid":"runtime-ns"}},
                "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":role},
                "subjects":[{"kind":"ServiceAccount","name":ROUTER_SA,"namespace":RUNTIME_NAMESPACE}]});
            if name == "kars-sre-private-renew" {
                binding["subjects"].as_array_mut().unwrap().push(json!({"kind":"User","name":"unrelated","apiGroup":"rbac.authorization.k8s.io"}));
            }
            state.objects.insert(format!("{CRBS}/{name}"), binding);
        }
    }
    assert!(reconcile(&client, &reg).await.is_err());
    let state = state.lock().unwrap();
    assert!(state.objects.contains_key(&format!("{SECRETS}/alias")));
    assert!(
        !state
            .objects
            .contains_key(&format!("{CRBS}/kars-sre-private-reader"))
    );
    assert!(
        state
            .objects
            .contains_key(&format!("{CRBS}/kars-sre-private-author"))
    );
    assert_eq!(
        state.objects[&format!("{CRBS}/kars-sre-private-renew")]["subjects"],
        json!([{"kind":"User","name":"unrelated","apiGroup":"rbac.authorization.k8s.io"}])
    );
    assert_eq!(state.objects[REG]["status"]["phase"], "Blocked");
    assert_eq!(
        state.objects[REG]["status"]["legacySecretAccessDenied"],
        false
    );
    let patch = &state
        .calls
        .iter()
        .find(|(_, path, _)| path.ends_with("/status"))
        .unwrap()
        .2;
    assert!(patch["status"]["privacyEpoch"].is_null());
    assert!(patch["status"]["privacyRevision"].is_null());
    assert!(state.calls.iter().all(|(method, path, _)| method == "GET"
        || path.starts_with(CRBS)
        || path.ends_with("/status")));
}

#[tokio::test]
async fn watch_only_and_wildcard_group_grants_fail_review_before_mutation() {
    for rule in [
        json!({"apiGroups":[""],"resources":["secrets"],"verbs":["watch"]}),
        json!({"apiGroups":["*"],"resources":["*"],"verbs":["watch"]}),
        json!({"apiGroups":[""],"resources":["secrets"],"verbs":["*"]}),
        json!({"apiGroups":[""],"resources":["pods","secrets"],"verbs":["get","watch"]}),
    ] {
        let (_server, client, state) = fixture().await;
        {
            let mut state = state.lock().unwrap();
            state.binding["subjects"] = json!([{"kind":"Group","name":"system:serviceaccounts:kars-sre",
                "apiGroup":"rbac.authorization.k8s.io"}]);
            state.objects.insert(
                "/apis/rbac.authorization.k8s.io/v1/clusterroles/kars-sre-reader".into(),
                json!({"metadata":{"name":"kars-sre-reader"},"rules":[rule]}),
            );
        }
        assert!(
            bindings::review(&client, &registration())
                .await
                .err()
                .unwrap()
                .contains("broad group")
        );
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET")
        );
    }
}

#[tokio::test]
async fn live_reviews_cover_namespace_and_cluster_watch_including_named_grants() {
    for (namespace, name) in [
        (Some("kars-sre"), None),
        (None, None),
        (Some("kars-sre"), Some(PRIVATE_SECRET)),
        (None, Some("router-services-admin")),
    ] {
        let (_server, client, state) = fixture().await;
        state.lock().unwrap().watch_allowed =
            Some((namespace.map(str::to_owned), name.map(str::to_owned)));
        assert!(check_secret_denial(&client, "kars-sre").await.is_err());
        assert!(privacy_epoch(&client, "kars-sre").await.is_err());
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|(_, _, request)| request["spec"]["resourceAttributes"]["verb"] == "watch")
        );
    }
}

#[tokio::test]
async fn inventory_api_errors_and_prior_get_list_only_status_are_not_privacy_safe() {
    let (_server, client, state) = fixture().await;
    admission_ready(&state);
    let mut reg = ready();
    reg.status.as_mut().unwrap().privacy_revision = None;
    state
        .lock()
        .unwrap()
        .objects
        .insert(REG.into(), serde_json::to_value(&reg).unwrap());
    assert!(privacy_epoch(&client, RUNTIME_NAMESPACE).await.is_err());
    state.lock().unwrap().api_error = Some(403);
    let error = credential_guard::scan(&client, &reg).await.unwrap_err();
    assert!(!error.contains("PRIVATE_SENTINEL"));
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, path, _)| method == "GET" || path.ends_with("/subjectaccessreviews"))
    );
}

#[tokio::test]
async fn blocked_recovery_replaces_the_owned_token_anchor_uid_without_recreating_pod_identity() {
    let (_server, client, state) = fixture().await;
    let mut reg = registration();
    let authority = live::verify(&client, &reg).await.unwrap();
    let sa = credentials::ensure_service_account(&client, &reg, &authority)
        .await
        .unwrap();
    credentials::ensure_for_test(&client, &reg, &authority, &sa)
        .await
        .unwrap();
    let path = format!("{SECRETS}/{PRIVATE_SECRET}");
    let old_uid = state.lock().unwrap().objects[&path]["metadata"]["uid"].clone();
    reg.status = ready().status;
    reg.status.as_mut().unwrap().phase = "Blocked".into();
    credentials::ensure_for_test(&client, &reg, &authority, &sa)
        .await
        .unwrap();
    let state = state.lock().unwrap();
    assert_ne!(state.objects[&path]["metadata"]["uid"], old_uid);
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|(method, path, _)| method == "DELETE" && path.ends_with(PRIVATE_SECRET))
            .count(),
        1
    );
    assert!(
        !state
            .calls
            .iter()
            .any(|(method, path, _)| method == "DELETE" && path.contains("/serviceaccounts/"))
    );
}

#[tokio::test]
async fn newly_discovered_watch_group_revokes_previously_ready_owned_reader() {
    let (_server, client, state) = fixture().await;
    let reg = ready();
    admission_ready(&state);
    {
        let mut state = state.lock().unwrap();
        state
            .objects
            .insert(REG.into(), serde_json::to_value(&reg).unwrap());
        state.binding["subjects"] = json!([{"kind":"Group","name":"system:serviceaccounts:kars-sre",
            "apiGroup":"rbac.authorization.k8s.io"}]);
        state.objects.insert("/apis/rbac.authorization.k8s.io/v1/clusterroles/kars-sre-reader".into(),
            json!({"metadata":{"name":"kars-sre-reader"},"rules":[{"apiGroups":[""],"resources":["secrets"],"verbs":["watch"]}]}));
        state.objects.insert(format!("{CRBS}/kars-sre-private-reader"),json!({
            "apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRoleBinding",
            "metadata":{"name":"kars-sre-private-reader","uid":"owned-reader","resourceVersion":"1",
                "annotations":{OWNER:"registration","kars.azure.com/namespace-uid":"runtime-ns"}},
            "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"kars-sre-private-diagnostics"},
            "subjects":[{"kind":"ServiceAccount","name":ROUTER_SA,"namespace":RUNTIME_NAMESPACE}]}));
    }
    assert!(reconcile(&client, &reg).await.is_err());
    let state = state.lock().unwrap();
    assert!(
        !state
            .objects
            .contains_key(&format!("{CRBS}/kars-sre-private-reader"))
    );
    assert_eq!(state.objects[REG]["status"]["phase"], "Blocked");
    assert!(state.calls.iter().all(|(method, path, _)| method == "GET"
        || path == &format!("{CRBS}/kars-sre-private-reader")
        || path.ends_with("/status")));
}

#[tokio::test]
async fn annotation_changes_and_secret_recreation_are_rescanned_and_preserved() {
    let (_server, client, state) = fixture().await;
    let reg = ready();
    let path = format!("{SECRETS}/alias");
    state
        .lock()
        .unwrap()
        .objects
        .insert(path.clone(), alias("alias", "ordinary", None));
    credential_guard::scan(&client, &reg).await.unwrap();
    for uid in ["original", "recreated"] {
        {
            let mut state = state.lock().unwrap();
            state.objects.get_mut(&path).unwrap()["metadata"]["uid"] = uid.into();
            state.objects.get_mut(&path).unwrap()["metadata"]["annotations"]["kubernetes.io/service-account.name"] =
                ROUTER_SA.into();
        }
        assert!(credential_guard::scan(&client, &reg).await.is_err());
        assert_eq!(state.lock().unwrap().objects[&path]["metadata"]["uid"], uid);
    }
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[tokio::test]
async fn quarantine_retires_owned_identity_but_preserves_the_unsafe_unknown_alias() {
    let (_server, client, state) = fixture().await;
    let mut reg = registration();
    let authority = live::verify(&client, &reg).await.unwrap();
    let sa = credentials::ensure_service_account(&client, &reg, &authority)
        .await
        .unwrap();
    credentials::ensure_for_test(&client, &reg, &authority, &sa)
        .await
        .unwrap();
    reg.status = ready().status;
    reg.status.as_mut().unwrap().router_service_account_uid = sa.metadata.uid.clone();
    admission_ready(&state);
    {
        let mut state = state.lock().unwrap();
        state
            .objects
            .insert(REG.into(), serde_json::to_value(&reg).unwrap());
        state.objects.insert(
            format!("{SECRETS}/unsafe-alias"),
            alias("unsafe-alias", ROUTER_SA, sa.metadata.uid.as_deref()),
        );
        state.calls.clear();
    }
    assert!(reconcile(&client, &reg).await.is_err());
    let state = state.lock().unwrap();
    assert!(
        state
            .objects
            .contains_key(&format!("{SECRETS}/unsafe-alias"))
    );
    assert!(!state.objects.contains_key(SA));
    assert!(
        !state
            .objects
            .contains_key(&format!("{SECRETS}/{PRIVATE_SECRET}"))
    );
    assert!(state.calls.iter().all(|(method, path, _)| {
        method != "DELETE"
            || [
                SA,
                &format!("{SECRETS}/{PRIVATE_SECRET}"),
                &format!("{SECRETS}/{AGENT_SECRET}"),
            ]
            .contains(&path.as_str())
    }));
}

#[tokio::test]
async fn private_service_account_replacement_uid_is_never_deleted_during_quarantine() {
    let (_server, client, state) = fixture().await;
    let reg = ready();
    state.lock().unwrap().objects.insert(SA.into(),json!({
        "metadata":{"name":ROUTER_SA,"namespace":RUNTIME_NAMESPACE,"uid":"replacement","resourceVersion":"2",
            "annotations":{OWNER:"registration","kars.azure.com/sandbox-uid":"source","kars.azure.com/namespace-uid":"runtime-ns"}}}));
    assert!(credentials::retire(&client, &reg).await.is_err());
    assert!(state.lock().unwrap().objects.contains_key(SA));
    assert!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}
