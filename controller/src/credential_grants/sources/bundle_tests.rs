// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::Value;

mod fixture;

fn assert_no_values(f: &fixture::Fixture) {
    let state = f.state.lock().unwrap();
    assert_eq!(state.writes, 0);
    assert!(state.calls.iter().all(|(method, _, _)| method != "DELETE"));
    assert!(
        state
            .calls
            .iter()
            .filter(|(method, path, _)| method == "PATCH" && path == &state.target_path)
            .all(|(_, _, patch)| patch
                .as_object()
                .unwrap()
                .keys()
                .all(|key| key == "metadata"))
    );
}

fn forged(f: &fixture::Fixture, filled: bool) -> Value {
    json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":bundle_name(&f.target),"namespace":f.target.namespace,
            "uid":"lookalike-uid","resourceVersion":"30","ownerReferences":[owner_ref(&f.target)],
            "annotations":{PURPOSE:BUNDLE_PURPOSE,TARGET_KIND:f.target.kind,TARGET:f.target.name,
                TARGET_UID:f.target.uid,WORKSPACE:f.target.namespace,GRANT_UID:f.bindings.grant.uid}},
        "data":if filled {json!({"TELEGRAM_BOT_TOKEN":ByteString(b"untouched".to_vec())})} else {json!({})}})
}

#[tokio::test]
async fn exclusive_create_then_real_target_cas_conflict_recovers_only_the_anchor() {
    for change in ["suspension", "status"] {
        let f = fixture::setup("KarsSandbox").await;
        f.state.lock().unwrap().after_create = Some(change);
        assert_eq!(f.prepare().await.unwrap_err(), bundle::RECONCILE_REQUIRED);
        assert_no_values(&f);
        {
            let state = f.state.lock().unwrap();
            assert_eq!(state.creates, 1);
            assert_eq!(state.anchors, 2);
            assert_eq!(
                state.cas_conflicts, 1,
                "the first PATCH must fail the actual UID/RV comparison"
            );
            let patches: Vec<_> = state
                .calls
                .iter()
                .filter(|(method, path, _)| method == "PATCH" && path == &state.target_path)
                .map(|(_, _, patch)| patch)
                .collect();
            assert_eq!(patches[0]["metadata"]["resourceVersion"], "10");
            assert_ne!(patches[1]["metadata"]["resourceVersion"], "10");
            for patch in patches {
                assert_eq!(patch["metadata"]["uid"], "target-uid");
                assert_eq!(
                    patch["metadata"]["annotations"][bundle::UID_ANNOTATION],
                    "exclusive-bundle-uid"
                );
            }
            assert_eq!(
                state.objects[&state.target_path]["metadata"]["annotations"]
                    [bundle::UID_ANNOTATION],
                "exclusive-bundle-uid"
            );
            assert!(state.objects[&state.bundle_path].get("data").is_none());
            assert!(
                state
                    .calls
                    .iter()
                    .all(|(method, path, _)| method != "PATCH" || path == &state.target_path)
            );
        }
        {
            let mut state = f.state.lock().unwrap();
            fixture::mutate(&mut state, "source-value");
            fixture::mutate(&mut state, "status");
        }
        let ready = f.prepare().await.unwrap();
        assert_eq!(ready.metadata.uid.as_deref(), Some("exclusive-bundle-uid"));
        assert_eq!(
            ready.data.as_ref().unwrap()["TELEGRAM_BOT_TOKEN"].0,
            b"fresh-value"
        );
        let input: Value =
            serde_json::from_str(annotation(&ready.metadata, INPUT_STATE).unwrap()).unwrap();
        assert_eq!(input["sources"][0]["resourceVersion"], "41");
        assert_eq!(f.state.lock().unwrap().writes, 1);
        assert_eq!(f.state.lock().unwrap().creates, 1);
    }
}

