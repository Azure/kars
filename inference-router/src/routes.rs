//! Axum route handlers for the inference router.

use axum::extract::Path;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use bytes::Bytes;
use std::sync::Arc;

use crate::auth::WorkloadIdentityAuth;
use crate::blocklist::Blocklist;
use crate::budget::TokenBudgetTracker;
use crate::config::Config;
use crate::mesh::{MeshInbox, MeshMetrics};
use crate::proxy::{self, UpstreamConfig};
use crate::safety;
use crate::sidecar::SidecarProxy;
use crate::spawn;
use futures::stream::StreamExt;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<WorkloadIdentityAuth>,
    pub client: reqwest::Client,
    pub config: Arc<Config>,
    pub budget: TokenBudgetTracker,
    pub sidecar: SidecarProxy,
    pub blocklist: Blocklist,
    pub sandbox_name: Arc<String>,
    pub inbox: Arc<MeshInbox>,
    pub mesh_metrics: Arc<MeshMetrics>,
    /// Live model override (set via /admin/model). Takes priority over config.default_model.
    pub model_override: Arc<std::sync::RwLock<Option<String>>>,
    /// Admin token for sensitive mutations (trust updates). None = no auth required.
    pub admin_token: Option<Arc<String>>,
    /// Models that don't support chat/completions (need Responses API).
    /// Populated on first 400 "unsupported" — avoids redundant round-trips.
    pub responses_only_models: Arc<std::sync::RwLock<std::collections::HashSet<String>>>,
}

impl AppState {
    pub async fn new(_config: &Config) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(32)
            .redirect(reqwest::redirect::Policy::none()) // Never follow redirects — return 3xx as-is
            .build()?;

        let config = Config::from_env()?;
        let budget =
            TokenBudgetTracker::new(config.token_budget_daily, config.token_budget_per_request);

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
            client: client.clone(),
            config: Arc::new(config),
            budget,
            sidecar: SidecarProxy::new(&client),
            blocklist,
            sandbox_name: Arc::new(sandbox_name),
            inbox: Arc::new(MeshInbox::new()),
            mesh_metrics: Arc::new(MeshMetrics::new()),
            model_override: Arc::new(std::sync::RwLock::new(None)),
            responses_only_models: Arc::new(std::sync::RwLock::new(std::collections::HashSet::new())),
            admin_token: std::fs::read_to_string("/run/secrets/admin-token")
                .or_else(|_| std::env::var("ADMIN_TOKEN"))
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| Arc::new(s.trim().to_string())),
        })
    }

    fn upstream_config(&self, sandbox_name: &str) -> UpstreamConfig {
        // For inference (chat completions, embeddings): prefer the dedicated OpenAI endpoint
        // (openai.azure.com) over the Foundry project endpoint (services.ai.azure.com).
        // Foundry project endpoint is used for agent/memory/knowledge APIs, not inference.
        let endpoint = self
            .config
            .azure_openai_endpoint
            .clone()
            .or_else(|| self.config.foundry_endpoint.clone())
            .unwrap_or_default();

        // Live model override takes priority over config default
        let deployment = self
            .model_override
            .read()
            .ok()
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
        .route("/v1/responses", post(responses))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/models", get(list_models))
        .route("/v1/deployments", get(list_deployments))
        // Image generation (gpt-image-1, DALL-E, etc.)
        .route(
            "/openai/deployments/{deployment}/images/generations",
            post(images_generations),
        )
}

