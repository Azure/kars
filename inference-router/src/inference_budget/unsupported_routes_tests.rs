// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    config::{Config, ProviderEndpoint},
    inference_policy_loader::{LoadedInferencePolicy, ModelPreference, ModelRef},
    proxy::failure::{Acceptance, FailureCategory, ForwardFailure},
    routes::AppState,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode},
};
use tower::ServiceExt;

fn default_upstream(fixture: &Fixture, governed: bool) -> UpstreamConfig {
    let mut upstream = UpstreamConfig::azure(fixture.provider.uri(), "model".into(), "task".into());
    upstream.inference_budget = governed.then(|| fixture.client.clone());
    upstream
}

async fn assert_budget_denial(error: anyhow::Error, stage: &'static str) {
    let typed = error.downcast_ref::<Error>().expect("typed budget denial");
    assert_eq!(typed.stage, stage);
    assert!(!crate::proxy::failure::retryable_failure(&error));
    let response = crate::inference_budget::response::denial(&error).unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "inference_budget_unavailable");
}

async fn endpoint_router(fixture: &Fixture, governed: bool) -> Router {
    let mut config = Config::from_env().unwrap();
    config.azure_openai_endpoint = Some(fixture.provider.uri());
    config.default_model = "model".into();
    config.providers = std::collections::HashMap::from([(
        "budget-fixture".into(),
        ProviderEndpoint {
            tag: "budget-fixture".into(),
            endpoint: fixture.provider.uri(),
            api_key: None,
        },
    )]);
    let policy_status = Arc::new(crate::policy_status::PolicyStatusRegistry::new());
    let governance = Arc::new(crate::governance::Governance::new_with_status(
        "task",
        policy_status.clone(),
    ));
    let state = AppState {
        services: Default::default(),
        auth: Arc::new(WorkloadIdentityAuth::for_test(None, None)),
        copilot: Arc::new(crate::copilot_auth::CopilotTokenCache::with_test_exchange(
            "unused-default-seat",
            format!("{}/unexpected-token-exchange", fixture.provider.uri()),
        )),
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        config: Arc::new(config),
        budget: crate::budget::TokenBudgetTracker::new(0, 0),
        inference_budget: governed.then(|| fixture.client.clone()),
        policy_provider: governance.clone(),
        audit_sink: governance.clone(),
        signing_provider: governance.clone(),
        governance,
        blocklist: crate::blocklist::Blocklist::disabled(),
        blocked_egress: Arc::new(crate::egress_blocked::BlockedBuffer::with_defaults()),
        sandbox_name: Arc::new("task".into()),
        inbox: Arc::new(crate::mesh::MeshInbox::new()),
        mesh_metrics: Arc::new(crate::mesh::MeshMetrics::new()),
        model_override: Default::default(),
        responses_only_models: Default::default(),
        unavailable_models: Default::default(),
        admin_token: None,
        handoff_tokens: crate::handoff::HandoffTokenStore::new(),
        handoff_session: crate::handoff::HandoffSession::new(),
        drain_state: crate::handoff::DrainState::new(),
        pending_handoff: crate::handoff::PendingHandoffStore::new(),
        policy_status,
        inference_policy: crate::inference_policy_loader::empty_handle(),
        memory_binding: crate::memory_binding_loader::empty_handle(),
        egress_allowlist: crate::egress_allowlist_loader::empty_handle(),
        deployment_health: Arc::new(crate::deployment_health::DeploymentHealthRegistry::new()),
    };
    // Match the native fixture: a named primary, not an explicit legacy provider.
    // Embeddings still starts from the default Azure upstream, without a key.
    *state.inference_policy.write().await = Some(LoadedInferencePolicy {
        digest: "budget-fixture".into(),
        source_path: "budget-fixture".into(),
        per_request_tokens: None,
        daily_tokens: None,
        monthly_tokens: None,
        content_safety: Default::default(),
        model_preference: Some(ModelPreference {
            primary: ModelRef {
                provider: "budget-fixture".into(),
                deployment: "model".into(),
            },
            fallback: vec![],
        }),
        provider: None,
        guardrails: vec![],
        raw: json!({}),
    });
    Router::new()
        .merge(crate::routes::inference_routes())
        .with_state(state)
}

