// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::requests_tests::{latest, requested_team, reserve, stage};
use super::*;
use crate::kars_team_reconciler::{qualification, requests};
use qualification::contract;

// Persisted-boundary fixture: model a crash after activation but before ack.
// It exercises recovery without inventing qualification evidence or invoking
// provider admission; newly staged attempts still go through the real gate.
fn seal_attempt(
    store: &Arc<Mutex<Store>>,
    prepared: &requests::Prepared,
    admitted: bool,
) -> requests::Prepared {
    let mut state = store.lock().unwrap();
    let now = Utc::now() - chrono::Duration::hours(1);
    let record = contract::Record {
        approved_by: "Operator".into(),
        approved_by_sub: "operator".into(),
        approved_at: now,
        expires_at: now + chrono::Duration::minutes(45),
        team_uid: state.team["metadata"]["uid"].as_str().unwrap().into(),
        package_sha256: contract::package(&state.team),
        resources: Default::default(),
        backlog_uid: "backlog-uid".into(),
        item_id: "first".into(),
        work_sha256: "b".repeat(64),
        request: prepared.record.request.clone(),
    };
    let raw = serde_json::to_string(&record).unwrap();
    state.team["metadata"]["annotations"][contract::RECORD] = json!(raw);
    let mut task: KarsTask =
        serde_json::from_value(state.tasks[&prepared.record.task_name].clone()).unwrap();
    task.annotations_mut().insert(contract::RECORD.into(), raw);
    task.annotations_mut().insert(
        work::contract::ANNOTATION.into(),
        serde_json::to_string(&work::contract::Binding {
            backlog_uid: record.backlog_uid,
            item_id: record.item_id,
            request_digest: record.work_sha256,
        })
        .unwrap(),
    );
    task.annotations_mut().insert(
        "kars.azure.com/team-request-state".into(),
        if admitted { "admitted" } else { "staged" }.into(),
    );
    task.spec.execution.as_mut().unwrap().launch = admitted;
    let digest = requests::task_digest(&task).unwrap();
    task.annotations_mut()
        .insert("kars.azure.com/team-request-spec".into(), digest.clone());
    state.tasks.insert(
        prepared.record.task_name.clone(),
        serde_json::to_value(task).unwrap(),
    );
    let mut admission = prepared.record.clone();
    admission.task_spec_digest = digest;
    state.team["status"]["runAdmission"] = serde_json::to_value(&admission).unwrap();
    requests::Prepared {
        record: admission,
        newly_reserved: false,
    }
}

#[tokio::test]
async fn team_qualification_recovers_consumption_after_expiry_then_pauses_exact_terminal_attempt() {
    let (_server, client, store) = setup(&requested_team()).await;
    let prepared = stage(&client, &store).await;
    reserve(&client, &store, &prepared).await;
    let prepared = seal_attempt(&store, &prepared, true);
    let teams = Api::namespaced(client.clone(), "tenant-a");
    let tasks = Api::namespaced(client.clone(), "tenant-a");
    assert!(
        qualification::recover_consumption(&client, &teams, &tasks, &latest(&store))
            .await
            .unwrap()
    );
    let consumed = latest(&store);
    assert_eq!(
        consumed.annotations().get(contract::CONSUMED),
        Some(&prepared.record.task_uid)
    );
    assert!(!consumed.annotations().contains_key(requests::REQUEST));
    assert!(
        !qualification::recover_consumption(&client, &teams, &tasks, &consumed)
            .await
            .unwrap()
    );
    assert!(
        !qualification::pause_finished(&teams, &tasks, &consumed)
            .await
            .unwrap()
    );
    assert!(qualification::authorization(&consumed, &requests_tests::request(2)).is_err());
    store
        .lock()
        .unwrap()
        .tasks
        .get_mut(&prepared.record.task_name)
        .unwrap()["metadata"]["annotations"]["kars.azure.com/run-completed"] =
        json!(prepared.record.task_name);
    assert!(
        qualification::pause_finished(&teams, &tasks, &consumed)
            .await
            .unwrap()
    );
    let paused = latest(&store);
    assert!(paused.spec.paused);
    assert!(paused.annotations().contains_key(contract::RECORD));
    assert_eq!(
        paused.annotations().get(contract::CONSUMED),
        Some(&prepared.record.task_uid)
    );
}

