// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_team_reconciler::requests::{self, Prepared, REQUEST};

const STATE: &str = "kars.azure.com/team-request-state";

fn request(sequence: u64) -> String {
    format!("manual-{sequence}-{}", "a".repeat(64))
}

fn requested_team() -> KarsTeam {
    let mut team = team();
    team.spec.envelope.budget = None;
    team.annotations_mut().insert(REQUEST.into(), request(1));
    team
}

fn latest(store: &Arc<Mutex<Store>>) -> KarsTeam {
    serde_json::from_value(store.lock().unwrap().team.clone()).unwrap()
}

async fn cycle(client: &Client, store: &Arc<Mutex<Store>>) -> Result<Action, ReconcileError> {
    reconcile(
        Arc::new(latest(store)),
        Arc::new(Ctx {
            client: client.clone(),
        }),
    )
    .await
}

async fn stage(client: &Client, store: &Arc<Mutex<Store>>) -> Prepared {
    let team = latest(store);
    crate::team_commons::ensure_commons(client, &team)
        .await
        .unwrap();
    requests::prepare(
        client,
        &Api::namespaced(client.clone(), "tenant-a"),
        &team,
        true,
    )
    .await
    .unwrap()
    .unwrap()
}

async fn reserve(client: &Client, store: &Arc<Mutex<Store>>, prepared: &Prepared) -> KarsTeam {
    let team = latest(store);
    let mut status = team.status.clone().unwrap_or_default();
    status.run_admission = Some(prepared.record.clone());
    if prepared.newly_reserved {
        status.generated_task_count += 1;
    }
    write_status(&Api::namespaced(client.clone(), "tenant-a"), &team, status)
        .await
        .unwrap()
}

async fn finish(
    client: &Client,
    team: &KarsTeam,
    prepared: &Prepared,
) -> Result<(), ReconcileError> {
    requests::finish(
        client,
        &Api::namespaced(client.clone(), "tenant-a"),
        &Api::namespaced(client.clone(), "tenant-a"),
        team,
        prepared,
    )
    .await
}

fn launched(store: &Arc<Mutex<Store>>, prepared: &Prepared) -> bool {
    store.lock().unwrap().tasks[&prepared.record.task_name]["spec"]["execution"]["launch"] == true
}

#[tokio::test]
async fn reservation_precedes_activation_and_manual_request_suppresses_cadence() {
    let mut team = requested_team();
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    let (_server, client, store) = setup(&team).await;
    cycle(&client, &store).await.unwrap();
    let state = store.lock().unwrap();
    assert_eq!(state.tasks.len(), 2, "principal plus manual run only");
    assert_eq!(state.team["status"]["generatedTaskCount"], 1);
    let record = state.team["status"]["runAdmission"].clone();
    assert_eq!(state.activation_reservations, vec![record.clone()]);
    let task = &state.tasks[record["taskName"].as_str().unwrap()];
    assert_eq!(record["taskUid"], task["metadata"]["uid"]);
    assert_eq!(task["spec"]["execution"]["launch"], true);
    assert_eq!(task["metadata"]["annotations"][STATE], "admitted");
    assert!(state.team["metadata"]["annotations"].get(REQUEST).is_none());
}

#[tokio::test]
async fn stage_cannot_activate_without_its_persisted_reservation() {
    let (_server, client, store) = setup(&requested_team()).await;
    let prepared = stage(&client, &store).await;
    assert!(!launched(&store, &prepared));
    assert!(finish(&client, &latest(&store), &prepared).await.is_err());
    assert!(!launched(&store, &prepared));
    assert!(store.lock().unwrap().activation_reservations.is_empty());
    let persisted = reserve(&client, &store, &prepared).await;
    finish(&client, &persisted, &prepared).await.unwrap();
    assert!(launched(&store, &prepared));
}

