//! Axum route handlers for the inference router.

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json,
};
use bytes::Bytes;
use std::sync::Arc;

use crate::auth::WorkloadIdentityAuth;
use crate::config::Config;
use crate::proxy::{self, UpstreamConfig};
use crate::safety;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<WorkloadIdentityAuth>,
    pub client: reqwest::Client,
    pub config: Arc<Config>,
}

impl AppState {
    pub async fn new(_config: &Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(32)
            .build()?;

        Ok(Self {
            auth: Arc::new(WorkloadIdentityAuth::new()),
            client,
            config: Arc::new(Config::from_env()?),
        })
    }

    fn upstream_config(&self, sandbox_name: &str) -> UpstreamConfig {
        UpstreamConfig {
            endpoint: self
                .config
                .azure_openai_endpoint
                .clone()
                .unwrap_or_default(),
            deployment: self.config.default_model.clone(),
            api_version: "2024-12-01-preview".to_string(),
            sandbox_name: sandbox_name.to_string(),
        }
    }
}

/// Inference API routes — proxied to Azure OpenAI / AI Foundry.
pub fn inference_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
}

/// Health and readiness routes.
pub fn health_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// Prometheus metrics endpoint.
pub fn metrics_routes() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics))
}

/// POST /v1/chat/completions — the primary inference endpoint.
async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Extract sandbox identity from header (set by Envoy sidecar)
    let sandbox_name = headers
        .get("x-azureclaw-sandbox")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // Content Safety check (on by default)
    if state.config.content_safety_enabled {
        if let Some(ref endpoint) = state.config.content_safety_endpoint {
            // Extract user message from request body for safety check
            if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&body) {
                if let Some(messages) = body_json.get("messages").and_then(|m| m.as_array()) {
                    if let Some(last_user_msg) = messages
                        .iter()
                        .rev()
                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                    {
                        if let Err(e) = safety::check_content_safety(endpoint, last_user_msg).await
                        {
                            tracing::warn!(sandbox = %sandbox_name, "Content safety blocked: {e}");
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({
                                    "error": {
                                        "message": "Request blocked by content safety policy",
                                        "type": "content_safety_violation",
                                        "code": "content_filtered"
                                    }
                                })),
                            )
                                .into_response();
                        }
                    }
                }
            }
        }
    }

    // Prompt Shields check
    if state.config.prompt_shields_enabled {
        if let Some(ref endpoint) = state.config.content_safety_endpoint {
            if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&body) {
                if let Some(messages) = body_json.get("messages").and_then(|m| m.as_array()) {
                    let full_prompt: String = messages
                        .iter()
                        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Err(e) = safety::check_prompt_shields(endpoint, &full_prompt).await {
                        tracing::warn!(sandbox = %sandbox_name, "Prompt shield blocked: {e}");
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({
                                "error": {
                                    "message": "Request blocked by prompt shield — possible injection detected",
                                    "type": "prompt_shield_violation",
                                    "code": "prompt_filtered"
                                }
                            })),
                        )
                            .into_response();
                    }
                }
            }
        }
    }

    // Forward to Azure OpenAI
    let upstream = state.upstream_config(sandbox_name);
    match proxy::forward_to_azure_openai(
        &state.auth,
        &state.client,
        &upstream,
        axum::http::Method::POST,
        "chat/completions",
        &headers,
        body,
    )
    .await
    {
        Ok((status, resp_headers, resp_body)) => {
            let mut response = (status, Body::from(resp_body)).into_response();
            // Forward content-type from upstream
            if let Some(ct) = resp_headers.get("content-type") {
                response.headers_mut().insert("content-type", ct.clone());
            }
            response
        }
        Err(e) => {
            tracing::error!(sandbox = %sandbox_name, "Proxy error: {e:#}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": {
                        "message": "Failed to reach inference backend",
                        "type": "proxy_error",
                        "code": "bad_gateway"
                    }
                })),
            )
                .into_response()
        }
    }
}

async fn completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let sandbox_name = headers
        .get("x-azureclaw-sandbox")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let upstream = state.upstream_config(sandbox_name);
    match proxy::forward_to_azure_openai(
        &state.auth,
        &state.client,
        &upstream,
        axum::http::Method::POST,
        "completions",
        &headers,
        body,
    )
    .await
    {
        Ok((status, _, resp_body)) => (status, Body::from(resp_body)).into_response(),
        Err(e) => {
            tracing::error!("Proxy error: {e:#}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let sandbox_name = headers
        .get("x-azureclaw-sandbox")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let upstream = state.upstream_config(sandbox_name);
    match proxy::forward_to_azure_openai(
        &state.auth,
        &state.client,
        &upstream,
        axum::http::Method::POST,
        "embeddings",
        &headers,
        body,
    )
    .await
    {
        Ok((status, _, resp_body)) => (status, Body::from(resp_body)).into_response(),
        Err(e) => {
            tracing::error!("Proxy error: {e:#}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    // Check that we can acquire a token (validates Workload Identity setup)
    match state
        .auth
        .get_token("https://cognitiveservices.azure.com")
        .await
    {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            tracing::warn!("Readiness check failed: {e}");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready — token acquisition failed")
                .into_response()
        }
    }
}

async fn metrics() -> String {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap_or_default()
}