#[tokio::test]
async fn team_qualification_staged_expiry_and_replaced_or_changed_task_never_activate_or_consume() {
    for case in [
        "expired-staged",
        "task-replacement",
        "approval-change",
        "work-change",
    ] {
        let (_server, client, store) = setup(&requested_team()).await;
        let prepared = stage(&client, &store).await;
        reserve(&client, &store, &prepared).await;
        let prepared = seal_attempt(&store, &prepared, case != "expired-staged");
        {
            let mut state = store.lock().unwrap();
            let task = state.tasks.get_mut(&prepared.record.task_name).unwrap();
            match case {
                "task-replacement" => task["metadata"]["uid"] = json!("replacement"),
                "approval-change" => {
                    task["metadata"]["annotations"][contract::RECORD] = json!("{}")
                }
                "work-change" => {
                    task["metadata"]["annotations"][work::contract::ANNOTATION] = json!("{}")
                }
                _ => {}
            }
        }
        let current = latest(&store);
        let teams = Api::namespaced(client.clone(), "tenant-a");
        let tasks = Api::namespaced(client.clone(), "tenant-a");
        if case == "expired-staged" {
            assert!(
                !qualification::recover_consumption(&client, &teams, &tasks, &current)
                    .await
                    .unwrap()
            );
        } else {
            assert!(
                qualification::recover_consumption(&client, &teams, &tasks, &current)
                    .await
                    .is_err(),
                "{case}"
            );
        }
        assert!(
            requests::finish(&client, &teams, &tasks, &current, &prepared)
                .await
                .is_err(),
            "{case}"
        );
        assert!(
            !latest(&store)
                .annotations()
                .contains_key(contract::CONSUMED)
        );
        assert!(store.lock().unwrap().activation_reservations.is_empty());
    }
}

#[tokio::test]
async fn team_qualification_stopped_or_retired_attempt_pauses_without_recreation() {
    for missing in [false, true] {
        let (_server, client, store) = setup(&requested_team()).await;
        let prepared = stage(&client, &store).await;
        reserve(&client, &store, &prepared).await;
        let prepared = seal_attempt(&store, &prepared, true);
        let teams = Api::namespaced(client.clone(), "tenant-a");
        let tasks = Api::namespaced(client.clone(), "tenant-a");
        qualification::recover_consumption(&client, &teams, &tasks, &latest(&store))
            .await
            .unwrap();
        {
            let mut state = store.lock().unwrap();
            if missing {
                state.tasks.remove(&prepared.record.task_name);
            } else {
                state.tasks.get_mut(&prepared.record.task_name).unwrap()["spec"]["execution"]["launch"] =
                    json!(false);
            }
        }
        let before = store.lock().unwrap().tasks.len();
        assert!(
            qualification::pause_finished(&teams, &tasks, &latest(&store))
                .await
                .unwrap()
        );
        assert!(latest(&store).spec.paused);
        assert_eq!(store.lock().unwrap().tasks.len(), before);
    }
}

#[test]
fn team_qualification_pending_and_consumed_markers_fail_closed_but_ordinary_requests_remain_normal()
{
    let mut team = requested_team();
    assert!(!qualification::marked(&team));
    assert!(
        qualification::authorization(&team, &requests_tests::request(1))
            .unwrap()
            .is_none()
    );
    for marker in [contract::PENDING, contract::CONSUMED, contract::RECORD] {
        team.annotations_mut()
            .insert(marker.into(), "malformed".into());
        assert!(qualification::marked(&team));
        assert!(qualification::authorization(&team, &requests_tests::request(1)).is_err());
        team.annotations_mut().remove(marker);
    }
}
