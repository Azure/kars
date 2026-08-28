// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Route-level tests for the NATIVE buffered Anthropic passthrough
//! guardrail contract (Pal's round-4 HIGH). These drive the real
//! `/v1/messages` handler end to end, not `deny_policy` directly:
//! an `InferencePolicy` selecting `provider: anthropic` with an
//! `openai-moderation` output stage, a wiremock Anthropic upstream, and
//! a wiremock moderation backend.
//!
//! The identical request must return the same stable `error.code` and
//! `x-kars-decision*` headers whether it is buffered or streamed. Here
//! we pin the buffered (`stream: false`) native path: a flagged output
//! gives 403 `guardrail_blocked` and a moderation outage gives 502
//! `guardrail_unavailable`, both with the decision headers attached.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
use kars_inference_router::routes::{AppState, inference_routes};

fn test_state(anthropic_endpoint: String, moderation_endpoint: String) -> AppState {
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
            foundry_endpoint: None,
            foundry_project_endpoint: None,
            azure_openai_endpoint: None,
            default_model: "claude-x".into(),
            content_safety_enabled: false,
            prompt_shields_enabled: false,
            content_safety_endpoint: None,
            token_budget_daily: 1_000_000_000,
            token_budget_per_request: 1_000_000_000,
            registry_mode: RegistryMode::Local,
            registry_url: None,
            provider_override: None,
            anthropic_endpoint,
            anthropic_api_key: Some("sk-ant-router-held".into()),
            ollama_endpoint: None,
            openai_moderation_endpoint: moderation_endpoint,
            openai_moderation_api_key: Some("sk-mod-test".into()),
            openai_moderation_model: "omni-moderation-latest".into(),
        }),
        budget: TokenBudgetTracker::new(1_000_000_000, 1_000_000_000),
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

async fn install_anthropic_output_guardrail(state: &AppState) {
    let policy = LoadedInferencePolicy {
        digest: "sha256:test".into(),
        source_path: "/tmp/test-policy".into(),
        per_request_tokens: None,
        daily_tokens: None,
        monthly_tokens: None,
        content_safety: Default::default(),
        model_preference: None,
        provider: Some("anthropic".into()),
        guardrails: vec![GuardrailStageCfg {
            provider: "openai-moderation".into(),
            apply_to: ApplyTo::Output,
        }],
        raw: serde_json::json!({}),
    };
    *state.inference_policy.write().await = Some(policy);
}

/// A buffered (`stream: false`) native Anthropic reply whose text flags
/// the moderation backend must be blocked with the stable coded error
/// and the decision headers, matching the streaming/translated paths.
#[tokio::test]
async fn native_buffered_violation_is_coded_and_carries_decision_headers() {
    let anthropic = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "here is RANSOM material" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 4, "output_tokens": 5 }
        })))
        .mount(&anthropic)
        .await;

    let moderation = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/moderations"))
        .respond_with(move |req: &wiremock::Request| {
            let flagged = String::from_utf8_lossy(&req.body).contains("RANSOM");
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{ "flagged": flagged, "categories": { "illicit": flagged } }]
            }))
        })
        .mount(&moderation)
        .await;

    let state = test_state(anthropic.uri(), moderation.uri());
    install_anthropic_output_guardrail(&state).await;
    let app = Router::new().merge(inference_routes()).with_state(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(
            r#"{"model":"claude-x","max_tokens":32,"stream":false,"messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    let status = resp.status();
    let decision = resp
        .headers()
        .get("x-kars-decision")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

    assert_eq!(status, StatusCode::FORBIDDEN, "buffered violation: {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("guardrail_blocked"),
        "stable machine code required: {body}"
    );
    assert_eq!(
        body["error"]["type"].as_str(),
        Some("content_policy_violation"),
        "Anthropic-native type preserved: {body}"
    );
    assert_eq!(
        decision.as_deref(),
        Some("blocked"),
        "x-kars-decision header must be attached"
    );
}

/// A moderation-backend outage on the buffered native path must fail
/// closed with the specific `guardrail_unavailable` code (not a generic
/// `api_error`) and the decision headers.
#[tokio::test]
async fn native_buffered_moderation_outage_is_coded_unavailable() {
    let anthropic = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "anything at all" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 4, "output_tokens": 3 }
        })))
        .mount(&anthropic)
        .await;

    // Moderation is down → the pipeline must fail closed.
    let moderation = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/moderations"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&moderation)
        .await;

    let state = test_state(anthropic.uri(), moderation.uri());
    install_anthropic_output_guardrail(&state).await;
    let app = Router::new().merge(inference_routes()).with_state(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(
            r#"{"model":"claude-x","max_tokens":32,"stream":false,"messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    let status = resp.status();
    let decision = resp
        .headers()
        .get("x-kars-decision")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(resp.into_body(), 1_048_576)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "outage fails closed: {body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("guardrail_unavailable"),
        "misconfig vs outage must not collapse: {body}"
    );
    assert_eq!(
        decision.as_deref(),
        Some("blocked"),
        "x-kars-decision header must be attached"
    );
}