/// Foundry Agent API routes — agents, threads, runs (for tools needing agent execution).
/// These are proxied to the Foundry project endpoint, authenticated via IMDS with ai.azure.com audience.
pub fn foundry_agent_routes() -> Router<AppState> {
    Router::new()
        .route("/agents", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/agents/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
}

/// Foundry standalone API routes — Memory Store, Foundry IQ (Knowledge), Evaluations.
/// These work without a hosted Foundry agent. Direct project-level APIs.
pub fn foundry_standalone_routes() -> Router<AppState> {
    Router::new()
        // Memory Store APIs (persistent long-term memory) — uses underscores per REST spec
        .route("/memory_stores", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/memory_stores/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        // Foundry IQ / Knowledge Base APIs (agentic retrieval)
        .route("/knowledgebases", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/knowledgebases/{*path}",
            get(foundry_proxy).post(foundry_proxy),
        )
        // Evaluations APIs
        .route("/evaluations", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/evaluations/{*path}",
            get(foundry_proxy).post(foundry_proxy),
        )
        // Evaluators APIs
        .route("/evaluators", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/evaluators/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        // Evaluation rules APIs
        .route("/evaluationrules", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/evaluationrules/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        // Indexes APIs
        .route("/indexes", get(foundry_proxy))
        .route(
            "/indexes/{*path}",
            get(foundry_proxy)
                .post(foundry_proxy)
                .delete(foundry_proxy)
                .patch(foundry_proxy),
        )
        // Connections APIs
        .route("/connections", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/connections/{*path}",
            get(foundry_proxy).post(foundry_proxy),
        )
        // Deployments APIs
        .route("/deployments", get(foundry_proxy))
        .route("/deployments/{*path}", get(foundry_proxy))
        // Datasets APIs
        .route("/datasets", get(foundry_proxy))
        .route(
            "/datasets/{*path}",
            get(foundry_proxy)
                .post(foundry_proxy)
                .delete(foundry_proxy)
                .patch(foundry_proxy),
        )
        // Insights APIs
        .route("/insights", get(foundry_proxy).post(foundry_proxy))
        .route("/insights/{*path}", get(foundry_proxy))
        // OpenAI Conversations + Responses + Evals + Vector Stores + Files
        .route(
            "/openai/conversations",
            get(foundry_proxy).post(foundry_proxy),
        )
        .route(
            "/openai/conversations/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        .route("/openai/responses", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/openai/responses/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        .route("/openai/evals", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/openai/evals/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        .route(
            "/openai/vector_stores",
            get(foundry_proxy).post(foundry_proxy),
        )
        .route(
            "/openai/vector_stores/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        .route("/openai/files", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/openai/files/{*path}",
            get(foundry_proxy).post(foundry_proxy).delete(foundry_proxy),
        )
        .route(
            "/openai/fine-tuning/{*path}",
            get(foundry_proxy).post(foundry_proxy),
        )
        // Red Teams APIs
        .route("/redTeams/runs", get(foundry_proxy).post(foundry_proxy))
        .route("/redTeams/runs/{*path}", get(foundry_proxy))
        // Schedules APIs
        .route("/schedules", get(foundry_proxy))
        .route(
            "/schedules/{*path}",
            get(foundry_proxy).put(foundry_proxy).delete(foundry_proxy),
        )
        // Evaluation Taxonomies APIs
        .route(
            "/evaluationtaxonomies",
            get(foundry_proxy).post(foundry_proxy),
        )
        .route(
            "/evaluationtaxonomies/{*path}",
            get(foundry_proxy)
                .put(foundry_proxy)
                .delete(foundry_proxy)
                .patch(foundry_proxy),
        )
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
    Router::new().route("/admin/model", get(admin_get_model).put(admin_set_model))
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
        .route(
            "/agt/registry/{*path}",
            get(agt_registry_proxy).post(agt_registry_proxy),
        )
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
        .filter(|v| {
            // Validate against K8s name rules: lowercase alphanumeric + hyphens, max 63 chars
            !v.is_empty()
                && v.len() <= 63
                && v.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && v.as_bytes()[0].is_ascii_alphanumeric()
        })
        .unwrap_or("unknown");

    // Foundry guardrails (DefaultV2) handle content safety and prompt shields
    // at inference time — no pre-flight calls needed. We parse the response
    // annotations after forwarding and report flags to AGT sidecar.

    // AGT policy check — evaluate inference action via sidecar (audit mode: log, don't block)
    if state.sidecar.enabled {
        let model = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("model")?.as_str().map(String::from))
            .unwrap_or_default();
        let eval_body = serde_json::json!({
            "action": format!("inference:chat_completions:{}", model),
            "context": { "sandbox": sandbox_name, "model": model }
        });
        match state
            .sidecar
            .forward("POST", "/evaluate", Some(&eval_body))
            .await
        {
            Ok((status, json)) if status == 403 => {
                let reason = json
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("policy denied");
                tracing::warn!(sandbox = %sandbox_name, %reason, "AGT policy DENIED inference (enforcing)");
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("Blocked by governance policy: {}", reason),
                            "type": "policy_violation",
                            "code": "policy_denied"
                        }
                    })),
                )
                    .into_response();
            }
            Ok((_, json)) => {
                let decision = json
                    .get("decision")
                    .and_then(|d| d.as_str())
                    .unwrap_or("allow");
                tracing::debug!(sandbox = %sandbox_name, %decision, "AGT policy evaluated inference");
            }
            Err(e) => {
                if state.sidecar.should_fail_closed() {
                    tracing::error!(sandbox = %sandbox_name, error = %e,
                        "AGT sidecar unreachable (fail-closed) — blocking inference");
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": {
                                "message": "Governance sidecar unavailable — inference blocked (fail-closed)",
                                "type": "governance_unavailable",
                                "code": "sidecar_unreachable"
                            }
                        })),
                    )
                        .into_response();
                }
                // Grace window — fail-open for first few failures (cold start)
                tracing::warn!(sandbox = %sandbox_name, error = %e,
                    "AGT sidecar unreachable, allowing request (grace window)");
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

    // Check if this model is known to require Responses API (cached from prior 400s)
    let model_name = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from))
        .unwrap_or_else(|| upstream.deployment.clone());
    let is_responses_only = state
        .responses_only_models
        .read()
        .ok()
        .map(|set| set.contains(&model_name))
        .unwrap_or(false);

    if is_responses_only {
        // Skip chat/completions — go directly to Responses API
        tracing::info!(sandbox = %sandbox_name, model = %model_name, "Using cached Responses API path");
        let responses_body = chat_to_responses_body(&body);
        match proxy::forward(
            &state.auth,
            &state.client,
            &upstream,
            axum::http::Method::POST,
            "responses",
            &headers,
            responses_body,
        )
        .await
        {
            Ok((resp_status, resp_hdrs, resp_body)) => {
                let chat_body = responses_to_chat_body(&resp_body);
                if let Ok(bj) = serde_json::from_slice::<serde_json::Value>(&chat_body)
                    && let Some(total) = bj.get("usage").and_then(|u| u.get("total_tokens")).and_then(|v| v.as_u64())
                {
                    state.budget.record_usage(sandbox_name, total).await;
                }
                let is_stream = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("stream")?.as_bool())
                    .unwrap_or(false);
                if is_stream {
                    // Wrap as SSE for streaming clients
                    let sse = format!("data: {}\n\ndata: [DONE]\n\n", String::from_utf8_lossy(&chat_body));
                    let mut response = (resp_status, Body::from(sse)).into_response();
                    response.headers_mut().insert("content-type", axum::http::HeaderValue::from_static("text/event-stream"));
                    return response;
                }
                let mut response = (resp_status, Body::from(chat_body)).into_response();
                if let Some(ct) = resp_hdrs.get("content-type") {
                    response.headers_mut().insert("content-type", ct.clone());
                }
                return response;
            }
            Err(e) => {
                tracing::error!(sandbox = %sandbox_name, "Responses API error: {e:#}");
                return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                    "error": {"message": "Failed to reach inference backend", "type": "proxy_error"}
                }))).into_response();
            }
        }
    }

    // Check if client requested streaming
    let is_stream = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream")?.as_bool())
        .unwrap_or(false);

    if is_stream {
        // SSE streaming — wrap stream to capture token usage from final [DONE] chunk
        let sandbox_owned = sandbox_name.to_string();
        let budget = state.budget.clone();
        match proxy::forward_stream(
            state.auth.clone(),
            state.client.clone(),
            upstream.clone(),
            "chat/completions",
            headers.clone(),
            body.clone(),
        )
        .await
        {
            Ok((status, _resp_headers, stream)) if status == StatusCode::BAD_REQUEST => {
                // Might be a Responses-only model — buffer the error and check
                use futures::TryStreamExt;
                let err_bytes: Vec<u8> = stream
                    .try_fold(Vec::new(), |mut acc, chunk| async move {
                        acc.extend_from_slice(&chunk);
                        Ok(acc)
                    })
                    .await
                    .unwrap_or_default();
                let is_unsupported = serde_json::from_slice::<serde_json::Value>(&err_bytes)
                    .ok()
                    .and_then(|v| v.get("error")?.get("message")?.as_str().map(|s| s.contains("unsupported")))
                    .unwrap_or(false);

                if is_unsupported {
                    // Cache this model as Responses-only to skip future chat/completions attempts
                    if let Ok(mut set) = state.responses_only_models.write() {
                        set.insert(model_name.clone());
                        tracing::info!(model = %model_name, "Cached as Responses-only model");
                    }
                    // Fallback: convert to Responses API, return result as single SSE frame
                    tracing::info!(sandbox = %sandbox_name, "Streaming chat/completions unsupported, falling back to Responses API");
                    let responses_body = chat_to_responses_body(&body);
                    match proxy::forward(
                        &state.auth,
                        &state.client,
                        &upstream,
                        axum::http::Method::POST,
                        "responses",
                        &headers,
                        responses_body,
                    )
                    .await
                    {
                        Ok((resp_status, _, resp_body)) => {
                            let chat_body = responses_to_chat_body(&resp_body);
                            if let Ok(bj) = serde_json::from_slice::<serde_json::Value>(&chat_body)
                                && let Some(total) = bj.get("usage").and_then(|u| u.get("total_tokens")).and_then(|v| v.as_u64())
                            {
                                state.budget.record_usage(sandbox_name, total).await;
                            }
                            // Wrap as SSE so the streaming client can parse it
                            let sse = format!(
                                "data: {}\n\ndata: [DONE]\n\n",
                                String::from_utf8_lossy(&chat_body)
                            );
                            let mut response = (resp_status, Body::from(sse)).into_response();
                            response.headers_mut().insert(
                                "content-type",
                                axum::http::HeaderValue::from_static("text/event-stream"),
                            );
                            response
                        }
                        Err(e) => {
                            tracing::error!(sandbox = %sandbox_name, "Responses fallback error: {e:#}");
                            (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                                "error": {"message": "Failed to reach inference backend", "type": "proxy_error"}
                            }))).into_response()
                        }
                    }
                } else {
                    // Genuine 400 error — return as-is
                    (StatusCode::BAD_REQUEST, Body::from(err_bytes)).into_response()
                }
            }
            Ok((status, resp_headers, stream)) => {
                tracing::info!(sandbox = %sandbox_owned, status = %status.as_u16(), "Stream response status");
                // Wrap stream to intercept the first SSE chunk for guardrail
                // annotations and the final chunk for token usage.
                let sidecar_for_stream = state.sidecar.clone();
                let sandbox_for_flags = sandbox_owned.clone();
                let checked_flags = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let wrapped = stream.map(move |chunk| {
                    if let Ok(ref bytes) = chunk {
                        if let Ok(text) = std::str::from_utf8(bytes) {
                            // Check first chunk for Foundry guardrail annotations
                            if !checked_flags.load(std::sync::atomic::Ordering::Relaxed)
                                && text.contains("prompt_filter_results")
                            {
                                checked_flags.store(true, std::sync::atomic::Ordering::Relaxed);
                                let flags = safety::parse_streaming_prompt_filter(text);
                                if flags.any_detected() {
                                    tracing::warn!(
                                        sandbox = %sandbox_for_flags,
                                        jailbreak = flags.jailbreak_detected,
                                        "Foundry guardrail flags in stream"
                                    );
                                    let sc = sidecar_for_stream.clone();
                                    let sb = sandbox_for_flags.clone();
                                    tokio::spawn(async move {
                                        safety::report_content_flags_to_agt(&sc, &sb, &flags).await;
                                    });
                                }
                            }

                            for line in text.lines() {
                                let data = line.strip_prefix("data: ").unwrap_or("");
                                if data.is_empty() || data == "[DONE]" {
                                    continue;
                                }
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                    if let Some(total) = v
                                        .get("usage")
                                        .and_then(|u| u.get("total_tokens"))
                                        .and_then(|t| t.as_u64())
                                    {
                                        let b = budget.clone();
                                        let s = sandbox_owned.clone();
                                        tokio::spawn(async move {
                                            b.record_usage(&s, total).await;
                                        });
                                    }
                                }
                            }
                        }
                    }
                    chunk
                });
                let body = Body::from_stream(wrapped);
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
            body.clone(),
        )
        .await;

        match result {
            Ok((status, _resp_headers, resp_body))
                if status == StatusCode::BAD_REQUEST
                    && serde_json::from_slice::<serde_json::Value>(&resp_body)
                        .ok()
                        .and_then(|v| {
                            v.get("error")?
                                .get("message")?
                                .as_str()
                                .map(|s| s.contains("unsupported"))
                        })
                        .unwrap_or(false) =>
            {
                // Model doesn't support chat/completions — auto-fallback to Responses API.
                // Cache this model to skip future chat/completions attempts.
                if let Ok(mut set) = state.responses_only_models.write() {
                    set.insert(model_name.clone());
                    tracing::info!(model = %model_name, "Cached as Responses-only model");
                }
                // Convert messages → input and proxy to /openai/v1/responses.
                tracing::info!(sandbox = %sandbox_name, "chat/completions unsupported, falling back to Responses API");
                let responses_body = chat_to_responses_body(&body);
                match proxy::forward(
                    &state.auth,
                    &state.client,
                    &upstream,
                    axum::http::Method::POST,
                    "responses",
                    &headers,
                    responses_body,
                )
                .await
                {
                    Ok((resp_status, resp_hdrs, resp_body)) => {
                        // Convert Responses API output back to chat/completions format
                        let chat_body = responses_to_chat_body(&resp_body);
                        if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&chat_body)
                            && let Some(total) = body_json
                                .get("usage")
                                .and_then(|u| u.get("total_tokens"))
                                .and_then(|v| v.as_u64())
                        {
                            state.budget.record_usage(sandbox_name, total).await;
                        }
                        let mut response = (resp_status, Body::from(chat_body)).into_response();
                        if let Some(ct) = resp_hdrs.get("content-type") {
                            response.headers_mut().insert("content-type", ct.clone());
                        }
                        response
                    }
                    Err(e) => {
                        tracing::error!(sandbox = %sandbox_name, "Responses fallback proxy error: {e:#}");
                        (StatusCode::BAD_GATEWAY, Json(serde_json::json!({
                            "error": {"message": "Failed to reach inference backend", "type": "proxy_error"}
                        }))).into_response()
                    }
                }
            }
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

                // Parse Foundry guardrail annotations and report to AGT sidecar.
                // On 200: prompt_filter_results at top level
                // On 400: error.innererror.content_filter_result
                if state.sidecar.enabled {
                    if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&resp_body) {
                        let flags = if status.is_success() {
                            safety::parse_prompt_filter_results(&body_json)
                        } else {
                            safety::parse_error_content_filter(&body_json)
                        };

                        if flags.any_detected() {
                            tracing::warn!(
                                sandbox = %sandbox_name,
                                jailbreak = flags.jailbreak_detected,
                                filtered = ?flags.filtered_categories,
                                detected = ?flags.detected_categories,
                                "Foundry guardrail flags detected"
                            );
                            let sidecar = state.sidecar.clone();
                            let sandbox = sandbox_name.to_string();
                            tokio::spawn(async move {
                                safety::report_content_flags_to_agt(&sidecar, &sandbox, &flags)
                                    .await;
                            });
                        }

                        // AGT output validation — fire-and-forget
                        let response_text = body_json
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("message"))
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !response_text.is_empty() {
                            let sidecar = state.sidecar.clone();
                            let sandbox = sandbox_name.to_string();
                            tokio::spawn(async move {
                                let eval_body = serde_json::json!({
                                    "action": format!("output:{}", response_text),
                                    "agent_id": sandbox,
                                    "context": {
                                        "response_length": response_text.len(),
                                    }
                                });
                                match sidecar.forward("POST", "/evaluate", Some(&eval_body)).await {
                                    Ok((s, json)) if s == 403 => {
                                        let reason = json
                                            .get("reason")
                                            .and_then(|r| r.as_str())
                                            .unwrap_or("output policy");
                                        tracing::warn!(sandbox = %sandbox, %reason,
                                            "AGT: model response flagged by output policy");
                                    }
                                    Ok(_) => {}
                                    Err(e) => {
                                        tracing::debug!(sandbox = %sandbox,
                                            "AGT output validation skipped: {e}");
                                    }
                                }
                            });
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

/// POST /v1/responses — Responses API for reasoning and Responses-only models.
/// Proxies to Azure OpenAI /openai/v1/responses.
async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let sandbox_name = headers
        .get("x-azureclaw-sandbox")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 63
                && v.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && v.as_bytes()[0].is_ascii_alphanumeric()
        })
        .unwrap_or("unknown");

    // AGT policy check
    if state.sidecar.enabled {
        let model = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("model")?.as_str().map(String::from))
            .unwrap_or_default();
        let eval_body = serde_json::json!({
            "action": format!("inference:responses:{}", model),
            "context": { "sandbox": sandbox_name, "model": model }
        });
        match state
            .sidecar
            .forward("POST", "/evaluate", Some(&eval_body))
            .await
        {
            Ok((status, json)) if status == 403 => {
                let reason = json
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("policy denied");
                tracing::warn!(sandbox = %sandbox_name, %reason, "AGT policy DENIED responses inference");
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("Blocked by governance policy: {}", reason),
                            "type": "policy_violation",
                            "code": "policy_denied"
                        }
                    })),
                )
                    .into_response();
            }
            Ok(_) => {}
            Err(e) => {
                if state.sidecar.should_fail_closed() {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": {
                                "message": "Governance sidecar unavailable — inference blocked (fail-closed)",
                                "type": "governance_unavailable"
                            }
                        })),
                    )
                        .into_response();
                }
                tracing::warn!(sandbox = %sandbox_name, error = %e,
                    "AGT sidecar unreachable, allowing responses request (grace window)");
            }
        }
    }

    // Budget check
    if let Err(msg) = state.budget.check_budget(sandbox_name).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": { "message": msg, "type": "token_budget_exceeded" } })),
        )
            .into_response();
    }

    let upstream = state.upstream_config(sandbox_name);
    tracing::info!(sandbox = %sandbox_name, model = %upstream.deployment, "Responses API request");

    match proxy::forward(
        &state.auth,
        &state.client,
        &upstream,
        axum::http::Method::POST,
        "responses",
        &headers,
        body,
    )
    .await
    {
        Ok((status, resp_headers, resp_body)) => {
            // Record token usage
            if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&resp_body)
                && let Some(total) = body_json
                    .get("usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|v| v.as_u64())
            {
                state.budget.record_usage(sandbox_name, total).await;
            }
            let mut response = (status, Body::from(resp_body)).into_response();
            if let Some(ct) = resp_headers.get("content-type") {
                response.headers_mut().insert("content-type", ct.clone());
            }
            response
        }
        Err(e) => {
            tracing::error!(sandbox = %sandbox_name, "Responses proxy error: {e:#}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "message": "Failed to reach inference backend", "type": "proxy_error" }
                })),
            )
                .into_response()
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
            let clean = model
                .trim_start_matches("azure/")
                .trim_start_matches("azure-openai/");
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

