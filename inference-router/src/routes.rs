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
use axum::extract::Path;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use bytes::Bytes;
use std::sync::Arc;

use crate::auth::WorkloadIdentityAuth;
use crate::blocklist::Blocklist;
use crate::budget::TokenBudgetTracker;
use crate::config::Config;
use crate::governance::GovernanceState;
use crate::proxy::{self, UpstreamConfig};
use crate::safety;
use crate::spawn;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<WorkloadIdentityAuth>,
    pub client: reqwest::Client,
    pub config: Arc<Config>,
    pub budget: TokenBudgetTracker,
    pub governance: Arc<GovernanceState>,
    pub blocklist: Blocklist,
    /// Live model override (set via /admin/model). Takes priority over config.default_model.
    pub model_override: Arc<std::sync::RwLock<Option<String>>>,
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

        let sandbox_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());

        // Initialize blocklist — enabled via BLOCKLIST_ENABLED=true
        let blocklist_enabled = std::env::var("BLOCKLIST_ENABLED")
            .unwrap_or_else(|_| "true".into())
            .parse::<bool>()
            .unwrap_or(true);

        let blocklist = if blocklist_enabled {
            let seed_path = std::env::var("BLOCKLIST_SEED_PATH")
                .unwrap_or_else(|_| "/etc/azureclaw/blocklist/domains.txt".into());
            let bl = Blocklist::new(Some(&seed_path)).await;

            let refresh_secs = std::env::var("BLOCKLIST_REFRESH_SECS")
                .ok()
                .and_then(|s| s.parse().ok());

            bl.start_refresh_task(client.clone(), refresh_secs, Some(seed_path));
            tracing::info!("Blocklist enabled — auto-refresh active");
            bl
        } else {
            tracing::info!("Blocklist disabled");
            Blocklist::disabled()
        };

        // Learn mode: observe all egress domains (blocklist still enforced)
        let learn_mode = std::env::var("EGRESS_LEARN_MODE")
            .unwrap_or_else(|_| "false".into())
            .parse::<bool>()
            .unwrap_or(false);
        if learn_mode {
            blocklist.set_learn_mode(true);
        }

        Ok(Self {
            auth: Arc::new(WorkloadIdentityAuth::new()),
            client,
            config: Arc::new(config),
            budget,
            governance: Arc::new(GovernanceState::new(&sandbox_name)),
            blocklist,
            model_override: Arc::new(std::sync::RwLock::new(None)),
        })
    }

    fn upstream_config(&self, sandbox_name: &str) -> UpstreamConfig {
        // For inference (chat completions, embeddings): prefer the dedicated OpenAI endpoint
        // (openai.azure.com) over the Foundry project endpoint (services.ai.azure.com).
        // Foundry project endpoint is used for agent/memory/knowledge APIs, not inference.
        let endpoint = self.config.azure_openai_endpoint.clone()
            .or_else(|| self.config.foundry_endpoint.clone())
            .unwrap_or_default();

        // Live model override takes priority over config default
        let deployment = self.model_override.read().ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(|| self.config.default_model.clone());

        UpstreamConfig {
            endpoint,
            deployment,
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
        .route("/v1/deployments", get(list_deployments))
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
        .route("/indexes", get(foundry_proxy))
        .route("/indexes/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy).patch(foundry_proxy))
        // Connections APIs
        .route("/connections", get(foundry_proxy).post(foundry_proxy))
        .route("/connections/{*path}", get(foundry_proxy).post(foundry_proxy))
        // Deployments APIs
        .route("/deployments", get(foundry_proxy))
        .route("/deployments/{*path}", get(foundry_proxy))
        // Datasets APIs
        .route("/datasets", get(foundry_proxy))
        .route("/datasets/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy).patch(foundry_proxy))
        // Insights APIs
        .route("/insights", get(foundry_proxy).post(foundry_proxy))
        .route("/insights/{*path}", get(foundry_proxy))
        // OpenAI Conversations + Responses + Evals + Vector Stores + Files
        .route("/openai/conversations", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/conversations/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/responses", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/responses/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/evals", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/evals/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/vector_stores", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/vector_stores/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/files", get(foundry_proxy).post(foundry_proxy))
        .route("/openai/files/{*path}", get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy))
        .route("/openai/fine-tuning/{*path}", get(foundry_proxy).post(foundry_proxy))
        // Red Teams APIs
        .route("/redTeams/runs", get(foundry_proxy).post(foundry_proxy))
        .route("/redTeams/runs/{*path}", get(foundry_proxy))
        // Schedules APIs
        .route("/schedules", get(foundry_proxy))
        .route("/schedules/{*path}", get(foundry_proxy).put(foundry_proxy).delete(foundry_proxy))
        // Evaluation Taxonomies APIs
        .route("/evaluationtaxonomies", get(foundry_proxy).post(foundry_proxy))
        .route("/evaluationtaxonomies/{*path}", get(foundry_proxy).put(foundry_proxy).delete(foundry_proxy).patch(foundry_proxy))
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

/// Admin routes — live configuration (localhost only, for dev mode model switching).
pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/admin/model", get(admin_get_model).put(admin_set_model))
}

/// AGT governance routes that expose sensitive data — require admin token.
pub fn sensitive_agt_routes() -> Router<AppState> {
    Router::new()
        // Policy evaluation
        .route("/agt/evaluate", post(agt_evaluate))
        // Trust management
        .route("/agt/trust", get(agt_trust_list))
        .route("/agt/trust/{agent_id}", get(agt_trust_get))
        // Audit log
        .route("/agt/audit", get(agt_audit))
        .route("/agt/audit/verify", get(agt_audit_verify))
        // Status (exposes trust scores, audit entries, inbox count)
        .route("/agt/status", get(agt_status))
        // Trust update (plugin pushes reputation changes to the router's trust store)
        .route("/agt/trust", post(agt_trust_update))
        // Registry reputation (proxied from agentmesh-registry)
        .route("/agt/reputation", get(agt_reputation))
}

/// AGT mesh + relay routes — public (protected by E2E encryption + NetworkPolicy).
pub fn mesh_routes() -> Router<AppState> {
    Router::new()
        // Inter-agent mesh (E2E encrypted via AGT relay only — no plaintext HTTP)
        .route("/agt/mesh/inbox", get(agt_mesh_inbox))
        // AGT relay proxy (WebSocket + HTTP registry)
        .route("/agt/relay", get(agt_relay_proxy))
        .route("/agt/registry/{*path}", get(agt_registry_proxy).post(agt_registry_proxy))
        // Blocklist (read-only, informational)
        .route("/blocklist/status", get(blocklist_status))
        .route("/blocklist/check", post(blocklist_check))
}

/// Egress management routes — require admin token (approve, deny, enforce, learn).
pub fn egress_routes() -> Router<AppState> {
    Router::new()
        .route("/egress/learn", post(egress_learn_toggle))
        .route("/egress/learned", get(egress_learned))
        .route("/egress/learned/clear", post(egress_learned_clear))
        .route("/egress/fetch", post(egress_fetch))
        .route("/egress/allowlist", get(egress_allowlist))
        .route("/egress/approve", post(egress_approve))
        .route("/egress/deny", post(egress_deny))
        .route("/egress/pending", get(egress_pending))
        .route("/egress/enforce", post(egress_enforce))
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
    if state.config.content_safety_enabled
        && let Some(ref endpoint) = state.config.content_safety_endpoint
    {
        // Extract user message from request body for safety check
        if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&body)
            && let Some(messages) = body_json.get("messages").and_then(|m| m.as_array())
            && let Some(last_user_msg) = messages
                .iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                .and_then(|m| m.get("content").and_then(|c| c.as_str()))
            && let Err(e) = safety::check_content_safety(endpoint, last_user_msg).await
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

    // Prompt Shields check
    if state.config.prompt_shields_enabled
        && let Some(ref endpoint) = state.config.content_safety_endpoint
        && let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&body)
        && let Some(messages) = body_json.get("messages").and_then(|m| m.as_array())
    {
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
            if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&resp_body)
                && let Some(total) = body_json
                    .get("usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|v| v.as_u64())
            {
                state.budget.record_usage(sandbox_name, total).await;

                if let Err(msg) = state.budget.check_per_request(total) {
                    tracing::warn!(sandbox = %sandbox_name, "Per-request limit: {msg}");
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

    // Embeddings need a different deployment than chat — extract model from request body
    let mut upstream = state.upstream_config(sandbox_name);
    if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&body) {
        if let Some(model) = body_json.get("model").and_then(|m| m.as_str()) {
            // Strip "azure/" or "azure-openai/" prefix if present
            let clean = model.trim_start_matches("azure/").trim_start_matches("azure-openai/");
            upstream.deployment = clean.to_string();
        }
    }

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
    let audience = if state.config.foundry_endpoint.as_deref()
        .is_some_and(|ep| ep.contains("services.ai.azure.com") && ep.contains("/api/projects/"))
    {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    };
    match state
        .auth
        .get_token(audience)
        .await
    {
        Ok(_) => {
            // Also check Content Safety endpoint if configured
            if let Some(ref cs_endpoint) = state.config.content_safety_endpoint
                && state.config.content_safety_enabled
            {
                let url = format!("{}/contentsafety/text:analyze?api-version=2024-09-01", cs_endpoint.trim_end_matches('/'));
                let reachable = state.client.post(&url).timeout(std::time::Duration::from_secs(3)).send().await.is_ok();
                if !reachable {
                    tracing::warn!("Content Safety endpoint unreachable: {cs_endpoint}");
                    return (StatusCode::OK, "ok (content safety unreachable — failing open)").into_response();
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

/// GET /admin/model — show current model
async fn admin_get_model(State(state): State<AppState>) -> impl IntoResponse {
    let current = state.model_override.read().ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| state.config.default_model.clone());
    Json(serde_json::json!({ "model": current, "default": state.config.default_model }))
}

/// PUT /admin/model — switch model live (body: {"model": "gpt-5-mini"})
async fn admin_set_model(
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from));

    match model {
        Some(m) => {
            let prev = state.model_override.read().ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| state.config.default_model.clone());
            if let Ok(mut guard) = state.model_override.write() {
                *guard = Some(m.clone());
            }
            tracing::info!(from = %prev, to = %m, "Model switched via /admin/model");
            Json(serde_json::json!({ "model": m, "previous": prev }))
        }
        None => Json(serde_json::json!({ "error": "body must contain {\"model\": \"<name>\"}" })),
    }
}

/// GET /v1/models — list available models from the OpenAI endpoint.
async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let endpoint = state.config.azure_openai_endpoint.clone()
        .or_else(|| state.config.foundry_endpoint.clone())
        .unwrap_or_default();

    // Azure OpenAI uses /openai/models?api-version=... (NOT /openai/v1/models)
    let models_url = format!("{}/openai/models?api-version=2024-10-21", endpoint.trim_end_matches('/'));

    // Get token — use correct audience for Foundry project vs legacy AOAI
    let audience = if endpoint.contains("services.ai.azure.com") && endpoint.contains("/api/projects/") {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    };
    let token = match state.auth.get_token(audience).await {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Token error: {e}"), "type": "auth_error"}
            }))).into_response();
        }
    };

    let mut req = state.client.get(&models_url);
    if state.auth.is_api_key_mode() {
        req = req.header("api-key", &token);
    } else {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let resp = req.send().await;

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

/// List deployed models (not the full catalog).
/// For Foundry/AI Services endpoints, /openai/deployments is available on the data plane.
/// For legacy Azure OpenAI, this may return 404 — callers should fall back to /v1/models.
async fn list_deployments(State(state): State<AppState>) -> impl IntoResponse {
    let endpoint = state.config.azure_openai_endpoint.clone()
        .or_else(|| state.config.foundry_endpoint.clone())
        .unwrap_or_default();

    let deployments_url = format!("{}/openai/deployments?api-version=2024-10-21", endpoint.trim_end_matches('/'));

    let audience = if endpoint.contains("services.ai.azure.com") {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    };
    let token = match state.auth.get_token(audience).await {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Token error: {e}"), "type": "auth_error"}
            }))).into_response();
        }
    };

    let mut req = state.client.get(&deployments_url);
    if state.auth.is_api_key_mode() {
        req = req.header("api-key", &token);
    } else {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let resp = req.send().await;

    match resp {
        Ok(r) => {
            let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = r.bytes().await.unwrap_or_default();
            (status, Body::from(body)).into_response()
        }
        Err(e) => {
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Failed to list deployments: {e}"), "type": "proxy_error"}
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

    // Blocklist check — block requests to known-malicious Foundry project endpoints
    if let crate::blocklist::BlockResult::Blocked { reason, domain } =
        state.blocklist.is_blocked(&endpoint).await
    {
        tracing::warn!(
            sandbox = %sandbox_name,
            domain = %domain,
            reason = %reason,
            "Blocklist: blocked Foundry proxy request"
        );
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": {
                "message": format!("Request blocked by threat intelligence: {reason}"),
                "type": "blocklist_violation",
                "domain": domain
            }
        }))).into_response();
    }

    // Learn mode: record this domain as accessed (for allowlist generation)
    state.blocklist.record_learned(&endpoint).await;

    // Forward the request path + query string to Foundry
    let path = uri.path();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();

    // Detect whether this is a Foundry project endpoint (services.ai.azure.com or ai.azure.com)
    // vs a plain Azure OpenAI endpoint (openai.azure.com). The URL rewriting and auth
    // audience differ between the two.
    let is_foundry_project = endpoint.contains("services.ai.azure.com")
        || endpoint.contains("ai.azure.com") && !endpoint.contains("openai.azure.com");
    let is_azure_openai = endpoint.contains("openai.azure.com");

    // For plain Azure OpenAI, management APIs live under /openai/ prefix and need
    // a different api-version. Rewrite paths that don't already have the prefix.
    let (upstream_path, upstream_query) = if is_azure_openai && !path.starts_with("/openai/") {
        // /deployments → /openai/deployments, /connections → not available (skip)
        let aoai_path = format!("/openai{}", path);
        // Replace Foundry api-version with Azure OpenAI compatible one
        let aoai_query = if query.contains("api-version=") {
            query.replace("api-version=2025-11-15-preview", "api-version=2024-10-21")
        } else {
            "?api-version=2024-10-21".to_string()
        };
        (aoai_path, aoai_query)
    } else {
        (path.to_string(), query)
    };

    let upstream_url = format!("{}{}{}", endpoint.trim_end_matches('/'), upstream_path, upstream_query);

    tracing::info!(
        sandbox = %sandbox_name,
        method = %method,
        path = %upstream_path,
        is_foundry = %is_foundry_project,
        "Proxying Foundry Agent API"
    );

    // Foundry project endpoints (services.ai.azure.com) require https://ai.azure.com audience.
    // Plain Azure OpenAI endpoints require https://cognitiveservices.azure.com audience.
    let audience = if is_foundry_project {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    };
    let token = match state.auth.get_token(audience).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Foundry proxy auth failed: {e}");
            return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": {"message": format!("Auth error: {e}"), "type": "auth_error"}
            }))).into_response();
        }
    };

    // For Azure OpenAI endpoints with API key auth (dev mode), use api-key header
    let use_api_key_header = is_azure_openai && state.auth.is_api_key_mode();

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
    if use_api_key_header {
        upstream_headers.insert(
            "api-key",
            axum::http::HeaderValue::from_str(&token).unwrap(),
        );
    } else {
        upstream_headers.insert(
            "authorization",
            axum::http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
    }
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

// ── AGT Governance Handlers ──────────────────────────────────────────────────

/// POST /agt/evaluate — evaluate a tool action against loaded policy.
async fn agt_evaluate(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if action.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "Missing 'action' field"
        }))).into_response();
    }

    let decision = state.governance.evaluate_action(action).await;
    let (status, result) = match decision {
        crate::governance::PolicyDecision::Allow => (StatusCode::OK, serde_json::json!({
            "decision": "allow", "action": action
        })),
        crate::governance::PolicyDecision::Deny(reason) => (StatusCode::FORBIDDEN, serde_json::json!({
            "decision": "deny", "action": action, "reason": reason
        })),
        crate::governance::PolicyDecision::RequiresApproval(reason) => (StatusCode::ACCEPTED, serde_json::json!({
            "decision": "requires_approval", "action": action, "reason": reason
        })),
        crate::governance::PolicyDecision::RateLimited { retry_after_secs } => (StatusCode::TOO_MANY_REQUESTS, serde_json::json!({
            "decision": "rate_limited", "action": action, "retry_after_secs": retry_after_secs
        })),
    };

    (status, Json(result)).into_response()
}

