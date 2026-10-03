// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

const DETAIL: &str = "/api/namespaces/kars-system/tasks/briefing";
const STREAM: &str = "/api/namespaces/kars-system/tasks/briefing/stream?run_nonce=run-1";

fn changed_tasks() -> Vec<(Value, StatusCode)> {
    let mut changes = Vec::new();
    for (pointer, value, code) in [
        ("/metadata/uid", "replacement", StatusCode::CONFLICT),
        (
            "/metadata/annotations/kars.azure.com~1run-requested",
            "run-2",
            StatusCode::CONFLICT,
        ),
        (
            "/metadata/annotations/kars.azure.com~1run-completed",
            "run-2",
            StatusCode::CONFLICT,
        ),
        (
            "/metadata/annotations/kars.azure.com~1owner-sub",
            "another",
            StatusCode::NOT_FOUND,
        ),
        ("/metadata/namespace", "another", StatusCode::NOT_FOUND),
    ] {
        let mut next = task();
        *next.pointer_mut(pointer).unwrap() = json!(value);
        changes.push((next, code));
    }
    let mut deleting = task();
    deleting["metadata"]["deletionTimestamp"] = json!("2026-10-03T00:00:00Z");
    changes.push((deleting, StatusCode::NOT_FOUND));
    changes.push((Value::Null, StatusCode::NOT_FOUND));
    changes
}

#[tokio::test]
async fn revision_detail_rechecks_after_reading_output() {
    for (next, expected) in changed_tasks() {
        let fixture = Fixture::new().await;
        {
            let mut store = fixture.store.lock().unwrap();
            store.task.as_object_mut().unwrap().remove("status");
            store.rotate_on_trace = Some(next.clone());
        }
        let (code, body) = fixture.read_route(DETAIL).await;
        assert_eq!(code, expected, "{next}");
        assert!(!body.contains("Useful briefing"));
        let store = fixture.store.lock().unwrap();
        assert!(
            store
                .requests
                .iter()
                .any(|(_, path)| path.ends_with("kars-mission-output-briefing"))
        );
        assert!(
            store
                .requests
                .iter()
                .all(|(method, _)| method == Method::GET)
        );
    }
}

#[tokio::test]
async fn revision_detail_returns_bound_result_and_allows_status_updates() {
    let fixture = Fixture::new().await;
    {
        let mut store = fixture.store.lock().unwrap();
        store.task.as_object_mut().unwrap().remove("status");
        let mut next = store.task.clone();
        next["metadata"]["resourceVersion"] = json!("999");
        store.rotate_on_trace = Some(next);
    }
    let (code, body) = fixture.read_route(DETAIL).await;
    assert_eq!(code, StatusCode::OK);
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["result"]["assignment_nonce"], "run-1");
    assert_eq!(value["result"]["output"], "Useful briefing");
    assert_eq!(value["result"]["reviewable"], true);
    assert_eq!(value["activity"], json!([]));
}

#[tokio::test]
async fn revision_list_joins_against_final_task_snapshot() {
    for (next, _) in changed_tasks() {
        let fixture = Fixture::new().await;
        fixture.store.lock().unwrap().rotate_on_second_list = Some(next.clone());
        let (code, body) = fixture
            .read_route("/api/namespaces/kars-system/tasks")
            .await;
        assert_eq!(code, StatusCode::OK);
        let rows: Vec<Value> = serde_json::from_str(&body).unwrap();
        assert!(
            rows.iter().all(|row| row["delivered"] != true),
            "{next}: {body}"
        );
        assert_eq!(fixture.store.lock().unwrap().task_lists, 2);
    }
    let fixture = Fixture::new().await;
    let (_, body) = fixture
        .read_route("/api/namespaces/kars-system/tasks")
        .await;
    let rows: Vec<Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["delivered"], true);
}

#[tokio::test]
async fn revision_stream_refuses_stale_nonce_and_unsupported_namespace() {
    let fixture = Fixture::new().await;
    for (uri, expected) in [
        (
            "/api/namespaces/kars-system/tasks/briefing/stream?run_nonce=old",
            StatusCode::CONFLICT,
        ),
        (
            "/api/namespaces/another/tasks/briefing/stream?run_nonce=run-1",
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(fixture.read_route(uri).await.0, expected);
    }
    assert!(
        fixture
            .store
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(_, path)| path == TASK)
    );
}

#[tokio::test]
async fn revision_stream_rechecks_completed_nonce_before_terminal_emission() {
    for (next, _) in changed_tasks() {
        let fixture = Fixture::new().await;
        fixture.store.lock().unwrap().rotate_on_trace = Some(next.clone());
        let (code, body) = fixture.read_route(STREAM).await;
        assert_eq!(code, StatusCode::OK);
        assert!(body.contains("event: end"), "{next}: {body}");
        assert!(!body.contains("event: done"), "{next}: {body}");
        assert!(!body.contains("Useful briefing"));
    }
}

#[tokio::test]
async fn revision_stream_completes_active_revision_and_filters_trace() {
    let fixture = Fixture::new().await;
    {
        let mut store = fixture.store.lock().unwrap();
        store.task["metadata"]["annotations"][COMPLETED] = json!("previous");
        store.rotate_on_output = Some(task());
        let mut trace = output(&task());
        trace["metadata"]["name"] = json!("kars-mission-trace-briefing");
        trace["data"]["trace.json"] = json!(
            serde_json::to_string(&json!([
                {"runNonce":"run-1","marker":"CURRENT"},
                {"runNonce":"old","marker":"OLD"},
                {"marker":"UNBOUND"},
                {"runNonce":"run-1","assignmentNonce":"old","marker":"CONFLICT"}
            ]))
            .unwrap()
        );
        store.trace = Some(trace);
    }
    let (code, body) = fixture.read_route(STREAM).await;
    assert_eq!(code, StatusCode::OK);
    assert!(body.contains("CURRENT"), "{body}");
    assert!(body.contains("event: done"));
    for forbidden in ["OLD", "UNBOUND", "CONFLICT"] {
        assert!(!body.contains(forbidden));
    }
    assert!(
        fixture
            .store
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, path)| method == Method::GET
                && !path.contains("proxy")
                && !path.contains("pods"))
    );
}

#[tokio::test]
async fn revision_stream_artifact_presence_is_not_completion() {
    let fixture = Fixture::new().await;
    {
        let mut store = fixture.store.lock().unwrap();
        store.output["data"]
            .as_object_mut()
            .unwrap()
            .remove("status");
        store.output["data"]["artifactCount"] = json!("2");
    }
    let shared = fixture.store.clone();
    let mutation = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        shared.lock().unwrap().task["metadata"]["annotations"][REQUESTED] = json!("new-run");
    });
    let (_, body) = fixture.read_route(STREAM).await;
    mutation.await.unwrap();
    assert!(body.contains("event: unavailable"));
    assert!(body.contains("event: end"));
    assert!(!body.contains("event: done"));
}
