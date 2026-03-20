//! Axum route handlers for the inference router.

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json,
};
use bytes::Bytes;
use std::sync::Arc;

use crate::auth::WorkloadIdentityAuth;
use crate::budget::TokenBudgetTracker;
use crate::config::Config;
use crate::proxy::{self, UpstreamConfig};
use crate::safety;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<WorkloadIdentityAuth>,
    pub client: reqwest::Client,
    pub config: Arc<Config>,
    pub budget: TokenBudgetTracker,
}

impl AppState {
    pub async fn new(_config: &Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(32)
            .build()?;

        let config = Config::from_env()?;
        let budget = TokenBudgetTracker::new(
            config.token_budget_daily,
            config.token_budget_per_request,
        );

        Ok(Self {
            auth: Arc::new(WorkloadIdentityAuth::new()),
            client,
            config: Arc::new(config),
            budget,
        })
    }

    fn upstream_config(&self, sandbox_name: &str) -> UpstreamConfig {
        let endpoint = self.config.foundry_endpoint.clone()
            .or_else(|| self.config.azure_openai_endpoint.clone())
            .unwrap_or_default();

        UpstreamConfig {
            endpoint,
            deployment: self.config.default_model.clone(),
            sandbox_name: sandbox_name.to_string(),
        }
    }
}

/// Inference API routes — proxied to Azure AI Foundry.
pub fn inference_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/models", get(list_models))
}

/// Foundry Agent API routes — agents, threads, runs (for tools needing agent execution).
/// These are proxied to the Foundry project endpoint, authenticated via IMDS with ai.azure.com audience.
pub fn foundry_agent_routes() -> Router<AppState> {
    Router::new()
        .route("/agents", get(foundry_proxy).post(foundry_proxy))
        .route("/agents/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
}

/// Foundry standalone API routes — Memory Store, Foundry IQ (Knowledge), Evaluations.
/// These work without a hosted Foundry agent. Direct project-level APIs.
pub fn foundry_standalone_routes() -> Router<AppState> {
    Router::new()
        // Memory Store APIs (persistent long-term memory) — uses underscores per REST spec
        .route("/memory_stores", get(foundry_proxy).post(foundry_proxy))
        .route("/memory_stores/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        // Foundry IQ / Knowledge Base APIs (agentic retrieval)
        .route("/knowledgebases", get(foundry_proxy).post(foundry_proxy))
        .route("/knowledgebases/{*path}", get(foundry_proxy).post(foundry_proxy))
        // Evaluations APIs
        .route("/evaluations", get(foundry_proxy).post(foundry_proxy))
        .route("/evaluations/{*path}", get(foundry_proxy).post(foundry_proxy))
        // Evaluators APIs
        .route("/evaluators", get(foundry_proxy).post(foundry_proxy))
        .route("/evaluators/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        // Evaluation rules APIs
        .route("/evaluationrules", get(foundry_proxy).post(foundry_proxy))
        .route("/evaluationrules/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        // Indexes APIs
        .route("/indexes", get(foundry_proxy).post(foundry_proxy))
        .route("/indexes/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        // Connections APIs
        .route("/connections", get(foundry_proxy).post(foundry_proxy))
        .route("/connections/{*path}", get(foundry_proxy).post(foundry_proxy))
        // Deployments APIs
        .route("/deployments", get(foundry_proxy))
        .route("/deployments/{*path}", get(foundry_proxy))
        // Datasets APIs
        .route("/datasets", get(foundry_proxy))
        .route("/datasets/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        // Insights APIs
        .route("/insights", get(foundry_proxy).post(foundry_proxy))
        .route("/insights/{*path}", get(foundry_proxy))
        // OpenAI Conversations + Responses + Evals
        .route("/openai/conversations", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/conversations/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/responses", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/responses/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/evals", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/evals/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/fine-tuning/{*path}", get(foundry_proxy).post(foundry_proxy))
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
    // Extract sandbox identity from header (set by controller)
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

    // Check token budget before forwarding
    if let Err(msg) = state.budget.check_budget(sandbox_name).await {
        tracing::warn!(sandbox = %sandbox_name, "Token budget exceeded: {msg}");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {
                    "message": msg,
                    "type": "token_budget_exceeded",
                    "code": "budget_exceeded"
                }
            })),
        )
            .into_response();
    }

    // Forward to Foundry
    let upstream = state.upstream_config(sandbox_name);

    // Check if client requested streaming
    let is_stream = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream")?.as_bool())
        .unwrap_or(false);

    if is_stream {
        // SSE streaming — pipe response directly for low TTFT
        match proxy::forward_stream(
            state.auth.clone(),
            state.client.clone(),
            upstream,
            "chat/completions",
            headers.clone(),
            body,
        ).await {
            Ok((status, resp_headers, stream)) => {
                let body = Body::from_stream(stream);
                let mut response = (status, body).into_response();
                if let Some(ct) = resp_headers.get("content-type") {
                    response.headers_mut().insert("content-type", ct.clone());
                }
                response
            }
            Err(e) => {
                tracing::error!(sandbox = %sandbox_name, "Stream proxy error: {e:#}");
                (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                    "error": {"message": "Failed to reach inference backend", "type": "proxy_error"}
                }))).into_response()
            }
        }
    } else {
        // Buffered — extract token usage for budget tracking
        let result = proxy::forward(
            &state.auth,
            &state.client,
            &upstream,
            axum::http::Method::POST,
            "chat/completions",
            &headers,
            body,
        ).await;

    match result {
        Ok((status, resp_headers, resp_body)) => {
            // Record token usage from response for budget tracking
            if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&resp_body) {
                if let Some(total) = body_json
                    .get("usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|v| v.as_u64())
                {
                    state.budget.record_usage(sandbox_name, total).await;

                    if let Err(msg) = state.budget.check_per_request(total) {
                        tracing::warn!(sandbox = %sandbox_name, "Per-request limit: {msg}");
                    }
                }
            }

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
    } // end else (buffered)
    } // end if is_stream
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
    match proxy::forward(
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
    match proxy::forward(
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
    // Check that we can acquire a token (validates Workload Identity / IMDS setup)
    match state
        .auth
        .get_token("https://cognitiveservices.azure.com")
        .await
    {
        Ok(_) => {
            // Also check Content Safety endpoint if configured
            if let Some(ref cs_endpoint) = state.config.content_safety_endpoint {
                if state.config.content_safety_enabled {
                    let url = format!("{}/contentsafety/text:analyze?api-version=2024-09-01", cs_endpoint.trim_end_matches('/'));
                    let reachable = state.client.post(&url).timeout(std::time::Duration::from_secs(3)).send().await.is_ok();
                    if !reachable {
                        tracing::warn!("Content Safety endpoint unreachable: {cs_endpoint}");
                        return (StatusCode::OK, "ok (content safety unreachable — failing open)").into_response();
                    }
                }
            }
            (StatusCode::OK, "ok").into_response()
        }
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

/// GET /v1/models — list available models from Foundry.
async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let endpoint = state.config.foundry_endpoint.clone()
        .or_else(|| state.config.azure_openai_endpoint.clone())
        .unwrap_or_default();

    let models_url = format!("{}/openai/v1/models", endpoint.trim_end_matches('/'));

    // Get token
    let token = match state.auth.get_token("https://cognitiveservices.azure.com").await {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Token error: {e}"), "type": "auth_error"}
            }))).into_response();
        }
    };

    let resp = state.client
        .get(&models_url)
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = r.bytes().await.unwrap_or_default();
            (status, Body::from(body)).into_response()
        }
        Err(e) => {
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Failed to list models: {e}"), "type": "proxy_error"}
            }))).into_response()
        }
    }
}

