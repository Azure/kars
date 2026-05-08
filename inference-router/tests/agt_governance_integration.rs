// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Integration tests for the AGT governance HTTP endpoints.
//!
//! These tests spin up the axum Router with a real AppState and make actual
//! HTTP requests through the tower Service layer to verify JSON contracts,
//! auth enforcement, and end-to-end handler behaviour.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt; // for oneshot()

use azureclaw_inference_router::auth::WorkloadIdentityAuth;
use azureclaw_inference_router::blocklist::Blocklist;
use azureclaw_inference_router::budget::TokenBudgetTracker;
use azureclaw_inference_router::config::{Config, RegistryMode};
use azureclaw_inference_router::governance::Governance;
use azureclaw_inference_router::handoff::{
    DrainState, HandoffSession, HandoffTokenStore, PendingHandoffStore,
};
use azureclaw_inference_router::mesh::{MeshInbox, MeshMetrics};
use azureclaw_inference_router::providers::{AuditSink, PolicyDecisionProvider, SigningProvider};
use azureclaw_inference_router::routes::{AppState, mesh_routes, sensitive_agt_routes};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal AppState suitable for testing (no env vars, no network).
fn test_state(sandbox: &str, admin_token: Option<&str>) -> AppState {
    let governance = Arc::new(Governance::new(sandbox));
    AppState {
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
        blocked_egress: Arc::new(
            azureclaw_inference_router::egress_blocked::BlockedBuffer::with_defaults(),
        ),
        sandbox_name: Arc::new(sandbox.to_string()),
        inbox: Arc::new(MeshInbox::new()),
        mesh_metrics: Arc::new(MeshMetrics::new()),
        model_override: Arc::new(std::sync::RwLock::new(None)),
        admin_token: admin_token.map(|t| Arc::new(t.to_string())),
        responses_only_models: Arc::new(std::sync::RwLock::new(Default::default())),
        handoff_tokens: HandoffTokenStore::new(),
        handoff_session: HandoffSession::new(),
        drain_state: DrainState::new(),
        pending_handoff: PendingHandoffStore::new(),
    }
}

/// Build the AGT sub-router with state.
fn test_app(state: AppState) -> Router {
    Router::new()
        .merge(sensitive_agt_routes())
        .merge(mesh_routes())
        .with_state(state)
}

/// Helper: send a GET request and return (status, json body).
async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::get(uri).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap_or(json!(null));
    (status, json)
}

/// Helper: send a POST request with JSON body.
async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (status, json)
}

/// Helper: send a POST with custom headers.
async fn post_with_headers(
    app: &Router,
    uri: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::post(uri).header("content-type", "application/json");
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (status, json)
}

