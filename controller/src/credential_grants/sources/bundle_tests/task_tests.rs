// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{crd::KarsSandbox, kars_task::KarsTask};

const CONSUMER: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/agent";

async fn materialized() -> fixture::Fixture {
    let mut f = fixture::setup("KarsTask").await;
    f.bindings.sources = vec![CredentialSelection {
        scope: CredentialScope::Workspace,
        source: ObjectIdentity {
            name: format!("{INPUT_PREFIX}workspace"),
            uid: "workspace-source-uid".into(),
        },
        keys: vec!["SLACK_BOT_TOKEN".into()],
        owner: None,
    }];
    {
        let mut state = f.state.lock().unwrap();
        let old_source = state.source_path.clone();
        state.objects.remove(&old_source);
        state.source_path = format!("{}/{}", fixture::SECRETS, f.bindings.sources[0].source.name);
        let source_path = state.source_path.clone();
        state.objects.insert(source_path, json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":f.bindings.sources[0].source.name,"namespace":"work","uid":"workspace-source-uid","resourceVersion":"40",
                "ownerReferences":[{"apiVersion":"v1","kind":"Namespace","name":"work","uid":"workspace-uid","controller":true,"blockOwnerDeletion":false}],
                "annotations":{PURPOSE:INPUT_PURPOSE,WORKSPACE:"work",TARGET_KIND:"Workspace",TARGET:"work",
                    TARGET_UID:"workspace-uid",GRANT_UID:"grant-uid",INTENT:"explicit-reference-v2",
                    "kars.azure.com/credential-import-revision":"enrolled"}},
            "data":{"SLACK_BOT_TOKEN":ByteString(b"initial-task-value".to_vec())}}));
        let task_path = state.target_path.clone();
        let task = state.objects.get_mut(&task_path).unwrap();
        task["spec"]["blueprint"]["credentialBindings"] = json!(f.bindings);
        let typed: KarsTask = serde_json::from_value(task.clone()).unwrap();
        task["status"]["envelopeDigest"] = json!(typed.envelope_digest());
        task["status"]["executionPhase"] = json!("Launching");
        state.consumer_path = Some(CONSUMER.into());
        state.objects.insert(CONSUMER.into(), json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"agent","namespace":"work","uid":"materialized-sandbox-uid","resourceVersion":"50","generation":1,
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"agent","uid":f.target.uid,
                    "controller":true,"blockOwnerDeletion":true}]},
            "spec":{"inferenceRef":{"name":"policy"},"sandbox":{"isolation":"standard"},"suspended":false,
                "credentialBindings":f.bindings}}));
    }
    f
}

fn snapshot(f: &fixture::Fixture) -> KarsSandbox {
    serde_json::from_value(f.state.lock().unwrap().objects[CONSUMER].clone()).unwrap()
}

async fn consume(f: &fixture::Fixture) -> Result<Secret, String> {
    super::super::for_sandbox(&f.client, &snapshot(f)).await
}

#[tokio::test]
async fn materialized_task_execution_status_cas_recovers_its_task_anchor_not_its_sandbox() {
    for change in ["task-progress", "consumer-status"] {
        let f = materialized().await;
        f.state.lock().unwrap().after_create = Some(change);
        assert_eq!(consume(&f).await.unwrap_err(), bundle::RECONCILE_REQUIRED);
        assert_no_values(&f);
        {
            let state = f.state.lock().unwrap();
            assert_eq!(
                (state.creates, state.anchors, state.cas_conflicts),
                (1, 2, 1)
            );
            let task = &state.objects[&state.target_path];
            assert_eq!(
                task["metadata"]["annotations"][bundle::UID_ANNOTATION],
                "exclusive-bundle-uid"
            );
            assert_eq!(task["status"]["phase"], "Ready");
            assert_eq!(
                task["metadata"]["generation"],
                task["status"]["observedGeneration"]
            );
            if change == "task-progress" {
                assert_eq!(task["status"]["executionPhase"], "Degraded");
            }
            assert!(
                state.objects[CONSUMER]["metadata"]["annotations"][bundle::UID_ANNOTATION]
                    .is_null()
            );
            assert!(
                state
                    .calls
                    .iter()
                    .filter(|(method, _, _)| method == "PATCH")
                    .all(|(_, path, _)| path == &state.target_path)
            );
            assert!(state.objects[&state.bundle_path].get("data").is_none());
        }
        fixture::mutate(&mut f.state.lock().unwrap(), "source-slack-value");
        let values = consume(&f).await.unwrap();
        assert_eq!(
            values.data.as_ref().unwrap()["SLACK_BOT_TOKEN"].0,
            b"fresh-task-value"
        );
        assert_eq!(
            values.metadata.owner_references.as_ref().unwrap()[0].uid,
            f.target.uid
        );
        let input: Value =
            serde_json::from_str(annotation(&values.metadata, INPUT_STATE).unwrap()).unwrap();
        assert_eq!(input["target"]["kind"], "KarsTask");
        assert_eq!(input["sources"][0]["uid"], "workspace-source-uid");
        assert_eq!(input["sources"][0]["resourceVersion"], "41");
        assert_eq!(f.state.lock().unwrap().writes, 1);
    }
}

