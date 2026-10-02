// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

pub(super) fn output(task: &Value) -> Value {
    let name = task["metadata"]["name"].as_str().unwrap();
    let uid = &task["metadata"]["uid"];
    let nonce = &task["metadata"]["annotations"][ANNOT_RUN_REQUESTED];
    json!({"apiVersion":"v1", "kind":"ConfigMap",
        "metadata":{"name":format!("kars-mission-output-{name}"), "namespace":"tenant-a",
            "annotations":{"kars.azure.com/mission-task-uid":uid, "kars.azure.com/mission-run-nonce":nonce,
                "kars.azure.com/mission-principal-name":name},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask", "name":name, "uid":uid, "controller":true}]},
        "data":{"taskName":name, "taskUid":uid, "assignmentNonce":nonce, "status":"ok",
            "totalTokens":"25", "artifactCount":"1", "output":"Useful run result.", "finishedAt":"2026-10-02T20:00:00Z"}})
}

async fn completed_run() -> (MockServer, Client, Arc<Mutex<Store>>, KarsTeam, String) {
    let mut team = team();
    team.spec.envelope.budget = None;
    let (server, client, store) = setup(&team).await;
    crate::team_commons::ensure_commons(&client, &team)
        .await
        .unwrap();
    let name = runs::cadence_name(&team).unwrap();
    tasks::apply_task(
        &Api::namespaced(client.clone(), "tenant-a"),
        &team,
        &name,
        specs::run_spec(&team, "").unwrap(),
        "taskforce",
    )
    .await
    .unwrap();
    {
        let mut state = store.lock().unwrap();
        state.tasks.get_mut(&name).unwrap()["metadata"]["annotations"]["kars.azure.com/run-completed"] =
            json!(name);
        let cm = output(&state.tasks[&name]);
        state.cms.insert(format!("kars-mission-output-{name}"), cm);
    }
    (server, client, store, team, name)
}

#[tokio::test]
async fn unbound_or_stale_output_never_counts_populates_commons_or_retires() {
    for (pointer, replacement) in [
        ("/metadata/annotations", json!({})),
        (
            "/metadata/annotations/kars.azure.com~1mission-task-uid",
            json!("old-uid"),
        ),
        (
            "/metadata/annotations/kars.azure.com~1mission-run-nonce",
            json!("old-nonce"),
        ),
        (
            "/metadata/annotations/kars.azure.com~1mission-principal-name",
            json!("other-task"),
        ),
        ("/metadata/ownerReferences", json!([])),
        ("/metadata/ownerReferences/0/uid", json!("foreign-uid")),
        ("/metadata/ownerReferences/0/name", json!("foreign-name")),
        ("/metadata/ownerReferences/0/kind", json!("KarsTeam")),
        ("/metadata/ownerReferences/0/apiVersion", json!("other/v1")),
        ("/metadata/ownerReferences/0/controller", json!(false)),
        ("/metadata/namespace", json!("other-namespace")),
        ("/metadata/name", json!("other-output")),
        ("/data/taskUid", json!("old-uid")),
        ("/data/taskName", json!("other-task")),
        ("/data/assignmentNonce", json!("old-nonce")),
    ] {
        let (server, client, store, team, name) = completed_run().await;
        {
            let mut state = store.lock().unwrap();
            *state
                .cms
                .get_mut(&format!("kars-mission-output-{name}"))
                .unwrap()
                .pointer_mut(pointer)
                .unwrap() = replacement;
        }
        let stats = runs::harvest_and_retire_runs(
            &client,
            &Api::namespaced(client.clone(), "tenant-a"),
            &team,
        )
        .await
        .unwrap();
        assert_eq!(
            (
                stats.active,
                stats.succeeded,
                stats.barren,
                stats.tokens_total
            ),
            (1, 0, 0, 0),
            "{pointer}"
        );
        assert_eq!(
            crate::team_commons::entry_count(&client, &team)
                .await
                .unwrap(),
            0,
            "{pointer}"
        );
        assert_eq!(
            store.lock().unwrap().tasks[&name]["spec"]["execution"]["launch"],
            true
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.method == "PUT")
        );
    }
}

