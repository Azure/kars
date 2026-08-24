// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fail-closed guard on inference-bearing Foundry proxy routes.
//!
//! An `InferencePolicy` that declares guardrails (or selects a
//! non-Azure provider) is enforced on `/v1/chat/completions` and the
//! Anthropic Messages routes — but `foundry_proxy` implements neither.
//! These tests prove the guard makes the bypass impossible: an agent
//! blocked by a guardrail on `/v1/chat/completions` must NOT be able
//! to rerun the same inference through `/openai/responses*`,
//! `/openai/conversations*`, or `/agents*` and receive an unscanned
//! response.
//!
//! The router is assembled with the same `Router::merge` wiring
//! `main.rs` uses, so the guard is exercised through real routing.
//! `foundry_endpoint` points at the GitHub Models marketplace so
//! un-guarded requests terminate in the deterministic, network-free
//! `is_github_models()` 501 — reaching that branch proves the request
//! got PAST the guard.

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
use kars_inference_router::guardrails::{ApplyTo, GuardrailStageCfg};
use kars_inference_router::handoff::{
    DrainState, HandoffSession, HandoffTokenStore, PendingHandoffStore,
};
use kars_inference_router::inference_policy_loader::LoadedInferencePolicy;
use kars_inference_router::mesh::{MeshInbox, MeshMetrics};
use kars_inference_router::policy_status::PolicyStatusRegistry;
use kars_inference_router::providers::{AuditSink, PolicyDecisionProvider, SigningProvider};
use kars_inference_router::routes::{
    AppState, foundry_agent_routes, foundry_standalone_routes, inference_routes,
};

fn test_state() -> AppState {
    let policy_status = Arc::new(PolicyStatusRegistry::new());
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
            // GitHub Models marketplace endpoint ⇒ foundry_proxy's own
            // is_github_models() 501 fires for any request the guard
            // lets through — deterministic, no network.
            foundry_endpoint: Some("https://models.github.ai/inference".into()),
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
            anthropic_endpoint: "https://api.anthropic.com".into(),
            anthropic_api_key: None,
            ollama_endpoint: None,
            openai_moderation_endpoint: "https://api.openai.com".into(),
            openai_moderation_api_key: Some("sk-mod-test".into()),
            openai_moderation_model: "omni-moderation-latest".into(),
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
        deployment_health: Arc::new(
            kars_inference_router::deployment_health::DeploymentHealthRegistry::new(),
        ),
    }
}

/// Same public wiring as `main.rs` — the guard must hold through real
/// routing, not a handler called directly.
fn app(state: AppState) -> Router {
    Router::new()
        .merge(inference_routes())
        .merge(foundry_agent_routes())
        .merge(foundry_standalone_routes())
        .with_state(state)
}

async fn install_policy(state: &AppState, provider: Option<&str>, guardrails: bool) {
    let policy = LoadedInferencePolicy {
        digest: "sha256:test".into(),
        source_path: "/tmp/test-policy".into(),
        per_request_tokens: None,
        daily_tokens: None,
        monthly_tokens: None,
        content_safety: Default::default(),
        model_preference: None,
        provider: provider.map(String::from),
        guardrails: if guardrails {
            vec![GuardrailStageCfg {
                provider: "openai-moderation".into(),
                apply_to: ApplyTo::Both,
            }]
        } else {
            vec![]
        },
        raw: serde_json::json!({}),
    };
    *state.inference_policy.write().await = Some(policy);
}

async fn send(app: &Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Every inference-bearing Foundry route must refuse a guardrail
/// policy — 403 with the same code the sibling `/v1/*` routes use.
#[tokio::test]
async fn guardrail_policy_blocks_inference_bearing_foundry_routes() {
    let state = test_state();
    install_policy(&state, None, true).await;
    let app = app(state);

    for (method, uri) in [
        ("POST", "/agents"),
        ("GET", "/agents/a1/runs/r1"),
        ("POST", "/openai/responses"),
        ("GET", "/openai/responses/resp_123"),
        ("POST", "/openai/conversations"),
        ("GET", "/openai/conversations/c1/items"),
    ] {
        let (status, body) = send(&app, method, uri).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} must fail closed under a guardrail policy, got {status}: {body}"
        );
        assert_eq!(
            body["error"]["type"].as_str(),
            Some("guardrail_route_unsupported"),
            "{method} {uri}: {body}"
        );
    }
}

/// A policy selecting a non-Azure provider must also fail closed —
/// these routes only speak to the Azure/Foundry upstream.
#[tokio::test]
async fn non_azure_provider_blocks_inference_bearing_foundry_routes() {
    let state = test_state();
    install_policy(&state, Some("anthropic"), false).await;
    let app = app(state);

    for uri in ["/agents", "/openai/responses", "/openai/conversations"] {
        let (status, body) = send(&app, "POST", uri).await;
        assert_eq!(
            status,
            StatusCode::NOT_IMPLEMENTED,
            "POST {uri} must refuse a non-Azure provider, got {status}: {body}"
        );
        assert_eq!(
            body["error"]["type"].as_str(),
            Some("provider_unimplemented"),
            "POST {uri}: {body}"
        );
    }
}

/// Back-compat: with no policy loaded (or a plain Azure policy), the
/// guard must not fire — the request reaches the proxy body, which in
/// this fixture terminates in the network-free GitHub Models 501.
#[tokio::test]
async fn plain_policy_leaves_foundry_routes_unguarded() {
    let state = test_state();
    install_policy(&state, None, false).await;
    let app = app(state);

    let (status, body) = send(&app, "POST", "/openai/responses").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert_eq!(
        body["error"]["type"].as_str(),
        Some("unsupported_for_provider"),
        "expected the proxy's own github-models 501 (proof the guard let it through): {body}"
    );
}

/// Non-inference Foundry surfaces (storage/management) stay unguarded
/// even under a guardrail policy — enforcement scope is documented in
/// docs/api/crd-reference.md.
#[tokio::test]
async fn guardrail_policy_leaves_non_inference_foundry_routes_unguarded() {
    let state = test_state();
    install_policy(&state, None, true).await;
    let app = app(state);

    let (status, body) = send(&app, "POST", "/openai/files").await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert_eq!(
        body["error"]["type"].as_str(),
        Some("unsupported_for_provider"),
        "expected the proxy's own github-models 501 (proof the guard let it through): {body}"
    );
}