#[tokio::test]
async fn task_runtime_without_explicit_launch_cannot_create_or_recover_a_bundle() {
    for execution in [None, Some(json!({"launch":false}))] {
        let f = materialized().await;
        {
            let mut state = f.state.lock().unwrap();
            let task_path = state.target_path.clone();
            let task = state.objects.get_mut(&task_path).unwrap();
            if let Some(value) = execution {
                task["spec"]["execution"] = value;
            } else {
                task["spec"].as_object_mut().unwrap().remove("execution");
            }
            let typed: KarsTask = serde_json::from_value(task.clone()).unwrap();
            task["status"]["envelopeDigest"] = json!(typed.envelope_digest());
        }
        assert!(consume(&f).await.is_err());
        assert_no_values(&f);
        let state = f.state.lock().unwrap();
        assert_eq!((state.creates, state.anchors), (0, 0));
    }
}

#[tokio::test]
async fn task_recovery_rejects_changed_governance_spec_parent_or_authority_status() {
    for change in [
        "target-uid",
        "target-delete",
        "target-owner",
        "target-annotation",
        "task-phase",
        "task-ready-condition",
        "task-envelope",
        "task-generation",
        "task-observed",
        "task-spec",
        "task-bindings",
        "task-parent",
        "task-lineage",
        "task-budget",
        "task-launch",
        "task-sandbox-ref",
        "task-stopping",
        "task-detail-type",
        "task-unknown-status",
    ] {
        let f = materialized().await;
        f.state.lock().unwrap().after_create = Some(change);
        let error = consume(&f).await.unwrap_err();
        assert_ne!(error, bundle::RECONCILE_REQUIRED, "{change}");
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1, "{change}");
    }
}

#[tokio::test]
async fn task_recovery_requires_the_same_actual_materialized_consumer() {
    for change in [
        "consumer-uid",
        "consumer-name",
        "consumer-namespace",
        "consumer-delete",
        "consumer-generation",
        "consumer-owner",
        "consumer-spec",
        "consumer-bindings",
        "consumer-legacy",
        "consumer-annotation",
    ] {
        let f = materialized().await;
        f.state.lock().unwrap().after_create = Some(change);
        assert!(consume(&f).await.is_err(), "{change}");
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1, "{change}");
    }
    let f = materialized().await;
    let old = snapshot(&f);
    fixture::mutate(&mut f.state.lock().unwrap(), "consumer-uid");
    assert!(super::super::for_sandbox(&f.client, &old).await.is_err());
    assert_no_values(&f);
    assert_eq!(f.state.lock().unwrap().creates, 0);
}

#[tokio::test]
async fn task_recovery_keeps_grant_source_namespace_and_empty_create_fences() {
    for change in [
        "grant-uid",
        "grant-rv",
        "grant-spec",
        "namespace-uid",
        "namespace-delete",
        "source-uid",
        "source-rv",
        "source-delete",
        "source-slack-value",
        "bundle-uid",
        "bundle-rv",
        "bundle-data",
        "bundle-owner",
        "bundle-purpose",
        "bundle-workspace",
        "bundle-type",
        "bundle-state",
        "bundle-delete",
    ] {
        let f = materialized().await;
        f.state.lock().unwrap().after_create = Some(change);
        assert!(consume(&f).await.is_err(), "{change}");
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1, "{change}");
    }
}

