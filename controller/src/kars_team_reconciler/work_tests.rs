// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::requests_tests::{cycle, finish, latest, request, requested_team, reserve, stage};
use super::*;
use crate::kars_team_reconciler::work::contract;

fn item(id: &str, review_required: bool) -> contract::TeamTask {
    contract::TeamTask {
        id: id.into(),
        title: format!("Deliver {id}"),
        status: "pending".into(),
        acceptance_criteria: vec!["No personal data".into()],
        review_required,
        ..Default::default()
    }
}

fn owner(team: &KarsTeam) -> contract::Owner {
    contract::Owner {
        name: team.name_any(),
        namespace: team.namespace().unwrap(),
        uid: team.uid().unwrap(),
    }
}

async fn queue(client: &Client, team: &KarsTeam, items: Vec<contract::TeamTask>) {
    contract::update(
        &Api::namespaced(client.clone(), "tenant-a"),
        &owner(team),
        None,
        true,
        |current| {
            *current = items.clone();
            Ok(())
        },
    )
    .await
    .unwrap();
}

fn backlog(store: &Arc<Mutex<Store>>) -> Vec<contract::TeamTask> {
    contract::parse(
        store.lock().unwrap().cms["kars-team-tasks-eng"]["data"]["tasks.json"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

fn current_run(store: &Arc<Mutex<Store>>) -> String {
    store.lock().unwrap().team["status"]["runAdmission"]["taskName"]
        .as_str()
        .unwrap()
        .into()
}

fn complete(store: &Arc<Mutex<Store>>, name: &str, successful: bool) {
    let mut state = store.lock().unwrap();
    let task = state.tasks.get_mut(name).unwrap();
    task["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!(name);
    let mut output = super::harvest::output(task);
    if !successful {
        output["data"]["status"] = json!("error");
    }
    state
        .cms
        .insert(format!("kars-mission-output-{name}"), output);
}

fn decide(store: &Arc<Mutex<Store>>, run: &str, approved: bool) -> String {
    let mut state = store.lock().unwrap();
    let task: crate::kars_task::KarsTask =
        serde_json::from_value(state.tasks[run].clone()).unwrap();
    let (name, approval) = state
        .approvals
        .iter_mut()
        .find(|(_, approval)| approval["spec"]["taskRef"]["name"] == run)
        .unwrap();
    let typed: crate::kars_approval::KarsApproval =
        serde_json::from_value(approval.clone()).unwrap();
    approval["status"] = json!({
        "phase": if approved { "Approved" } else { "Denied" },
        "boundTaskUid": approval["metadata"]["annotations"][contract::TASK_UID],
        "boundEnvelopeDigest": task.envelope_digest(),
        "boundRequest": crate::kars_approval::request_snapshot(&typed.spec),
    });
    approval["spec"]["decision"] = json!({
        "verdict": if approved { "approve" } else { "deny" },
        "decider":"reviewer",
        "reason":"Remove personal data; keep the checklist useful.",
    });
    name.clone()
}

#[tokio::test]
async fn operator_stop_without_root_output_requeues_only_its_claim_and_retains_cancellation() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    {
        let mut state = store.lock().unwrap();
        state.team["spec"]["paused"] = json!(true);
        let task = state.tasks.get_mut(&first).unwrap();
        task["spec"]["execution"]["launch"] = json!(false);
        task["metadata"]["annotations"]["kars.azure.com/halted"] =
            json!("halted by operator at 2026-10-09T01:14:00Z: Investigate failed phase");
    }
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let items = backlog(&store);
    assert_eq!(items[0].status, "pending");
    assert!(items[0].run.is_none());
    assert_eq!(items[0].history.len(), 1);
    let attempt = &items[0].history[0];
    assert_eq!(attempt.run, first);
    assert_eq!(attempt.outcome, "cancelled");
    assert_eq!(attempt.finished_at, "2026-10-09T01:14:00Z");
    assert_eq!(
        attempt.feedback.as_deref(),
        Some("Investigate failed phase")
    );
    assert!(
        attempt.output_sha256.is_none(),
        "a stop must not invent runtime output"
    );
    assert!(store.lock().unwrap().approvals.is_empty());
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );
    store.lock().unwrap().team["spec"]["paused"] = json!(false);
    ask_again(&store, 2);
    cycle(&client, &store).await.unwrap();
    assert_ne!(current_run(&store), first);
    assert_eq!(backlog(&store)[0].history, items[0].history);
    assert_eq!(backlog(&store)[0].status, "active");
}

#[tokio::test]
async fn team_pause_releases_only_unfinished_work_and_resume_uses_a_fresh_run() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    let mut later = item("later", true);
    later.depends_on.push("checklist".into());
    queue(&client, &team, vec![item("checklist", true), later]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    let first_claim = backlog(&store)[0].clone();
    store.lock().unwrap().team["spec"]["paused"] = json!(true);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let items = backlog(&store);
    assert_eq!(items[0].status, "pending");
    assert!(items[0].run.is_none());
    assert_eq!(items[0].history.len(), 1);
    let attempt = &items[0].history[0];
    assert_eq!(attempt.run, first);
    assert_eq!(Some(&attempt.run_uid), first_claim.run_uid.as_ref());
    assert_eq!(
        Some(&attempt.assignment_nonce),
        first_claim.assignment_nonce.as_ref()
    );
    assert_eq!(attempt.outcome, "cancelled");
    assert!(super::parse_rfc3339(&attempt.finished_at).is_some());
    assert!(
        attempt
            .feedback
            .as_deref()
            .unwrap()
            .contains("Team is paused")
    );
    assert!(attempt.output_sha256.is_none());
    assert_eq!(items[1].status, "pending");
    assert!(items[1].history.is_empty());
    assert!(store.lock().unwrap().approvals.is_empty());
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );
    store.lock().unwrap().team["spec"]["paused"] = json!(false);
    ask_again(&store, 2);
    cycle(&client, &store).await.unwrap();
    assert_ne!(current_run(&store), first);
    assert_eq!(backlog(&store)[0].history, items[0].history);
    assert_eq!(backlog(&store)[0].status, "active");
}

#[tokio::test]
async fn unannotated_stop_does_not_invent_cancellation_and_changed_claim_is_rejected() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    {
        let mut state = store.lock().unwrap();
        state.tasks.get_mut(&first).unwrap()["spec"]["execution"]["launch"] = json!(false);
    }
    cycle(&client, &store).await.unwrap();
    assert_eq!(backlog(&store)[0].status, "active");
    assert!(backlog(&store)[0].history.is_empty());
    {
        let mut state = store.lock().unwrap();
        state.tasks.get_mut(&first).unwrap()["metadata"]["annotations"]["kars.azure.com/halted"] =
            json!("halted by operator at 2026-10-09T01:14:00Z: Investigate");
        let raw = &mut state.cms.get_mut("kars-team-tasks-eng").unwrap()["data"]["tasks.json"];
        let mut items = contract::parse(raw.as_str().unwrap()).unwrap();
        items[0].run_uid = Some("different-run-uid".into());
        *raw = json!(serde_json::to_string(&items).unwrap());
    }
    assert!(cycle(&client, &store).await.is_err());
    assert_eq!(backlog(&store)[0].status, "active");
    assert!(backlog(&store)[0].history.is_empty());
}

