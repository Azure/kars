// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use credentials::{GITHUB, OBSERVER, OBSERVER_TLS};

#[tokio::test]
async fn governed_private_purpose_issuers_share_privacy_rotation_without_cross_purpose_material() {
    for purpose in [OBSERVER, OBSERVER_TLS, GITHUB] {
        let (_server, client, state) = fixture().await;
        let first = credentials::ensure_for(
            &client,
            &source(),
            &namespace(),
            purpose,
            Some(r#"{"scope":"first"}"#),
        )
        .await
        .unwrap();
        assert_eq!(first.version, "new-secret:8");
        {
            let data = state.lock().unwrap();
            let secret = &data.objects[&format!("{SECRETS}/{}", purpose.secret)];
            assert_eq!(
                secret["metadata"]["annotations"][REVISION],
                crate::sre_privacy::REVISION
            );
            assert!(secret["data"].get("control-token").is_none());
            assert_eq!(
                secret["data"].as_object().unwrap().len(),
                if purpose.token_key.is_some() { 2 } else { 1 }
            );
            for verb in ["get", "list", "watch"] {
                assert!(
                    data.calls
                        .iter()
                        .any(|(_, _, body)| body["spec"]["resourceAttributes"]["verb"] == verb)
                );
            }
        }
        let unchanged = credentials::ensure_for(
            &client,
            &source(),
            &namespace(),
            purpose,
            Some(r#"{"scope":"first"}"#),
        )
        .await
        .unwrap();
        assert_eq!(unchanged.version, first.version);
        let rotated = credentials::ensure_for(
            &client,
            &source(),
            &namespace(),
            purpose,
            Some(r#"{"scope":"second"}"#),
        )
        .await
        .unwrap();
        assert_ne!(rotated.version, first.version);
        let data = state.lock().unwrap();
        assert_eq!(secret_writes(&data), 2);
        assert!(
            data.calls
                .iter()
                .filter(|(method, path, _)| method == "PATCH" && path.starts_with(SECRETS))
                .all(|(_, _, body)| body["metadata"]["uid"] == "new-secret"
                    && body["metadata"]["resourceVersion"] == "8")
        );
    }
}

#[tokio::test]
async fn governed_private_purpose_foreign_uid_or_privacy_loss_never_issues_or_adopts() {
    for purpose in [OBSERVER, OBSERVER_TLS, GITHUB] {
        let (_server, client, state) = fixture().await;
        credentials::ensure_for(&client, &source(), &namespace(), purpose, Some("{}"))
            .await
            .unwrap();
        let key = format!("{SECRETS}/{}", purpose.secret);
        {
            let mut data = state.lock().unwrap();
            data.objects.get_mut(&key).unwrap()["metadata"]["annotations"][SOURCE_UID] =
                "foreign".into();
            data.calls.clear();
        }
        assert!(
            credentials::ensure_for(&client, &source(), &namespace(), purpose, Some("{}"))
                .await
                .is_err()
        );
        assert_eq!(secret_writes(&state.lock().unwrap()), 0);
        {
            let mut data = state.lock().unwrap();
            data.objects.get_mut(&key).unwrap()["metadata"]["annotations"][SOURCE_UID] =
                "source".into();
            data.allow_verb = Some("watch".into());
            data.objects.insert(DEPLOY.into(), deployment());
        }
        assert!(
            credentials::ensure_for(&client, &source(), &namespace(), purpose, Some("{}"))
                .await
                .is_err()
        );
        let data = state.lock().unwrap();
        assert_eq!(
            data.objects[&key]["metadata"]["annotations"][RETIRED],
            "true"
        );
        assert_eq!(data.objects[DEPLOY]["spec"]["replicas"], 0);
        assert!(
            data.calls
                .iter()
                .all(|(_, _, body)| body["stringData"].is_null())
        );
    }
}

#[tokio::test]
async fn governed_observer_rotated_version_waits_for_old_terminating_router_consumers() {
    let (_server, client, state) = fixture().await;
    let projection =
        credentials::ensure_for(&client, &source(), &namespace(), OBSERVER, Some("{}"))
            .await
            .unwrap();
    state.lock().unwrap().pods = vec![json!({
        "apiVersion":"v1","kind":"Pod","metadata":{"name":"old","namespace":NS,"uid":"old-pod",
            "deletionTimestamp":"2026-01-01T00:00:00Z","annotations":{OBSERVER.version_annotation:"old-version"}},
        "spec":{"containers":[{"name":"inference-router","image":"test"}]}}
    )];
    assert!(
        !projection
            .consumers_current(&client, NS, "normal")
            .await
            .unwrap()
    );
    state.lock().unwrap().pods.clear();
    assert!(
        projection
            .consumers_current(&client, NS, "normal")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn governed_github_identical_config_with_changed_source_revision_requires_consumer_rotation()
{
    let (_server, client, state) = fixture().await;
    let first = credentials::ensure_bound(
        &client,
        &source(),
        &namespace(),
        GITHUB,
        Some("{}"),
        Some("source-uid:1"),
    )
    .await
    .unwrap();
    let initial =
        state.lock().unwrap().objects[&format!("{SECRETS}/{}", GITHUB.secret)]["data"].clone();
    let same = credentials::ensure_bound(
        &client,
        &source(),
        &namespace(),
        GITHUB,
        Some("{}"),
        Some("source-uid:1"),
    )
    .await
    .unwrap();
    assert_eq!(same.version, first.version);
    let changed = credentials::ensure_bound(
        &client,
        &source(),
        &namespace(),
        GITHUB,
        Some("{}"),
        Some("source-uid:2"),
    )
    .await
    .unwrap();
    assert_ne!(changed.version, first.version);
    {
        let mut data = state.lock().unwrap();
        let stored = &data.objects[&format!("{SECRETS}/{}", GITHUB.secret)];
        assert_eq!(stored["data"], initial);
        assert_eq!(
            stored["metadata"]["annotations"][credentials::SOURCE_REVISION],
            "source-uid:2"
        );
        data.pods = vec![
            json!({"metadata":{"name":"old","namespace":NS,"uid":"old-pod",
            "deletionTimestamp":"2026-01-01T00:00:00Z",
            "annotations":{GITHUB.version_annotation:first.version}}}),
        ];
    }
    assert!(
        !changed
            .consumers_current(&client, NS, "normal")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn governed_github_pending_privacy_is_typed_and_does_not_retire_a_rollout() {
    let (_server, client, state) = fixture().await;
    credentials::ensure_bound(
        &client,
        &source(),
        &namespace(),
        GITHUB,
        Some("{}"),
        Some("source-uid:1"),
    )
    .await
    .unwrap();
    {
        let mut data = state.lock().unwrap();
        enroll(&mut data);
        data.objects.get_mut(REG).unwrap()["status"]["phase"] = "Migrating".into();
        data.objects.insert(DEPLOY.into(), deployment());
        data.calls.clear();
    }
    let result = credentials::ensure_bound(
        &client,
        &source(),
        &namespace(),
        GITHUB,
        Some("{}"),
        Some("source-uid:2"),
    )
    .await;
    assert!(matches!(
        result,
        Err(credentials::IssuanceError::PrivacyPending)
    ));
    let data = state.lock().unwrap();
    assert_eq!(secret_writes(&data), 0);
    assert_eq!(data.objects[DEPLOY]["spec"]["replicas"], 1);
    assert!(data.calls.iter().all(|(method, _, _)| method != "PATCH"));
}

#[tokio::test]
async fn governed_github_retirement_disables_legacy_mount_and_waits_for_old_consumers() {
    let (_server, client, state) = fixture().await;
    let retired = crate::credential_grants::github::Projection::Retired(
        credentials::Projection::retired(GITHUB, &source()).unwrap(),
    );
    assert_eq!(retired.required_mount(), None);
    assert_eq!(
        crate::credential_grants::github::Projection::Legacy.required_mount(),
        Some(false)
    );
    let mut deployment: Deployment = serde_json::from_value(deployment()).unwrap();
    retired.decorate(&mut deployment);
    let version = deployment
        .spec
        .as_ref()
        .unwrap()
        .template
        .metadata
        .as_ref()
        .unwrap()
        .annotations
        .as_ref()
        .unwrap()[GITHUB.version_annotation]
        .clone();
    state.lock().unwrap().pods = vec![json!({"metadata":{"name":"old","uid":"old",
        "deletionTimestamp":"2026-01-01T00:00:00Z",
        "annotations":{GITHUB.version_annotation:"old-credential-version"}}})];
    assert!(
        !retired
            .consumers_current(&client, NS, "normal")
            .await
            .unwrap()
    );
    state.lock().unwrap().pods[0]["metadata"]["annotations"][GITHUB.version_annotation] =
        version.into();
    assert!(
        retired
            .consumers_current(&client, NS, "normal")
            .await
            .unwrap()
    );
}
