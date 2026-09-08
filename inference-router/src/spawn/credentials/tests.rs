// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::test_server::{NS_PATH, SECRET_PATH, SENTINEL, Scenario, State, sandbox, start};
use super::*;

fn target() -> Target {
    let mut created = sandbox();
    created["metadata"]["annotations"] = json!({});
    Target::from_created(
        &serde_json::from_value(created).unwrap(),
        "workspace-a",
        "demo",
    )
    .unwrap()
}

fn values() -> BTreeMap<String, String> {
    BTreeMap::from([("TELEGRAM_BOT_TOKEN".into(), SENTINEL.into())])
}

async fn write(client: &Client, target: &Target) -> Result<(), String> {
    write_credentials(
        client,
        target,
        &values(),
        "parent",
        Wait {
            attempts: 3,
            delay: Duration::ZERO,
        },
    )
    .await
}

#[test]
fn target_uses_the_created_response_identity_not_a_later_same_name_lookup() {
    let created: DynamicObject = serde_json::from_value(sandbox()).unwrap();
    let target = Target::from_created(&created, "workspace-a", "demo").unwrap();
    assert_eq!(target.workspace, "workspace-a");
    assert_eq!(target.sandbox_uid, "created-uid");
    assert_eq!(target.namespace_uid.as_deref(), Some("namespace-uid"));
    assert!(Target::from_created(&created, "workspace-b", "demo").is_err());
    assert!(Target::from_created(&created, "workspace-a", "another").is_err());
    let mut missing = created.clone();
    missing.metadata.uid = None;
    assert!(Target::from_created(&missing, "workspace-a", "demo").is_err());
    missing = created;
    missing.metadata.namespace = None;
    assert!(Target::from_created(&missing, "workspace-a", "demo").is_err());
}

#[tokio::test]
async fn matching_claim_propagates_with_metadata_only_reads_and_a_uid_version_fence() {
    for existing in [false, true] {
        let initial = if existing {
            State::default().with_existing_secret()
        } else {
            State::default()
        };
        let (_server, client, state) = start(initial).await;
        write(&client, &target()).await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.value_writes, 1);
        assert_eq!(state.value_attempts, 1);
        let secret = state.secret.as_ref().unwrap();
        assert_eq!(secret["data"]["TELEGRAM_BOT_TOKEN"], SENTINEL);
        if existing {
            assert_eq!(secret["metadata"]["labels"]["existing"], "preserve");
            assert_eq!(secret["data"]["EXISTING"], "preserve");
        }
        assert_eq!(
            secret["metadata"]["labels"]["kars.azure.com/predecessor"],
            "parent"
        );
        let patches: Vec<_> = state
            .requests
            .iter()
            .filter(|request| request.method == "PATCH")
            .collect();
        assert_eq!(patches.len(), 2);
        let anchor: serde_json::Value = patches[0].body_json().unwrap();
        assert!(anchor.get("data").is_none() && anchor.get("stringData").is_none());
        let update: serde_json::Value = patches[1].body_json().unwrap();
        assert_eq!(update["metadata"]["uid"], "secret-uid");
        assert_eq!(update["metadata"]["resourceVersion"], "2");
        assert!(
            state
                .requests
                .iter()
                .all(|request| { request.url.path() != SECRET_PATH || request.method == "PATCH" })
        );
    }
}

#[tokio::test]
async fn same_named_sandbox_in_another_workspace_cannot_write_the_claimed_namespace() {
    let mut initial = State::default();
    initial.sandbox.as_mut().unwrap()["metadata"]["namespace"] = "workspace-b".into();
    initial.sandbox.as_mut().unwrap()["metadata"]["uid"] = "workspace-b-sandbox".into();
    let created: DynamicObject = serde_json::from_value(initial.sandbox.clone().unwrap()).unwrap();
    let requested = Target::from_created(&created, "workspace-b", "demo").unwrap();
    let (_server, client, state) = start(initial).await;
    assert!(write(&client, &requested).await.is_err());
    let state = state.lock().unwrap();
    assert_eq!(state.value_writes, 0);
    assert!(state.requests.iter().all(|request| request.method == "GET"));
    assert!(
        state.requests[0]
            .url
            .path()
            .contains("/namespaces/workspace-b/")
    );
}

