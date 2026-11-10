// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn parallel_root(limit: usize) -> KarsTask {
    let mut task = root();
    let plan = task
        .spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    for name in ["reviewer", "observer"] {
        let mut role = plan.roles[0].clone();
        role.name = name.into();
        plan.roles.push(role);
    }
    plan.max_parallel = i32::try_from(limit).unwrap();
    ready(&mut task);
    task
}

#[tokio::test]
async fn current_distinguishes_stale_snapshots_from_replaced_or_deleting_tasks() {
    let task = root();
    let (_server, client, memory) = fixture_with(task.clone()).await;
    current(&client, &task).await.unwrap();

    let mut fresh = task.clone();
    fresh.metadata.resource_version = Some("2".into());
    memory.put(object_path(&fresh), &fresh);
    let before = memory.0.lock().unwrap().clone();
    let error = current(&client, &task).await.unwrap_err();
    assert!(error.is::<TaskSnapshotChanged>());
    assert_eq!(*memory.0.lock().unwrap(), before);
    current(&client, &fresh).await.unwrap();

    fresh.metadata.uid = Some("replacement-uid".into());
    memory.put(object_path(&fresh), &fresh);
    let error = current(&client, &task).await.unwrap_err();
    assert!(!error.is::<TaskSnapshotChanged>());
    assert!(error.to_string().contains("changed during reconciliation"));

    fresh.metadata.uid = task.metadata.uid.clone();
    fresh.metadata.deletion_timestamp = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
        k8s_openapi::jiff::Timestamp::now(),
    ));
    memory.put(object_path(&fresh), &fresh);
    let error = current(&client, &task).await.unwrap_err();
    assert!(!error.is::<TaskSnapshotChanged>());
    assert!(error.to_string().contains("missing or deleting"));
}

#[tokio::test]
async fn independent_roles_progress_in_the_same_reconcile_without_exceeding_capacity() {
    for limit in [1, 2, 3] {
        let (_server, client, memory) = fixture_with(parallel_root(limit)).await;
        assert!(!tick(&client, &memory).await.unwrap());
        assert!(!tick(&client, &memory).await.unwrap());
        let (_, reserved) = load(&client, &memory.task("report")).await.unwrap();
        assert_eq!(
            reserved.children.len(),
            limit,
            "reserve every eligible role in one pass"
        );
        for r in 0..limit {
            let role = memory.ready(&reserved.children[&slot(r, None)].name);
            assert!(!launched(&role));
        }
        assert!(!tick(&client, &memory).await.unwrap());
        let (_, phases) = load(&client, &memory.task("report")).await.unwrap();
        assert_eq!(
            phases.children.len(),
            2 * limit,
            "reserve every eligible phase in one pass"
        );
        for r in 0..limit {
            let phase = memory.task(&phases.children[&slot(r, Some(0))].name);
            assert!(!launched(&phase), "reservation must precede activation");
        }
        assert!(!tick(&client, &memory).await.unwrap());
        for r in 0..limit {
            let phase = memory.ready(&phases.children[&slot(r, Some(0))].name);
            assert!(
                launched(&phase),
                "activate independent phases in the same pass"
            );
            assert_eq!(
                phase.uid().as_deref(),
                Some(phases.children[&slot(r, Some(0))].uid.as_str())
            );
            assert_eq!(
                phase.spec.envelope.budget.as_ref().unwrap().tokens,
                Some(5000)
            );
            assert!(grant(&client, &phase).await.unwrap().is_some());
        }
        assert!(
            !tick(&client, &memory).await.unwrap(),
            "unfinished phases cannot permit synthesis"
        );
        let (_, after) = load(&client, &memory.task("report")).await.unwrap();
        assert_eq!(after.children.len(), 2 * limit);
    }
}

#[tokio::test]
async fn unready_role_does_not_block_an_independent_ready_role() {
    let (_server, client, memory) = fixture_with(parallel_root(2)).await;
    for _ in 0..2 {
        assert!(!tick(&client, &memory).await.unwrap());
    }
    let (_, state) = load(&client, &memory.task("report")).await.unwrap();
    assert!(state.children.contains_key("r1"));
    memory.ready(&state.children["r1"].name);
    for _ in 0..2 {
        assert!(!tick(&client, &memory).await.unwrap());
    }
    let (_, state) = load(&client, &memory.task("report")).await.unwrap();
    assert!(!state.children.contains_key("r0-p0"));
    assert!(!state.children.contains_key("r2"));
    assert!(launched(&memory.task(&state.children["r1-p0"].name)));
}

