// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Inference + Foundry proxy handlers.

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bytes::Bytes;

use super::AppState;
use crate::errors;
use crate::proxy;

use super::anthropic_messages::anthropic_messages;
use super::chat_completions::chat_completions;

/// Strip the `/api/projects/<project>` segment from a Foundry endpoint URL,
/// returning the bare account-scoped base URL.
///
/// Foundry chat-completions tolerates the project-scoped prefix; image
/// generation does not (returns 404). Use this for any route that must hit
/// the account-level base.
fn strip_project_prefix(endpoint: &str) -> &str {
    // Trim a trailing slash so callers don't end up with `https://x.com//openai/...`.
    let trimmed = endpoint.trim_end_matches('/');
    if let Some(idx) = trimmed.find("/api/projects/") {
        &trimmed[..idx]
    } else {
        trimmed
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
        // OpenAI-compatible endpoint: extracts model from body, defaults to gpt-image-1
        .route("/v1/images/generations", post(images_generations_v1))
        // Anthropic Messages API translation: Anthropic SDK -> Foundry chat
        .route("/anthropic/v1/messages", post(anthropic_messages))
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
        // Containers APIs — code_interpreter generated files (`cfile_...`) live
        // inside per-run containers and are retrieved via
        // GET /openai/containers/{container_id}/files/{file_id}/content.
        // Without this route, foundry_code_execute can never extract the
        // matplotlib PNGs / CSVs that its container produces.
        .route("/openai/containers", get(foundry_proxy).post(foundry_proxy))
        .route(
            "/openai/containers/{*path}",
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

    // AGT policy check via the four-seam PolicyDecisionProvider.
    {
        let model = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("model")?.as_str().map(String::from))
            .unwrap_or_default();
        let action = format!("inference:responses:{model}");
        if let super::inference_policy::InferenceDecision::Deny(reason) =
            super::inference_policy::check(&state, sandbox_name, &action).await
        {
            tracing::warn!(sandbox = %sandbox_name, %reason, "AGT policy DENIED responses inference");
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("Blocked by governance policy: {reason}"),
                        "type": "policy_violation",
                        "code": "policy_denied"
                    }
                })),
            )
                .into_response();
        }
    }

    // Budget check
    if let Err(msg) = state.budget.check_budget(sandbox_name).await {
        return errors::openai(
            StatusCode::TOO_MANY_REQUESTS,
            msg,
            errors::TOKEN_BUDGET_EXCEEDED,
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
            errors::openai(
                StatusCode::BAD_GATEWAY,
                "Failed to reach inference backend",
                errors::PROXY_ERROR,
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
    {
        let action = format!("image_generation:{deployment}");
        if let super::inference_policy::InferenceDecision::Deny(reason) =
            super::inference_policy::check(&state, sandbox_name, &action).await
        {
            tracing::warn!(sandbox = sandbox_name, deployment = %deployment, "Image generation denied: {}", reason);
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": {"message": format!("Image generation denied by policy: {reason}")}})),
            )
                .into_response();
        }
    }

    let mut upstream = state.upstream_config(sandbox_name);
    upstream.deployment = deployment.clone();

    // Foundry's /openai/v1/images/generations endpoint is account-scoped only —
    // it does NOT accept the /api/projects/<project>/ prefix that chat-completions
    // and most other v1 routes tolerate. Without this strip the upstream returns
    // a fast 404 and image generation silently degrades to written descriptions.
    upstream.endpoint = strip_project_prefix(&upstream.endpoint).to_string();

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
        api_path,
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

/// POST /v1/images/generations — OpenAI-compatible image generation endpoint.
/// Translates OpenAI-format requests to Azure OpenAI format: extracts `model` for the
/// deployment path and strips parameters Azure doesn't accept (response_format, model).
async fn images_generations_v1(
    state: State<AppState>,
    headers: HeaderMap,
    query: axum::extract::RawQuery,
    body: Bytes,
) -> impl IntoResponse {
    let mut parsed: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or_else(|_| serde_json::json!({}));
    let deployment = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("gpt-image-1")
        .to_string();
    // Strip params Azure doesn't accept — it uses deployment in URL (not body)
    // and returns b64_json by default (response_format is rejected as unknown)
    if let Some(obj) = parsed.as_object_mut() {
        obj.remove("model");
        obj.remove("response_format");
    }
    let patched_body = Bytes::from(serde_json::to_vec(&parsed).unwrap_or_default());

    images_generations(state, Path(deployment), headers, query, patched_body).await
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
            return errors::openai(
                StatusCode::BAD_GATEWAY,
                format!("Token error: {e}"),
                errors::AUTH_ERROR,
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
        Err(e) => errors::openai(
            StatusCode::BAD_GATEWAY,
            format!("Failed to list models: {e}"),
            errors::PROXY_ERROR,
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
            return errors::openai(
                StatusCode::BAD_GATEWAY,
                format!("Token error: {e}"),
                errors::AUTH_ERROR,
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
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = r.bytes().await.unwrap_or_default();
            (status, Body::from(body)).into_response()
        }
        Err(e) => errors::openai(
            StatusCode::BAD_GATEWAY,
            format!("Failed to list deployments: {e}"),
            errors::PROXY_ERROR,
        )
        .into_response(),
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

    // GitHub Models mode does not provide Foundry-only APIs (Memory Store,
    // agents, evaluations, indexes, knowledge bases, datasets, deployments,
    // connections, etc.). Return a clear 501 instead of letting the call
    // hit GitHub Models with a path it doesn't understand.
    if state.config.is_github_models() {
        tracing::info!(
            sandbox = %sandbox_name,
            path = %uri.path(),
            "Rejected Foundry-only route under GitHub Models mode"
        );
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "error": {
                    "message": format!(
                        "GitHub Models mode does not support `{}`. This endpoint requires Azure AI Foundry. Re-run `azureclaw dev` without --github-token (or set up Foundry) to enable Memory Stores, agents, evaluations, indexes, and other Foundry-only features.",
                        uri.path()
                    ),
                    "type": "unsupported_for_provider",
                    "provider": "github-models"
                }
            })),
        )
            .into_response();
    }

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

    // Derive a governance action from the Foundry API path.
    // e.g. /memory_stores/x:search_memories → foundry:memory:search_memories
    //      /agents/x/runs               → foundry:agents:runs
    //      /knowledgebases/x/queries     → foundry:file_search:queries
    let foundry_action = {
        let segments: Vec<&str> = upstream_path.trim_start_matches('/').split('/').collect();
        let category = match segments.first().copied().unwrap_or("") {
            s if s.starts_with("memory_stores") => "memory",
            s if s.starts_with("knowledgebases") => "file_search",
            "agents" => "agents",
            "evaluations" | "evaluators" | "evaluationrules" => "evaluations",
            "deployments" => "deployments",
            "connections" => "connections",
            "indexes" => "indexes",
            other => other,
        };
        // Extract the operation (last segment or :action suffix)
        let last_seg = segments.last().copied().unwrap_or("list");
        let detail = last_seg.rsplit(':').next().unwrap_or("list");
        format!("foundry:{category}:{detail}")
    };

    let result = state
        .governance
        .evaluate(sandbox_name, &foundry_action, None);
    let allowed = result
        .get("allowed")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !allowed {
        let reason = result
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("Denied by governance policy");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": {
                    "message": reason,
                    "type": "governance_denial",
                    "action": foundry_action
                }
            })),
        )
            .into_response();
    }

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
            return errors::openai(
                StatusCode::BAD_GATEWAY,
                format!("Auth error: {e}"),
                errors::AUTH_ERROR,
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
            errors::openai(
                StatusCode::BAD_GATEWAY,
                format!("Foundry Agent API error: {e}"),
                errors::PROXY_ERROR,
            )
            .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::strip_project_prefix;

    #[test]
    fn strips_foundry_project_prefix() {
        assert_eq!(
            strip_project_prefix(
                "https://azureclaw-foundry-services.services.ai.azure.com/api/projects/azureclaw"
            ),
            "https://azureclaw-foundry-services.services.ai.azure.com"
        );
    }

    #[test]
    fn strips_foundry_project_prefix_with_trailing_slash() {
        assert_eq!(
            strip_project_prefix("https://foo.services.ai.azure.com/api/projects/myproj/"),
            "https://foo.services.ai.azure.com"
        );
    }

    #[test]
    fn passes_through_account_endpoint() {
        // Pure Azure OpenAI account endpoint (dev mode) — no project prefix to strip.
        assert_eq!(
            strip_project_prefix("https://my-aoai.openai.azure.com"),
            "https://my-aoai.openai.azure.com"
        );
        assert_eq!(
            strip_project_prefix("https://my-aoai.openai.azure.com/"),
            "https://my-aoai.openai.azure.com"
        );
    }

    #[test]
    fn passes_through_when_project_segment_absent() {
        assert_eq!(
            strip_project_prefix("https://x.services.ai.azure.com"),
            "https://x.services.ai.azure.com"
        );
    }
}