/// GET /agt/trust — list all known agent trust states.
async fn agt_trust_list(State(state): State<AppState>) -> impl IntoResponse {
    let agents = state.governance.trust.all_agents().await;
    Json(serde_json::json!({ "agents": agents }))
}

/// GET /agt/trust/:agent_id — get trust state for a specific agent.
async fn agt_trust_get(
    State(state): State<AppState>,
    axum::extract::Path(agent_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let trust = state.governance.trust.get_trust(&agent_id).await;
    Json(serde_json::json!(trust))
}

/// GET /agt/audit — get the full audit log.
async fn agt_audit(State(state): State<AppState>) -> impl IntoResponse {
    let entries = state.governance.audit.entries().await;
    Json(serde_json::json!({
        "entries": entries,
        "count": entries.len(),
        "sandbox": state.governance.sandbox_name
    }))
}

/// GET /agt/audit/verify — verify hash-chain integrity.
async fn agt_audit_verify(State(state): State<AppState>) -> impl IntoResponse {
    let valid = state.governance.audit.verify_integrity().await;
    let count = state.governance.audit.entries().await.len();
    Json(serde_json::json!({
        "integrity": if valid { "valid" } else { "COMPROMISED" },
        "entries": count,
        "sandbox": state.governance.sandbox_name
    }))
}

/// REMOVED: POST /agt/mesh/send — plaintext HTTP mesh send.
/// All inter-agent communication now uses E2E encrypted AGT relay exclusively.
/// Keeping as dead code for reference until next cleanup.
#[allow(dead_code)]
async fn agt_mesh_send(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let to_agent = body.get("to_agent").and_then(|v| v.as_str()).unwrap_or("");
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let msg_type = body.get("type").and_then(|v| v.as_str()).unwrap_or("request");

    if to_agent.is_empty() || content.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "Missing 'to_agent' or 'content' field"
        }))).into_response();
    }

    // Trust gate: check if target agent is trusted
    if !state.governance.trust.is_trusted(to_agent).await {
        let trust = state.governance.trust.get_trust(to_agent).await;
        state.governance.audit.append(
            &format!("mesh:send:{}", to_agent),
            "deny",
            &format!("Trust score {} below threshold", trust.score),
        ).await;
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": "Target agent trust score below threshold",
            "agent": to_agent,
            "score": trust.score,
            "threshold": state.governance.trust.default_score
        }))).into_response();
    }

    let msg = crate::governance::MeshMessage {
        id: format!("{}-{}", state.governance.sandbox_name, uuid_v4()),
        from_agent: state.governance.sandbox_name.clone(),
        to_agent: to_agent.to_string(),
        content: content.to_string(),
        message_type: msg_type.to_string(),
        timestamp: format!("{}Z", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
        signature: String::new(),
    };

    // Resolve target agent's router via K8s DNS or explicit URL override.
    // Priority: AGT_MESH_TARGET_URL env var > CRD namespace lookup > K8s DNS in parent namespace
    let target_url = if let Ok(url) = std::env::var("AGT_MESH_TARGET_URL") {
        url
    } else {
        // Try to resolve the target's actual namespace from ClawSandbox CRD status
        let target_ns = match spawn::get_sandbox_status(to_agent).await {
            Ok(resp) => resp.namespace.unwrap_or_else(|| format!("azureclaw-{}", to_agent)),
            Err(_) => {
                // Not a spawned sub-agent — try same namespace (peer agents like agent-alpha/agent-beta)
                std::env::var("AGT_MESH_NAMESPACE")
                    .unwrap_or_else(|_| std::env::var("NAMESPACE")
                        .unwrap_or_else(|_| "azureclaw-foundry-test".into()))
            }
        };
        format!("http://{}.{}.svc.cluster.local:8443", to_agent, target_ns)
    };
    let receive_url = format!("{}/agt/mesh/receive", target_url.trim_end_matches('/'));

    tracing::info!(
        from = %state.governance.sandbox_name,
        to = %to_agent,
        url = %receive_url,
        "Sending mesh message"
    );

    match state.client.post(&receive_url)
        .json(&msg)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            state.governance.trust.record_success(to_agent).await;
            state.governance.mesh_metrics.messages_sent.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            state.governance.audit.append(
                &format!("mesh:send:{}", to_agent),
                "delivered",
                &format!("Message {} delivered to {}", msg.id, to_agent),
            ).await;
            tracing::info!(from = %state.governance.sandbox_name, to = %to_agent, id = %msg.id, "Mesh message delivered");
            (StatusCode::OK, Json(serde_json::json!({
                "status": "delivered",
                "message_id": msg.id,
                "to_agent": to_agent,
                "from_agent": state.governance.sandbox_name
            }))).into_response()
        }
        Ok(resp) => {
            let status_code = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            state.governance.trust.record_failure(to_agent).await;
            state.governance.audit.append(
                &format!("mesh:send:{}", to_agent),
                "failed",
                &format!("Target returned {}: {}", status_code, &body_text[..body_text.len().min(100)]),
            ).await;
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": format!("Target agent returned {}", status_code),
                "to_agent": to_agent,
                "details": &body_text[..body_text.len().min(200)]
            }))).into_response()
        }
        Err(e) => {
            tracing::warn!(to = %to_agent, error = %e, "Target agent unreachable");
            state.governance.audit.append(
                &format!("mesh:send:{}", to_agent),
                "unreachable",
                &format!("Target unreachable: {}", e),
            ).await;
            (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({
                "error": format!("Target agent '{}' unreachable: {}", to_agent, e),
                "to_agent": to_agent,
                "message_id": msg.id
            }))).into_response()
        }
    }
}

