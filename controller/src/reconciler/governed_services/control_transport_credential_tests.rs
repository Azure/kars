// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn task_identity() -> Value {
    json!({"managed":true,"sandbox":{"namespace":"kars-system","name":"normal","uid":"source"},
        "namespace_uid":"namespace","task":{"namespace":"kars-system","name":"mission","uid":"task"},
        "task_generation":1,"task_authorization":format!("sha256:{}", "a".repeat(64))})
}

#[tokio::test]
async fn control_tls_issues_and_reuses_private_admin_configuration_without_observer_grants() {
    for legacy in [false, true] {
        let (_server, client, state) = fixture().await;
        if legacy {
            state
                .lock()
                .unwrap()
                .objects
                .insert(format!("{SECRETS}/{SECRET}"), secret(None, true));
        }
        let first = control_transport::ensure(&client, &source(), &namespace(), &task_identity())
            .await
            .unwrap();
        let writes = secret_writes(&state.lock().unwrap());
        let second = control_transport::ensure(&client, &source(), &namespace(), &task_identity())
            .await
            .unwrap();
        assert_eq!(first.version, second.version);
        let state = state.lock().unwrap();
        assert_eq!(secret_writes(&state), writes);
        assert_eq!(writes, 1);
        let secret = &state.objects[&format!("{SECRETS}/{SECRET}")];
        let bytes = STANDARD
            .decode(secret["data"]["config.json"].as_str().unwrap())
            .unwrap();
        let tls: crate::service_control::TlsIdentity = serde_json::from_slice(&bytes).unwrap();
        assert!(tls.valid_for(&task_identity(), chrono::Utc::now().timestamp()));
        assert!(
            state
                .calls
                .iter()
                .all(|(_, path, _)| !path.contains("observer"))
        );
        assert!(!state.objects.keys().any(|path| path.contains("observer")));
    }
}

#[tokio::test]
async fn control_tls_pending_privacy_waits_without_mutation_then_continues_when_qualified() {
    for existing in [false, true] {
        let (_server, client, state) = fixture().await;
        enroll(&mut state.lock().unwrap());
        if existing {
            control_transport::ensure(&client, &source(), &namespace(), &task_identity())
                .await
                .unwrap();
        }
        let key = format!("{SECRETS}/{SECRET}");
        let before = {
            let mut state = state.lock().unwrap();
            state.objects.insert(DEPLOY.into(), deployment());
            state.objects.get_mut(REG).unwrap()["status"]["phase"] = "Migrating".into();
            state.calls.clear();
            state.objects.get(&key).cloned()
        };
        assert!(matches!(
            control_transport::ensure(&client, &source(), &namespace(), &task_identity()).await,
            Err(credentials::IssuanceError::PrivacyPending)
        ));
        {
            let mut state = state.lock().unwrap();
            assert_eq!(state.objects.get(&key), before.as_ref());
            assert_eq!(state.objects[DEPLOY]["spec"]["replicas"], 1);
            assert_eq!(secret_writes(&state), 0);
            assert_eq!(token_issuances(&state), 0);
            assert!(state.calls.iter().all(|(method, _, _)| method != "PATCH"));
            state.objects.insert(REG.into(), registration());
        }
        let projection =
            control_transport::ensure(&client, &source(), &namespace(), &task_identity())
                .await
                .unwrap();
        assert!(!projection.version.is_empty());
        let state = state.lock().unwrap();
        assert_eq!(secret_writes(&state), if existing { 0 } else { 1 });
        if existing {
            assert_eq!(state.objects.get(&key), before.as_ref());
        }
    }
}

#[tokio::test]
async fn control_tls_privacy_loss_quarantines_and_restoration_replaces_both_key_and_token() {
    let (_server, client, state) = fixture().await;
    control_transport::ensure(&client, &source(), &namespace(), &task_identity())
        .await
        .unwrap();
    let before = {
        let mut state = state.lock().unwrap();
        state.objects.insert(
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/normal".into(),
            json!(source()),
        );
        state.objects.insert(DEPLOY.into(), deployment());
        state.allow_verb = Some("watch".into());
        state.objects[&format!("{SECRETS}/{SECRET}")]["data"].clone()
    };
    assert!(matches!(
        control_transport::ensure(&client, &source(), &namespace(), &task_identity()).await,
        Err(credentials::IssuanceError::Rejected(_))
    ));
    {
        let mut state = state.lock().unwrap();
        assert_eq!(state.objects[DEPLOY]["spec"]["replicas"], 0);
        let secret = &state.objects[&format!("{SECRETS}/{SECRET}")];
        assert_eq!(secret["metadata"]["annotations"][RETIRED], "true");
        assert_eq!(secret["data"], before);
        state.allow_verb = None;
    }
    control_transport::ensure(&client, &source(), &namespace(), &task_identity())
        .await
        .unwrap();
    let state = state.lock().unwrap();
    let secret = &state.objects[&format!("{SECRETS}/{SECRET}")];
    assert!(secret["metadata"]["annotations"].get(RETIRED).is_none());
    assert_ne!(secret["data"]["control-token"], before["control-token"]);
    assert_ne!(secret["data"]["config.json"], before["config.json"]);
}
