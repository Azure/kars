// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    routes::tasks::run_evidence::live::tests::{fixture, observed, record},
    state::AppState,
};
use axum::{
    body::Body,
    http::{Method, Request, Response},
};
use k8s_openapi::api::core::v1::ConfigMap;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::service_fn;

fn active() -> (KarsTask, ConfigMap) {
    let (task, cm, mut attempt) = fixture();
    let mut value = serde_json::to_value(task).unwrap();
    value["metadata"]["annotations"]["kars.azure.com/owner-sub"] = json!("owner");
    value["spec"]["execution"] = json!({"launch":true});
    attempt["reply"]["routerObservations"] = json!(observed().to_string());
    (
        serde_json::from_value(value).unwrap(),
        record(&cm, &attempt),
    )
}

async fn read(cluster: &Cluster, task: &KarsTask, principal: &Principal) -> AppResult<Snapshot> {
    super::read(
        cluster,
        task,
        principal,
        Some(&mut reviewed_summary::SummaryBudget::new()),
    )
    .await
}

fn principal() -> Principal {
    Principal {
        sub: "owner".into(),
        name: "Owner".into(),
        roles: vec![],
    }
}

fn cluster(
    task: KarsTask,
    attempt: Option<ConfigMap>,
    pod_status: u16,
) -> (AppState, Arc<Mutex<Vec<String>>>) {
    cluster_with_output(task, attempt, pod_status, None)
}

