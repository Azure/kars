// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Regression for the HIGH review finding: a buffered chat-completions
//! response whose upstream body is NOT valid JSON must still be scanned
//! by a declared output guardrail (via the raw-text fallback), not
//! returned to the client verbatim.
//!
//! Driven through the real router: an Ollama-provider policy (no
//! upstream auth needed) with an `openai-moderation` output guardrail.
//! The Ollama mock returns a non-JSON 200 body containing a flagged
//! marker; the moderation mock flags any input containing it. The
//! handler must block with 403 rather than pass the body through.

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

fn test_state(ollama_endpoint: String, moderation_endpoint: String) -> AppState {
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
            default_model: "llama3.1".into(),
            content_safety_enabled: false,
            prompt_shields_enabled: false,
            content_safety_endpoint: None,
            token_budget_daily: 1_000_000_000,
            token_budget_per_request: 1_000_000_000,
            registry_mode: RegistryMode::Local,
            registry_url: None,
            provider_override: None,
            anthropic_endpoint: "https://api.anthropic.com".into(),
            anthropic_api_key: None,
            ollama_endpoint: Some(ollama_endpoint),
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

async fn install_output_guardrail_ollama(state: &AppState) {
    let policy = LoadedInferencePolicy {
        digest: "sha256:test".into(),
        source_path: "/tmp/test-policy".into(),
        per_request_tokens: None,
        daily_tokens: None,
        monthly_tokens: None,
        content_safety: Default::default(),
        model_preference: None,
        provider: Some("ollama".into()),
        guardrails: vec![GuardrailStageCfg {
            provider: "openai-moderation".into(),
            apply_to: ApplyTo::Output,
        }],
        raw: serde_json::json!({}),
    };
    *state.inference_policy.write().await = Some(policy);
}

async fn post_chat(app: &Router, body: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4_194_304)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn non_json_upstream_body_is_still_scanned_and_blocked() {
    // Ollama upstream returns a NON-JSON 200 body carrying the flagged
    // marker, the exact shape that previously slipped past the
    // JSON-nested output scan.
    let ollama = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("upstream meltdown: RANSOM instructions here <not json>"),
        )
        .mount(&ollama)
        .await;

    // Moderation flags any input containing "RANSOM".
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

    let state = test_state(ollama.uri(), moderation.uri());
    install_output_guardrail_ollama(&state).await;
    let app = Router::new().merge(inference_routes()).with_state(state);

    let (status, body) = post_chat(
        &app,
        r#"{"model":"llama3.1","messages":[{"role":"user","content":"hi"}]}"#,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "non-JSON upstream body must be scanned and blocked, got {status}: {body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("guardrail_blocked"),
        "expected a guardrail violation block: {body}"
    );
    // The flagged upstream text must not have leaked to the client.
    assert!(
        !serde_json::to_string(&body)
            .unwrap()
            .contains("RANSOM instructions"),
        "flagged upstream text must not reach the client: {body}"
    );
}

#[tokio::test]
async fn non_json_clean_upstream_body_passes_through() {
    // Control: a clean non-JSON body (no flagged marker) is scanned and
    // released unchanged, the raw-text fallback does not over-block.
    let ollama = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("a perfectly benign non-json reply"),
        )
        .mount(&ollama)
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

    let state = test_state(ollama.uri(), moderation.uri());
    install_output_guardrail_ollama(&state).await;
    let app = Router::new().merge(inference_routes()).with_state(state);

    let (status, _body) = post_chat(
        &app,
        r#"{"model":"llama3.1","messages":[{"role":"user","content":"hi"}]}"#,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "clean non-JSON body must pass");
}