/// Helper: send a DELETE with custom headers.
async fn delete_with_headers(
    app: &Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::delete(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder.body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (status, json)
}

// ===========================================================================
// POST /agt/evaluate
// ===========================================================================

#[tokio::test]
async fn evaluate_allows_without_policy() {
    let state = test_state("test-sandbox", None);
    let app = test_app(state);

    let (status, body) = post(
        &app,
        "/agt/evaluate",
        json!({"action": "shell:ls -la /tmp"}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allowed"], true);
    assert_eq!(body["decision"], "allow");
    assert_eq!(body["rate_limited"], false);
}

#[tokio::test]
async fn evaluate_uses_sandbox_name_as_default_agent_id() {
    let state = test_state("my-sandbox", None);
    let app = test_app(state);

    let (status, body) = post(
        &app,
        "/agt/evaluate",
        json!({"action": "inference:chat_completions"}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allowed"], true);
}

#[tokio::test]
async fn evaluate_with_explicit_agent_id() {
    let state = test_state("test-sandbox", None);
    let app = test_app(state);

    let (status, body) = post(
        &app,
        "/agt/evaluate",
        json!({
            "agent_id": "peer-agent-1",
            "action": "http_fetch:https://api.github.com",
            "context": {"url": "https://api.github.com"}
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allowed"], true);
}

#[tokio::test]
async fn evaluate_creates_audit_entry() {
    let state = test_state("audit-sandbox", None);
    let app = test_app(state);

    // Evaluate an action
    post(
        &app,
        "/agt/evaluate",
        json!({"action": "shell:whoami", "agent_id": "test-agent"}),
    )
    .await;

    // Check audit log now has an entry
    let (status, body) = get(&app, "/agt/audit").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["count"].as_u64().unwrap() >= 1);

    let entries = body["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert_eq!(entries[0]["action"], "shell:whoami");
    assert_eq!(entries[0]["agent_id"], "test-agent");
}

// ===========================================================================
// GET /agt/trust (list)
// ===========================================================================

#[tokio::test]
async fn trust_list_empty_by_default() {
    let state = test_state("trust-empty-test", None);
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/trust").await;

    assert_eq!(status, StatusCode::OK);
    // Trust list contains an array (may have entries from other tests sharing
    // the on-disk trust DB at /tmp/agt/trust_scores.json).
    assert!(body["agents"].as_array().is_some());
}

#[tokio::test]
async fn trust_list_after_update() {
    let state = test_state("trust-update-test", None);
    let governance = state.governance.clone();
    let app = test_app(state);

    // Seed a trust entry directly
    governance.update_trust("peer-1", 600, 3).unwrap();

    let (status, body) = get(&app, "/agt/trust").await;
    assert_eq!(status, StatusCode::OK);

    let agents = body["agents"].as_array().unwrap();
    // Should contain at least the entry we just added (may have more from
    // other tests sharing /tmp/agt/trust_scores.json).
    assert!(agents.iter().any(|a| a["agent_id"] == "peer-1"));
    let peer = agents.iter().find(|a| a["agent_id"] == "peer-1").unwrap();
    assert!(peer["score"].as_u64().unwrap() > 0);
}

// ===========================================================================
// GET /agt/trust/{agent_id}
// ===========================================================================

#[tokio::test]
async fn trust_get_unknown_agent_returns_default() {
    let state = test_state("test-sandbox", None);
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/trust/nonexistent-agent").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["agent_id"], "nonexistent-agent");
    assert!(body["score"].is_number());
}

#[tokio::test]
async fn trust_get_known_agent() {
    let state = test_state("test-sandbox", None);
    state.governance.update_trust("known-peer", 750, 5).unwrap();
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/trust/known-peer").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["agent_id"], "known-peer");
    assert!(body["score"].as_u64().unwrap() > 0);
    assert!(body["tier"].is_string());
}

// ===========================================================================
// POST /agt/trust (update) — auth enforcement
// ===========================================================================

#[tokio::test]
async fn trust_update_no_auth_when_no_admin_token() {
    let state = test_state("test-sandbox", None);
    let app = test_app(state);

    let (status, body) = post(
        &app,
        "/agt/trust",
        json!({"agent_id": "peer-1", "score": 700, "interactions": 1}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["agent_id"], "peer-1");
}

#[tokio::test]
async fn trust_update_requires_admin_token() {
    let state = test_state("test-sandbox", Some("secret-token"));
    let app = test_app(state);

    // Without token → 403
    let (status, body) = post(
        &app,
        "/agt/trust",
        json!({"agent_id": "peer-1", "score": 700}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("Admin token"));
}

#[tokio::test]
async fn trust_update_with_valid_admin_header() {
    let state = test_state("test-sandbox", Some("secret-token"));
    let app = test_app(state);

    let (status, body) = post_with_headers(
        &app,
        "/agt/trust",
        json!({"agent_id": "peer-1", "score": 700, "interactions": 1}),
        &[("x-azureclaw-admin", "secret-token")],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn trust_update_with_bearer_token() {
    let state = test_state("test-sandbox", Some("secret-token"));
    let app = test_app(state);

    let (status, body) = post_with_headers(
        &app,
        "/agt/trust",
        json!({"agent_id": "peer-1", "score": 700}),
        &[("authorization", "Bearer secret-token")],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn trust_update_with_wrong_token() {
    let state = test_state("test-sandbox", Some("secret-token"));
    let app = test_app(state);

    let (status, _) = post_with_headers(
        &app,
        "/agt/trust",
        json!({"agent_id": "peer-1", "score": 700}),
        &[("x-azureclaw-admin", "wrong-token")],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn trust_update_rejects_self_trust() {
    let state = test_state("test-sandbox", None);
    let app = test_app(state);

    let (status, body) = post(
        &app,
        "/agt/trust",
        json!({"agent_id": "test-sandbox", "score": 999}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("own trust"));
}

#[tokio::test]
async fn trust_update_clamps_score() {
    let state = test_state("test-sandbox", None);
    let app = test_app(state);

    // Request score of 2000 — should be clamped
    let (status, body) = post(
        &app,
        "/agt/trust",
        json!({"agent_id": "peer-1", "score": 2000}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["score"].as_u64().unwrap() <= 1000);
}

// ===========================================================================
// DELETE /agt/trust/{agent_id} — auth enforcement
// ===========================================================================

#[tokio::test]
async fn trust_delete_requires_admin_token() {
    let state = test_state("test-sandbox", Some("admin-key"));
    state.governance.update_trust("target", 500, 1).unwrap();
    let app = test_app(state);

    // Without token → 403
    let (status, _) = delete_with_headers(&app, "/agt/trust/target", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn trust_delete_with_valid_token() {
    let state = test_state("test-sandbox", Some("admin-key"));
    state.governance.update_trust("target", 500, 1).unwrap();
    let app = test_app(state);

    let (status, body) = delete_with_headers(
        &app,
        "/agt/trust/target",
        &[("x-azureclaw-admin", "admin-key")],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["deleted"], true);
}

// ===========================================================================
// GET /agt/audit
// ===========================================================================

#[tokio::test]
async fn audit_empty_on_fresh_state() {
    let state = test_state("audit-test", None);
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/audit").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 0);
    assert!(body["entries"].as_array().unwrap().is_empty());
    assert_eq!(body["sandbox"], "audit-test");
}

// ===========================================================================
// GET /agt/audit/verify
// ===========================================================================

#[tokio::test]
async fn audit_verify_valid_on_empty_log() {
    let state = test_state("verify-test", None);
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/audit/verify").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["integrity"], "valid");
    assert_eq!(body["sandbox"], "verify-test");
}

#[tokio::test]
async fn audit_verify_valid_after_evaluations() {
    let state = test_state("verify-test", None);
    let app = test_app(state);

    // Run several evaluations to build an audit chain
    for i in 0..5 {
        post(
            &app,
            "/agt/evaluate",
            json!({"action": format!("shell:cmd-{}", i), "agent_id": "agent-x"}),
        )
        .await;
    }

    let (status, body) = get(&app, "/agt/audit/verify").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["integrity"], "valid");
    assert_eq!(body["entries"], 5);
}

// ===========================================================================
// GET /agt/status — full status endpoint
// ===========================================================================

#[tokio::test]
async fn status_returns_expected_fields() {
    let state = test_state("status-sandbox", None);
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/status").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["governance_mode"], "native");
    assert_eq!(body["sandbox"], "status-sandbox");

    // Governance fields
    assert!(body["policy_rules"].is_number());
    assert!(body["audit_entries"].is_number());
    assert!(body["known_agents"].is_number());
    assert!(body.get("audit_integrity").is_some());

    // Router-enriched fields
    assert!(body["inbox_messages"].is_number());
    assert!(body["blocklist_domains"].is_number());
    assert!(body["mesh_sessions"].is_number());
    assert!(body["mesh_messages_sent"].is_number());
    assert!(body["mesh_messages_received"].is_number());
}

#[tokio::test]
async fn status_reflects_evaluations() {
    let state = test_state("counter-sandbox", None);
    let app = test_app(state);

    // Run a few evaluations
    for _ in 0..3 {
        post(
            &app,
            "/agt/evaluate",
            json!({"action": "shell:test", "agent_id": "peer"}),
        )
        .await;
    }

    let (_, body) = get(&app, "/agt/status").await;
    assert!(body["policy_evaluations"].as_u64().unwrap() >= 3);
    assert!(body["audit_entries"].as_u64().unwrap() >= 3);
}

// ===========================================================================
// GET /agt/mesh/inbox
// ===========================================================================

#[tokio::test]
async fn mesh_inbox_empty_by_default() {
    let state = test_state("test-sandbox", None);
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/mesh/inbox").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 0);
    assert!(body["messages"].as_array().unwrap().is_empty());
}

// ===========================================================================
// GET /agt/reputation (without registry — graceful degradation)
// ===========================================================================

#[tokio::test]
async fn reputation_without_registry_returns_local_trust() {
    let state = test_state("rep-sandbox", None);
    state.governance.update_trust("peer-1", 600, 2).unwrap();
    let app = test_app(state);

    let (status, body) = get(&app, "/agt/reputation").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sandbox"], "rep-sandbox");
    // Registry field should be null (no registry running)
    assert!(body["registry"].is_null());
    // Local trust should include peer-1
    let local = body["local_trust"].as_array().unwrap();
    assert!(!local.is_empty());
}

// ===========================================================================
// End-to-end flows — multi-step interaction sequences
// ===========================================================================

#[tokio::test]
async fn full_lifecycle_evaluate_trust_audit_verify() {
    let state = test_state("lifecycle-sandbox", None);
    let app = test_app(state);

    // 1. Evaluate a policy action
    let (s, body) = post(
        &app,
        "/agt/evaluate",
        json!({"action": "shell:ls", "agent_id": "agent-a"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["allowed"], true);

    // 2. Update trust for the agent
    let (s, body) = post(
        &app,
        "/agt/trust",
        json!({"agent_id": "agent-a", "score": 700, "interactions": 1}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["ok"], true);

    // 3. Verify trust was recorded
    let (s, body) = get(&app, "/agt/trust/agent-a").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["agent_id"], "agent-a");
    assert!(body["score"].as_u64().unwrap() > 0);

    // 4. Verify audit has the evaluation entry
    let (s, body) = get(&app, "/agt/audit").await;
    assert_eq!(s, StatusCode::OK);
    assert!(body["count"].as_u64().unwrap() >= 1);

    // 5. Verify hash chain integrity
    let (s, body) = get(&app, "/agt/audit/verify").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["integrity"], "valid");

    // 6. Check overall status reflects everything
    let (s, body) = get(&app, "/agt/status").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["governance_mode"], "native");
    assert!(body["policy_evaluations"].as_u64().unwrap() >= 1);
    assert!(body["known_agents"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn trust_update_then_delete_cleans_up() {
    let state = test_state("cleanup-sandbox", Some("tk"));
    let app = test_app(state);

    // Create trust entry
    let (s, _) = post_with_headers(
        &app,
        "/agt/trust",
        json!({"agent_id": "temp-peer", "score": 600}),
        &[("x-azureclaw-admin", "tk")],
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Verify it exists
    let (_, body) = get(&app, "/agt/trust").await;
    assert!(
        body["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["agent_id"] == "temp-peer")
    );

    // Delete it
    let (s, body) =
        delete_with_headers(&app, "/agt/trust/temp-peer", &[("x-azureclaw-admin", "tk")]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["deleted"], true);
}