/// POST /openai/deployments/{deployment}/images/generations — image generation proxy.
/// Forwards to Azure OpenAI's images API (gpt-image-1, DALL-E, etc.)
async fn images_generations(
    State(state): State<AppState>,
    Path(deployment): Path<String>,
    headers: HeaderMap,
    _query: axum::extract::RawQuery,
    body: Bytes,
) -> impl IntoResponse {
    let sandbox_name = headers
        .get("x-azureclaw-sandbox")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("self");

    // AGT policy check — image generation is a tool invocation
    if state.sidecar.enabled {
        let action = format!("image_generation:{}", deployment);
        match state
            .sidecar
            .forward(
                "POST",
                "/evaluate",
                Some(&serde_json::json!({"action": action, "agent_id": sandbox_name})),
            )
            .await
        {
            Ok((_, decision)) if decision.get("allowed") == Some(&serde_json::Value::Bool(false)) => {
                let reason = decision
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("policy denied");
                tracing::warn!(sandbox = sandbox_name, deployment = %deployment, "Image generation denied: {}", reason);
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error": {"message": format!("Image generation denied by policy: {}", reason)}})),
                )
                    .into_response();
            }
            _ => {}
        }
    }

    let mut upstream = state.upstream_config(sandbox_name);
    upstream.deployment = deployment.clone();

    // Unified format: proxy builds {endpoint}/openai/v1/images/generations
    // Model (deployment) is injected into request body automatically
    let api_path = "images/generations";

    tracing::info!(
        sandbox = sandbox_name,
        deployment = %deployment,
        "Image generation request"
    );

    match proxy::forward(
        &state.auth,
        &state.client,
        &upstream,
        axum::http::Method::POST,
        &api_path,
        &headers,
        body,
    )
    .await
    {
        Ok((status, _, resp_body)) => {
            tracing::info!(
                sandbox = sandbox_name,
                deployment = %deployment,
                status = %status,
                "Image generation complete"
            );
            (status, Body::from(resp_body)).into_response()
        }
        Err(e) => {
            tracing::error!(deployment = %deployment, "Image generation proxy error: {e:#}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": {"message": format!("Image generation proxy error: {e}")}})),
            )
                .into_response()
        }
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    // Check that we can acquire a token (validates Workload Identity / IMDS setup)
    let audience = if state
        .config
        .foundry_endpoint
        .as_deref()
        .is_some_and(|ep| ep.contains("services.ai.azure.com") && ep.contains("/api/projects/"))
    {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    };
    match state.auth.get_token(audience).await {
        Ok(_) => {
            // Also check Content Safety endpoint if configured
            if let Some(ref cs_endpoint) = state.config.content_safety_endpoint
                && state.config.content_safety_enabled
            {
                let url = format!(
                    "{}/contentsafety/text:analyze?api-version=2024-09-01",
                    cs_endpoint.trim_end_matches('/')
                );
                let reachable = state
                    .client
                    .post(&url)
                    .timeout(std::time::Duration::from_secs(3))
                    .send()
                    .await
                    .is_ok();
                if !reachable {
                    tracing::warn!("Content Safety endpoint unreachable: {cs_endpoint}");
                    return (
                        StatusCode::OK,
                        "ok (content safety unreachable — failing open)",
                    )
                        .into_response();
                }
            }
            (StatusCode::OK, "ok").into_response()
        }
        Err(e) => {
            tracing::warn!("Readiness check failed: {e}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "not ready — token acquisition failed",
            )
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
    let current = state
        .model_override
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| state.config.default_model.clone());
    Json(serde_json::json!({ "model": current, "default": state.config.default_model }))
}

