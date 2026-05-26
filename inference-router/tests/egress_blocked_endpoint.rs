// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Integration tests for `GET /egress/learned/blocked` (S12.f).
//!
//! Mounts the egress sub-router with a real `AppState` and verifies the
//! blocked-egress ring buffer is surfaced over HTTP.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt;

use kars_inference_router::auth::WorkloadIdentityAuth;
use kars_inference_router::blocklist::Blocklist;
use kars_inference_router::budget::TokenBudgetTracker;
use kars_inference_router::config::{Config, RegistryMode};
use kars_inference_router::egress_blocked::BlockedBuffer;
use kars_inference_router::governance::Governance;
use kars_inference_router::handoff::{
    DrainState, HandoffSession, HandoffTokenStore, PendingHandoffStore,
};
use kars_inference_router::mesh::{MeshInbox, MeshMetrics};
use kars_inference_router::providers::{AuditSink, PolicyDecisionProvider, SigningProvider};
use kars_inference_router::routes::{AppState, egress_routes};

fn test_state() -> AppState {
    let policy_status = Arc::new(kars_inference_router::policy_status::PolicyStatusRegistry::new());
    let governance = Arc::new(Governance::new_with_status(
        "sb-test",
        policy_status.clone(),
    ));
    AppState {
        auth: Arc::new(WorkloadIdentityAuth::new()),
        copilot: Arc::new(kars_inference_router::copilot_auth::CopilotTokenCache::from_env()),
        client: reqwest::Client::new(),
        config: Arc::new(Config {
            port: 0,
            foundry_endpoint: None,
            foundry_project_endpoint: None,
            azure_openai_endpoint: None,
            default_model: "gpt-4".into(),
            content_safety_enabled: false,
            prompt_shields_enabled: false,
            content_safety_endpoint: None,
            token_budget_daily: 1_000_000,
            token_budget_per_request: 100_000,
            registry_mode: RegistryMode::Local,
            registry_url: None,
            provider_override: None,
        }),
        budget: TokenBudgetTracker::new(1_000_000, 100_000),
        policy_provider: Arc::clone(&governance) as Arc<dyn PolicyDecisionProvider>,
        audit_sink: Arc::clone(&governance) as Arc<dyn AuditSink>,
        signing_provider: Arc::clone(&governance) as Arc<dyn SigningProvider>,
        governance,
        blocklist: Blocklist::disabled(),
        blocked_egress: Arc::new(BlockedBuffer::with_defaults()),
        sandbox_name: Arc::new("sb-test".to_string()),
        inbox: Arc::new(MeshInbox::new()),
        mesh_metrics: Arc::new(MeshMetrics::new()),
        model_override: Arc::new(std::sync::RwLock::new(None)),
        admin_token: None,
        responses_only_models: Arc::new(std::sync::RwLock::new(Default::default())),
        handoff_tokens: HandoffTokenStore::new(),
        handoff_session: HandoffSession::new(),
        drain_state: DrainState::new(),
        pending_handoff: PendingHandoffStore::new(),
        policy_status,
        inference_policy: kars_inference_router::inference_policy_loader::empty_handle(),
        memory_binding: kars_inference_router::memory_binding_loader::empty_handle(),
        egress_allowlist: kars_inference_router::egress_allowlist_loader::empty_handle(),
        deployment_health: std::sync::Arc::new(
            kars_inference_router::deployment_health::DeploymentHealthRegistry::new(),
        ),
    }
}

fn app(state: AppState) -> Router {
    Router::new().merge(egress_routes()).with_state(state)
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::get(uri).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn endpoint_returns_empty_when_no_blocks() {
    let state = test_state();
    let app = app(state);
    let (status, body) = get_json(&app, "/egress/learned/blocked").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);
    assert_eq!(body["count"], 0);
    assert!(body["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn endpoint_returns_recorded_blocks() {
    let state = test_state();
    state
        .blocked_egress
        .record("sb-test", "evil.example.com", 443);
    state
        .blocked_egress
        .record("sb-test", "evil.example.com", 443); // dedup
    state
        .blocked_egress
        .record("sb-test", "other.example.com", 80);

    let app = app(state);
    let (status, body) = get_json(&app, "/egress/learned/blocked").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["count"], 2);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);

    // Newest first: "other.example.com" was inserted last.
    assert_eq!(entries[0]["host"], "other.example.com");
    assert_eq!(entries[0]["port"], 80);
    assert_eq!(entries[0]["count"], 1);
    assert_eq!(entries[1]["host"], "evil.example.com");
    assert_eq!(entries[1]["port"], 443);
    assert_eq!(entries[1]["count"], 2);
    assert_eq!(entries[1]["source_sandbox"], "sb-test");
}

