// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Slice 2d.2 — integration test for `forward_with_failover` end-to-end.
//!
//! Stands up a tiny axum upstream that branches on the `model` field of
//! the request body (set by `proxy::build_upstream_url`):
//! * `primary-down` → 503
//! * `fallback-up` → 200
//!
//! Asserts: failover walker tries `primary-down` first (503 recorded),
//! then `fallback-up` (200 returned), and the health cache reflects
//! the streak + success.

#![allow(clippy::await_holding_lock)]

use axum::Router;
use axum::body::Bytes as AxumBytes;
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::any;
use bytes::Bytes;
use kars_inference_router::auth::WorkloadIdentityAuth;
use kars_inference_router::deployment_health::DeploymentHealthRegistry;
use kars_inference_router::failover::forward_with_failover;
use kars_inference_router::inference_policy_loader::{
    InferencePolicySnapshot, ModelPreference, ModelRef,
};
use kars_inference_router::proxy::UpstreamConfig;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::net::TcpListener;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn scrub_env() {
    unsafe {
        for k in [
            "AZURE_OPENAI_API_KEY",
            "AZURE_TENANT_ID",
            "AZURE_CLIENT_ID",
            "IMDS_CLIENT_ID",
            "AZURE_FEDERATED_TOKEN_FILE",
            "AZURE_AD_ENDPOINT",
            "AZURE_IMDS_ENDPOINT",
        ] {
            std::env::remove_var(k);
        }
    }
}

unsafe fn set_env(k: &str, v: &str) {
    unsafe { std::env::set_var(k, v) }
}

async fn branch_dispatch(req: Request) -> Response {
    let (_parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let model = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_default();

    let (status, body) = match model.as_str() {
        "primary-down" => (
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":{"code":"upstream_down","message":"primary deployment is unavailable"}}"#,
        ),
        "fallback-up" => (
            StatusCode::OK,
            r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"total_tokens":7}}"#,
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":{"code":"unknown_deployment"}}"#,
        ),
    };

    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(AxumBytes::from(body)))
        .unwrap()
}

async fn start_upstream() -> SocketAddr {
    let app: Router = Router::new().fallback(any(branch_dispatch));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

fn snapshot(primary: &str, fallback: &[&str]) -> InferencePolicySnapshot {
    InferencePolicySnapshot {
        digest: "sha256:test".into(),
        model_preference: Some(ModelPreference {
            primary: ModelRef {
                provider: "Foundry".into(),
                deployment: primary.into(),
            },
            fallback: fallback
                .iter()
                .map(|d| ModelRef {
                    provider: "Foundry".into(),
                    deployment: (*d).into(),
                })
                .collect(),
        }),
        ..InferencePolicySnapshot::default()
    }
}

#[tokio::test]
async fn primary_503_falls_through_to_fallback_200() {
    let _g = ENV_LOCK.lock().unwrap();
    scrub_env();
    unsafe { set_env("AZURE_OPENAI_API_KEY", "test-key") };

    let addr = start_upstream().await;
    let base = format!("http://{addr}");

    let auth = WorkloadIdentityAuth::new();
    let client = reqwest::Client::new();
    let health = Arc::new(DeploymentHealthRegistry::new());

    let upstream = UpstreamConfig {
        endpoint: base,
        deployment: "fallback-up".into(),
        sandbox_name: "sbx".into(),
    };
    let snap = snapshot("primary-down", &["fallback-up"]);

    let (status, _hdrs, body) = forward_with_failover(
        &auth,
        None,
        &client,
        &health,
        &upstream,
        &snap,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"messages":[]}"#),
    )
    .await
    .expect("forward_with_failover error");

    assert_eq!(status, StatusCode::OK);
    let body_json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body_json["usage"]["total_tokens"], 7);

    // Health cache should reflect: primary observed a failure,
    // fallback observed a success.
    let snaps = health.snapshot();
    let primary = snaps
        .iter()
        .find(|s| s.deployment == "primary-down")
        .expect("primary health entry missing");
    let fallback = snaps
        .iter()
        .find(|s| s.deployment == "fallback-up")
        .expect("fallback health entry missing");
    assert_eq!(primary.failure_streak, 1);
    assert!(primary.healthy, "single 503 under 3-strike threshold");
    assert_eq!(fallback.failure_streak, 0);
    assert!(fallback.last_success_at_ms > 0);
}

#[tokio::test]
async fn unhealthy_primary_is_skipped_in_second_pass() {
    let _g = ENV_LOCK.lock().unwrap();
    scrub_env();
    unsafe { set_env("AZURE_OPENAI_API_KEY", "test-key") };

    let addr = start_upstream().await;
    let base = format!("http://{addr}");

    let auth = WorkloadIdentityAuth::new();
    let client = reqwest::Client::new();
    let health = Arc::new(DeploymentHealthRegistry::new());

    // Pre-mark primary unhealthy (3 failures = at threshold).
    for _ in 0..3 {
        health.record_failure("primary-down");
    }
    assert!(!health.is_healthy("primary-down"));

    let upstream = UpstreamConfig {
        endpoint: base,
        deployment: "fallback-up".into(),
        sandbox_name: "sbx".into(),
    };
    let snap = snapshot("primary-down", &["fallback-up"]);

    let (status, _hdrs, _body) = forward_with_failover(
        &auth,
        None,
        &client,
        &health,
        &upstream,
        &snap,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"messages":[]}"#),
    )
    .await
    .expect("forward_with_failover error");

    assert_eq!(status, StatusCode::OK);
    // Primary's streak was NOT bumped — it was skipped, not attempted.
    let snaps = health.snapshot();
    let primary = snaps
        .iter()
        .find(|s| s.deployment == "primary-down")
        .unwrap();
    assert_eq!(primary.failure_streak, 3, "no new failure recorded");
}

#[tokio::test]
async fn all_unhealthy_still_punches_primary_for_last_resort() {
    let _g = ENV_LOCK.lock().unwrap();
    scrub_env();
    unsafe { set_env("AZURE_OPENAI_API_KEY", "test-key") };

    let addr = start_upstream().await;
    let base = format!("http://{addr}");

    let auth = WorkloadIdentityAuth::new();
    let client = reqwest::Client::new();
    let health = Arc::new(DeploymentHealthRegistry::new());

    // Mark BOTH unhealthy.
    for _ in 0..3 {
        health.record_failure("primary-down");
        health.record_failure("fallback-up");
    }
    assert!(!health.is_healthy("primary-down"));
    assert!(!health.is_healthy("fallback-up"));

    let upstream = UpstreamConfig {
        endpoint: base,
        deployment: "primary-down".into(),
        sandbox_name: "sbx".into(),
    };
    let snap = snapshot("primary-down", &["fallback-up"]);

    let (status, _hdrs, _body) = forward_with_failover(
        &auth,
        None,
        &client,
        &health,
        &upstream,
        &snap,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"messages":[]}"#),
    )
    .await
    .expect("forward_with_failover error");

    // Punch-through attempted primary (still 503) and recorded
    // another failure.
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let snaps = health.snapshot();
    let primary = snaps
        .iter()
        .find(|s| s.deployment == "primary-down")
        .unwrap();
    assert_eq!(primary.failure_streak, 4);
}