fn cluster_with_output(
    task: KarsTask,
    attempt: Option<ConfigMap>,
    pod_status: u16,
    output: Option<Value>,
) -> (AppState, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let requests = calls.clone();
    let service = service_fn(move |request: Request<_>| {
        assert_eq!(request.method(), Method::GET);
        let path = request.uri().path();
        requests.lock().unwrap().push(path.to_owned());
        let (status, value) = match path {
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karstasks/briefing" => {
                (200, Some(serde_json::to_value(&task).unwrap()))
            }
            "/api/v1/namespaces/kars-briefing/pods" => (
                pod_status,
                (pod_status == 200).then(|| {
                    json!({
                        "apiVersion":"v1", "kind":"PodList", "metadata":{},
                        "items":[{"metadata":{"name":"worker-pod"},"status":{"phase":"Running"}}]
                    })
                }),
            ),
            "/api/v1/namespaces/kars-system/configmaps/kars-mission-attempt-b91b6299b2f5d3e71056cd50f6e4ac6d9874eae1" => {
                (
                    if attempt.is_some() { 200 } else { 404 },
                    attempt.as_ref().map(|cm| serde_json::to_value(cm).unwrap()),
                )
            }
            "/api/v1/namespaces/kars-system/configmaps/kars-mission-output-briefing" => {
                (if output.is_some() { 200 } else { 404 }, output.clone())
            }
            _ => panic!("unexpected fleet API read: {path}"),
        };
        let value = value.unwrap_or_else(|| json!({"apiVersion":"v1","kind":"Status","status":"Failure", "reason":if status == 404 {"NotFound"} else {"Forbidden"},"code":status,"message":"unavailable"}));
        async move {
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system"),
        calls,
    )
}

#[tokio::test]
async fn direct_worker_reuses_validated_assignment_observations_without_proxy() {
    let (task, cm) = active();
    let (state, calls) = cluster(task.clone(), Some(cm), 200);
    let snapshot = read(state.cluster().unwrap(), &task, &principal())
        .await
        .unwrap();
    assert_eq!(snapshot.phase, "Running");
    assert_eq!(snapshot.workers.len(), 1);
    assert!(!snapshot.workers[0].events.as_ref().unwrap().is_empty());
    let calls = calls.lock().unwrap();
    assert!(calls.last().unwrap().ends_with("/karstasks/briefing"));
    assert!(
        calls
            .iter()
            .all(|path| !path.contains("proxy") && !path.contains("mission-trace"))
    );
}

#[tokio::test]
async fn missing_assignment_is_unavailable_not_measured_zero() {
    let (task, _) = active();
    let (state, _) = cluster(task.clone(), None, 200);
    let snapshot = read(state.cluster().unwrap(), &task, &principal())
        .await
        .unwrap();
    assert_eq!(snapshot.workers.len(), 1);
    assert!(snapshot.workers[0].events.is_none());
}

#[tokio::test]
async fn missing_pod_authority_never_becomes_an_idle_fleet() {
    let (task, cm) = active();
    let (state, _) = cluster(task.clone(), Some(cm), 403);
    assert!(
        read(state.cluster().unwrap(), &task, &principal())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn changed_authority_after_reads_revokes_the_whole_snapshot() {
    let (task, cm) = active();
    for (key, value) in [
        ("kars.azure.com/run-requested", "run-2"),
        ("kars.azure.com/owner-sub", "someone-else"),
        ("kars.azure.com/reviewed-state", "replaced-binding"),
    ] {
        let mut fresh = task.clone();
        fresh
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(key.into(), value.into());
        let (state, _) = cluster(fresh, Some(cm.clone()), 200);
        assert!(
            read(state.cluster().unwrap(), &task, &principal())
                .await
                .is_err(),
            "{key}"
        );
    }
}

#[tokio::test]
async fn completed_and_stopped_assignments_never_count_lingering_pods_as_workers() {
    let (task, cm) = active();
    for completed in [false, true] {
        let mut value = serde_json::to_value(&task).unwrap();
        if completed {
            value["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!("run-1");
        } else {
            value["spec"]["execution"]["launch"] = json!(false);
            value["metadata"]["annotations"]["kars.azure.com/halted"] = json!("Stopped by owner");
        }
        let task: KarsTask = serde_json::from_value(value).unwrap();
        let (state, calls) = cluster(task.clone(), Some(cm.clone()), 200);
        let snapshot = read(state.cluster().unwrap(), &task, &principal())
            .await
            .unwrap();
        assert!(snapshot.workers.is_empty());
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|path| !path.ends_with("/pods") && !path.contains("mission-attempt"))
        );
    }
}

fn reviewed_fixture(
    attempt_phase: &str,
    changed_child: bool,
) -> (KarsTask, AppState, Arc<Mutex<Vec<String>>>) {
    reviewed_fixture_with_direct(attempt_phase, changed_child, false, false)
}

fn reviewed_fixture_with_direct(
    attempt_phase: &str,
    changed_child: bool,
    direct: bool,
    invalid_state: bool,
) -> (KarsTask, AppState, Arc<Mutex<Vec<String>>>) {
    use kube::Resource;
    let mut root = serde_json::to_value(active().0).unwrap();
    root["spec"]["execution"]["launch"] = json!(false);
    root["status"] = json!({"executionPhase":"Idle"});
    root["spec"]["blueprint"] = json!({"executionPlan":{
        "schema":"kars.execution-plan/v1", "maxParallel":1,
        "roles":[{"name":"tester", "objective":"Check delivered files", "dependsOn":[],
            "phases":[{"name":"check", "objective":"Execute retained tests", "capabilities":[],
                "minToolCalls":0, "maxToolCalls":6, "freshContext":true}]}],
        "synthesis":{"objective":"Collect verified results", "capabilities":[], "maxToolCalls":6},
        "deliverables":[{"name":"report.md"}]
    }});
    if direct {
        root["spec"]["blueprint"]["executionPlan"]["roles"] = json!([]);
        root["metadata"]["generation"] = json!(3);
        root["status"]["observedGeneration"] = json!(3);
        root["metadata"]["annotations"]["kars.azure.com/run-ack"] =
            root["metadata"]["annotations"]["kars.azure.com/run-requested"].clone();
    }
    let pin = json!({"name":"kars-reviewed-test", "uid":"state-uid"}).to_string();
    root["metadata"]["annotations"]["kars.azure.com/reviewed-state"] = json!(pin);
    let root: KarsTask = serde_json::from_value(root).unwrap();
    let child = |name: &str, slot: &str, parent: &KarsTask| {
        let mut value = serde_json::to_value(&root).unwrap();
        value["metadata"]["name"] = json!(name);
        value["metadata"]["uid"] = json!(format!("{name}-uid"));
        value["metadata"]["ownerReferences"] = json!([parent.controller_owner_ref(&()).unwrap()]);
        value["metadata"]["annotations"]["kars.azure.com/run-requested"] =
            json!(format!("plan-test-{slot}"));
        value["metadata"]["annotations"]["kars.azure.com/reviewed-root"] = json!(root.name_any());
        value["metadata"]["annotations"]["kars.azure.com/reviewed-slot"] = json!(slot);
        value["spec"]["parentRef"] = json!({"name":parent.name_any()});
        serde_json::from_value::<KarsTask>(value).unwrap()
    };
    let role = child("tester", "r0", &root);
    let phase = child("tester-check", "r0-p0", &role);
    let mut plan = serde_json::to_value(
        root.spec
            .blueprint
            .as_ref()
            .unwrap()
            .execution_plan
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.values_mut().for_each(sort);
                map.sort_keys();
            }
            Value::Array(values) => values.iter_mut().for_each(sort),
            _ => {}
        }
    }
    sort(&mut plan);
    let mut state = json!({"schema":"kars.reviewed-run/v2", "rootName":root.name_any(), "rootUid":root.uid(),
    "admission":{"state":"run", "runNonce":root.annotations()["kars.azure.com/run-requested"]},
    "planDigest":format!("sha256:{}", crate::providers::signing::sha256_hex(&serde_json::to_vec(&plan).unwrap())),
    "plan":plan, "children":{
        "r0":{"name":role.name_any(), "uid":role.uid(), "spec":role.spec},
        "r0-p0":{"name":phase.name_any(), "uid":phase.uid(), "spec":phase.spec}
    }});
    if direct {
        state["children"] = json!({});
    }
    if invalid_state {
        state["rootUid"] = json!("replacement-root");
    }
    let identity = json!({"namespace":"kars-system", "taskName":phase.name_any(), "taskUid":phase.uid(),
        "runNonce":phase.annotations()["kars.azure.com/run-requested"]});
    let attempt_name = Cluster::mission_attempt_name(
        &phase.uid().unwrap(),
        &phase.annotations()["kars.azure.com/run-requested"],
    );
    let attempt = json!({"apiVersion":"v1", "kind":"ConfigMap",
        "metadata":{"name":attempt_name, "namespace":"kars-system", "uid":"attempt-uid", "resourceVersion":"1",
            "ownerReferences":[phase.controller_owner_ref(&()).unwrap()]},
        "data":{"attempt.json":json!({"version":1, "candidate":identity, "assignment":identity,
            "phase":attempt_phase, "reply":{"status":"running"}}).to_string()}});
    let map = json!({"apiVersion":"v1", "kind":"ConfigMap",
        "metadata":{"name":"kars-reviewed-test", "namespace":"kars-system", "uid":"state-uid",
            "ownerReferences":[root.controller_owner_ref(&()).unwrap()]},
        "data":{"state.json":state.to_string()}});
    let mut resources = BTreeMap::new();
    for task in [&root, &role, &phase] {
        resources.insert(
            format!(
                "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karstasks/{}",
                task.name_any()
            ),
            serde_json::to_value(task).unwrap(),
        );
    }
    resources.insert(
        "/api/v1/namespaces/kars-system/configmaps/kars-reviewed-test".into(),
        map,
    );
    resources.insert(
        format!("/api/v1/namespaces/kars-system/configmaps/{attempt_name}"),
        attempt,
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let requests = calls.clone();
    let service = service_fn(move |request: Request<_>| {
        assert_eq!(request.method(), Method::GET);
        let path = request.uri().path().to_owned();
        let mut calls = requests.lock().unwrap();
        calls.push(path.clone());
        let mut value = resources.get(&path).cloned();
        if changed_child
            && path.ends_with("/karstasks/tester-check")
            && calls.iter().filter(|p| *p == &path).count() > 1
        {
            value.as_mut().unwrap()["metadata"]["uid"] = json!("replacement-uid");
        }
        let (status, value) = match value {
            Some(value) => (200, value),
            None => {
                assert!(
                    path.ends_with("/configmaps/kars-mission-output-briefing"),
                    "unexpected read: {path}"
                );
                (
                    404,
                    json!({"apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"NotFound", "code":404}),
                )
            }
        };
        async move {
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        root,
        AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system"),
        calls,
    )
}

#[tokio::test]
async fn idled_reviewed_runs_use_retained_attempts_without_output_or_pod_reads() {
    for (attempt, expected) in [
        ("failed", "Errored"),
        ("uncertain", "Held"),
        ("succeeded", "Unavailable"),
    ] {
        let (task, state, calls) = reviewed_fixture(attempt, false);
        let snapshot = read(state.cluster().unwrap(), &task, &principal())
            .await
            .unwrap();
        assert_eq!(snapshot.phase, expected, "{attempt}");
        assert!(snapshot.output.is_none());
        assert!(snapshot.workers.is_empty());
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|path| !path.ends_with("/pods") && !path.contains("proxy"))
        );
    }
}

#[tokio::test]
async fn changed_reviewed_child_is_unavailable_not_idle_or_failed() {
    let (task, state, _) = reviewed_fixture("failed", true);
    let snapshot = read(state.cluster().unwrap(), &task, &principal())
        .await
        .unwrap();
    assert_eq!(snapshot.phase, "Unavailable");
    assert!(snapshot.workers.is_empty());
}

fn stopped_direct() -> KarsTask {
    let mut task = serde_json::to_value(active().0).unwrap();
    task["spec"]["execution"]["launch"] = json!(false);
    task["metadata"]["generation"] = json!(3);
    task["metadata"]["annotations"]["kars.azure.com/run-ack"] =
        task["metadata"]["annotations"]["kars.azure.com/run-requested"].clone();
    task["status"] = json!({"observedGeneration":3, "executionPhase":"Idle"});
    serde_json::from_value(task).unwrap()
}

#[tokio::test]
async fn ordinary_direct_stop_uses_fresh_drained_state_without_live_reads() {
    let task = stopped_direct();
    let (state, calls) = cluster(task.clone(), None, 200);
    let snapshot = read(state.cluster().unwrap(), &task, &principal())
        .await
        .unwrap();
    assert_eq!(snapshot.phase, "Stopped");
    assert!(snapshot.workers.is_empty());
    assert!(snapshot.output.is_none());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|path| !path.ends_with("/pods") && !path.contains("mission-attempt"))
    );

    let mut fresh = serde_json::to_value(&task).unwrap();
    fresh["status"]["executionPhase"] = json!("Stopping");
    let (state, _) = cluster(serde_json::from_value(fresh).unwrap(), None, 200);
    assert_eq!(
        read(state.cluster().unwrap(), &task, &principal())
            .await
            .unwrap()
            .phase,
        "Stopping"
    );
}

#[tokio::test]
async fn empty_reviewed_plan_requires_valid_custody_before_stopped_badge() {
    for (invalid, expected) in [(false, "Stopped"), (true, "Unavailable")] {
        let (task, state, calls) = reviewed_fixture_with_direct("failed", false, true, invalid);
        let snapshot = read(state.cluster().unwrap(), &task, &principal())
            .await
            .unwrap();
        assert_eq!(snapshot.phase, expected);
        assert!(snapshot.workers.is_empty());
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|path| !path.ends_with("/pods") && !path.contains("mission-attempt"))
        );
    }
}

