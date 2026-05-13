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
        inference_policy: azureclaw_inference_router::inference_policy_loader::empty_handle(),
        memory_binding: azureclaw_inference_router::memory_binding_loader::empty_handle(),
        egress_allowlist: azureclaw_inference_router::egress_allowlist_loader::empty_handle(),
        deployment_health: std::sync::Arc::new(
            azureclaw_inference_router::deployment_health::DeploymentHealthRegistry::new(),
        ),
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

#[tokio::test]
async fn deployment_health_surfaces_via_route() {
    // Slice 2d.2: the failover walker records per-deployment health
    // into the registry; `/internal/policy-status` echoes the snapshot
    // so the controller can see fallback activity without scraping
    // request logs.
    let (state, _reg) = test_state();
    state.deployment_health.record_failure("gpt-4o-primary");
    state.deployment_health.record_success("gpt-4o-fallback");

    let app = app(state);
    let (status, body) = get(&app, "/internal/policy-status").await;
    assert_eq!(status, StatusCode::OK);
    let health = body["deployment_health"]
        .as_array()
        .expect("deployment_health is always an array");
    assert_eq!(health.len(), 2, "two deployments observed");
    let names: Vec<&str> = health
        .iter()
        .map(|v| v["deployment"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"gpt-4o-primary"));
    assert!(names.contains(&"gpt-4o-fallback"));
    for entry in health {
        // Single failure below default threshold (3) keeps primary healthy.
        assert_eq!(entry["healthy"].as_bool(), Some(true));
    }
}

#[tokio::test]
async fn empty_deployment_health_serializes_as_empty_array() {
    let (state, _reg) = test_state();
    let app = app(state);
    let (_, body) = get(&app, "/internal/policy-status").await;
    let health = body["deployment_health"]
        .as_array()
        .expect("deployment_health is always an array, even when empty");
    assert!(health.is_empty());
}

#[tokio::test]
async fn memory_kind_digest_surfaces_via_route() {
    // Slice 3a: the ClawMemory binding loader registers its digest
    // under PolicyKind::Memory so the controller's
    // `claw_memory_reconciler` poller can confirm the §3 echo loop.
    // This test simulates the loader having run successfully on
    // startup and asserts the /internal/policy-status envelope
    // surfaces the new kind verbatim — same shape as AgtProfile /
    // InferencePolicy.
    let (state, reg) = test_state();
    let canonical = azureclaw_inference_router::memory_binding_loader::canonical_bytes_for_digest(
        azureclaw_inference_router::memory_binding_loader::MEMORY_BINDING_FILENAME,
        br#"{"storeName":"abc","scope":"agent:demo"}"#,
    );
    reg.record_success(
        azureclaw_inference_router::policy_status::PolicyKind::Memory,
        "/etc/azureclaw/memory/binding.json",
        &canonical,
    );

    let app = app(state);
    let (status, body) = get(&app, "/internal/policy-status").await;
    assert_eq!(status, StatusCode::OK);
    let entries = body["entries"]
        .as_array()
        .expect("entries is always an array");
    let memory = entries
        .iter()
        .find(|e| e["kind"].as_str() == Some("Memory"))
        .expect("expected a Memory entry");
    let digest = memory["digest"]
        .as_str()
        .expect("digest set after record_success");
    assert!(
        digest.starts_with("sha256:") && digest.len() == 71,
        "expected sha256 hex digest, got {digest}"
    );
    assert!(memory["last_error"].is_null());
    assert_eq!(
        memory["source_path"].as_str(),
        Some("/etc/azureclaw/memory/binding.json")
    );
}

#[tokio::test]
async fn memory_binding_loader_digest_matches_canonical_layout() {
    // Cross-validation: the bytes the router hashes for a Memory
    // binding must be byte-identical to the controller-side
    // `claw_memory_compile::canonical_bytes_for_digest`. Asserts the
    // wire contract end-to-end (controller writes ConfigMap bytes →
    // router loads + hashes → controller's poller compares digest).
    use sha2::{Digest, Sha256};
    let body = br#"{"storeName":"abc","scope":"agent:demo"}"#;
    let canonical = azureclaw_inference_router::memory_binding_loader::canonical_bytes_for_digest(
        azureclaw_inference_router::memory_binding_loader::MEMORY_BINDING_FILENAME,
        body,
    );
    let raw = Sha256::digest(&canonical);
    let mut hexstr = String::with_capacity(raw.len() * 2);
    for b in raw {
        use std::fmt::Write;
        let _ = write!(hexstr, "{b:02x}");
    }
    let expected = format!("sha256:{hexstr}");

    let (state, reg) = test_state();
    reg.record_success(
        azureclaw_inference_router::policy_status::PolicyKind::Memory,
        "/tmp/binding.json",
        &canonical,
    );
    let app = app(state);
    let (_, body_json) = get(&app, "/internal/policy-status").await;
    let memory = body_json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"].as_str() == Some("Memory"))
        .unwrap();
    assert_eq!(memory["digest"].as_str(), Some(expected.as_str()));
}