#[tokio::test]
async fn task_refresh_after_output_rejects_revision_recreation_or_reassignment() {
    for mode in [
        "new-pending-run",
        "new-completed-run",
        "replacement",
        "foreign-team",
        "seat-role",
        "deleted",
    ] {
        let (server, client, store, team, name) = completed_run().await;
        let setup_requests = server.received_requests().await.unwrap().len();
        {
            let mut state = store.lock().unwrap();
            let mut task = state.tasks[&name].clone();
            match mode {
                "new-pending-run" => {
                    task["metadata"]["annotations"][ANNOT_RUN_REQUESTED] = json!("rev-new")
                }
                "new-completed-run" => {
                    task["metadata"]["annotations"][ANNOT_RUN_REQUESTED] = json!("rev-new");
                    task["metadata"]["annotations"]["kars.azure.com/run-completed"] =
                        json!("rev-new");
                }
                "replacement" => task["metadata"]["uid"] = json!("replacement-uid"),
                "foreign-team" => {
                    task["metadata"]["ownerReferences"][0]["uid"] = json!("foreign-team-uid")
                }
                "seat-role" => task["metadata"]["annotations"][ANNOT_TEAM_ROLE] = json!("seat"),
                "deleted" => task["metadata"]["deletionTimestamp"] = json!("2026-10-02T20:00:00Z"),
                _ => unreachable!(),
            }
            state.task_on_output_read = Some(task);
        }
        let stats = runs::harvest_and_retire_runs(
            &client,
            &Api::namespaced(client.clone(), "tenant-a"),
            &team,
        )
        .await
        .unwrap();
        assert_eq!(
            (
                stats.active,
                stats.succeeded,
                stats.barren,
                stats.tokens_total
            ),
            (1, 0, 0, 0),
            "{mode}"
        );
        assert_eq!(
            crate::team_commons::entry_count(&client, &team)
                .await
                .unwrap(),
            0
        );
        let requests = server.received_requests().await.unwrap();
        let reads = &requests[setup_requests..];
        let output_read = reads
            .iter()
            .position(|r| r.url.path().contains("/configmaps/kars-mission-output-"))
            .unwrap();
        let fresh_read = reads
            .iter()
            .position(|r| r.method == "GET" && r.url.path() == format!("{TASKS_PATH}/{name}"))
            .unwrap();
        assert!(output_read < fresh_read, "{mode}");
        assert!(!reads.iter().any(|r| r.method == "PUT"), "{mode}");
    }
}

#[tokio::test]
async fn malformed_completion_is_rejected_before_commons_write() {
    for (field, value) in [
        ("finishedAt", "not-a-date"),
        ("totalTokens", "-1"),
        ("artifactCount", "no-count"),
    ] {
        let (server, client, store, team, name) = completed_run().await;
        store
            .lock()
            .unwrap()
            .cms
            .get_mut(&format!("kars-mission-output-{name}"))
            .unwrap()["data"][field] = json!(value);
        assert!(
            runs::harvest_and_retire_runs(
                &client,
                &Api::namespaced(client.clone(), "tenant-a"),
                &team
            )
            .await
            .is_err(),
            "{field}"
        );
        assert_eq!(
            crate::team_commons::entry_count(&client, &team)
                .await
                .unwrap(),
            0
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.method == "PUT")
        );
    }
}

#[tokio::test]
async fn revisions_have_distinct_commons_keys_and_retries_deduplicate() {
    let (_server, client, store, team, name) = completed_run().await;
    let tasks_api = Api::namespaced(client.clone(), "tenant-a");
    for nonce in [name.as_str(), "rev-2"] {
        {
            let mut state = store.lock().unwrap();
            let task = state.tasks.get_mut(&name).unwrap();
            task["metadata"]["annotations"][ANNOT_RUN_REQUESTED] = json!(nonce);
            task["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!(nonce);
            task["spec"]["execution"]["launch"] = json!(true);
            let mut cm = output(task);
            cm["data"]["output"] = json!(format!("Useful result for {nonce}"));
            state.cms.insert(format!("kars-mission-output-{name}"), cm);
        }
        for _ in 0..2 {
            let stats = runs::harvest_and_retire_runs(&client, &tasks_api, &team)
                .await
                .unwrap();
            assert_eq!(
                (stats.active, stats.succeeded, stats.tokens_total),
                (0, 1, 25)
            );
        }
    }
    assert_eq!(
        crate::team_commons::entry_count(&client, &team)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn concurrent_revision_after_harvest_cannot_be_retired() {
    let (_server, client, store, team, name) = completed_run().await;
    {
        let mut state = store.lock().unwrap();
        let mut task = state.tasks[&name].clone();
        task["metadata"]["resourceVersion"] = json!("new-rv");
        task["metadata"]["annotations"][ANNOT_RUN_REQUESTED] = json!("rev-2");
        state.task_on_retirement = Some(task);
    }
    assert!(
        runs::harvest_and_retire_runs(&client, &Api::namespaced(client.clone(), "tenant-a"), &team)
            .await
            .is_err()
    );
    let state = store.lock().unwrap();
    assert_eq!(state.tasks[&name]["spec"]["execution"]["launch"], true);
    assert_eq!(
        state.tasks[&name]["metadata"]["annotations"][ANNOT_RUN_REQUESTED],
        "rev-2"
    );
}