#[tokio::test]
async fn ordinary_stop_does_not_override_matching_terminal_outputs() {
    for (status, expected) in [("ok", "Delivered"), ("error", "Errored")] {
        let mut task = stopped_direct();
        let nonce = task.annotations()["kars.azure.com/run-requested"].clone();
        task.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/run-completed".into(), nonce.clone());
        let output = json!({"apiVersion":"v1", "kind":"ConfigMap",
            "metadata":{"name":"kars-mission-output-briefing", "namespace":"kars-system",
                "annotations":{"kars.azure.com/mission-task-uid":task.uid(),
                    "kars.azure.com/mission-run-nonce":nonce,
                    "kars.azure.com/mission-principal-name":task.name_any()},
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
                    "name":task.name_any(), "uid":task.uid(), "controller":true}]},
            "data":{"taskName":task.name_any(), "taskUid":task.uid(), "assignmentNonce":nonce, "status":status}});
        let (state, _) = cluster_with_output(task.clone(), None, 200, Some(output));
        assert_eq!(
            read(state.cluster().unwrap(), &task, &principal())
                .await
                .unwrap()
                .phase,
            expected
        );
    }
}

#[test]
fn ordinary_stop_excludes_drafts_draining_stale_status_completed_and_team_runs() {
    let mut task = serde_json::to_value(stopped_direct()).unwrap();
    task["status"]["sandboxRef"] = Value::Null;
    assert!(ordinary_stopped_direct_mission(
        &serde_json::from_value(task.clone()).unwrap()
    ));
    for (pointer, value) in [
        ("/spec/execution", Value::Null),
        ("/spec/execution/launch", json!(true)),
        ("/status", Value::Null),
        ("/status/observedGeneration", json!(2)),
        ("/status/executionPhase", json!("Stopping")),
        ("/status/sandboxRef", json!({"name":"lingering"})),
        ("/metadata/generation", Value::Null),
        (
            "/metadata/annotations/kars.azure.com~1run-ack",
            json!("other-run"),
        ),
        (
            "/metadata/annotations/kars.azure.com~1run-requested",
            json!("invalid nonce"),
        ),
    ] {
        let mut changed = task.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            !ordinary_stopped_direct_mission(&serde_json::from_value(changed).unwrap()),
            "{pointer}"
        );
    }
    for (key, value) in [
        ("kars.azure.com/team-role", json!("taskforce")),
        ("kars.azure.com/reviewed-root", json!("root")),
        ("kars.azure.com/reviewed-slot", json!("r0-p0")),
        (
            "kars.azure.com/run-completed",
            task["metadata"]["annotations"]["kars.azure.com/run-requested"].clone(),
        ),
    ] {
        let mut changed = task.clone();
        changed["metadata"]["annotations"][key] = value;
        assert!(
            !ordinary_stopped_direct_mission(&serde_json::from_value(changed).unwrap()),
            "{key}"
        );
    }
    let mut changed = task.clone();
    changed["metadata"]["labels"] = json!({"kars.azure.com/team":"standing-team"});
    assert!(!ordinary_stopped_direct_mission(
        &serde_json::from_value(changed).unwrap()
    ));
    let mut changed = task;
    changed["spec"]["parentRef"] = json!({"name":"parent"});
    assert!(!ordinary_stopped_direct_mission(
        &serde_json::from_value(changed).unwrap()
    ));
}

#[test]
fn repeated_tool_ids_in_different_rounds_are_distinct_results() {
    let result = counts(&[
        json!({"kind":"tool_result","round":1,"call_id":"a"}),
        json!({"kind":"tool_result","round":2,"call_id":"a"}),
        json!({"kind":"tool_result","round":2,"call_id":"a"}),
    ]);
    assert_eq!(result.tools, 2);
}