#[tokio::test]
async fn ordinary_sandbox_task_and_team_creation_keep_existing_value_fences_and_idempotence() {
    for kind in ["KarsSandbox", "KarsTask", "KarsTeam"] {
        let f = fixture::setup(kind).await;
        let ready = f.prepare().await.unwrap();
        assert_eq!(
            ready.data.as_ref().unwrap()["TELEGRAM_BOT_TOKEN"].0,
            b"initial-value"
        );
        let again = f.prepare().await.unwrap();
        assert_eq!(ready.metadata.uid, again.metadata.uid);
        assert_eq!(ready.data, again.data);
        let state = f.state.lock().unwrap();
        assert_eq!(
            (state.creates, state.anchors, state.writes),
            (1, 1, 1),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn preexisting_unanchored_lookalikes_are_never_adopted_or_cleared() {
    for filled in [false, true] {
        let f = fixture::setup("KarsSandbox").await;
        let original = forged(&f, filled);
        {
            let mut state = f.state.lock().unwrap();
            let path = state.bundle_path.clone();
            state.objects.insert(path, original.clone());
        }
        for _ in 0..2 {
            assert_eq!(
                f.prepare().await.unwrap_err(),
                "Existing credential bundle is not owned by the exact target"
            );
        }
        assert_no_values(&f);
        let state = f.state.lock().unwrap();
        assert_eq!((state.creates, state.anchors), (0, 0));
        assert_eq!(state.objects[&state.bundle_path], original);
    }
}

#[tokio::test]
async fn anchor_recovery_rejects_changed_target_grant_workspace_or_source_authority() {
    for change in [
        "target-uid",
        "target-delete",
        "target-bindings",
        "target-direct",
        "target-spec",
        "target-owner",
        "target-anchor",
        "target-annotation",
        "target-suspension-type",
        "grant-uid",
        "grant-rv",
        "grant-spec",
        "grant-unready",
        "grant-delete",
        "namespace-uid",
        "namespace-delete",
        "source-uid",
        "source-rv",
        "source-value",
        "source-delete",
    ] {
        let f = fixture::setup("KarsSandbox").await;
        f.state.lock().unwrap().after_create = Some(change);
        let error = f.prepare().await.unwrap_err();
        assert_ne!(error, bundle::RECONCILE_REQUIRED, "{change}");
        assert!(!error.contains("PRIVATE_SERVER_DETAIL"));
        assert_no_values(&f);
        let state = f.state.lock().unwrap();
        assert_eq!(
            state.anchors, 1,
            "{change}: no rebased anchor with changed authority"
        );
        assert_eq!(state.cas_conflicts, 1);
        assert!(state.objects[&state.bundle_path].get("data").is_none());
    }
}

#[tokio::test]
async fn anchor_recovery_keeps_the_captured_create_uid_rv_empty_state_and_all_owner_tags() {
    for change in [
        "bundle-uid",
        "bundle-rv",
        "bundle-data",
        "bundle-string-data",
        "bundle-owner",
        "bundle-purpose",
        "bundle-kind",
        "bundle-target",
        "bundle-workspace",
        "bundle-grant",
        "bundle-type",
        "bundle-state",
        "bundle-immutable",
        "bundle-delete",
    ] {
        let f = fixture::setup("KarsSandbox").await;
        f.state.lock().unwrap().after_create = Some(change);
        assert!(f.prepare().await.is_err(), "{change}");
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1, "{change}");
        if change == "bundle-data" {
            let persisted: Secret = serde_json::from_value(f.bundle()).unwrap();
            assert_eq!(
                persisted.data.unwrap()["TELEGRAM_BOT_TOKEN"].0,
                b"foreign-value"
            );
        }
    }
}

#[tokio::test]
async fn malformed_or_nonempty_create_responses_never_authorize_even_the_first_anchor_patch() {
    for change in [
        "bundle-no-uid",
        "bundle-no-rv",
        "bundle-name",
        "bundle-namespace",
        "bundle-data",
        "bundle-string-data",
        "bundle-owner",
        "bundle-purpose",
        "bundle-kind",
        "bundle-target",
        "bundle-workspace",
        "bundle-grant",
        "bundle-type",
        "bundle-state",
        "bundle-immutable",
        "bundle-delete",
    ] {
        let f = fixture::setup("KarsSandbox").await;
        f.state.lock().unwrap().create_response = Some(change);
        assert!(f.prepare().await.is_err(), "{change}");
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 0, "{change}");
    }
}

#[tokio::test]
async fn absent_create_acknowledgement_is_not_reconstructed_from_a_later_get() {
    for malformed in [false, true] {
        let f = fixture::setup("KarsSandbox").await;
        {
            let mut state = f.state.lock().unwrap();
            state.lose_create_ack = !malformed;
            state.malformed_create_ack = malformed;
        }
        let error = f.prepare().await.unwrap_err();
        assert!(!error.contains("PRIVATE_SERVER_DETAIL"));
        let created = f.bundle();
        assert!(
            f.prepare()
                .await
                .unwrap_err()
                .contains("not owned by the exact target")
        );
        assert_no_values(&f);
        let state = f.state.lock().unwrap();
        assert_eq!((state.creates, state.anchors), (1, 0));
        assert_eq!(state.objects[&state.bundle_path], created);
    }
}

#[tokio::test]
async fn create_failures_and_nonconflict_anchor_failures_are_never_retried_as_adoption() {
    for status in [403, 409, 500] {
        let f = fixture::setup("KarsSandbox").await;
        f.state.lock().unwrap().create_error = Some(status);
        assert!(f.prepare().await.is_err());
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 0);
    }
    for status in [403, 422, 500] {
        let f = fixture::setup("KarsSandbox").await;
        f.state.lock().unwrap().anchor_error = Some((1, status));
        assert!(f.prepare().await.is_err());
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1);
        assert!(
            f.prepare()
                .await
                .unwrap_err()
                .contains("not owned by the exact target")
        );
    }
}

