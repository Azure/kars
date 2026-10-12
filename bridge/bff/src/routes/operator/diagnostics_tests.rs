// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, Response, StatusCode};
use axum::routing::get;
use serde_json::{Value, json};
use tower::{ServiceExt, service_fn};

fn fixture(failure: Option<&'static str>, pods: Vec<Value>, present: bool) -> AppState {
    let service = service_fn(move |request: Request<_>| {
        let pods = pods.clone();
        async move {
            assert_eq!(request.method(), "GET", "health must not invoke inference");
            let path = request.uri().path();
            let (target, value) = if path.ends_with("/karssandboxes") {
                let items = if present {
                    vec![
                        json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
                            "metadata":{"name":"bridge-orchestrator","namespace":"kars-system",
                                "labels":{"kars.azure.com/orchestrator":"true"}},
                            "status":{"phase":"Pending"}}),
                        json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
                            "metadata":{"name":"sre","namespace":"kars-system"},
                            "status":{"phase":"Pending"}}),
                    ]
                } else {
                    Vec::new()
                };
                (
                    "sandboxes",
                    json!({"apiVersion":"kars.azure.com/v1alpha1",
                    "kind":"KarsSandboxList","metadata":{},"items":items}),
                )
            } else if path.ends_with("/pods") {
                let target = if path == "/api/v1/pods" {
                    if request
                        .uri()
                        .query()
                        .unwrap_or("")
                        .contains("labelSelector")
                    {
                        "candidates"
                    } else {
                        "pods"
                    }
                } else {
                    "health"
                };
                (
                    target,
                    json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":pods}),
                )
            } else if path.ends_with("/deployments/headlamp") {
                ("headlamp", Value::Null)
            } else {
                panic!("unexpected health request: {path}");
            };
            let (status, value) = if failure == Some(target) {
                (
                    StatusCode::FORBIDDEN,
                    json!({"apiVersion":"v1","kind":"Status",
                    "status":"Failure","code":403,"reason":"Forbidden",
                    "message":"private cluster error detail"}),
                )
            } else if target == "headlamp" {
                (
                    StatusCode::NOT_FOUND,
                    json!({"apiVersion":"v1","kind":"Status",
                    "status":"Failure","code":404,"reason":"NotFound","message":"not found"}),
                )
            } else {
                (StatusCode::OK, value)
            };
            Ok::<_, std::convert::Infallible>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
            )
        }
    });
    AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system")
}

fn routes(state: AppState) -> Router {
    Router::new()
        .route("/diagnostics", get(get_diagnostics))
        .route("/orchestrator", get(get_orchestrator))
        .route("/integrations", get(get_integrations))
        .with_state(state)
}

fn pod(name: &str, created: &str, phase: &str, ready: bool) -> Value {
    json!({"apiVersion":"v1","kind":"Pod",
        "metadata":{"name":name,"namespace":"kars-bridge-orchestrator",
            "creationTimestamp":created,"labels":{"kars.azure.com/sandbox":"bridge-orchestrator"}},
        "spec":{"nodeName":"kind-node","containers":[
            {"name":"agent","image":"agent:latest"},{"name":"inference-router","image":"router:latest"}]},
        "status":{"phase":phase,"containerStatuses":[
            {"name":"agent","image":"agent:latest","imageID":"fixture","ready":ready,"restartCount":0},
            {"name":"inference-router","image":"router:latest","imageID":"fixture","ready":ready,"restartCount":0}]}})
}

#[tokio::test]
async fn unavailable_cluster_reads_never_become_healthy_or_missing_integrations() {
    for (route, target) in [
        ("/diagnostics", "pods"),
        ("/diagnostics", "sandboxes"),
        ("/orchestrator", "sandboxes"),
        ("/orchestrator", "health"),
        ("/orchestrator", "candidates"),
        ("/integrations", "sandboxes"),
        ("/integrations", "health"),
        ("/integrations", "headlamp"),
    ] {
        let response = routes(fixture(Some(target), Vec::new(), true))
            .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_GATEWAY,
            "{route}: {target}"
        );
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "upstream_error");
        assert!(!String::from_utf8_lossy(&body).contains("private cluster error"));
    }
}

#[tokio::test]
async fn successful_empty_reads_remain_distinct_from_api_failures() {
    let app = routes(fixture(None, Vec::new(), false));
    for route in ["/diagnostics", "/integrations"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(route).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        if route == "/diagnostics" {
            assert_eq!(body["scanned_pods"], 0);
            assert_eq!(body["scanned_sandboxes"], 0);
            assert_eq!(body["issues"], json!([]));
        } else {
            assert_eq!(body["sre_present"], false);
            assert_eq!(body["headlamp_deployed"], false);
        }
    }
}