#[tokio::test]
async fn failures_before_status_and_after_each_persisted_step_recover_without_duplicate_activation()
{
    for fault in [
        "before-status",
        "after-status",
        "after-activation",
        "after-ack",
    ] {
        let (_server, client, store) = setup(&requested_team()).await;
        {
            let mut state = store.lock().unwrap();
            state.fail_status_once = fault == "before-status";
            state.fail_after_status_once = fault == "after-status";
            state.fail_after_activation_once = fault == "after-activation";
            state.fail_after_ack_once = fault == "after-ack";
        }
        assert!(cycle(&client, &store).await.is_err(), "{fault}");
        let ids: BTreeMap<_, _> = store
            .lock()
            .unwrap()
            .tasks
            .iter()
            .map(|(name, task)| (name.clone(), task["metadata"]["uid"].clone()))
            .collect();
        if fault.contains("status") {
            let state = store.lock().unwrap();
            assert!(state.activation_reservations.is_empty(), "{fault}");
            assert!(
                state
                    .tasks
                    .values()
                    .all(|task| task["spec"]["execution"]["launch"] != true)
            );
        }
        cycle(&client, &store).await.unwrap();
        cycle(&client, &store).await.unwrap();
        let state = store.lock().unwrap();
        assert_eq!(state.tasks.len(), 2, "{fault}");
        for (name, uid) in &ids {
            assert_eq!(&state.tasks[name]["metadata"]["uid"], uid, "{fault}");
        }
        assert_eq!(state.team["status"]["generatedTaskCount"], 1, "{fault}");
        assert_eq!(
            state.activation_reservations,
            vec![state.team["status"]["runAdmission"].clone()],
            "{fault}"
        );
        assert!(
            state.team["metadata"]["annotations"].get(REQUEST).is_none(),
            "{fault}"
        );
    }
}

#[tokio::test]
async fn lost_ack_defers_credential_rebinding_without_relaunching_the_reserved_task() {
    use crate::kars_task_reconciler::rebind::{PAUSED, PENDING};

    let mut team = requested_team();
    let original = json!({"grant":{"name":"workspace","uid":"grant-old"},"sources":[]});
    team.spec
        .blueprint
        .get_or_insert_with(Default::default)
        .credential_bindings = Some(serde_json::from_value(original.clone()).unwrap());
    let (_server, client, store) = setup(&team).await;
    store.lock().unwrap().fail_after_activation_once = true;
    assert!(cycle(&client, &store).await.is_err());
    let record = latest(&store).status.unwrap().run_admission.unwrap();
    let desired = json!({"grant":{"name":"workspace","uid":"grant-new"},"sources":[]});
    {
        let mut state = store.lock().unwrap();
        state.team["spec"]["blueprint"]["credentialBindings"] = desired.clone();
        state.team["metadata"]["generation"] = json!(2);
        state.team["metadata"]["resourceVersion"] = json!("200");
    }
    let api = Api::namespaced(client.clone(), "tenant-a");
    credential_bindings::reconcile(&client, &api, &latest(&store))
        .await
        .unwrap();
    let reserved_spec = {
        let mut state = store.lock().unwrap();
        let task = state.tasks.get_mut(&record.task_name).unwrap();
        assert_eq!(task["metadata"]["annotations"][PENDING], "true");
        assert_eq!(task["spec"]["blueprint"]["credentialBindings"], original);
        task["status"] = json!({"executionPhase":PAUSED,
            "observedGeneration":task["metadata"]["generation"],
            "conditions":[{"type":"Ready","status":"False","reason":"CredentialsPaused",
                "message":"Credential runtime paused", "lastTransitionTime":"2026-10-03T00:00:00Z"}]});
        task["spec"].clone()
    };
    // Sandbox and namespace are absent: real quiescence checks succeed without a runtime.
    // A stale caller missing the pending request must still respect the fresh Team record.
    let mut stale = latest(&store);
    stale.annotations_mut().remove(REQUEST);
    credential_bindings::reconcile(&client, &api, &stale)
        .await
        .unwrap();
    assert_eq!(
        store.lock().unwrap().tasks[&record.task_name]["spec"],
        reserved_spec
    );
    cycle(&client, &store).await.unwrap();
    {
        let state = store.lock().unwrap();
        assert!(state.team["metadata"]["annotations"].get(REQUEST).is_none());
        assert_eq!(
            state.team["status"]["runAdmission"],
            serde_json::to_value(&record).unwrap()
        );
        assert_eq!(state.team["status"]["generatedTaskCount"], 1);
        assert_eq!(state.tasks[&record.task_name]["spec"], reserved_spec);
        assert_eq!(
            state.tasks[&record.task_name]["metadata"]["annotations"][PENDING],
            "true"
        );
    }
    credential_bindings::reconcile(&client, &api, &latest(&store))
        .await
        .unwrap();
    let state = store.lock().unwrap();
    let task = &state.tasks[&record.task_name];
    assert_eq!(task["metadata"]["uid"], record.task_uid);
    assert_eq!(task["spec"]["blueprint"]["credentialBindings"], desired);
    assert_eq!(task["spec"]["envelope"], reserved_spec["envelope"]);
    assert_eq!(task["spec"]["execution"], reserved_spec["execution"]);
    assert!(task["metadata"]["annotations"].get(PENDING).is_none());
    assert_eq!(state.activation_reservations.len(), 1);
    assert_eq!(state.tasks.len(), 2);
}