/// GET /agt/mesh/inbox — read received messages.
async fn agt_mesh_inbox(State(state): State<AppState>) -> impl IntoResponse {
    let messages = state.governance.inbox.peek().await;
    Json(serde_json::json!({
        "messages": messages,
        "count": messages.len()
    }))
}

/// REMOVED: POST /agt/mesh/receive — plaintext HTTP mesh receive.
/// All inter-agent communication now uses E2E encrypted AGT relay exclusively.
#[allow(dead_code)]
async fn agt_mesh_receive(
    State(state): State<AppState>,
    Json(msg): Json<crate::governance::MeshMessage>,
) -> impl IntoResponse {
    tracing::info!(from = %msg.from_agent, to = %msg.to_agent, msg_type = %msg.message_type, "Mesh message received");

    // Trust gate on incoming messages too
    if !state.governance.trust.is_trusted(&msg.from_agent).await {
        state.governance.audit.append(
            &format!("mesh:receive:{}", msg.from_agent),
            "deny",
            "Sender trust below threshold",
        ).await;
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": "Sender trust score below threshold"
        }))).into_response();
    }

    state.governance.trust.record_success(&msg.from_agent).await;
    state.governance.mesh_metrics.messages_received.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.governance.audit.append(
        &format!("mesh:receive:{}", msg.from_agent),
        "allow",
        &format!("Message {} received", msg.id),
    ).await;

    let _from_agent = msg.from_agent.clone();
    let _msg_id = msg.id.clone();
    state.governance.inbox.receive(msg).await;

    // No auto-processing here. Task_request messages are handled by the OpenClaw
    // plugin via the AGT relay (E2E encrypted) onMessage handler, which has full
    // tool access (exec/shell) through the node host.

    (StatusCode::OK, Json(serde_json::json!({"status": "received"}))).into_response()
}