#[tokio::test]
async fn lost_anchor_acknowledgements_require_fresh_prepare_not_stale_values() {
    for attempt in [1, 2] {
        let f = fixture::setup("KarsSandbox").await;
        {
            let mut state = f.state.lock().unwrap();
            if attempt == 2 {
                state.after_create = Some("suspension");
            }
            state.lose_anchor_ack = Some(attempt);
        }
        assert!(f.prepare().await.is_err());
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, attempt);
        {
            let mut state = f.state.lock().unwrap();
            fixture::mutate(&mut state, "source-value");
        }
        assert_eq!(
            f.prepare().await.unwrap().data.unwrap()["TELEGRAM_BOT_TOKEN"].0,
            b"fresh-value"
        );
        assert_eq!(f.state.lock().unwrap().writes, 1);
    }
}

#[tokio::test]
async fn recovery_read_failures_and_persistent_conflicts_leave_empty_objects_for_operator_recovery()
{
    for stage in ["target", "grant", "namespace", "source", "bundle"] {
        let f = fixture::setup("KarsSandbox").await;
        {
            let mut state = f.state.lock().unwrap();
            state.after_create = Some("status");
            state.fail_recovery_get = Some(stage);
        }
        assert!(f.prepare().await.is_err(), "{stage}");
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1);
    }
    let f = fixture::setup("KarsSandbox").await;
    f.state.lock().unwrap().persistent_conflict = true;
    assert!(
        f.prepare()
            .await
            .unwrap_err()
            .contains("exhausted metadata CAS retries")
    );
    assert_no_values(&f);
    assert_eq!(
        f.state.lock().unwrap().anchors,
        1 + bundle::RECOVERY_PATCHES
    );
    let held = f.bundle();
    assert!(
        f.prepare()
            .await
            .unwrap_err()
            .contains("not owned by the exact target")
    );
    assert_eq!(f.bundle(), held);
    assert_eq!(
        f.state.lock().unwrap().anchors,
        1 + bundle::RECOVERY_PATCHES
    );
}

#[tokio::test]
async fn ordinary_value_writes_still_reject_target_grant_source_and_bundle_cas_changes() {
    for change in ["status", "grant-rv", "source-value", "bundle-rv"] {
        let f = fixture::setup("KarsSandbox").await;
        f.state.lock().unwrap().after_anchor = Some(change);
        assert!(f.prepare().await.is_err(), "{change}");
        assert_no_values(&f);
        if change == "bundle-rv" {
            let state = f.state.lock().unwrap();
            let attempted = state
                .calls
                .iter()
                .find(|(method, path, _)| method == "PATCH" && path == &state.bundle_path)
                .unwrap();
            assert_eq!(attempted.2["metadata"]["resourceVersion"], "30");
            assert_eq!(state.cas_conflicts, 1);
        }
        let fresh = f.prepare().await.unwrap();
        assert_eq!(fresh.metadata.uid.as_deref(), Some("exclusive-bundle-uid"));
        assert_eq!(f.state.lock().unwrap().writes, 1);
    }
}