#[tokio::test]
async fn current_phase_hold_is_actionable_on_root_and_clears_after_recovery() {
    for root in [root(), hermes_root()] {
        let (_server, client, memory, mut phase) = phase_fixture_with(root).await;
        let status = phase.status.as_mut().unwrap();
        status.execution_phase = Some("Degraded".into());
        status.execution_detail = Some(
            "ReviewedInputCapabilityRequired: review filesystem-read before a fresh run".into(),
        );
        memory.put(object_path(&phase), &phase);
        let task = memory.task("report");
        let mut root_status = task.status.clone().unwrap();
        crate::kars_task_reconciler::reconcile_execution(
            &client,
            "default",
            &task,
            &mut root_status,
            false,
        )
        .await;
        assert_eq!(root_status.execution_phase.as_deref(), Some("Degraded"));
        let detail = root_status.execution_detail.unwrap();
        for expected in [
            "ReviewedPhaseHeld",
            "writer",
            "draft",
            &phase.name_any(),
            "ReviewedInputCapabilityRequired",
            "filesystem-read",
        ] {
            assert!(detail.contains(expected), "{detail}");
        }
        assert!(root_status.sandbox_ref.is_none());
        let (_, state) = load(&client, &memory.task("report")).await.unwrap();
        assert_eq!(state.children.len(), 2);
        assert_eq!(state.children["r0-p0"].uid, phase.uid().unwrap());
        phase.status.as_mut().unwrap().execution_phase = Some("Running".into());
        memory.put(object_path(&phase), &phase);
        assert!(!tick(&client, &memory).await.unwrap());
    }
}

#[tokio::test]
async fn held_phase_still_allows_independent_roles_to_progress() {
    let (_server, client, memory, mut phase) = phase_fixture_with(parallel_root(2)).await;
    phase.status.as_mut().unwrap().execution_phase = Some("Degraded".into());
    memory.put(object_path(&phase), &phase);
    let (_, state) = load(&client, &memory.task("report")).await.unwrap();
    memory.ready(&state.children["r1"].name);
    for _ in 0..2 {
        assert!(
            tick(&client, &memory)
                .await
                .unwrap_err()
                .to_string()
                .contains("ReviewedPhaseHeld")
        );
    }
    let (_, state) = load(&client, &memory.task("report")).await.unwrap();
    assert!(launched(&memory.task(&state.children["r1-p0"].name)));
    assert_eq!(state.children["r0-p0"].uid, phase.uid().unwrap());
    assert!(!state.children.contains_key("r2"));
}

#[tokio::test]
async fn stale_or_unobserved_phase_hold_does_not_override_current_execution() {
    for observed in [None, Some(0)] {
        let (_server, client, memory, mut phase) = phase_fixture().await;
        let status = phase.status.as_mut().unwrap();
        status.observed_generation = observed;
        status.execution_phase = Some("Degraded".into());
        status.execution_detail = Some("obsolete phase hold".into());
        memory.put(object_path(&phase), &phase);
        assert!(!tick(&client, &memory).await.unwrap());
    }
}

#[derive(Clone)]
struct ChangeAuthorityAfterReservation {
    memory: Memory,
    root_path: String,
    change_root: bool,
}

impl Respond for ChangeAuthorityAfterReservation {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let response = self.memory.respond(request);
        let mut objects = self.memory.0.lock().unwrap();
        if self.change_root {
            objects.get_mut(&self.root_path).unwrap()["metadata"]["resourceVersion"] =
                json!("changed");
        } else {
            objects.get_mut(request.url.path()).unwrap()["metadata"]["uid"] = json!("replaced");
        }
        response
    }
}

#[tokio::test]
async fn changed_authority_stops_same_pass_admission_before_the_next_role() {
    for change_root in [true, false] {
        let (server, client, memory) = fixture_with(parallel_root(2)).await;
        assert!(!tick(&client, &memory).await.unwrap());
        let task = memory.task("report");
        let (map, _) = load(&client, &task).await.unwrap();
        Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path(map_path(&map.name_any())))
            .respond_with(ChangeAuthorityAfterReservation {
                memory: memory.clone(),
                root_path: object_path(&task),
                change_root,
            })
            .with_priority(1)
            .mount(&server)
            .await;
        let error = tick(&client, &memory).await.unwrap_err().to_string();
        assert!(error.contains(if change_root {
            "reviewed Task changed during reconciliation"
        } else {
            "reserved reviewed state replaced"
        }));
        let objects = memory.0.lock().unwrap();
        let state: State = serde_json::from_str(
            objects[&map_path(&map.name_any())]["data"]["state.json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(state.children.len(), 1);
        assert!(state.children.contains_key("r0"));
        assert!(objects.values().filter(|v| v["kind"] == "KarsTask").all(
            |v| v["metadata"]["name"] == "report" || v["spec"]["execution"]["launch"] == false
        ));
    }
}

#[tokio::test]
async fn same_pass_progress_still_waits_for_reviewed_dependencies() {
    let mut task = parallel_root(3);
    task.spec
        .blueprint
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap()
        .roles[2]
        .depends_on = vec!["writer".into()];
    ready(&mut task);
    let (_server, client, memory) = fixture_with(task).await;
    for _ in 0..2 {
        assert!(!tick(&client, &memory).await.unwrap());
    }
    let (_, state) = load(&client, &memory.task("report")).await.unwrap();
    assert_eq!(state.children.len(), 2);
    assert!(state.children.contains_key("r0") && state.children.contains_key("r1"));
    assert!(!state.children.contains_key("r2"));
}