/// PUT /admin/model — switch model live (body: {"model": "gpt-5-mini"})
async fn admin_set_model(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from));

    match model {
        Some(m) => {
            let prev = state
                .model_override
                .read()
                .ok()
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
    let endpoint = state
        .config
        .azure_openai_endpoint
        .clone()
        .or_else(|| state.config.foundry_endpoint.clone())
        .unwrap_or_default();

    // Azure OpenAI uses /openai/models?api-version=... (NOT /openai/v1/models)
    let models_url = format!(
        "{}/openai/models?api-version=2024-10-21",
        endpoint.trim_end_matches('/')
    );

    // Get token — use correct audience for Foundry project vs legacy AOAI
    let audience =
        if endpoint.contains("services.ai.azure.com") && endpoint.contains("/api/projects/") {
            "https://ai.azure.com"
        } else {
            "https://cognitiveservices.azure.com"
        };
    let token = match state.auth.get_token(audience).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": {"message": format!("Token error: {e}"), "type": "auth_error"}
                })),
            )
                .into_response();
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
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = r.bytes().await.unwrap_or_default();
            (status, Body::from(body)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": {"message": format!("Failed to list models: {e}"), "type": "proxy_error"}
            })),
        )
            .into_response(),
    }
}

/// List deployed models (not the full catalog).
/// For Foundry/AI Services endpoints, /openai/deployments is available on the data plane.
/// For legacy Azure OpenAI, this may return 404 — callers should fall back to /v1/models.
async fn list_deployments(State(state): State<AppState>) -> impl IntoResponse {
    let endpoint = state
        .config
        .azure_openai_endpoint
        .clone()
        .or_else(|| state.config.foundry_endpoint.clone())
        .unwrap_or_default();

    let deployments_url = format!(
        "{}/openai/deployments?api-version=2024-10-21",
        endpoint.trim_end_matches('/')
    );

    let audience = if endpoint.contains("services.ai.azure.com") {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    };
    let token = match state.auth.get_token(audience).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": {"message": format!("Token error: {e}"), "type": "auth_error"}
                })),
            )
                .into_response();
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
    let endpoint = state
        .config
        .foundry_project_endpoint
        .clone()
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
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": {
                    "message": format!("Request blocked by threat intelligence: {reason}"),
                    "type": "blocklist_violation",
                    "domain": domain
                }
            })),
        )
            .into_response();
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

    let upstream_url = format!(
        "{}{}{}",
        endpoint.trim_end_matches('/'),
        upstream_path,
        upstream_query
    );

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
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": {"message": format!("Auth error: {e}"), "type": "auth_error"}
                })),
            )
                .into_response();
        }
    };

    // For Azure OpenAI endpoints with API key auth (dev mode), use api-key header
    let use_api_key_header = is_azure_openai && state.auth.is_api_key_mode();

    // Build upstream request — strip sandbox headers, inject auth
    let mut upstream_headers = HeaderMap::new();
    for (name, value) in headers.iter() {
        match name.as_str() {
            "authorization"
            | "api-key"
            | "x-api-key"
            | "host"
            | "connection"
            | "transfer-encoding"
            | "content-length"
            | "x-azureclaw-sandbox" => continue,
            _ => {
                upstream_headers.insert(name.clone(), value.clone());
            }
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
    upstream_headers
        .entry("content-type")
        .or_insert(axum::http::HeaderValue::from_static("application/json"));

    let resp = state
        .client
        .request(method, &upstream_url)
        .headers(upstream_headers)
        .body(body)
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
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
// When the sidecar is enabled, these proxy to localhost:8081.
// When disabled, they fall back to the local governance.rs engine.

/// POST /agt/evaluate — evaluate a tool action against loaded policy.
async fn agt_evaluate(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !state.sidecar.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "AGT governance sidecar not enabled"})),
        )
            .into_response();
    }
    match state
        .sidecar
        .forward("POST", "/evaluate", Some(&body))
        .await
    {
        Ok((status, json)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(json),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("AGT sidecar: {}", e)})),
        )
            .into_response(),
    }
}