fn ask_again(store: &Arc<Mutex<Store>>, sequence: u64) {
    store.lock().unwrap().team["metadata"]["annotations"]["kars.azure.com/run-now"] =
        json!(request(sequence));
}

#[tokio::test]
async fn work_review_revision_and_dependency_advancement_keep_distinct_run_identity() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    let mut follow_up = item("follow-up", false);
    follow_up.depends_on.push("checklist".into());
    queue(&client, &team, vec![item("checklist", true), follow_up]).await;
    cycle(&client, &store).await.unwrap();
    let first = current_run(&store);
    let claimed = backlog(&store);
    assert_eq!(claimed[0].status, "active");
    assert_eq!(claimed[0].run.as_deref(), Some(first.as_str()));
    assert!(
        store.lock().unwrap().tasks[&first]["spec"]["objective"]
            .as_str()
            .unwrap()
            .contains("No personal data")
    );
    complete(&store, &first, true);
    cycle(&client, &store).await.unwrap();
    assert!(
        store.lock().unwrap().approvals.is_empty(),
        "checkpoint must not bind before execution retires"
    );
    cycle(&client, &store).await.unwrap();
    assert_eq!(backlog(&store)[0].status, "awaiting_review");
    assert_eq!(
        store.lock().unwrap().team["status"]["health"],
        "AwaitingReview"
    );
    assert_eq!(
        store.lock().unwrap().tasks[&first]["spec"]["execution"]["launch"],
        false
    );
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );
    ask_again(&store, 2);
    cycle(&client, &store).await.unwrap();
    assert_eq!(
        current_run(&store),
        first,
        "review barrier must block a pending later request"
    );
    let first_approval = decide(&store, &first, false);
    cycle(&client, &store).await.unwrap();
    let second = current_run(&store);
    assert_ne!(first, second);
    let state = backlog(&store);
    assert_eq!(state[0].history.len(), 1);
    assert_eq!(state[0].history[0].outcome, "changes_requested");
    assert_ne!(
        state[0].history[0].run_uid,
        state[0].run_uid.clone().unwrap()
    );
    assert!(
        store.lock().unwrap().tasks[&second]["spec"]["objective"]
            .as_str()
            .unwrap()
            .contains("Remove personal data")
    );
    assert_eq!(state[1].status, "pending");
    complete(&store, &second, true);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let second_approval = decide(&store, &second, true);
    assert_ne!(first_approval, second_approval);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    assert_eq!(backlog(&store)[0].status, "done");
    assert_eq!(backlog(&store)[0].history.len(), 2);
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        1
    );
    ask_again(&store, 3);
    cycle(&client, &store).await.unwrap();
    let third = current_run(&store);
    assert_ne!(third, second);
    assert_eq!(backlog(&store)[1].run.as_deref(), Some(third.as_str()));
    complete(&store, &third, true);
    cycle(&client, &store).await.unwrap();
    assert_eq!(
        backlog(&store)[1].history[0].outcome,
        "completed",
        "automatic completion is not human approval"
    );
}

