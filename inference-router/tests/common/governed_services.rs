// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![allow(dead_code)]

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use kars_inference_router::{
    access_request::{Identity, scope::ResourceIdentity},
    auth::WorkloadIdentityAuth,
    blocklist::Blocklist,
    budget::TokenBudgetTracker,
    config::{Config, RegistryMode},
    egress_blocked::BlockedBuffer,
    governance::Governance,
    governed_services::GovernedServices,
    handoff::{DrainState, HandoffSession, HandoffTokenStore, PendingHandoffStore},
    mesh::{MeshInbox, MeshMetrics},
    policy_status::PolicyStatusRegistry,
    routes::{self, AppState},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

pub const CONTROL: &str = "operator-only-service-control-credential-0123456789abcdef";
pub const AGENT_ADMIN: &str = "legacy-agent-admin-credential-0123456789abcdef0123456789";

pub fn state(workspace: &str, uid: &str) -> AppState {
    let registry = Arc::new(PolicyStatusRegistry::new());
    let governance = Arc::new(Governance::new_with_status("test", registry.clone()));
    let services = Arc::new(GovernedServices::new(
        Identity {
            sandbox: ResourceIdentity {
                namespace: workspace.into(),
                name: "test".into(),
                uid: uid.into(),
            },
            namespace_uid: format!("namespace-{uid}"),
            task: Some(ResourceIdentity {
                namespace: workspace.into(),
                name: "task".into(),
                uid: format!("task-{uid}"),
            }),
            task_authorization: Some(format!("sha256:{}", "a".repeat(64))),
            task_generation: Some(1),
            managed: true,
        },
        Some(CONTROL.into()),
    ));
    let blocked = Arc::new(BlockedBuffer::with_defaults());
    blocked.bind_services(&services);
    AppState {
        services,
        auth: Arc::new(WorkloadIdentityAuth::new()),
        copilot: Arc::new(kars_inference_router::copilot_auth::CopilotTokenCache::from_env()),
        client: reqwest::Client::new(),
        config: Arc::new(Config {
            port: 0,
            foundry_endpoint: None,
            foundry_project_endpoint: None,
            azure_openai_endpoint: None,
            default_model: "model".into(),
            content_safety_enabled: false,
            prompt_shields_enabled: false,
            content_safety_endpoint: None,
            token_budget_daily: 1000,
            token_budget_per_request: 100,
            registry_mode: RegistryMode::Local,
            registry_url: None,
            provider_override: None,
            anthropic_endpoint: "https://api.anthropic.com".into(),
            anthropic_api_key: None,
            ollama_endpoint: None,
            openai_moderation_endpoint: "https://api.openai.com".into(),
            openai_moderation_api_key: None,
            openai_moderation_model: "omni-moderation-latest".into(),
            providers: Default::default(),
        }),
        budget: TokenBudgetTracker::new(1000, 100),
        policy_provider: governance.clone(),
        audit_sink: governance.clone(),
        signing_provider: governance.clone(),
        governance,
        blocklist: Blocklist::disabled(),
        blocked_egress: blocked,
        sandbox_name: Arc::new("test".into()),
        inbox: Arc::new(MeshInbox::new()),
        mesh_metrics: Arc::new(MeshMetrics::new()),
        model_override: Default::default(),
        admin_token: Some(Arc::new(AGENT_ADMIN.into())),
        responses_only_models: Default::default(),
        unavailable_models: Default::default(),
        handoff_tokens: HandoffTokenStore::new(),
        handoff_session: HandoffSession::new(),
        drain_state: DrainState::new(),
        pending_handoff: PendingHandoffStore::new(),
        policy_status: registry,
        inference_policy: kars_inference_router::inference_policy_loader::empty_handle(),
        memory_binding: kars_inference_router::memory_binding_loader::empty_handle(),
        egress_allowlist: kars_inference_router::egress_allowlist_loader::empty_handle(),
        deployment_health: Arc::new(
            kars_inference_router::deployment_health::DeploymentHealthRegistry::new(),
        ),
    }
}
pub fn app(state: AppState) -> Router {
    routes::governed_service_routes(state.clone())
        .merge(routes::egress_routes())
        .merge(routes::sensitive_agt_routes())
        .with_state(state)
}
pub fn request(
    method: &str,
    path: &str,
    body: Value,
    token: Option<&str>,
    scope: Option<&str>,
    local: bool,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(scope) = scope {
        request = request.header("x-kars-service-scope", scope);
    }
    let mut request = request.body(Body::from(body.to_string())).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo::<std::net::SocketAddr>(
            if local {
                "127.0.0.1:32100"
            } else {
                "192.0.2.10:32100"
            }
            .parse()
            .unwrap(),
        ));
    request
}
pub async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&body)})),
    )
}
pub async fn queue(
    app: &Router,
    scope: &str,
    kind: &str,
    target: &str,
    port: Option<u16>,
) -> Value {
    let (status, body) = send(
        app,
        request(
            "POST",
            "/v1/access-request",
            json!({"scope_id":scope,"kind":kind,"target":target,"reason":"needed", "port":port}),
            None,
            None,
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    body["request"].clone()
}
pub async fn decision(app: &Router, scope: &str, id: &str, verdict: &str) -> (StatusCode, Value) {
    send(
        app,
        request(
            "POST",
            "/internal/access-requests/decision",
            json!({"scope_id":scope,"request_id":id,"verdict":verdict}),
            Some(CONTROL),
            None,
            true,
        ),
    )
    .await
}