#[tokio::test]
async fn a_ready_task_and_real_consumer_do_not_authorize_an_existing_unanchored_lookalike() {
    let f = materialized().await;
    let original = forged(&f, false);
    {
        let mut state = f.state.lock().unwrap();
        let path = state.bundle_path.clone();
        state.objects.insert(path, original.clone());
    }
    assert!(
        consume(&f)
            .await
            .unwrap_err()
            .contains("not owned by the exact target")
    );
    assert_no_values(&f);
    assert_eq!(f.bundle(), original);
    let state = f.state.lock().unwrap();
    assert_eq!((state.creates, state.anchors), (0, 0));
}

#[tokio::test]
async fn task_caller_without_create_ack_or_with_nonconflict_failure_cannot_recover() {
    for malformed in [false, true] {
        let f = materialized().await;
        {
            let mut state = f.state.lock().unwrap();
            state.lose_create_ack = !malformed;
            state.malformed_create_ack = malformed;
        }
        assert!(consume(&f).await.is_err());
        assert!(
            consume(&f)
                .await
                .unwrap_err()
                .contains("not owned by the exact target")
        );
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 0);
    }
    for code in [403, 422, 500] {
        let f = materialized().await;
        f.state.lock().unwrap().anchor_error = Some((1, code));
        assert!(consume(&f).await.is_err());
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1);
    }
}

#[tokio::test]
async fn task_conflicts_are_bounded_and_lost_anchor_ack_still_requires_fresh_input() {
    let f = materialized().await;
    f.state.lock().unwrap().persistent_conflict = true;
    assert!(
        consume(&f)
            .await
            .unwrap_err()
            .contains("exhausted metadata CAS retries")
    );
    assert_no_values(&f);
    assert_eq!(
        f.state.lock().unwrap().anchors,
        1 + bundle::RECOVERY_PATCHES
    );
    assert!(
        consume(&f)
            .await
            .unwrap_err()
            .contains("not owned by the exact target")
    );

    let f = materialized().await;
    {
        let mut state = f.state.lock().unwrap();
        state.after_create = Some("task-progress");
        state.lose_anchor_ack = Some(2);
    }
    assert!(consume(&f).await.is_err());
    assert_no_values(&f);
    fixture::mutate(&mut f.state.lock().unwrap(), "source-slack-value");
    assert_eq!(
        consume(&f).await.unwrap().data.unwrap()["SLACK_BOT_TOKEN"].0,
        b"fresh-task-value"
    );
}

#[tokio::test]
async fn task_and_consumer_revocation_after_metadata_recovery_cannot_write_old_values() {
    for change in [
        "task-launch",
        "task-bindings",
        "consumer-bindings",
        "consumer-owner",
        "consumer-legacy",
    ] {
        let f = materialized().await;
        {
            let mut state = f.state.lock().unwrap();
            state.after_create = Some("task-progress");
            state.after_anchor = Some(change);
        }
        assert_eq!(consume(&f).await.unwrap_err(), bundle::RECONCILE_REQUIRED);
        assert_no_values(&f);
        assert!(consume(&f).await.is_err(), "{change}");
        assert_no_values(&f);
    }
}

#[tokio::test]
async fn materialized_v2_consumer_can_retain_only_its_existing_exact_bundle_reference() {
    let f = materialized().await;
    let bundle = consume(&f).await.unwrap();
    {
        let mut state = f.state.lock().unwrap();
        state.objects.get_mut(CONSUMER).unwrap()["spec"]["credentialsRef"] =
            json!({"name":bundle.name_any(),"uid":bundle.metadata.uid});
        fixture::bump(state.objects.get_mut(CONSUMER).unwrap());
    }
    assert_eq!(consume(&f).await.unwrap().metadata.uid, bundle.metadata.uid);
    assert_eq!(f.state.lock().unwrap().writes, 1);
    fixture::mutate(&mut f.state.lock().unwrap(), "consumer-legacy");
    assert!(consume(&f).await.is_err());
    assert_eq!(f.state.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn materialized_consumer_is_rechecked_before_normal_value_writes() {
    for change in [
        "consumer-uid",
        "consumer-owner",
        "consumer-spec",
        "task-phase",
    ] {
        let f = materialized().await;
        f.state.lock().unwrap().after_anchor = Some(change);
        assert!(consume(&f).await.is_err(), "{change}");
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1);
    }
}
