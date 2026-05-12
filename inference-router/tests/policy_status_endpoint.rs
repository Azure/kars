// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Integration test for `GET /internal/policy-status` — exercises the
//! full axum stack (router, state extraction, JSON serialization) so
//! the wire contract consumed by the controller poller, `azureclaw
//! inspect`, and the headlamp panel is locked down.
//!
//! Unit tests for the registry and the response DTO live in
//! `inference-router/src/policy_status.rs` and
//! `inference-router/src/routes/internal.rs`. This file additionally
//! verifies:
//!   1. The route is reachable through the same `Router::merge` wiring
//!      `main.rs` uses (no helper that bypasses real routing).
//!   2. Loading AGT policies into a real `Governance` populates the
//!      registry as a side effect (end-to-end producer→consumer wire).
//!   3. The empty-registry state serializes to a well-formed envelope.

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
use azureclaw_inference_router::policy_status::PolicyStatusRegistry;
use azureclaw_inference_router::providers::{AuditSink, PolicyDecisionProvider, SigningProvider};
use azureclaw_inference_router::routes::{AppState, internal_routes};

fn test_state() -> (AppState, Arc<PolicyStatusRegistry>) {
    let policy_status = Arc::new(PolicyStatusRegistry::new());
    let governance = Arc::new(Governance::new_with_status(
        "sb-test",
        policy_status.clone(),
    ));
    let state = AppState {
        auth: Arc::new(WorkloadIdentityAuth::new()),
        copilot: Arc::new(azureclaw_inference_router::copilot_auth::CopilotTokenCache::from_env()),
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
        policy_status: policy_status.clone(),
    };
    (state, policy_status)
}

fn app(state: AppState) -> Router {
    Router::new().merge(internal_routes()).with_state(state)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::get(uri).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn empty_registry_returns_200_with_empty_entries() {
    let (state, _reg) = test_state();
    let app = app(state);
    let (status, body) = get(&app, "/internal/policy-status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["schema_version"].as_u64(), Some(1));
    assert_eq!(body["count"].as_u64(), Some(0));
    assert!(body["entries"].is_array());
    assert_eq!(body["entries"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn registry_with_agt_profile_entry_round_trips_through_route() {
    let (state, reg) = test_state();
    reg.record_success(
        azureclaw_inference_router::policy_status::PolicyKind::AgtProfile,
        "/etc/azureclaw/policies",
        b"hello",
    );
    let app = app(state);
    let (status, body) = get(&app, "/internal/policy-status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"].as_u64(), Some(1));
    let entry = &body["entries"][0];
    assert_eq!(entry["kind"].as_str(), Some("AgtProfile"));
    assert_eq!(
        entry["digest"].as_str(),
        Some("sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"),
    );
    assert_eq!(
        entry["source_path"].as_str(),
        Some("/etc/azureclaw/policies")
    );
    let loaded_at = entry["loaded_at"].as_str().expect("loaded_at present");
    assert!(
        loaded_at.ends_with("Z"),
        "expected RFC3339 'Z' suffix, got {loaded_at}"
    );
    assert!(loaded_at.contains("T"), "expected RFC3339 'T' separator");
    assert!(entry["last_error"].is_null());
}

#[tokio::test]
async fn governance_load_propagates_into_registry_via_route() {
    use std::io::Write;
    // Write a minimal valid AGT policy YAML to a tempdir, load it
    // through Governance, then assert the route reports the matching
    // digest. This is the producer→consumer end-to-end wire:
    // controller will use this exact path to confirm "router has
    // echoed my published artifact".
    let dir = tempfile::tempdir().unwrap();
    let policy_path = dir.path().join("test.yaml");
    let mut f = std::fs::File::create(&policy_path).unwrap();
    // PolicyEngine::load_from_file expects a `policies:` top-level
    // sequence — empty seq is valid (counts as 0 rules) so we don't
    // depend on internal rule schema details.
    f.write_all(b"version: \"1.0\"\nagent: test\npolicies:\n  - name: noop\n    type: capability\n    denied_actions: []\n    priority: 1\n").unwrap();
    drop(f);

    let (state, _reg) = test_state();
    let count = state
        .governance
        .load_policies_from_dir(dir.path().to_str().unwrap())
        .expect("load ok");
    assert!(count >= 1, "expected at least one rule loaded, got {count}");

    let app = app(state);
    let (status, body) = get(&app, "/internal/policy-status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"].as_u64(), Some(1));
    let entry = &body["entries"][0];
    assert_eq!(entry["kind"].as_str(), Some("AgtProfile"));
    let digest = entry["digest"]
        .as_str()
        .expect("digest present after successful load");
    assert!(
        digest.starts_with("sha256:") && digest.len() == 71,
        "expected sha256 hex digest, got {digest}",
    );
    assert_eq!(
        entry["source_path"].as_str(),
        Some(dir.path().to_str().unwrap()),
    );
    assert!(entry["last_error"].is_null());
}

#[tokio::test]
async fn governance_load_of_missing_dir_records_error() {
    let (state, _reg) = test_state();
    state
        .governance
        .load_policies_from_dir("/nonexistent/path/will/never/exist")
        .expect("load returns Ok even when dir missing");

    let app = app(state);
    let (status, body) = get(&app, "/internal/policy-status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"].as_u64(), Some(1));
    let entry = &body["entries"][0];
    assert_eq!(entry["kind"].as_str(), Some("AgtProfile"));
    assert!(entry["digest"].is_null(), "no prior load → null digest");
    let err = entry["last_error"]
        .as_str()
        .expect("missing-dir records an error");
    assert!(
        err.to_lowercase().contains("not found"),
        "expected 'not found' in error, got {err}",
    );
}