#[tokio::test]
async fn credential_deferral_is_bound_to_the_pending_reservation_and_exact_task_owner() {
    let (_server, client, store) = setup(&requested_team()).await;
    let prepared = stage(&client, &store).await;
    let team = reserve(&client, &store, &prepared).await;
    let task: KarsTask =
        serde_json::from_value(store.lock().unwrap().tasks[&prepared.record.task_name].clone())
            .unwrap();
    assert!(requests::awaiting_ack(&team, &task));
    for mutation in ["request", "record", "name", "uid", "namespace", "owner"] {
        let mut team = team.clone();
        let mut task = task.clone();
        match mutation {
            "request" => {
                team.annotations_mut().insert(REQUEST.into(), request(2));
            }
            "record" => team.status.as_mut().unwrap().run_admission = None,
            "name" => task.metadata.name = Some("different".into()),
            "uid" => task.metadata.uid = Some("different".into()),
            "namespace" => task.metadata.namespace = Some("different".into()),
            "owner" => task.metadata.owner_references.as_mut().unwrap()[0].uid = "different".into(),
            _ => unreachable!(),
        }
        assert!(!requests::awaiting_ack(&team, &task), "{mutation}");
    }
}

#[tokio::test]
async fn reserved_task_missing_replaced_or_tampered_never_recreates_execution() {
    for mutation in [
        "missing",
        "uid",
        "owner",
        "spec",
        "nonce",
        "stage-ack",
        "stage-completed",
        "state",
        "deleting",
    ] {
        let (_server, client, store) = setup(&requested_team()).await;
        let prepared = stage(&client, &store).await;
        reserve(&client, &store, &prepared).await;
        {
            let mut state = store.lock().unwrap();
            if mutation == "missing" {
                state.tasks.remove(&prepared.record.task_name);
            } else {
                let task = state.tasks.get_mut(&prepared.record.task_name).unwrap();
                match mutation {
                    "uid" => task["metadata"]["uid"] = json!("replacement"),
                    "owner" => task["metadata"]["ownerReferences"][0]["uid"] = json!("foreign"),
                    "spec" => task["spec"]["objective"] = json!("different work"),
                    "nonce" => {
                        task["metadata"]["annotations"][ANNOT_RUN_REQUESTED] = json!("other")
                    }
                    "stage-ack" => {
                        task["metadata"]["annotations"]["kars.azure.com/run-ack"] = json!("")
                    }
                    "stage-completed" => {
                        task["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!("")
                    }
                    "state" => task["metadata"]["annotations"][STATE] = json!("unknown"),
                    "deleting" => {
                        task["metadata"]["deletionTimestamp"] = json!("2026-10-03T00:00:00Z")
                    }
                    _ => unreachable!(),
                }
            }
        }
        let before = store.lock().unwrap().tasks.clone();
        let team = latest(&store);
        assert!(
            requests::prepare(
                &client,
                &Api::namespaced(client.clone(), "tenant-a"),
                &team,
                true
            )
            .await
            .is_err(),
            "{mutation}"
        );
        assert_eq!(store.lock().unwrap().tasks, before, "{mutation}");
    }
}

#[tokio::test]
async fn terminal_admitted_task_is_only_acknowledged_never_relaunched() {
    let (_server, client, store) = setup(&requested_team()).await;
    store.lock().unwrap().fail_after_activation_once = true;
    assert!(cycle(&client, &store).await.is_err());
    {
        let mut state = store.lock().unwrap();
        let name = state.team["status"]["runAdmission"]["taskName"]
            .as_str()
            .unwrap()
            .to_string();
        let task = state.tasks.get_mut(&name).unwrap();
        task["spec"]["execution"]["launch"] = json!(false);
        task["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!(name);
    }
    let team = latest(&store);
    let prepared = requests::prepare(
        &client,
        &Api::namespaced(client.clone(), "tenant-a"),
        &team,
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!prepared.newly_reserved);
    finish(&client, &team, &prepared).await.unwrap();
    assert!(!launched(&store, &prepared));
    assert_eq!(store.lock().unwrap().activation_reservations.len(), 1);
}

#[tokio::test]
async fn later_request_is_distinct_and_watermark_rejects_deleted_historical_run_replay() {
    let (_server, client, store) = setup(&requested_team()).await;
    cycle(&client, &store).await.unwrap();
    let first = latest(&store).status.unwrap().run_admission.unwrap();
    {
        let mut state = store.lock().unwrap();
        state.tasks.get_mut(&first.task_name).unwrap()["spec"]["execution"]["launch"] =
            json!(false);
        state.team["metadata"]["annotations"][REQUEST] = json!(request(2));
        state.team["metadata"]["resourceVersion"] = json!("200");
    }
    cycle(&client, &store).await.unwrap();
    let second = latest(&store).status.unwrap().run_admission.unwrap();
    assert_ne!(first.task_name, second.task_name);
    assert_ne!(first.task_uid, second.task_uid);
    {
        let mut state = store.lock().unwrap();
        let first_spec = &state.tasks[&first.task_name]["spec"];
        let second_spec = &state.tasks[&second.task_name]["spec"];
        let principal = json!({"name": specs::principal_name(&team())});
        assert_eq!(first_spec["parentRef"], principal);
        assert_eq!(second_spec["parentRef"], principal);
        assert_eq!(first_spec["envelope"], second_spec["envelope"]);
        assert_eq!(state.team["status"]["generatedTaskCount"], 2);
        state.tasks.remove(&first.task_name);
        state.team["metadata"]["annotations"][REQUEST] = json!(request(1));
    }
    let before = store.lock().unwrap().tasks.clone();
    assert!(cycle(&client, &store).await.is_err());
    assert_eq!(store.lock().unwrap().tasks, before);
    assert_eq!(
        latest(&store).status.unwrap().run_admission.unwrap(),
        second
    );
}

#[tokio::test]
async fn stale_team_request_fences_and_activation_compensation_preserve_newer_authority() {
    for point in ["before-activation", "during-activation", "during-ack"] {
        for mutation in ["replacement", "pause", "new-request"] {
            let (_server, client, store) = setup(&requested_team()).await;
            let prepared = stage(&client, &store).await;
            let persisted = reserve(&client, &store, &prepared).await;
            let mut changed = serde_json::to_value(&persisted).unwrap();
            changed["metadata"]["resourceVersion"] = json!("900");
            match mutation {
                "replacement" => changed["metadata"]["uid"] = json!("replacement"),
                "pause" => {
                    changed["spec"]["paused"] = json!(true);
                    changed["metadata"]["generation"] = json!(2);
                }
                "new-request" => changed["metadata"]["annotations"][REQUEST] = json!(request(2)),
                _ => unreachable!(),
            }
            {
                let mut state = store.lock().unwrap();
                match point {
                    "before-activation" => state.team = changed.clone(),
                    "during-activation" => state.team_on_activation = Some(changed.clone()),
                    "during-ack" => state.team_on_ack = Some(changed.clone()),
                    _ => unreachable!(),
                }
            }
            assert!(
                finish(&client, &persisted, &prepared).await.is_err(),
                "{point}/{mutation}"
            );
            assert_eq!(store.lock().unwrap().team, changed, "{point}/{mutation}");
            if point != "during-ack" {
                assert!(!launched(&store, &prepared), "{point}/{mutation}");
            }
        }
    }
}

#[tokio::test]
async fn task_replacement_during_activation_is_not_overwritten() {
    let (_server, client, store) = setup(&requested_team()).await;
    let prepared = stage(&client, &store).await;
    let persisted = reserve(&client, &store, &prepared).await;
    let mut replacement = store.lock().unwrap().tasks[&prepared.record.task_name].clone();
    replacement["metadata"]["uid"] = json!("replacement");
    store.lock().unwrap().task_on_retirement = Some(replacement.clone());
    assert!(finish(&client, &persisted, &prepared).await.is_err());
    assert_eq!(
        store.lock().unwrap().tasks[&prepared.record.task_name],
        replacement
    );
    assert!(store.lock().unwrap().activation_reservations.is_empty());
}

#[tokio::test]
async fn blocked_admission_preserves_pending_request_without_staging() {
    for mutation in ["paused", "unsupported-budget", "governed-unbound-budget"] {
        let mut team = requested_team();
        match mutation {
            "paused" => team.spec.paused = true,
            "unsupported-budget" => team.spec.envelope.budget = super::team().spec.envelope.budget,
            "governed-unbound-budget" => {
                let mut budget = super::team().spec.envelope.budget.unwrap();
                budget.scope =
                    Some(crate::inference_budget_contract::BudgetScope::GovernedInference);
                team.spec.envelope.budget = Some(budget);
            }
            _ => unreachable!(),
        }
        let (_server, client, store) = setup(&team).await;
        cycle(&client, &store).await.unwrap();
        let state = store.lock().unwrap();
        assert_eq!(state.tasks.len(), 1, "only idle principal: {mutation}");
        assert_eq!(state.team["status"]["generatedTaskCount"], 0);
        assert_eq!(state.team["metadata"]["annotations"][REQUEST], request(1));
        assert!(state.team["status"]["runAdmission"].is_null());
    }
}

#[tokio::test]
async fn activation_rechecks_authority_and_namespace_wide_owned_capacity() {
    for mutation in ["authority", "paused", "capacity"] {
        let (_server, client, store) = setup(&requested_team()).await;
        let prepared = stage(&client, &store).await;
        reserve(&client, &store, &prepared).await;
        {
            let mut state = store.lock().unwrap();
            if mutation == "capacity" {
                for index in 0..MAX_CONCURRENT_RUNS {
                    let mut task = state.tasks[&prepared.record.task_name].clone();
                    task["metadata"]["name"] = json!(format!("other-{index}"));
                    task["metadata"]["uid"] = json!(format!("other-uid-{index}"));
                    task["metadata"].as_object_mut().unwrap().remove("labels");
                    task["spec"]["execution"]["launch"] = json!(true);
                    state.tasks.insert(format!("other-{index}"), task);
                }
            } else {
                if mutation == "authority" {
                    state.team["spec"]["charter"] = json!("Changed charter");
                } else {
                    state.team["spec"]["paused"] = json!(true);
                }
                state.team["metadata"]["generation"] = json!(2);
                state.team["metadata"]["resourceVersion"] = json!("300");
            }
        }
        assert!(
            finish(&client, &latest(&store), &prepared).await.is_err(),
            "{mutation}"
        );
        assert!(!launched(&store, &prepared));
        assert!(store.lock().unwrap().activation_reservations.is_empty());
    }
}

#[tokio::test]
async fn malformed_and_out_of_sequence_requests_fail_without_creating_tasks() {
    for value in [
        String::new(),
        "2026-10-03T00:00:00Z".into(),
        request(0),
        request(2),
        request(1).replacen("manual-1-", "manual-01-", 1),
        request(1).replacen("manual-1-", "manual-+1-", 1),
        format!("manual-1-{}", "A".repeat(64)),
        request(i64::MAX as u64 + 1),
    ] {
        let mut team = requested_team();
        team.annotations_mut().insert(REQUEST.into(), value.clone());
        let (_server, client, store) = setup(&team).await;
        assert!(
            requests::prepare(
                &client,
                &Api::namespaced(client.clone(), "tenant-a"),
                &team,
                true
            )
            .await
            .is_err(),
            "{value}"
        );
        assert!(store.lock().unwrap().tasks.is_empty());
    }
}