#[tokio::test]
async fn native_embeddings_returns_budget_503_before_unrelated_default_authentication() {
    for governed in [false, true] {
        let fixture = Fixture::new(100, false).await;
        let original = fixture.ledger.lock().unwrap().clone();
        let app = endpoint_router(&fixture, governed).await;
        for path in ["/v1/embeddings", "/v1/completions"] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json")
                        .header("x-kars-sandbox", "task")
                        .body(Body::from(r#"{"input":"fixture"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            if governed {
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                let body = to_bytes(response.into_body(), 4096).await.unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["error"]["code"], "inference_budget_unavailable");
                assert_eq!(body["error"]["type"], "inference_budget_unavailable");
            } else {
                assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            }
        }
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                ._broker
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(*fixture.ledger.lock().unwrap(), original);
    }
}

#[tokio::test]
async fn unsupported_finite_buffered_and_streaming_operations_never_reach_auth_or_broker() {
    let fixture = Fixture::new(100, false).await;
    let original = fixture.ledger.lock().unwrap().clone();
    for path in [
        "embeddings",
        "/v1/embeddings",
        "completions",
        "images/generations",
        "responses?background=true",
    ] {
        let upstream = default_upstream(&fixture, true);
        let auth = Arc::new(WorkloadIdentityAuth::for_test(None, None));
        let error = crate::proxy::forward(
            &auth,
            None,
            &reqwest::Client::new(),
            &upstream,
            Method::POST,
            path,
            &HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"input":"fixture"}"#),
        )
        .await
        .err()
        .unwrap();
        assert_budget_denial(error, "unsupported inference operation").await;
        let error = crate::proxy::forward_stream(
            auth,
            None,
            reqwest::Client::new(),
            upstream,
            path,
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"input":"fixture","stream":true}"#),
        )
        .await
        .err()
        .unwrap();
        assert_budget_denial(error, "unsupported inference operation").await;
    }
    let error = crate::proxy::forward(
        &WorkloadIdentityAuth::for_test(None, None),
        None,
        &reqwest::Client::new(),
        &default_upstream(&fixture, true),
        Method::GET,
        "chat/completions",
        &HeaderMap::new(),
        bytes::Bytes::new(),
    )
    .await
    .err()
    .unwrap();
    assert_budget_denial(error, "unsupported inference operation").await;
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            ._broker
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(*fixture.ledger.lock().unwrap(), original);
}

#[tokio::test]
async fn supported_finite_operation_still_resolves_credentials_before_acquiring_any_grant() {
    let fixture = Fixture::new(100, false).await;
    let original = fixture.ledger.lock().unwrap().clone();
    let error = crate::proxy::forward(
        &WorkloadIdentityAuth::for_test(None, None),
        None,
        &reqwest::Client::new(),
        &default_upstream(&fixture, true),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"text"}]}"#),
    )
    .await
    .err()
    .unwrap();
    let failure = error.downcast_ref::<ForwardFailure>().unwrap();
    assert_eq!(failure.category, FailureCategory::Authentication);
    assert_eq!(failure.acceptance, Acceptance::NotAccepted);
    assert!(crate::inference_budget::response::denial(&error).is_none());
    assert!(!crate::proxy::failure::retryable_failure(&error));
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            ._broker
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(*fixture.ledger.lock().unwrap(), original);
}

#[tokio::test]
async fn supported_broker_loss_blocks_new_sends_without_refunding_accepted_unknown_work() {
    let fixture = Fixture::new(100, false).await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[]})))
        .mount(&fixture.provider)
        .await;
    fixture.buffered().await.unwrap();
    let funded = fixture.ledger.lock().unwrap().clone();
    assert_eq!(funded.meters.uncertain.tokens, 30);
    assert_eq!(funded.meters.uncertain.usd_micros, 5);
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    assert_budget_denial(fixture.buffered().await.err().unwrap(), "/v1/catalog").await;
    let error = crate::proxy::forward_stream(
        Arc::new(WorkloadIdentityAuth::for_test(None, None)),
        None,
        reqwest::Client::new(),
        fixture.upstream(),
        "chat/completions",
        HeaderMap::new(),
        bytes::Bytes::from_static(
            br#"{"messages":[{"role":"user","content":"text"}],"stream":true}"#,
        ),
    )
    .await
    .err()
    .unwrap();
    assert_budget_denial(error, "/v1/catalog").await;
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
    assert_eq!(*fixture.ledger.lock().unwrap(), funded);
}
