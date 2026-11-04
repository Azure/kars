// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn verified_specialist_failure_releases_exact_work_without_principal_output() {
    use super::work::contract;
    for runtime in ["OpenClaw", "Hermes"] {
        for status in ["failed", "rejected"] {
            let owner = contract::Owner {
                namespace: "default".into(),
                name: "clinic".into(),
                uid: "clinic-uid".into(),
            };
            let mut item = contract::TeamTask {
                id: "report".into(),
                title: "Reviewed report".into(),
                status: "pending".into(),
                review_required: true,
                ..Default::default()
            };
            item.history.push(contract::Attempt {
                run: "earlier".into(),
                run_uid: "earlier-uid".into(),
                assignment_nonce: "earlier".into(),
                outcome: "failed".into(),
                finished_at: "2026-10-01T20:00:00Z".into(),
                output_sha256: Some("a".repeat(64)),
                feedback: Some("Retain prior failure".into()),
            });
            let queued = contract::TeamTask {
                id: "later".into(),
                title: "Later reviewed work".into(),
                status: "pending".into(),
                review_required: true,
                ..Default::default()
            };
            let binding = contract::Binding {
                backlog_uid: format!("uid-{}", owner.configmap_name()),
                item_id: item.id.clone(),
                request_digest: contract::request_digest(&item).unwrap(),
            };
            let (_server, client, objects, team, root) =
                crate::reviewed_execution::failed_team_fixture(
                    runtime,
                    serde_json::to_string(&binding).unwrap(),
                    status,
                )
                .await;
            let cms = Api::namespaced(client.clone(), "default");
            contract::update(&cms, &owner, None, true, |items| {
                *items = vec![item.clone(), queued.clone()];
                Ok(())
            })
            .await
            .unwrap();
            work::claim(&client, &team, &root, true).await.unwrap();
            let before = objects.lock().unwrap().clone();
            let tasks = Api::<crate::kars_task::KarsTask>::namespaced(client.clone(), "default");
            wiremock::Mock::given(wiremock::matchers::method("PUT"))
                .and(wiremock::matchers::path(
                    "/apis/kars.azure.com/v1alpha1/namespaces/default/karstasks/report",
                ))
                .respond_with(wiremock::ResponseTemplate::new(409).set_body_json(
                    serde_json::json!({
                        "apiVersion":"v1", "kind":"Status", "status":"Failure", "code":409,
                        "reason":"Conflict", "message":"concurrent root update"
                    }),
                ))
                .with_priority(1)
                .up_to_n_times(1)
                .expect(1)
                .mount(&_server)
                .await;
            assert!(
                runs::harvest_and_retire_runs(&client, &tasks, &team)
                    .await
                    .is_err()
            );
            let saved = contract::read(&cms, &owner).await.unwrap().unwrap().1;
            assert_eq!(saved[0].history.len(), 2);
            assert!(
                tasks
                    .get("report")
                    .await
                    .unwrap()
                    .spec
                    .execution
                    .unwrap()
                    .launch
            );
            let stats = runs::harvest_and_retire_runs(&client, &tasks, &team)
                .await
                .unwrap();
            assert_eq!(
                contract::read(&cms, &owner).await.unwrap().unwrap().1,
                saved
            );
            assert_eq!(
                (
                    stats.active,
                    stats.barren,
                    stats.succeeded,
                    stats.tokens_total
                ),
                (0, 1, 0, 0)
            );
            let (_, items) = contract::read(&cms, &owner).await.unwrap().unwrap();
            assert_eq!(items[0].status, "pending");
            assert!(
                items[0].run.is_none()
                    && items[0].run_uid.is_none()
                    && items[0].assignment_nonce.is_none()
            );
            assert_eq!(items[0].history[0], item.history[0]);
            assert_eq!(items[0].history.len(), 2);
            assert_eq!(items[0].history[1].outcome, "failed");
            assert_eq!(items[0].history[1].run_uid, root.uid().unwrap());
            assert_eq!(items[0].history[1].assignment_nonce, root.name_any());
            assert!(items[0].history[1].output_sha256.is_some());
            assert_eq!(items[1], queued);
            let retired = tasks.get(&root.name_any()).await.unwrap();
            assert!(!retired.spec.execution.as_ref().unwrap().launch);
            assert!(
                !retired
                    .annotations()
                    .contains_key("kars.azure.com/run-completed")
            );
            runs::harvest_and_retire_runs(&client, &tasks, &team)
                .await
                .unwrap();
            assert_eq!(
                contract::read(&cms, &owner).await.unwrap().unwrap().1,
                items
            );
            let after = objects.lock().unwrap();
            assert_eq!(
                after.len(),
                before.len(),
                "no fake output, approval, knowledge or budget writes"
            );
            for (path, value) in before.iter().filter(|(path, _)| {
                !path.ends_with("/report") && !path.ends_with(&owner.configmap_name())
            }) {
                assert_eq!(&after[path], value, "{path}");
            }
        }
    }
}

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
async fn charter_edit_preserves_harvested_commons_and_rejects_changed_output() {
    let (_server, client, store, mut team, name) = completed_run().await;
    let tasks_api = Api::namespaced(client.clone(), "tenant-a");
    runs::harvest_and_retire_runs(&client, &tasks_api, &team)
        .await
        .unwrap();
    let original_maps = store.lock().unwrap().cms.clone();
    let original_tasks = store.lock().unwrap().tasks.clone();

    team.spec.charter = "Check private-memory continuity in a fresh run.".into();
    let stats = runs::harvest_and_retire_runs(&client, &tasks_api, &team)
        .await
        .unwrap();
    assert_eq!(
        (stats.active, stats.succeeded, stats.tokens_total),
        (0, 1, 25)
    );
    assert_eq!(store.lock().unwrap().cms, original_maps);
    assert_eq!(store.lock().unwrap().tasks, original_tasks);

    store
        .lock()
        .unwrap()
        .cms
        .get_mut(&format!("kars-mission-output-{name}"))
        .unwrap()["data"]["output"] = json!("Different result under the same run identity.");
    let before_rejected = store.lock().unwrap().cms.clone();
    assert!(
        runs::harvest_and_retire_runs(&client, &tasks_api, &team)
            .await
            .is_err()
    );
    assert_eq!(store.lock().unwrap().cms, before_rejected);
    assert_eq!(store.lock().unwrap().tasks, original_tasks);
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