/// GET /agt/status — overall governance status.
async fn agt_status(State(state): State<AppState>) -> impl IntoResponse {
    let audit_entries = state.governance.audit.entries().await;
    let trust_agents = state.governance.trust.all_agents().await;
    let inbox = state.governance.inbox.peek().await;

    let blocklist_len = state.blocklist.len().await;

    let total_interactions: u64 = trust_agents.iter().map(|a| a.interactions).sum();

    Json(serde_json::json!({
        "enabled": state.governance.enabled,
        "sandbox": state.governance.sandbox_name,
        "policy_loaded": state.governance.policy.is_loaded(),
        "audit_entries": audit_entries.len(),
        "audit_integrity": state.governance.audit.verify_integrity().await,
        "known_agents": trust_agents.len(),
        "trust_states": trust_agents,
        "inbox_messages": inbox.len(),
        "blocklist_domains": blocklist_len,
        "egress_learn_mode": state.blocklist.is_learn_mode(),
        "egress_learned_domains": state.blocklist.learned_count().await,
        "mesh_sessions": state.governance.mesh_metrics.sessions.load(std::sync::atomic::Ordering::Relaxed),
        "mesh_messages_sent": state.governance.mesh_metrics.messages_sent.load(std::sync::atomic::Ordering::Relaxed),
        "mesh_messages_received": state.governance.mesh_metrics.messages_received.load(std::sync::atomic::Ordering::Relaxed),
        "trust_updates": state.governance.mesh_metrics.trust_updates.load(std::sync::atomic::Ordering::Relaxed),
        "total_interactions": total_interactions,
    }))
}