#[tokio::test]
async fn newest_pending_pod_exposes_init_failure_instead_of_old_running_health() {
    let old = pod("old", "2026-10-01T00:00:00Z", "Running", true);
    let mut pending = pod("new", "2026-10-02T00:00:00Z", "Pending", false);
    pending["status"]["containerStatuses"] = json!([]);
    pending["status"]["initContainerStatuses"] = json!([{
        "name":"egress-guard","image":"guard:latest","imageID":"fixture","ready":false,"restartCount":4,
        "state":{"waiting":{"reason":"ImagePullBackOff"}}
    }]);
    let mut deleting = pod("terminating", "2026-10-03T00:00:00Z", "Running", true);
    deleting["metadata"]["deletionTimestamp"] = json!("2026-10-03T00:01:00Z");
    let state = fixture(None, vec![old, pending, deleting], true);
    let health = state
        .cluster()
        .unwrap()
        .sandbox_pod_health("bridge-orchestrator")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((health.ready_containers, health.total_containers), (0, 2));
    assert_eq!(health.waiting_reason.as_deref(), Some("ImagePullBackOff"));
    assert_eq!(health.node.as_deref(), Some("kind-node"));
    assert_eq!(health.restarts, 4);
}

#[tokio::test]
async fn diagnostics_detect_init_failures_and_pods_without_start_or_container_status() {
    let created = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    let mut init_failure = pod("init-failure", &created, "Pending", false);
    init_failure["status"]["initContainerStatuses"] = json!([{
        "name":"egress-guard","image":"guard:latest","imageID":"fixture","ready":false,"restartCount":0,
        "state":{"waiting":{"reason":"ImagePullBackOff"}}
    }]);
    let unscheduled = pod("unscheduled", &created, "Pending", false);
    let mut missing_statuses = pod("missing-statuses", &created, "Running", false);
    missing_statuses["status"]["containerStatuses"] = json!([]);
    for (pod, expected) in [
        (init_failure, "ImagePullBackOff"),
        (unscheduled, "PodPending"),
        (missing_statuses, "NotReady"),
    ] {
        let response = routes(fixture(None, vec![pod], false))
            .oneshot(
                Request::builder()
                    .uri("/diagnostics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["healthy"], false, "{expected}");
        assert_eq!(body["issues"][0]["kind"], expected);
    }
}

#[tokio::test]
async fn unschedulable_pod_reports_the_scheduler_reason() {
    let mut pending = pod("pending", "2026-10-02T00:00:00Z", "Pending", false);
    pending["status"]["conditions"] = json!([{
        "type":"PodScheduled","status":"False","reason":"Unschedulable"
    }]);
    let state = fixture(None, vec![pending], true);
    let health = state
        .cluster()
        .unwrap()
        .sandbox_pod_health("bridge-orchestrator")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(health.waiting_reason.as_deref(), Some("Unschedulable"));
}

#[test]
fn ready_pod_and_direct_configuration_do_not_claim_successful_composition() {
    let health = crate::kars::cluster::PodHealth {
        ready_containers: 2,
        total_containers: 2,
        restarts: 3,
        uptime_seconds: Some(125),
        node: Some("kind-node".into()),
        waiting_reason: None,
    };
    let observation =
        orchestrator_observation(false, true, Some("Running".into()), Some(health), 1);
    assert_eq!(observation.mode, "sandbox");
    assert!(!observation.recommend_direct);
    assert_eq!(observation.sandbox_uptime_seconds, Some(125));
    assert_eq!(observation.sandbox_node.as_deref(), Some("kind-node"));
    assert_eq!(observation.sandbox_restarts, Some(3));
    assert!(observation.note.contains("does not verify"));
    let direct = orchestrator_observation(true, false, None, None, 0);
    assert_eq!(direct.mode, "direct");
    assert!(!direct.recommend_direct);
    assert!(direct.note.contains("not verified"));
}

#[test]
fn fallback_router_is_available_without_a_standing_orchestrator() {
    let observation = orchestrator_observation(false, false, None, None, 1);
    assert_eq!(observation.mode, "sandbox");
    assert!(!observation.recommend_direct);
    assert!(observation.note.contains("other ready sandbox routers"));
    let unavailable = orchestrator_observation(false, true, Some("Pending".into()), None, 0);
    assert_eq!(unavailable.mode, "none");
    assert!(unavailable.recommend_direct);
}