// ---------------------------------------------------------------------------
// Slice 5a — `/internal/egress/blocked` + `/internal/egress/blocked/top`
// ---------------------------------------------------------------------------

fn internal_app(state: AppState) -> Router {
    Router::new()
        .merge(kars_inference_router::routes::internal_routes())
        .with_state(state)
}

#[tokio::test]
async fn internal_blocked_returns_empty_when_no_blocks() {
    let state = test_state();
    let app = internal_app(state);
    let (status, body) = get_json(&app, "/internal/egress/blocked").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["total"], 0);
    assert_eq!(body["count"], 0);
    assert_eq!(body["since_unix"], 0);
    assert!(body["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn internal_blocked_returns_entries_with_rfc3339_strings() {
    let state = test_state();
    state
        .blocked_egress
        .record("sb-test", "evil.example.com", 443);

    let app = internal_app(state);
    let (status, body) = get_json(&app, "/internal/egress/blocked").await;
    assert_eq!(status, StatusCode::OK);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e["host"], "evil.example.com");
    assert_eq!(e["port"], 443);
    assert_eq!(e["count"], 1);
    // RFC 3339 strings present alongside raw Unix seconds.
    let first = e["first_seen"].as_str().unwrap();
    assert!(first.ends_with('Z'), "first_seen should be RFC 3339 UTC");
    assert!(e["last_seen"].as_str().unwrap().ends_with('Z'));
    assert!(e["first_seen_unix"].as_u64().is_some());
    assert!(e["last_seen_unix"].as_u64().is_some());
}

#[tokio::test]
async fn internal_blocked_since_relative_filter() {
    let state = test_state();
    state
        .blocked_egress
        .record("sb-test", "old.example.com", 443);
    state
        .blocked_egress
        .record("sb-test", "new.example.com", 443);

    let app = internal_app(state);
    // -1h relative: both records are <1h old → both returned.
    let (status, body) = get_json(&app, "/internal/egress/blocked?since=-1h").await;
    assert_eq!(status, StatusCode::OK);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    // since_unix is echoed and non-zero (now - 1h).
    assert!(body["since_unix"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn internal_blocked_top_aggregates_and_truncates() {
    let state = test_state();
    state.blocked_egress.record("sb1", "a.example.com", 443);
    state.blocked_egress.record("sb2", "a.example.com", 443);
    state.blocked_egress.record("sb1", "b.example.com", 443);

    let app = internal_app(state);
    let (status, body) = get_json(&app, "/internal/egress/blocked/top?window=1h&n=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["window"], "1h");
    let top = body["top"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    // a.example.com has count 2 across two sandboxes → first.
    assert_eq!(top[0]["host"], "a.example.com");
    assert_eq!(top[0]["count"], 2);
    assert_eq!(top[1]["host"], "b.example.com");
    assert_eq!(top[1]["count"], 1);
}

#[tokio::test]
async fn internal_blocked_top_defaults_n_and_window() {
    let state = test_state();
    state.blocked_egress.record("sb1", "h.example.com", 443);
    let app = internal_app(state);
    let (status, body) = get_json(&app, "/internal/egress/blocked/top").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["n"], 10);
    assert_eq!(body["window"], "5m");
    assert_eq!(body["top"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn internal_blocked_top_caps_n_at_100() {
    let state = test_state();
    let app = internal_app(state);
    let (status, body) = get_json(&app, "/internal/egress/blocked/top?n=9999").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["n"], 100);
}