/// POST /agt/trust — plugin pushes trust updates after mesh interactions.
/// Body: { "agent_id": "peer-name", "score": 510, "interactions": 1 }
async fn agt_trust_update(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let agent_id = body["agent_id"].as_str().unwrap_or("");
    let score = body["score"].as_u64().unwrap_or(500) as u32;
    let interactions = body["interactions"].as_u64().unwrap_or(0);

    if agent_id.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "agent_id required"})),
        );
    }

    state.governance.trust.update_trust(agent_id, score, interactions).await;
    state.governance.mesh_metrics.trust_updates.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.governance.mesh_metrics.sessions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Record in the audit chain so the operator panel sees trust changes
    state.governance.audit.append(
        &format!("trust_update:{}", agent_id),
        "applied",
        &format!("score={} interactions={}", score, interactions),
    ).await;

    tracing::info!(agent_id, score, interactions, "AGT trust updated via plugin");

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"ok": true, "agent_id": agent_id, "score": score})),
    )
}

/// GET /agt/reputation — fetch this agent's reputation from the AgentMesh registry.
/// Returns the registry-computed score (session history, peer feedback, tier bonus)
/// alongside the local trust store snapshot.
async fn agt_reputation(State(state): State<AppState>) -> impl IntoResponse {
    let registry_url = std::env::var("AGT_REGISTRY_URL")
        .unwrap_or_else(|_| "http://agentmesh-registry.agentmesh.svc.cluster.local:8080".into());

    let sandbox_name = &state.governance.sandbox_name;
    let base = registry_url.trim_end_matches('/');

    // Step 1: Look up our AMID by searching for our sandbox name as a capability.
    // The plugin registers with capabilities: ["azureclaw-agent", "task-execution", sandbox_name]
    let amid = match state.client
        .get(&format!("{}/v1/registry/search?capability={}", base, sandbox_name))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<serde_json::Value>().await.ok().and_then(|v| {
                // Pick the most recently seen agent matching our name
                v.get("results")?.as_array()?
                    .iter()
                    .filter(|a| a.get("display_name").and_then(|n| n.as_str()) == Some(sandbox_name))
                    .max_by_key(|a| a.get("last_seen").and_then(|t| t.as_str()).unwrap_or("").to_string())
                    .and_then(|a| a.get("amid").and_then(|v| v.as_str()).map(String::from))
            })
        }
        _ => None,
    };

    // Step 2: If we found our AMID, fetch reputation score
    let registry = if let Some(ref agent_amid) = amid {
        match state.client
            .get(&format!("{}/v1/registry/reputation/score?amid={}", base, agent_amid))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                resp.json::<serde_json::Value>().await.ok()
            }
            Ok(resp) => {
                tracing::debug!(status = %resp.status(), "Registry reputation lookup returned non-200");
                None
            }
            Err(e) => {
                tracing::debug!(error = %e, "Registry reputation lookup failed");
                None
            }
        }
    } else {
        tracing::debug!(sandbox = %sandbox_name, "Agent not found in registry — not yet registered");
        None
    };

    // Local trust store snapshot
    let local_trust = state.governance.trust.all_agents().await;

    Json(serde_json::json!({
        "amid": amid.as_deref().unwrap_or(sandbox_name),
        "sandbox": sandbox_name,
        "registry": registry,
        "local_trust": local_trust,
        "default_score": state.governance.trust.default_score,
    }))
}

/// GET /agt/relay — WebSocket proxy to the self-hosted AgentMesh relay.
/// The plugin (UID 1000) can only reach localhost. The router (UID 1001) proxies
/// WebSocket connections to the relay at agentmesh-relay.agentmesh.svc.cluster.local:8765.
async fn agt_relay_proxy(
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let relay_url = std::env::var("AGT_RELAY_URL")
        .unwrap_or_else(|_| "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765".into());

    ws.on_upgrade(move |client_socket| async move {
        relay_websocket_bridge(client_socket, &relay_url).await;
    })
}

