// Copyright (c) Pal Lakatos-Toth.
// Integration tests for the kars Bridge BFF router. Exercises the public
// HTTP surface in-process (no socket bind) via tower's oneshot. These run
// without a cluster — AppState falls back to web-only mode.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use kars_bridge_bff::routes;
use kars_bridge_bff::state::AppState;
use tower::ServiceExt;

fn test_router() -> axum::Router {
    let state = AppState::web_only("default".to_string());
    routes::router(state)
}

#[tokio::test]
async fn healthz_returns_ok() {
    let app = test_router();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn readyz_fails_closed_without_a_compatible_cluster() {
    let app = test_router();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"status": "unavailable", "cluster_configured": false})
    );
}

#[tokio::test]
async fn unknown_route_returns_structured_404() {
    let app = test_router();
    let resp = app
        .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "not_found");
}

#[tokio::test]
async fn list_tasks_without_cluster_reports_unavailable() {
    let app = test_router();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/namespaces/default/tasks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // No cluster wired in the test env → honest 503, structured body.
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "cluster_unavailable");
}