/// GET /agt/trust — list all known agent trust states.
async fn agt_trust_list(State(state): State<AppState>) -> impl IntoResponse {
    if !state.sidecar.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "AGT governance sidecar not enabled"})),
        )
            .into_response();
    }
    match state.sidecar.forward("GET", "/trust", None).await {
        Ok((_, json)) => Json(json).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("AGT sidecar: {}", e)})),
        )
            .into_response(),
    }
}

/// GET /agt/trust/:agent_id — get trust state for a specific agent.
async fn agt_trust_get(
    State(state): State<AppState>,
    axum::extract::Path(agent_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if !state.sidecar.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "AGT governance sidecar not enabled"})),
        )
            .into_response();
    }
    let path = format!("/trust/{}", agent_id);
    match state.sidecar.forward("GET", &path, None).await {
        Ok((_, json)) => Json(json).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("AGT sidecar: {}", e)})),
        )
            .into_response(),
    }
}

/// GET /agt/audit — get the full audit log.
async fn agt_audit(State(state): State<AppState>) -> impl IntoResponse {
    if !state.sidecar.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "AGT governance sidecar not enabled"})),
        )
            .into_response();
    }
    match state.sidecar.forward("GET", "/audit", None).await {
        Ok((_, json)) => Json(json).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("AGT sidecar: {}", e)})),
        )
            .into_response(),
    }
}

/// GET /agt/audit/verify — verify hash-chain integrity.
async fn agt_audit_verify(State(state): State<AppState>) -> impl IntoResponse {
    if !state.sidecar.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "AGT governance sidecar not enabled"})),
        )
            .into_response();
    }
    match state.sidecar.forward("GET", "/audit/verify", None).await {
        Ok((_, json)) => Json(json).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("AGT sidecar: {}", e)})),
        )
            .into_response(),
    }
}

/// GET /agt/mesh/inbox — read received messages.
async fn agt_mesh_inbox(State(state): State<AppState>) -> impl IntoResponse {
    let messages = state.inbox.peek().await;
    Json(serde_json::json!({
        "messages": messages,
        "count": messages.len()
    }))
}

/// GET /agt/status — overall governance status.
async fn agt_status(State(state): State<AppState>) -> impl IntoResponse {
    let sidecar_status = if state.sidecar.enabled {
        match state.sidecar.forward("GET", "/status", None).await {
            Ok((_, json)) => Some(json),
            Err(_) => None,
        }
    } else {
        None
    };

    let inbox = state.inbox.peek().await;
    let blocklist_len = state.blocklist.len().await;

    let mut result = sidecar_status.unwrap_or_else(|| {
        serde_json::json!({
            "sidecar": false,
            "sandbox": *state.sandbox_name,
        })
    });

    if let Some(obj) = result.as_object_mut() {
        obj.insert("inbox_messages".into(), serde_json::json!(inbox.len()));
        obj.insert("blocklist_domains".into(), serde_json::json!(blocklist_len));
        obj.insert(
            "egress_learn_mode".into(),
            serde_json::json!(state.blocklist.is_learn_mode()),
        );
        obj.insert(
            "egress_learned_domains".into(),
            serde_json::json!(state.blocklist.learned_count().await),
        );
        obj.insert(
            "mesh_sessions".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .sessions
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
        obj.insert(
            "mesh_messages_sent".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .messages_sent
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
        obj.insert(
            "mesh_messages_received".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .messages_received
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
        if state.sidecar.enabled {
            obj.insert("sidecar".into(), serde_json::json!(true));
        }
        obj.insert(
            "trust_updates".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .trust_updates
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
    }

    Json(result).into_response()
}

/// POST /agt/trust — plugin pushes trust updates after mesh interactions.
/// Body: { "agent_id": "peer-name", "score": 510, "interactions": 1 }
async fn agt_trust_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !state.sidecar.enabled {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "AGT governance sidecar not enabled"})),
        );
    }

    // Trust mutations require admin token even from localhost — prevents sandbox
    // (UID 1000) from forging peer trust scores via the localhost auth exemption.
    if let Some(ref expected) = state.admin_token {
        let provided = headers
            .get("x-azureclaw-admin")
            .or_else(|| headers.get("authorization"))
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v));
        match provided {
            Some(tok) if tok == expected.as_str() => {}
            _ => {
                tracing::warn!("POST /agt/trust denied: missing or invalid admin token");
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error": "Admin token required for trust mutations"})),
                );
            }
        }
    }

    match state.sidecar.forward("POST", "/trust", Some(&body)).await {
        Ok((status, json)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(json),
        ),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("AGT sidecar: {}", e)})),
        ),
    }
}

/// GET /agt/reputation — fetch this agent's reputation from the AgentMesh registry.
/// Returns the registry-computed score (session history, peer feedback, tier bonus)
/// alongside the local trust store snapshot.
async fn agt_reputation(State(state): State<AppState>) -> impl IntoResponse {
    let registry_url = std::env::var("AGT_REGISTRY_URL")
        .unwrap_or_else(|_| "http://agentmesh-registry.agentmesh.svc.cluster.local:8080".into());

    let sandbox_name: &str = &state.sandbox_name;
    let base = registry_url.trim_end_matches('/');

    // Step 1: Look up our AMID by searching for our sandbox name as a capability.
    // The plugin registers with capabilities: ["azureclaw-agent", "task-execution", sandbox_name]
    let amid = match state
        .client
        .get(&format!(
            "{}/v1/registry/search?capability={}",
            base, sandbox_name
        ))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<serde_json::Value>().await.ok().and_then(|v| {
                // Pick the most recently seen agent matching our name
                v.get("results")?
                    .as_array()?
                    .iter()
                    .filter(|a| {
                        a.get("display_name").and_then(|n| n.as_str()) == Some(sandbox_name)
                    })
                    .max_by_key(|a| {
                        a.get("last_seen")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string()
                    })
                    .and_then(|a| a.get("amid").and_then(|v| v.as_str()).map(String::from))
            })
        }
        _ => None,
    };

    // Step 2: If we found our AMID, fetch reputation score
    let registry = if let Some(ref agent_amid) = amid {
        match state
            .client
            .get(&format!(
                "{}/v1/registry/reputation/score?amid={}",
                base, agent_amid
            ))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp.json::<serde_json::Value>().await.ok(),
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

    // Local trust from sidecar (if available)
    let local_trust = if state.sidecar.enabled {
        match state.sidecar.forward("GET", "/trust", None).await {
            Ok((_, json)) => json.get("agents").cloned().unwrap_or(serde_json::json!([])),
            Err(_) => serde_json::json!([]),
        }
    } else {
        serde_json::json!([])
    };

    Json(serde_json::json!({
        "amid": amid.as_deref().unwrap_or(sandbox_name),
        "sandbox": sandbox_name,
        "registry": registry,
        "local_trust": local_trust,
    }))
}