/// Generic Foundry project-level API proxy.
/// Forwards requests to the Foundry project endpoint with IMDS auth (ai.azure.com audience).
/// Handles: /agents/*, /memory-stores/*, /knowledgebases/*, /evaluations/*
async fn foundry_proxy(
    State(state): State<AppState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let sandbox_name = headers
        .get("x-azureclaw-sandbox")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // Use project endpoint for agent/standalone APIs, fall back to foundry/openai endpoint
    let endpoint = state.config.foundry_project_endpoint.clone()
        .or_else(|| state.config.foundry_endpoint.clone())
        .or_else(|| state.config.azure_openai_endpoint.clone())
        .unwrap_or_default();

    // Forward the request path + query string to Foundry
    let path = uri.path();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let upstream_url = format!("{}{}{}", endpoint.trim_end_matches('/'), path, query);

    tracing::info!(
        sandbox = %sandbox_name,
        method = %method,
        path = %path,
        "Proxying Foundry Agent API"
    );

    // Foundry Agent API (project endpoints at services.ai.azure.com) requires
    // https://ai.azure.com audience, unlike inference endpoints which use
    // https://cognitiveservices.azure.com
    let token = match state.auth.get_token("https://ai.azure.com").await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Foundry proxy auth failed: {e}");
            return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Auth error: {e}"), "type": "auth_error"}
            }))).into_response();
        }
    };

    // Build upstream request — strip sandbox headers, inject auth
    let mut upstream_headers = HeaderMap::new();
    for (name, value) in headers.iter() {
        match name.as_str() {
            "authorization" | "api-key" | "x-api-key"
            | "host" | "connection" | "transfer-encoding" | "content-length"
            | "x-azureclaw-sandbox" => continue,
            _ => { upstream_headers.insert(name.clone(), value.clone()); }
        }
    }
    upstream_headers.insert(
        "authorization",
        axum::http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    upstream_headers.entry("content-type")
        .or_insert(axum::http::HeaderValue::from_static("application/json"));

    let resp = state.client
        .request(method, &upstream_url)
        .headers(upstream_headers)
        .body(body)
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let resp_headers = r.headers().clone();
            let body = r.bytes().await.unwrap_or_default();

            tracing::info!(sandbox = %sandbox_name, status = %status.as_u16(), path = %path, "Foundry Agent API complete");

            let mut response = (status, Body::from(body)).into_response();
            if let Some(ct) = resp_headers.get("content-type") {
                response.headers_mut().insert("content-type", ct.clone());
            }
            response
        }
        Err(e) => {
            tracing::error!(sandbox = %sandbox_name, "Foundry proxy error: {e}");
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Foundry Agent API error: {e}"), "type": "proxy_error"}
            }))).into_response()
        }
    }
}
