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

    for uri in ["/openai/files", "/memory_stores", "/evaluations"] {
        let (status, body) = send(&app, "POST", uri).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "POST {uri}: {body}");
        assert_eq!(
            body["error"]["type"].as_str(),
            Some("unsupported_for_provider"),
            "expected the proxy's own github-models 501 (proof the guard let it through): {body}"
        );
    }
}

/// Pins the premise of the dot-segment guard: URL parsing (as done by
/// reqwest when the raw path is concatenated into the upstream URL)
/// resolves `.`/`..` segments including percent-encoded `%2e` forms.
/// If this ever stops holding, the traversal rejection below is
/// defense-in-depth rather than load-bearing, but it must hold today.
#[test]
fn url_parsing_normalizes_dot_segments() {
    for (raw, normalized) in [
        ("/openai/files/../responses", "/openai/responses"),
        ("/openai/files/%2e%2e/responses", "/openai/responses"),
        ("/memory_stores/../openai/responses", "/openai/responses"),
        ("/evaluations/../openai/responses", "/openai/responses"),
    ] {
        let url = reqwest::Url::parse(&format!("https://upstream.example{raw}")).unwrap();
        assert_eq!(url.path(), normalized, "raw: {raw}");
    }
}

/// A dot-segment / encoded-dot / encoded-slash / malformed-escape path
/// routed through an UNGUARDED wildcard must not pivot into a guarded
/// inference path after normalization. These are rejected outright
/// (400) before classification or forwarding, through real router
/// wiring. (Benign empty segments are collapsed, not rejected, so the
/// `//../` case here is rejected on its `..`, not the `//`.)
#[tokio::test]
async fn traversal_paths_are_rejected_not_forwarded() {
    let state = test_state();
    install_policy(&state, None, true).await;
    let app = app(state);

    for uri in [
        // Literal dot segments through unguarded wildcards.
        "/openai/files/../responses",
        "/openai/vector_stores/../responses",
        "/openai/evals/../responses",
        "/memory_stores/../openai/responses",
        "/evaluations/../openai/responses",
        "/connections/../openai/responses",
        "/openai/files/../conversations",
        // Percent-encoded dot segments (lower + upper case).
        "/openai/files/%2e%2e/responses",
        "/openai/files/%2E%2E/responses",
        "/memory_stores/%2e%2e/openai/responses",
        // Single-dot segment.
        "/openai/files/./responses",
        // Encoded slash smuggled inside one segment.
        "/openai/files/..%2fresponses",
        // Empty segment (double slash) through a wildcard.
        "/openai/files//../responses",
        // Malformed percent escape.
        "/openai/files/%zz/responses",
    ] {
        let (status, body) = send(&app, "POST", uri).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "POST {uri} must be rejected before forwarding, got {status}: {body}"
        );
        assert_eq!(
            body["error"]["type"].as_str(),
            Some("invalid_path"),
            "POST {uri}: {body}"
        );
    }
}

/// Double-slash and trailing-slash variants of guarded families must
/// never reach the proxy body, they either 404 at routing or are
/// rejected/blocked at the top of `foundry_proxy`. The one outcome
/// that would be a bug is the github-models 501 marker (proof of
/// reaching the proxy body) or any 2xx.
#[tokio::test]
async fn slash_variants_of_guarded_families_never_reach_proxy_body() {
    let state = test_state();
    install_policy(&state, None, true).await;
    let app = app(state);

    for uri in [
        "/openai/responses/",
        "/openai//responses",
        "/agents/",
        "/agents//runs",
        "/openai/conversations/",
    ] {
        let (status, body) = send(&app, "POST", uri).await;
        assert!(
            matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ),
            "POST {uri} must not reach the proxy body, got {status}: {body}"
        );
        assert_ne!(
            body["error"]["type"].as_str(),
            Some("unsupported_for_provider"),
            "POST {uri} reached the proxy body (github-models 501 marker): {body}"
        );
    }
}

/// Compatibility: trailing/double-slash management paths that live
/// customers have always used must NOT be rejected by the path guard,
/// even under an active guardrail policy (they are exempt) and even
/// with no policy at all. They collapse to canonical form and reach the
/// proxy body (here the network-free github-models 501). Paths carry a
/// real segment before the slash so they match the `{*path}` wildcard
/// route (a bare `/openai/files/` 404s at axum routing, which is
/// pre-existing and orthogonal to the guard).
#[tokio::test]
async fn benign_trailing_slash_paths_are_not_rejected() {
    for guardrails in [false, true] {
        let state = test_state();
        install_policy(&state, None, guardrails).await;
        let app = app(state);
        for uri in [
            "/openai/files/f1/",  // trailing slash
            "/memory_stores/s1/", // trailing slash
            "/openai/files//f1",  // double slash
            "/evaluations/e1/",   // trailing slash
        ] {
            let (status, body) = send(&app, "POST", uri).await;
            assert_ne!(
                body["error"]["type"].as_str(),
                Some("invalid_path"),
                "POST {uri} (guardrails={guardrails}) must not be rejected by the path guard: {status} {body}"
            );
            assert_eq!(
                body["error"]["type"].as_str(),
                Some("unsupported_for_provider"),
                "POST {uri} (guardrails={guardrails}) should reach the proxy body: {status} {body}"
            );
        }
    }
}

/// Default-deny: with the guard inverted to an exempt list, an
/// unknown-but-routed path family fails closed rather than slipping
/// through. `/agents*` wildcards cover arbitrary suffixes, so use a
/// suffix no explicit rule names.
#[tokio::test]
async fn unknown_wildcard_suffixes_fail_closed() {
    let state = test_state();
    install_policy(&state, None, true).await;
    let app = app(state);

    let (status, body) = send(&app, "POST", "/agents/a1/some/new/api").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        body["error"]["type"].as_str(),
        Some("guardrail_route_unsupported"),
        "{body}"
    );
}