#[tokio::test]
async fn changed_or_replaced_work_cannot_activate_a_reserved_run() {
    for fault in [
        "payload",
        "store-uid",
        "foreign-owner",
        "malformed",
        "missing-key",
        "second-claim",
    ] {
        let team = requested_team();
        let (_server, client, store) = setup(&team).await;
        queue(&client, &team, vec![item("checklist", true)]).await;
        let prepared = stage(&client, &store).await;
        let persisted = reserve(&client, &store, &prepared).await;
        {
            let mut state = store.lock().unwrap();
            let cm = state.cms.get_mut("kars-team-tasks-eng").unwrap();
            match fault {
                "store-uid" => cm["metadata"]["uid"] = json!("replacement"),
                "foreign-owner" => {
                    cm["metadata"]["ownerReferences"][0]["uid"] = json!("other-team")
                }
                "malformed" => cm["data"]["tasks.json"] = json!("not json"),
                "missing-key" => {
                    cm["data"].as_object_mut().unwrap().remove("tasks.json");
                }
                _ => {
                    let mut items =
                        contract::parse(cm["data"]["tasks.json"].as_str().unwrap()).unwrap();
                    if fault == "payload" {
                        items[0].description = "Changed work".into();
                    } else {
                        items[0].run_uid = Some("other-run-uid".into());
                    }
                    cm["data"]["tasks.json"] = json!(serde_json::to_string(&items).unwrap());
                }
            }
        }
        assert!(
            finish(&client, &persisted, &prepared).await.is_err(),
            "{fault}"
        );
        assert_ne!(
            store.lock().unwrap().tasks[&prepared.record.task_name]["spec"]["execution"]["launch"],
            true
        );
        assert!(store.lock().unwrap().activation_reservations.is_empty());
    }
}