/// Bidirectional WebSocket bridge: client ↔ relay.
async fn relay_websocket_bridge(mut client_socket: WebSocket, relay_url: &str) {
    use futures::stream::StreamExt;
    use futures::sink::SinkExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio_tungstenite::tungstenite;

    // Connect to the upstream relay with a 30-second timeout
    let upstream = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio_tungstenite::connect_async(relay_url),
    ).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => {
            tracing::error!(error = %e, url = %relay_url, "Failed to connect to AGT relay");
            // Send close frame so the client gets an error instead of hanging
            let _ = client_socket.close().await;
            return;
        }
        Err(_) => {
            tracing::error!(url = %relay_url, "AGT relay connection timed out (30s)");
            let _ = client_socket.close().await;
            return;
        }
    };

    tracing::info!(url = %relay_url, "AGT relay WebSocket proxy connected");

    let (mut client_tx, mut client_rx) = client_socket.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    // Message counters (never log content — E2E encrypted)
    let outbound_count = std::sync::Arc::new(AtomicU64::new(0));
    let outbound_bytes = std::sync::Arc::new(AtomicU64::new(0));
    let inbound_count = std::sync::Arc::new(AtomicU64::new(0));
    let inbound_bytes = std::sync::Arc::new(AtomicU64::new(0));

    let out_count = outbound_count.clone();
    let out_bytes = outbound_bytes.clone();
    let in_count = inbound_count.clone();
    let in_bytes = inbound_bytes.clone();

    // Forward: client → relay (outbound encrypted messages)
    let mut client_to_relay = tokio::spawn(async move {
        while let Some(Ok(msg)) = client_rx.next().await {
            let (tung_msg, size) = match msg {
                Message::Text(ref t) => (
                    tungstenite::Message::Text(t.to_string().into()),
                    t.len(),
                ),
                Message::Binary(ref b) => (
                    tungstenite::Message::Binary(b.to_vec().into()),
                    b.len(),
                ),
                Message::Ping(p) => {
                    let _ = upstream_tx.send(tungstenite::Message::Ping(p.to_vec().into())).await;
                    continue;
                }
                Message::Pong(p) => {
                    let _ = upstream_tx.send(tungstenite::Message::Pong(p.to_vec().into())).await;
                    continue;
                }
                Message::Close(_) => break,
            };
            out_count.fetch_add(1, Ordering::Relaxed);
            out_bytes.fetch_add(size as u64, Ordering::Relaxed);
            // Hex-dump first 128 bytes for traffic capture / E2E encryption proof.
            // The relay only ever sees ciphertext — readable plaintext here means encryption failed.
            let raw_bytes: Vec<u8> = match &tung_msg {
                tungstenite::Message::Text(t) => t.as_bytes().to_vec(),
                tungstenite::Message::Binary(b) => b.to_vec(),
                _ => vec![],
            };
            let hex_preview: String = raw_bytes.iter().take(128).map(|b| format!("{b:02x}")).collect::<Vec<String>>().join(" ");
            let printable: String = raw_bytes.iter().take(128).map(|b| if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' }).collect();
            tracing::debug!(
                direction = "agent->relay",
                size,
                hex = %hex_preview,
                ascii = %printable,
                "AGT relay: TRAFFIC CAPTURE (outbound frame)"
            );
            if upstream_tx.send(tung_msg).await.is_err() {
                break;
            }
        }
    });

    // Forward: relay → client (inbound encrypted messages)
    let mut relay_to_client = tokio::spawn(async move {
        while let Some(Ok(msg)) = upstream_rx.next().await {
            let (axum_msg, size) = match msg {
                tungstenite::Message::Text(ref t) => (
                    Message::Text(t.to_string().into()),
                    t.len(),
                ),
                tungstenite::Message::Binary(ref b) => (
                    Message::Binary(b.to_vec().into()),
                    b.len(),
                ),
                tungstenite::Message::Ping(p) => {
                    let _ = client_tx.send(Message::Ping(p.to_vec().into())).await;
                    continue;
                }
                tungstenite::Message::Pong(p) => {
                    let _ = client_tx.send(Message::Pong(p.to_vec().into())).await;
                    continue;
                }
                tungstenite::Message::Close(_) => break,
                _ => continue,
            };
            in_count.fetch_add(1, Ordering::Relaxed);
            in_bytes.fetch_add(size as u64, Ordering::Relaxed);
            // Hex-dump first 128 bytes of each inbound frame for traffic capture.
            let raw_bytes: Vec<u8> = match &msg {
                tungstenite::Message::Text(t) => t.as_bytes().to_vec(),
                tungstenite::Message::Binary(b) => b.to_vec(),
                _ => vec![],
            };
            let hex_preview: String = raw_bytes.iter().take(128).map(|b| format!("{b:02x}")).collect::<Vec<String>>().join(" ");
            let printable: String = raw_bytes.iter().take(128).map(|b| if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' }).collect();
            tracing::debug!(
                direction = "relay->agent",
                size,
                hex = %hex_preview,
                ascii = %printable,
                "AGT relay: TRAFFIC CAPTURE (inbound frame)"
            );
            if client_tx.send(axum_msg).await.is_err() {
                break;
            }
        }
    });

    // Wait for either direction to close, then abort the other to prevent
    // zombie connections that trigger "Failed to send message: closed connection".
    tokio::select! {
        _ = &mut client_to_relay => { relay_to_client.abort(); },
        _ = &mut relay_to_client => { client_to_relay.abort(); },
    }

    let out_n = outbound_count.load(Ordering::Relaxed);
    let out_b = outbound_bytes.load(Ordering::Relaxed);
    let in_n = inbound_count.load(Ordering::Relaxed);
    let in_b = inbound_bytes.load(Ordering::Relaxed);
    tracing::info!(
        outbound_messages = out_n,
        outbound_bytes = out_b,
        inbound_messages = in_n,
        inbound_bytes = in_b,
        "AGT relay WebSocket proxy disconnected"
    );
}

/// GET/POST /agt/registry/* — HTTP proxy to the self-hosted AgentMesh registry.
/// Proxies all registry API calls so the plugin (UID 1000, localhost-only) can
/// reach the registry service via the router.
async fn agt_registry_proxy(
    State(state): State<AppState>,
    Path(path): Path<String>,
    query: axum::extract::RawQuery,
    headers: HeaderMap,
    method: axum::http::Method,
    body: Bytes,
) -> impl IntoResponse {
    let registry_url = std::env::var("AGT_REGISTRY_URL")
        .unwrap_or_else(|_| "http://agentmesh-registry.agentmesh.svc.cluster.local:8080".into());

    let mut url = format!("{}/v1/{}", registry_url.trim_end_matches('/'), path);
    // Forward query parameters (critical for search/lookup)
    if let Some(qs) = query.0 {
        url.push('?');
        url.push_str(&qs);
    }

    let mut req = state.client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
        &url,
    );

    // Forward Content-Type
    if let Some(ct) = headers.get("content-type") {
        req = req.header("content-type", ct);
    }

    if !body.is_empty() {
        req = req.body(body.to_vec());
    }

    match req.timeout(std::time::Duration::from_secs(10)).send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = resp.bytes().await.unwrap_or_default();
            (status, [(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "AGT registry proxy failed");
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": format!("Registry unreachable: {}", e)
            }))).into_response()
        }
    }
}