/// GET /agt/relay — WebSocket proxy to the self-hosted AgentMesh relay.
/// The plugin (UID 1000) can only reach localhost. The router (UID 1001) proxies
/// WebSocket connections to the relay at agentmesh-relay.agentmesh.svc.cluster.local:8765.
async fn agt_relay_proxy(ws: WebSocketUpgrade) -> impl IntoResponse {
    let relay_url = std::env::var("AGT_RELAY_URL")
        .unwrap_or_else(|_| "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765".into());

    ws.on_upgrade(move |client_socket| async move {
        relay_websocket_bridge(client_socket, &relay_url).await;
    })
}

/// Bidirectional WebSocket bridge: client ↔ relay.
async fn relay_websocket_bridge(mut client_socket: WebSocket, relay_url: &str) {
    use futures::sink::SinkExt;
    use futures::stream::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio_tungstenite::tungstenite;

    // Connect to the upstream relay with a 30-second timeout
    let upstream = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio_tungstenite::connect_async(relay_url),
    )
    .await
    {
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
                Message::Text(ref t) => (tungstenite::Message::Text(t.to_string().into()), t.len()),
                Message::Binary(ref b) => {
                    (tungstenite::Message::Binary(b.to_vec().into()), b.len())
                }
                Message::Ping(p) => {
                    let _ = upstream_tx
                        .send(tungstenite::Message::Ping(p.to_vec().into()))
                        .await;
                    continue;
                }
                Message::Pong(p) => {
                    let _ = upstream_tx
                        .send(tungstenite::Message::Pong(p.to_vec().into()))
                        .await;
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
            let hex_preview: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<String>>()
                .join(" ");
            let printable: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| {
                    if b.is_ascii_graphic() || *b == b' ' {
                        *b as char
                    } else {
                        '.'
                    }
                })
                .collect();
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
                tungstenite::Message::Text(ref t) => (Message::Text(t.to_string().into()), t.len()),
                tungstenite::Message::Binary(ref b) => {
                    (Message::Binary(b.to_vec().into()), b.len())
                }
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
            let hex_preview: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<String>>()
                .join(" ");
            let printable: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| {
                    if b.is_ascii_graphic() || *b == b' ' {
                        *b as char
                    } else {
                        '.'
                    }
                })
                .collect();
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

    // Allowlist valid registry API paths — prevent path traversal.
    // Paths arrive as the wildcard after /agt/registry/, e.g. "registry/search".
    let valid_prefixes = [
        "registry/", "lookup", "search", "register", "prekeys", "heartbeat",
        "agents", "health", "sessions",
    ];
    let path_valid = valid_prefixes
        .iter()
        .any(|prefix| path.starts_with(prefix))
        && !path.contains("..");
    if !path_valid {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid registry path"})),
        )
            .into_response();
    }

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
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = resp.bytes().await.unwrap_or_default();
            (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "AGT registry proxy failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("Registry unreachable: {}", e)
                })),
            )
                .into_response()
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
    let input = body
        .get("domain")
        .or_else(|| body.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if input.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Provide 'domain' or 'url' field"
            })),
        )
            .into_response();
    }

    match state.blocklist.is_blocked(input).await {
        crate::blocklist::BlockResult::Blocked { reason, domain } => (
            StatusCode::OK,
            Json(serde_json::json!({
                "blocked": true,
                "domain": domain,
                "reason": reason,
            })),
        )
            .into_response(),
        crate::blocklist::BlockResult::Allowed => (
            StatusCode::OK,
            Json(serde_json::json!({
                "blocked": false,
                "domain": input,
            })),
        )
            .into_response(),
    }
}

#[allow(dead_code)]
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
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
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
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
/// 5. Private/internal IP → always deny (SSRF protection)
/// 6. Redirects → returned as-is (never followed)
/// 7. Response capped at 2 MB
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
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Missing 'url' field"
            })),
        )
            .into_response();
    }

    // SSRF protection: reject requests to localhost/private IPs
    if let Ok(parsed) = reqwest::Url::parse(url) {
        if let Some(host) = parsed.host_str() {
            let is_private = match host.parse::<std::net::IpAddr>() {
                Ok(ip) => crate::forward_proxy::is_private_ip(&ip),
                Err(_) => {
                    // It's a hostname — check for common local hostnames
                    let h = host.to_lowercase();
                    h == "localhost" || h.ends_with(".local") || h.ends_with(".internal")
                }
            };
            if is_private {
                tracing::warn!(url = %url, "Egress fetch blocked: private/internal target");
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": "Cannot fetch from private/internal addresses",
                        "url": url,
                    })),
                )
                    .into_response();
            }
        }
    }

    let sandbox: &str = &state.sandbox_name;

    // Check egress access: blocklist → allowlist → pending
    if let Err(reason) = state.blocklist.check_egress(url, sandbox).await {
        tracing::warn!(url = %url, reason = %reason, "Egress fetch denied");
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": reason,
            "url": url,
            "action": "Run 'azureclaw egress <name> --pending' to see pending requests, then 'azureclaw egress <name> --approve <domain>' to allow.",
        }))).into_response();
    }

    // Record in learn mode
    state.blocklist.record_learned(url).await;

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

    // Allowlisted request headers — block dangerous ones
    const BLOCKED_REQ_HEADERS: &[&str] = &[
        "host",
        "transfer-encoding",
        "content-length",
        "proxy-authorization",
        "proxy-connection",
    ];
    if let Some(headers) = req_headers {
        for (k, v) in headers {
            let lower = k.to_lowercase();
            if BLOCKED_REQ_HEADERS.contains(&lower.as_str()) {
                continue;
            }
            if let Some(val) = v.as_str()
                && let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            {
                request = request.header(name, val);
            }
        }
    }

    if !req_body.is_empty() {
        request = request.body(req_body.to_string());
    }

    const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024; // 2 MB

    match request
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // Strip sensitive response headers
            const STRIPPED_RESP_HEADERS: &[&str] = &[
                "set-cookie",
                "authorization",
                "x-api-key",
                "x-auth-token",
                "proxy-authenticate",
                "proxy-authorization",
                "www-authenticate",
            ];
            let resp_headers: serde_json::Map<String, serde_json::Value> = resp
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    let name = k.as_str();
                    if STRIPPED_RESP_HEADERS.contains(&name) {
                        None
                    } else {
                        v.to_str().ok().map(|val| {
                            (name.to_string(), serde_json::Value::String(val.to_string()))
                        })
                    }
                })
                .collect();
            // Cap response body to prevent OOM
            let body_bytes = resp.bytes().await.unwrap_or_default();
            let body = if body_bytes.len() > MAX_RESPONSE_BYTES {
                let truncated = String::from_utf8_lossy(&body_bytes[..MAX_RESPONSE_BYTES]);
                format!(
                    "{}... [truncated at {} bytes]",
                    truncated, MAX_RESPONSE_BYTES
                )
            } else {
                String::from_utf8_lossy(&body_bytes).into_owned()
            };
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": status,
                    "headers": resp_headers,
                    "body": body,
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "Egress fetch failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("Request failed: {}", e),
                    "url": url,
                })),
            )
                .into_response()
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
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Missing 'domain' field"
            })),
        )
            .into_response();
    }
    state.blocklist.allow_domain(domain).await;
    tracing::info!(domain = %domain, "Egress domain approved");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "approved",
            "domain": domain,
        })),
    )
        .into_response()
}