#[tokio::test]
async fn an_already_recorded_exact_create_uid_still_requires_fresh_values() {
    let f = fixture::setup("KarsSandbox").await;
    f.state.lock().unwrap().after_create = Some("target-anchor-current");
    assert_eq!(f.prepare().await.unwrap_err(), bundle::RECONCILE_REQUIRED);
    assert_no_values(&f);
    assert_eq!(f.state.lock().unwrap().anchors, 1);
    assert_eq!(f.state.lock().unwrap().cas_conflicts, 1);
    f.prepare().await.unwrap();
    assert_eq!(f.state.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn task_and_team_conflicts_do_not_rebase_their_authority() {
    for kind in ["KarsTask", "KarsTeam"] {
        let f = fixture::setup(kind).await;
        f.state.lock().unwrap().after_create = Some("status");
        assert!(
            f.prepare()
                .await
                .unwrap_err()
                .contains("Kubernetes status 409")
        );
        assert_no_values(&f);
        assert_eq!(f.state.lock().unwrap().anchors, 1);
    }
}

#[tokio::test]
async fn anchored_bundles_still_require_complete_owned_metadata_without_rewriting_values() {
    for change in [
        "bundle-kind",
        "bundle-target",
        "bundle-workspace",
        "bundle-name",
        "bundle-namespace",
        "bundle-delete",
    ] {
        let f = fixture::setup("KarsSandbox").await;
        f.prepare().await.unwrap();
        {
            let mut state = f.state.lock().unwrap();
            fixture::mutate(&mut state, change);
        }
        let original = f.bundle();
        assert!(f.prepare().await.is_err(), "{change}");
        assert_eq!(f.bundle(), original);
        assert_eq!(f.state.lock().unwrap().writes, 1);
        assert_eq!(f.state.lock().unwrap().creates, 1);
    }
}

#[tokio::test]
async fn a_missing_previously_anchored_bundle_is_not_recreated() {
    let f = fixture::setup("KarsSandbox").await;
    f.prepare().await.unwrap();
    {
        let mut state = f.state.lock().unwrap();
        let path = state.bundle_path.clone();
        state.objects.remove(&path);
    }
    assert!(
        f.prepare()
            .await
            .unwrap_err()
            .contains("Previously bound credential bundle disappeared")
    );
    assert_eq!(f.state.lock().unwrap().creates, 1);
    assert_eq!(f.state.lock().unwrap().writes, 1);
}

#[tokio::test]
async fn changes_after_recovery_checks_are_consumed_only_by_fresh_authority() {
    let mut f = fixture::setup("KarsSandbox").await;
    {
        let mut state = f.state.lock().unwrap();
        state.after_create = Some("status");
        state.after_anchor = Some("target-bindings");
    }
    assert_eq!(f.prepare().await.unwrap_err(), bundle::RECONCILE_REQUIRED);
    assert_no_values(&f);
    assert!(f.prepare().await.unwrap_err().contains("bindings differ"));
    assert_no_values(&f);
    {
        let state = f.state.lock().unwrap();
        f.bindings = serde_json::from_value(
            state.objects[&state.target_path]["spec"]["credentialBindings"].clone(),
        )
        .unwrap();
    }
    assert!(f.prepare().await.unwrap().data.unwrap().is_empty());
    assert_eq!(f.state.lock().unwrap().writes, 1);

    let f = fixture::setup("KarsSandbox").await;
    {
        let mut state = f.state.lock().unwrap();
        state.after_create = Some("status");
        state.after_anchor = Some("source-value");
    }
    assert_eq!(f.prepare().await.unwrap_err(), bundle::RECONCILE_REQUIRED);
    assert_no_values(&f);
    assert_eq!(
        f.prepare().await.unwrap().data.unwrap()["TELEGRAM_BOT_TOKEN"].0,
        b"fresh-value"
    );
}