/// GET /blocklist/status — blocklist health and domain count.
async fn blocklist_status(State(state): State<AppState>) -> impl IntoResponse {
    let count = state.blocklist.len().await;
    Json(serde_json::json!({
        "enabled": count > 0 || std::env::var("BLOCKLIST_ENABLED").unwrap_or_else(|_| "true".into()) == "true",
        "domain_count": count,
        "learn_mode": state.blocklist.is_learn_mode(),
        "learned_domains": state.blocklist.learned_count().await,
    }))
}

/// POST /blocklist/check — check if a domain/URL is blocked.
async fn blocklist_check(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let input = body.get("domain")
        .or_else(|| body.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if input.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "Provide 'domain' or 'url' field"
        }))).into_response();
    }

    match state.blocklist.is_blocked(input).await {
        crate::blocklist::BlockResult::Blocked { reason, domain } => {
            (StatusCode::OK, Json(serde_json::json!({
                "blocked": true,
                "domain": domain,
                "reason": reason,
            }))).into_response()
        }
        crate::blocklist::BlockResult::Allowed => {
            (StatusCode::OK, Json(serde_json::json!({
                "blocked": false,
                "domain": input,
            }))).into_response()
        }
    }
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{:x}-{:x}", d.as_secs(), d.subsec_nanos())
}

/// GET /egress/learned — list all domains observed during learn mode.
async fn egress_learned(State(state): State<AppState>) -> impl IntoResponse {
    let domains = state.blocklist.get_learned_domains().await;
    Json(serde_json::json!({
        "learn_mode": state.blocklist.is_learn_mode(),
        "count": domains.len(),
        "domains": domains,
    }))
}

/// POST /egress/learn — toggle learn mode at runtime.
async fn egress_learn_toggle(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let enabled = body.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    state.blocklist.set_learn_mode(enabled);
    Json(serde_json::json!({
        "learn_mode": enabled,
    }))
}

/// POST /egress/learned/clear — clear learned domains (after export/review).
async fn egress_learned_clear(State(state): State<AppState>) -> impl IntoResponse {
    state.blocklist.clear_learned().await;
    Json(serde_json::json!({
        "status": "cleared",
        "learn_mode": state.blocklist.is_learn_mode(),
    }))
}

/// POST /egress/fetch — audited, allowlist-checked HTTP proxy for sandbox egress.
///
/// Security model:
/// 1. Blocklist → hard deny (threat intelligence)
/// 2. Allowlist → approved domains pass through
/// 3. Unknown domain → deny + create pending approval request
/// 4. Learn mode → log + allow (discovery phase only)
///
/// Body: { "url": "https://...", "method": "GET"|"POST"|..., "headers": {}, "body": "..." }
/// Returns: { "status": <http_code>, "headers": {...}, "body": "..." }
async fn egress_fetch(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
    let req_body = req.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let req_headers = req.get("headers").and_then(|v| v.as_object());

    if url.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "Missing 'url' field"
        }))).into_response();
    }

    let sandbox = &state.governance.sandbox_name;

    // Check egress access: blocklist → allowlist → pending
    if let Err(reason) = state.blocklist.check_egress(url, sandbox).await {
        tracing::warn!(url = %url, reason = %reason, "Egress fetch denied");
        state.governance.audit.append(
            &format!("egress:fetch:{}", url),
            "deny",
            &reason,
        ).await;
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": reason,
            "url": url,
            "action": "Run 'azureclaw egress <name> --pending' to see pending requests, then 'azureclaw egress <name> --approve <domain>' to allow.",
        }))).into_response();
    }

    // Record in learn mode
    state.blocklist.record_learned(url).await;

    // Audit log
    state.governance.audit.append(
        &format!("egress:fetch:{}", url),
        "allow",
        &format!("{} request to {}", method, url),
    ).await;

    tracing::info!(url = %url, method = %method, "Egress fetch proxied");

    // Build and send the request
    let http_method = match method.to_uppercase().as_str() {
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        "HEAD" => reqwest::Method::HEAD,
        _ => reqwest::Method::GET,
    };

    let mut request = state.client.request(http_method, url);

    if let Some(headers) = req_headers {
        for (k, v) in headers {
            if let Some(val) = v.as_str()
                && let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
                    request = request.header(name, val);
                }
        }
    }

    if !req_body.is_empty() {
        request = request.body(req_body.to_string());
    }

    match request.timeout(std::time::Duration::from_secs(30)).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let resp_headers: serde_json::Map<String, serde_json::Value> = resp.headers()
                .iter()
                .filter_map(|(k, v)| {
                    v.to_str().ok().map(|val| (k.to_string(), serde_json::Value::String(val.to_string())))
                })
                .collect();
            let body = resp.text().await.unwrap_or_default();
            (StatusCode::OK, Json(serde_json::json!({
                "status": status,
                "headers": resp_headers,
                "body": body,
            }))).into_response()
        }
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "Egress fetch failed");
            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                "error": format!("Request failed: {}", e),
                "url": url,
            }))).into_response()
        }
    }
}