/// POST /egress/deny — deny and remove a pending domain request.
/// Body: { "domain": "evil.example.com" }
async fn egress_deny(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let domain = body.get("domain").and_then(|v| v.as_str()).unwrap_or("");
    if domain.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Missing 'domain' field"
            })),
        )
            .into_response();
    }
    state.blocklist.deny_domain(domain).await;
    tracing::info!(domain = %domain, "Egress domain denied");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "denied",
            "domain": domain,
        })),
    )
        .into_response()
}

/// POST /egress/enforce — graduate from learn mode to enforcement.
/// Promotes all learned domains into the allowlist, disables learn mode,
/// and clears the learned set. After this, only allowlisted and non-blocklisted
/// domains pass through. New domains go to pending approval.
async fn egress_enforce(State(state): State<AppState>) -> impl IntoResponse {
    let learned = state.blocklist.get_learned_domains().await;
    if learned.is_empty() && !state.blocklist.is_learn_mode() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "already_enforcing",
                "learn_mode": false,
                "allowlist_count": state.blocklist.get_allowlist().await.len(),
            })),
        )
            .into_response();
    }

    // Promote each learned domain to the allowlist
    for domain in &learned {
        state.blocklist.allow_domain(domain).await;
    }

    // Disable learn mode and clear the learned set
    state.blocklist.set_learn_mode(false);
    state.blocklist.clear_learned().await;

    let allowlist = state.blocklist.get_allowlist().await;

    tracing::info!(
        promoted = learned.len(),
        total_allowlist = allowlist.len(),
        "Egress enforcement activated — learned domains promoted to allowlist"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "enforcing",
            "promoted": learned.len(),
            "allowlist_count": allowlist.len(),
            "allowlist": allowlist,
        })),
    )
        .into_response()
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
    State(state): State<AppState>,
    Json(req): Json<spawn::SpawnRequest>,
) -> impl IntoResponse {
    let parent_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());

    // AGT policy check — evaluate spawn action via sidecar
    if state.sidecar.enabled {
        let eval_body = serde_json::json!({
            "action": format!("spawn:create:{}", req.name),
            "context": {
                "parent": parent_name,
                "child": req.name,
                "model": req.model.as_deref().unwrap_or("default"),
            }
        });
        match state
            .sidecar
            .forward("POST", "/evaluate", Some(&eval_body))
            .await
        {
            Ok((status, json)) if status == 403 => {
                let reason = json
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("policy denied");
                tracing::warn!(parent = %parent_name, child = %req.name, %reason, "AGT policy DENIED spawn");
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "error": format!("Spawn blocked by policy: {}", reason) })),
                ).into_response();
            }
            Ok(_) => {}
            Err(e) => {
                if state.sidecar.should_fail_closed() {
                    tracing::error!(error = %e, "AGT sidecar unreachable (fail-closed) — blocking spawn");
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({ "error": "Governance sidecar unavailable — spawn blocked (fail-closed)" })),
                    ).into_response();
                }
                tracing::warn!(error = %e, "AGT sidecar unreachable, allowing spawn (grace window)");
            }
        }
    }

    match spawn::create_sandbox(&parent_name, &req).await {
        Ok(resp) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(resp).unwrap()),
        )
            .into_response(),
        Err(msg) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response(),
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
        )
            .into_response(),
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
        )
            .into_response(),
    }
}

// ── Chat ↔ Responses format translation ────────────────────────────────────

/// Convert a chat/completions request body to Responses API format.
/// messages[] → input, max_completion_tokens → max_output_tokens
fn chat_to_responses_body(chat_body: &Bytes) -> Bytes {
    let Ok(mut body) = serde_json::from_slice::<serde_json::Value>(chat_body) else {
        return chat_body.clone();
    };
    let obj = match body.as_object_mut() {
        Some(o) => o,
        None => return chat_body.clone(),
    };

    // Convert chat messages to Responses API input format.
    //
    // Chat format:
    //   {"role":"user","content":"text"}
    //   {"role":"assistant","content":null,"tool_calls":[{id,type,function:{name,arguments}}]}
    //   {"role":"tool","tool_call_id":"...","content":"result"}
    //
    // Responses API format:
    //   {"type":"message","role":"user","content":[{"type":"input_text","text":"..."}]}
    //   {"type":"function_call","name":"...","arguments":"...","call_id":"..."}
    //   {"type":"function_call_output","call_id":"...","output":"..."}
    if let Some(messages) = obj.remove("messages") {
        if let Some(msgs) = messages.as_array() {
            let mut converted: Vec<serde_json::Value> = Vec::new();
            for msg in msgs {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");

                match role {
                    "tool" => {
                        // Tool result → function_call_output
                        let call_id = msg.get("tool_call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let output = msg.get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        converted.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output
                        }));
                    }
                    "assistant" if msg.get("tool_calls").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false) => {
                        // Assistant with tool_calls → function_call items
                        // First emit any text content as a message
                        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                            if !content.is_empty() {
                                converted.push(serde_json::json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": content}]
                                }));
                            }
                        }
                        // Then emit each tool call as a function_call item
                        for tc in msg["tool_calls"].as_array().unwrap() {
                            let call_id = tc.get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = tc.get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = tc.get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}")
                                .to_string();
                            converted.push(serde_json::json!({
                                "type": "function_call",
                                "name": name,
                                "arguments": arguments,
                                "call_id": call_id
                            }));
                        }
                    }
                    _ => {
                        // Regular message (user/assistant/system/developer)
                        let resp_role = if role == "system" { "developer" } else { role };
                        let content = if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                            // Array content — convert type names
                            let items: Vec<serde_json::Value> = arr.iter().map(|item| {
                                let mut it = item.clone();
                                if let Some(t) = it.get("type").and_then(|t| t.as_str()).map(String::from) {
                                    let new_type = match (t.as_str(), role) {
                                        ("text", "assistant") => "output_text",
                                        ("text", _) => "input_text",
                                        ("image_url", _) => "input_image",
                                        ("refusal", _) => "refusal",
                                        _ => t.as_str(),
                                    };
                                    it.as_object_mut().unwrap().insert("type".into(), serde_json::json!(new_type));
                                    if new_type == "input_image" {
                                        if let Some(url_obj) = it.get("image_url").cloned() {
                                            let url = url_obj.get("url").cloned().unwrap_or(url_obj);
                                            let obj = it.as_object_mut().unwrap();
                                            obj.remove("image_url");
                                            obj.insert("image_url".into(), url);
                                        }
                                    }
                                }
                                it
                            }).collect();
                            serde_json::json!(items)
                        } else if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                            // String content — wrap in typed content block
                            let ct = if role == "assistant" { "output_text" } else { "input_text" };
                            serde_json::json!([{"type": ct, "text": text}])
                        } else {
                            serde_json::json!([])
                        };
                        converted.push(serde_json::json!({
                            "type": "message",
                            "role": resp_role,
                            "content": content
                        }));
                    }
                }
            }
            obj.insert("input".into(), serde_json::json!(converted));
        } else {
            obj.insert("input".into(), messages);
        }
    }

    // max_completion_tokens → max_output_tokens
    if let Some(max) = obj.remove("max_completion_tokens") {
        obj.insert("max_output_tokens".into(), max);
    }
    if let Some(max) = obj.remove("max_tokens") {
        obj.entry("max_output_tokens").or_insert(max);
    }

    // Convert tools format: chat uses {type, function:{name, parameters, ...}}
    // Responses API uses flattened {type, name, parameters, ...}
    if let Some(tools) = obj.remove("tools") {
        if let Some(tools_arr) = tools.as_array() {
            let converted_tools: Vec<serde_json::Value> = tools_arr
                .iter()
                .map(|tool| {
                    if let Some(func) = tool.get("function") {
                        let mut t = serde_json::json!({"type": "function"});
                        let t_obj = t.as_object_mut().unwrap();
                        if let Some(f_obj) = func.as_object() {
                            for (k, v) in f_obj {
                                t_obj.insert(k.clone(), v.clone());
                            }
                        }
                        t
                    } else {
                        tool.clone()
                    }
                })
                .collect();
            obj.insert("tools".into(), serde_json::json!(converted_tools));
        } else {
            obj.insert("tools".into(), tools);
        }
    }

    // Convert tool_choice format if present
    if let Some(tc) = obj.remove("tool_choice") {
        // Chat: {"type":"function","function":{"name":"foo"}}
        // Responses: {"type":"function","name":"foo"}
        if let Some(func) = tc.get("function") {
            if let Some(name) = func.get("name") {
                obj.insert("tool_choice".into(), serde_json::json!({
                    "type": "function",
                    "name": name
                }));
            }
        } else {
            // "auto", "none", "required" pass through unchanged
            obj.insert("tool_choice".into(), tc);
        }
    }

    // Remove chat-specific fields that Responses API doesn't accept
    obj.remove("stream");
    obj.remove("stop");
    obj.remove("frequency_penalty");
    obj.remove("presence_penalty");
    obj.remove("logprobs");
    obj.remove("top_logprobs");
    obj.remove("n");

    serde_json::to_vec(&body)
        .map(Bytes::from)
        .unwrap_or_else(|_| chat_body.clone())
}

