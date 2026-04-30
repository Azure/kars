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

use azureclaw_inference_router::auth::WorkloadIdentityAuth;
use azureclaw_inference_router::blocklist::Blocklist;
use azureclaw_inference_router::budget::TokenBudgetTracker;
use azureclaw_inference_router::config::{Config, RegistryMode};
use azureclaw_inference_router::egress_blocked::BlockedBuffer;
use azureclaw_inference_router::governance::Governance;
use azureclaw_inference_router::handoff::{
    DrainState, HandoffSession, HandoffTokenStore, PendingHandoffStore,
};
use azureclaw_inference_router::mesh::{MeshInbox, MeshMetrics};
use azureclaw_inference_router::providers::{AuditSink, PolicyDecisionProvider, SigningProvider};
use azureclaw_inference_router::routes::{AppState, egress_routes};

fn test_state() -> AppState {
    let governance = Arc::new(Governance::new("sb-test"));
    AppState {
        auth: Arc::new(WorkloadIdentityAuth::new()),
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