#[tokio::test]
async fn missing_namespace_and_partial_claims_wait_for_full_claim_and_backlink() {
    for scenario in [Scenario::ConvergeMissing, Scenario::ConvergePartial] {
        let mut initial = State {
            scenario,
            ..Default::default()
        };
        initial.sandbox.as_mut().unwrap()["metadata"]["annotations"] = json!({});
        if matches!(scenario, Scenario::ConvergeMissing) {
            initial.namespace = None;
        } else {
            let annotations = initial.namespace.as_mut().unwrap()["metadata"]["annotations"]
                .as_object_mut()
                .unwrap();
            annotations.remove(SOURCE_UID);
            annotations.insert(PRESTAGE.into(), "bind-next-sandbox".into());
        }
        let (_server, client, state) = start(initial).await;
        write(&client, &target()).await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.value_writes, 1);
        let anchor = state
            .requests
            .iter()
            .position(|request| request.method == "PATCH")
            .unwrap();
        assert_eq!(
            state.requests[..anchor]
                .iter()
                .filter(|request| request.url.path() == NS_PATH)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn unclaimed_labels_prestage_or_missing_backlink_never_authorize_a_write() {
    for shape in ["labels", "partial", "prestage", "backlink"] {
        let mut initial = State::default();
        match shape {
            "labels" => {
                initial.namespace.as_mut().unwrap()["metadata"]["annotations"] = json!({});
                initial.namespace.as_mut().unwrap()["metadata"]["labels"] =
                    json!({"kars.azure.com/sandbox":"demo"});
            }
            "partial" => {
                initial.namespace.as_mut().unwrap()["metadata"]["annotations"]
                    .as_object_mut()
                    .unwrap()
                    .remove(VERSION);
            }
            "prestage" => {
                let annotations = initial.namespace.as_mut().unwrap()["metadata"]["annotations"]
                    .as_object_mut()
                    .unwrap();
                annotations.remove(SOURCE_UID);
                annotations.insert(PRESTAGE.into(), "bind-next-sandbox".into());
            }
            _ => initial.sandbox.as_mut().unwrap()["metadata"]["annotations"] = json!({}),
        }
        let (_server, client, state) = start(initial).await;
        assert!(
            write(&client, &target())
                .await
                .unwrap_err()
                .contains("did not converge")
        );
        let state = state.lock().unwrap();
        assert_eq!(state.value_attempts, 0);
        assert!(state.requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn replaced_uids_foreign_owners_and_terminating_objects_fail_before_secret_access() {
    for shape in [
        "cr-uid",
        "ns-uid",
        "claim-uid",
        "claim-workspace",
        "claim-name",
        "version",
        "owner",
        "ns-delete",
        "ns-phase",
        "cr-delete",
        "ns-missing",
        "ns-no-uid",
        "ns-no-rv",
        "cr-no-rv",
    ] {
        let mut initial = State::default();
        match shape {
            "cr-uid" => initial.sandbox.as_mut().unwrap()["metadata"]["uid"] = "recreated".into(),
            "ns-uid" => initial.namespace.as_mut().unwrap()["metadata"]["uid"] = "recreated".into(),
            "claim-uid" => {
                initial.namespace.as_mut().unwrap()["metadata"]["annotations"][SOURCE_UID] =
                    "foreign".into()
            }
            "claim-workspace" => {
                initial.namespace.as_mut().unwrap()["metadata"]["annotations"][SOURCE_NAMESPACE] =
                    "workspace-b".into()
            }
            "claim-name" => {
                initial.namespace.as_mut().unwrap()["metadata"]["annotations"][SOURCE_NAME] =
                    "other".into()
            }
            "version" => {
                initial.namespace.as_mut().unwrap()["metadata"]["annotations"][VERSION] =
                    "v2".into()
            }
            "owner" => {
                initial.namespace.as_mut().unwrap()["metadata"]["ownerReferences"] =
                    json!([{"apiVersion":"v1","kind":"Namespace","name":"foreign","uid":"foreign"}])
            }
            "ns-delete" => {
                initial.namespace.as_mut().unwrap()["metadata"]["deletionTimestamp"] =
                    "2026-09-07T00:00:00Z".into()
            }
            "ns-phase" => {
                initial.namespace.as_mut().unwrap()["status"]["phase"] = "Terminating".into()
            }
            "cr-delete" => {
                initial.sandbox.as_mut().unwrap()["metadata"]["deletionTimestamp"] =
                    "2026-09-07T00:00:00Z".into()
            }
            "ns-no-uid" => {
                initial.namespace.as_mut().unwrap()["metadata"]["uid"] = serde_json::Value::Null
            }
            "ns-no-rv" => {
                initial.namespace.as_mut().unwrap()["metadata"]["resourceVersion"] =
                    serde_json::Value::Null
            }
            "cr-no-rv" => {
                initial.sandbox.as_mut().unwrap()["metadata"]["resourceVersion"] =
                    serde_json::Value::Null
            }
            _ => initial.namespace = None,
        }
        let (_server, client, state) = start(initial).await;
        assert!(write(&client, &target()).await.is_err(), "{shape}");
        let state = state.lock().unwrap();
        assert_eq!(state.value_attempts, 0);
        assert!(state.requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn uid_replacement_during_convergence_is_not_rebound_to_the_new_objects() {
    for scenario in [
        Scenario::ReplaceNamespaceWhileWaiting,
        Scenario::ReplaceSandboxWhileWaiting,
    ] {
        let mut initial = State {
            scenario,
            ..Default::default()
        };
        initial.sandbox.as_mut().unwrap()["metadata"]["annotations"] = json!({});
        initial.namespace.as_mut().unwrap()["metadata"]["annotations"] = json!({});
        let (_server, client, state) = start(initial).await;
        assert!(write(&client, &target()).await.is_err());
        assert!(
            state
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|request| request.method == "GET")
        );
    }
}

#[tokio::test]
async fn changed_claim_or_lifecycle_after_the_anchor_never_receives_credential_values() {
    for scenario in [
        Scenario::ReplaceNamespaceAtAnchor,
        Scenario::ReplaceSandboxAtAnchor,
        Scenario::TerminateNamespaceAtAnchor,
        Scenario::TerminateSandboxAtAnchor,
        Scenario::RemoveClaimAtAnchor,
    ] {
        let (_server, client, state) = start(State {
            scenario,
            ..Default::default()
        })
        .await;
        assert!(write(&client, &target()).await.is_err());
        let state = state.lock().unwrap();
        assert_eq!(state.value_attempts, 0);
        assert_eq!(state.value_writes, 0);
        assert!(
            state
                .requests
                .iter()
                .all(|request| !String::from_utf8_lossy(&request.body).contains(SENTINEL))
        );
    }
}

#[tokio::test]
async fn uid_version_fence_rejects_replacement_after_the_last_claim_read() {
    for scenario in [
        Scenario::ReplaceSecretAtWrite,
        Scenario::ReplaceNamespaceAtWrite,
        Scenario::ConflictAtWrite,
    ] {
        let (_server, client, state) = start(State {
            scenario,
            ..Default::default()
        })
        .await;
        let error = write(&client, &target()).await.unwrap_err();
        assert!(!error.contains(SENTINEL));
        let state = state.lock().unwrap();
        assert_eq!(state.value_attempts, 1);
        assert_eq!(state.value_writes, 0);
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|request| request.method == "PATCH")
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn api_failures_are_not_convergence_and_never_echo_secret_values() {
    for scenario in [
        Scenario::SandboxError(403),
        Scenario::SandboxError(404),
        Scenario::NamespaceError(403),
        Scenario::NamespaceError(500),
        Scenario::NamespaceError(503),
        Scenario::AnchorError(403),
        Scenario::WriteError(403),
        Scenario::WriteError(409),
        Scenario::WriteError(422),
    ] {
        let (_server, client, state) = start(State {
            scenario,
            ..Default::default()
        })
        .await;
        let error = write(&client, &target()).await.unwrap_err();
        assert!(error.contains("Kubernetes API status"));
        assert!(!error.contains(SENTINEL));
        let state = state.lock().unwrap();
        assert_eq!(state.value_writes, 0);
        assert!(state.namespace_reads <= 2);
    }
}

#[tokio::test]
async fn no_credentials_needs_no_cluster_access() {
    let (_server, client, state) = start(State::default()).await;
    write_credentials(
        &client,
        &target(),
        &BTreeMap::new(),
        "parent",
        Wait {
            attempts: 1,
            delay: Duration::ZERO,
        },
    )
    .await
    .unwrap();
    assert!(state.lock().unwrap().requests.is_empty());
}