/// Convert a Responses API response back to chat/completions format.
/// output[].content[].text → choices[].message.content
/// output[] function_call items → choices[].message.tool_calls
fn responses_to_chat_body(resp_body: &Bytes) -> Bytes {
    let Ok(resp) = serde_json::from_slice::<serde_json::Value>(resp_body) else {
        return resp_body.clone();
    };

    // If it's an error, pass through
    if resp.get("error").is_some() {
        return resp_body.clone();
    }

    // Extract text content and tool_calls from output
    let mut content = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();

    if let Some(items) = resp.get("output").and_then(|o| o.as_array()) {
        for item in items {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    if let Some(texts) = item.get("content").and_then(|c| c.as_array()) {
                        for c in texts {
                            if let Some(text) = c.get("text").and_then(|t| t.as_str()) {
                                content.push_str(text);
                            }
                        }
                    }
                }
                "function_call" => {
                    let call_id = item.get("call_id").or(item.get("id"))
                        .and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let arguments = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}").to_string();
                    tool_calls.push(serde_json::json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    // Build chat/completions-shaped response
    let usage = resp.get("usage").cloned().unwrap_or(serde_json::json!({}));
    let mut message = serde_json::json!({
        "role": "assistant",
    });
    let finish_reason;
    if !tool_calls.is_empty() {
        message["tool_calls"] = serde_json::json!(tool_calls);
        message["content"] = serde_json::Value::Null;
        if !content.is_empty() {
            message["content"] = serde_json::json!(content);
        }
        finish_reason = "tool_calls";
    } else {
        message["content"] = serde_json::json!(content);
        finish_reason = "stop";
    }

    let chat_resp = serde_json::json!({
        "id": resp.get("id").cloned().unwrap_or(serde_json::json!("")),
        "object": "chat.completion",
        "created": resp.get("created_at").cloned().unwrap_or(serde_json::json!(0)),
        "model": resp.get("model").cloned().unwrap_or(serde_json::json!("")),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
        "usage": usage
    });

    serde_json::to_vec(&chat_resp)
        .map(Bytes::from)
        .unwrap_or_else(|_| resp_body.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_chat_to_responses_simple_message() {
        let chat = serde_json::json!({
            "model": "gpt-5.4-pro",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_completion_tokens": 100,
            "stream": true
        });
        let body = Bytes::from(serde_json::to_vec(&chat).unwrap());
        let result = chat_to_responses_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        assert!(v.get("messages").is_none(), "messages should be removed");
        assert!(v.get("stream").is_none(), "stream should be removed");
        assert_eq!(v["max_output_tokens"], 100);

        let input = v["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn test_chat_to_responses_tool_calls() {
        let chat = serde_json::json!({
            "model": "gpt-5.4-pro",
            "messages": [
                {"role": "user", "content": "Search for cats"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {"name": "web_search", "arguments": "{\"q\":\"cats\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_123", "content": "Cats are great"},
                {"role": "assistant", "content": "Here's what I found about cats."}
            ],
            "tools": [{"type": "function", "function": {"name": "web_search", "parameters": {}}}]
        });
        let body = Bytes::from(serde_json::to_vec(&chat).unwrap());
        let result = chat_to_responses_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let input = v["input"].as_array().unwrap();
        assert_eq!(input.len(), 4);
        // User message
        assert_eq!(input[0]["type"], "message");
        // Function call
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["name"], "web_search");
        assert_eq!(input[1]["call_id"], "call_123");
        // Function call output
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_123");
        assert_eq!(input[2]["output"], "Cats are great");
        // Assistant response
        assert_eq!(input[3]["type"], "message");
        assert_eq!(input[3]["role"], "assistant");

        // Tools should be flattened
        let tools = v["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "web_search");
        assert!(tools[0].get("function").is_none());
    }

    #[test]
    fn test_responses_to_chat_with_tool_calls() {
        let resp = serde_json::json!({
            "id": "resp_123",
            "model": "gpt-5.4-pro",
            "created_at": 1234567890,
            "output": [
                {"type": "function_call", "call_id": "call_456", "name": "search", "arguments": "{\"q\":\"dogs\"}"}
            ],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let body = Bytes::from(serde_json::to_vec(&resp).unwrap());
        let result = responses_to_chat_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        let tc = &v["choices"][0]["message"]["tool_calls"];
        assert_eq!(tc[0]["id"], "call_456");
        assert_eq!(tc[0]["function"]["name"], "search");
    }

    #[test]
    fn test_chat_to_responses_system_to_developer() {
        let chat = serde_json::json!({
            "model": "gpt-5.4-pro",
            "messages": [
                {"role": "system", "content": "You are helpful"}
            ]
        });
        let body = Bytes::from(serde_json::to_vec(&chat).unwrap());
        let result = chat_to_responses_body(&body);
        let v: serde_json::Value = serde_json::from_slice(&result).unwrap();

        let input = v["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "developer");
    }
}