/// GET /egress/allowlist — list approved egress domains.
async fn egress_allowlist(State(state): State<AppState>) -> impl IntoResponse {
    let domains = state.blocklist.get_allowlist().await;
    Json(serde_json::json!({
        "count": domains.len(),
        "domains": domains,
    }))
}

/// GET /egress/pending — list pending approval requests.
async fn egress_pending(State(state): State<AppState>) -> impl IntoResponse {
    let pending = state.blocklist.get_pending_approvals().await;
    Json(serde_json::json!({
        "count": pending.len(),
        "pending": pending,
    }))
}

/// POST /egress/approve — approve a domain for egress.
/// Body: { "domain": "api.telegram.org" }
async fn egress_approve(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let domain = body.get("domain").and_then(|v| v.as_str()).unwrap_or("");
    if domain.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "Missing 'domain' field"
        }))).into_response();
    }
    state.blocklist.allow_domain(domain).await;
    state.governance.audit.append(
        &format!("egress:approve:{}", domain),
        "approved",
        &format!("Domain {} added to egress allowlist", domain),
    ).await;
    (StatusCode::OK, Json(serde_json::json!({
        "status": "approved",
        "domain": domain,
    }))).into_response()
}

/// POST /egress/deny — deny and remove a pending domain request.
/// Body: { "domain": "evil.example.com" }
async fn egress_deny(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let domain = body.get("domain").and_then(|v| v.as_str()).unwrap_or("");
    if domain.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "error": "Missing 'domain' field"
        }))).into_response();
    }
    state.blocklist.deny_domain(domain).await;
    state.governance.audit.append(
        &format!("egress:deny:{}", domain),
        "denied",
        &format!("Domain {} denied and removed from pending", domain),
    ).await;
    (StatusCode::OK, Json(serde_json::json!({
        "status": "denied",
        "domain": domain,
    }))).into_response()
}

/// POST /egress/enforce — graduate from learn mode to enforcement.
/// Promotes all learned domains into the allowlist, disables learn mode,
/// and clears the learned set. After this, only allowlisted and non-blocklisted
/// domains pass through. New domains go to pending approval.
async fn egress_enforce(State(state): State<AppState>) -> impl IntoResponse {
    let learned = state.blocklist.get_learned_domains().await;
    if learned.is_empty() && !state.blocklist.is_learn_mode() {
        return (StatusCode::OK, Json(serde_json::json!({
            "status": "already_enforcing",
            "learn_mode": false,
            "allowlist_count": state.blocklist.get_allowlist().await.len(),
        }))).into_response();
    }

    // Promote each learned domain to the allowlist
    for domain in &learned {
        state.blocklist.allow_domain(domain).await;
    }

    // Disable learn mode and clear the learned set
    state.blocklist.set_learn_mode(false);
    state.blocklist.clear_learned().await;

    let allowlist = state.blocklist.get_allowlist().await;

    state.governance.audit.append(
        "egress:enforce",
        "enforced",
        &format!("Graduated to enforcement: {} learned domains promoted to allowlist ({} total)", learned.len(), allowlist.len()),
    ).await;

    tracing::info!(
        promoted = learned.len(),
        total_allowlist = allowlist.len(),
        "Egress enforcement activated — learned domains promoted to allowlist"
    );

    (StatusCode::OK, Json(serde_json::json!({
        "status": "enforcing",
        "promoted": learned.len(),
        "allowlist_count": allowlist.len(),
        "allowlist": allowlist,
    }))).into_response()
}

// ==========================================================================
// Sub-agent spawn routes
// ==========================================================================

/// Sandbox spawn routes — create/list/status/delete sub-agent ClawSandbox CRDs.
pub fn spawn_routes() -> Router<AppState> {
    Router::new()
        .route("/sandbox/spawn", post(sandbox_spawn))
        .route("/sandbox/list", get(sandbox_list))
        .route("/sandbox/{name}/status", get(sandbox_status))
        .route("/sandbox/{name}", axum::routing::delete(sandbox_delete))
}

/// POST /sandbox/spawn — create a new sub-agent sandbox.
async fn sandbox_spawn(
    State(_state): State<AppState>,
    Json(req): Json<spawn::SpawnRequest>,
) -> impl IntoResponse {
    let parent_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());

    match spawn::create_sandbox(&parent_name, &req).await {
        Ok(resp) => (StatusCode::CREATED, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(msg) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        ).into_response(),
    }
}

/// GET /sandbox/list — list sub-agents spawned by this sandbox.
async fn sandbox_list(State(_state): State<AppState>) -> impl IntoResponse {
    let parent_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());
    let is_dev = std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true";

    let result = if is_dev {
        spawn::list_sandboxes_docker(&parent_name).await
    } else {
        spawn::list_sandboxes(&parent_name).await
    };

    match result {
        Ok(entries) => Json(serde_json::json!({
            "parent": parent_name,
            "count": entries.len(),
            "sandboxes": entries,
        })),
        Err(msg) => Json(serde_json::json!({
            "parent": parent_name,
            "error": msg,
            "count": 0,
            "sandboxes": [],
        })),
    }
}

/// GET /sandbox/{name}/status — get status of a specific sub-agent.
async fn sandbox_status(Path(name): Path<String>) -> impl IntoResponse {
    match spawn::get_sandbox_status(&name).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(msg) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": msg })),
        ).into_response(),
    }
}

/// DELETE /sandbox/{name} — tear down a sub-agent sandbox.
async fn sandbox_delete(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let parent_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());
    let is_dev = std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true";

    let result = if is_dev {
        spawn::delete_sandbox_docker(&parent_name, &name).await
    } else {
        spawn::delete_sandbox(&parent_name, &name).await
    };

    match result {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(msg) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": msg })),
        ).into_response(),
    }
}