#[tokio::test]
async fn failed_work_retains_attempt_and_never_creates_review_or_knowledge() {
    let team = requested_team();
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    let run = current_run(&store);
    complete(&store, &run, false);
    cycle(&client, &store).await.unwrap();
    cycle(&client, &store).await.unwrap();
    let items = backlog(&store);
    assert_eq!(items[0].status, "pending");
    assert!(items[0].run.is_none());
    assert_eq!(items[0].history.len(), 1);
    assert_eq!(items[0].history[0].outcome, "failed");
    assert!(store.lock().unwrap().approvals.is_empty());
    assert_eq!(
        crate::team_commons::entry_count(&client, &latest(&store))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn altered_checkpoint_or_output_cannot_release_the_review_gate() {
    for fault in [
        "owner",
        "nonce",
        "task-uid",
        "request",
        "output",
        "sealed-objective",
    ] {
        let team = requested_team();
        let (_server, client, store) = setup(&team).await;
        queue(&client, &team, vec![item("checklist", true)]).await;
        cycle(&client, &store).await.unwrap();
        let run = current_run(&store);
        complete(&store, &run, true);
        cycle(&client, &store).await.unwrap();
        cycle(&client, &store).await.unwrap();
        let name = decide(&store, &run, true);
        {
            let mut state = store.lock().unwrap();
            match fault {
                "output" => {
                    state
                        .cms
                        .get_mut(&format!("kars-mission-output-{run}"))
                        .unwrap()["data"]["output"] = json!("Changed bytes")
                }
                "sealed-objective" => {
                    state.tasks.get_mut(&run).unwrap()["spec"]["objective"] =
                        json!("Different task")
                }
                _ => {
                    let approval = state.approvals.get_mut(&name).unwrap();
                    match fault {
                        "owner" => {
                            approval["metadata"]["ownerReferences"][0]["uid"] = json!("other")
                        }
                        "nonce" => {
                            approval["metadata"]["annotations"][contract::RUN_NONCE] =
                                json!("other")
                        }
                        "task-uid" => approval["status"]["boundTaskUid"] = json!("other"),
                        _ => approval["spec"]["action"]["detail"] = json!("Changed request"),
                    }
                }
            }
        }
        assert!(cycle(&client, &store).await.is_err(), "{fault}");
        assert_eq!(backlog(&store)[0].status, "awaiting_review");
        assert_eq!(
            crate::team_commons::entry_count(&client, &latest(&store))
                .await
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn cadence_uses_the_same_work_claim_and_durable_admission_as_manual_runs() {
    let mut team = team();
    team.spec.envelope.budget = None;
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    let (_server, client, store) = setup(&team).await;
    queue(&client, &team, vec![item("checklist", true)]).await;
    cycle(&client, &store).await.unwrap();
    assert!(latest(&store).annotations()["kars.azure.com/run-now"].starts_with("cadence-1-"));
    assert!(store.lock().unwrap().activation_reservations.is_empty());
    cycle(&client, &store).await.unwrap();
    let state = store.lock().unwrap();
    assert_eq!(state.activation_reservations.len(), 1);
    let run = state.team["status"]["runAdmission"]["taskName"]
        .as_str()
        .unwrap();
    let items = contract::parse(
        state.cms["kars-team-tasks-eng"]["data"]["tasks.json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(items[0].run.as_deref(), Some(run));
    assert_eq!(
        items[0].run_uid.as_deref(),
        state.tasks[run]["metadata"]["uid"].as_str()
    );
}
