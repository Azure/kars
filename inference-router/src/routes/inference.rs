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
use super::inference_translate::{chat_to_responses_body, responses_to_chat_body};
use crate::errors;
use crate::proxy;
use crate::safety;
use futures::stream::StreamExt;
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
    // annotations after forwarding and report flags to AGT governance.

    // AGT policy check via the four-seam PolicyDecisionProvider.
    {
        let model = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("model")?.as_str().map(String::from))
            .unwrap_or_default();
        let action = format!("inference:chat_completions:{model}");
        if let super::inference_policy::InferenceDecision::Deny(reason) =
            super::inference_policy::check(&state, sandbox_name, &action).await
        {
            tracing::warn!(sandbox = %sandbox_name, %reason, "AGT policy DENIED inference (enforcing)");
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
        // Skip chat/completions — go directly to Responses API.
        // Use a streaming response body with SSE keepalive comments to prevent
        // client timeouts while the Responses API processes (30-50s for reasoning models).
        tracing::info!(sandbox = %sandbox_name, model = %model_name, "Using cached Responses API path");
        let responses_body = chat_to_responses_body(&body);

        let is_stream = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("stream")?.as_bool())
            .unwrap_or(false);

        if is_stream {
            // Stream keepalive comments while the Responses API call is in progress,
            // then send the converted result as a single SSE data frame.
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(16);
            let auth = state.auth.clone();
            let client = state.client.clone();
            let upstream = upstream.clone();
            let headers = headers.clone();
            let budget = state.budget.clone();
            let sandbox_owned = sandbox_name.to_string();

            tokio::spawn(async move {
                // Send keepalive comments every 5 seconds while waiting
                let forward_fut = proxy::forward(
                    &auth,
                    &client,
                    &upstream,
                    axum::http::Method::POST,
                    "responses",
                    &headers,
                    responses_body,
                );

                let keepalive_interval = tokio::time::interval(std::time::Duration::from_secs(5));
                tokio::pin!(forward_fut);
                tokio::pin!(keepalive_interval);

                let result = loop {
                    tokio::select! {
                        res = &mut forward_fut => { break res; }
                        _ = keepalive_interval.tick() => {
                            // SSE comment — keeps connection alive, ignored by SSE parsers
                            let _ = tx.send(Ok(bytes::Bytes::from(": keepalive\n\n"))).await;
                        }
                    }
                };

                match result {
                    Ok((_resp_status, _, resp_body)) => {
                        let chat_body = responses_to_chat_body(&resp_body);
                        if let Ok(bj) = serde_json::from_slice::<serde_json::Value>(&chat_body)
                            && let Some(total) = bj
                                .get("usage")
                                .and_then(|u| u.get("total_tokens"))
                                .and_then(|v| v.as_u64())
                        {
                            budget.record_usage(&sandbox_owned, total).await;
                        }
                        let sse_data = format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            String::from_utf8_lossy(&chat_body)
                        );
                        let _ = tx.send(Ok(bytes::Bytes::from(sse_data))).await;
                    }
                    Err(e) => {
                        tracing::error!(sandbox = %sandbox_owned, "Responses API error: {e:#}");
                        let err_sse = format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            serde_json::json!({"error":{"message":"Failed to reach inference backend","type":"proxy_error"}})
                        );
                        let _ = tx.send(Ok(bytes::Bytes::from(err_sse))).await;
                    }
                }
            });

            let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
            let body = Body::from_stream(stream);
            let mut response = (StatusCode::OK, body).into_response();
            response.headers_mut().insert(
                "content-type",
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            response.headers_mut().insert(
                "cache-control",
                axum::http::HeaderValue::from_static("no-cache"),
            );
            return response;
        }

        // Non-streaming: buffered request/response (no timeout concern)
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
                    && let Some(total) = bj
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
                return response;
            }
            Err(e) => {
                tracing::error!(sandbox = %sandbox_name, "Responses API error: {e:#}");
                return errors::openai(
                    StatusCode::BAD_GATEWAY,
                    "Failed to reach inference backend",
                    errors::PROXY_ERROR,
                )
                .into_response();
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
                    .and_then(|v| {
                        v.get("error")?
                            .get("message")?
                            .as_str()
                            .map(|s| s.contains("unsupported"))
                    })
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
                                && let Some(total) = bj
                                    .get("usage")
                                    .and_then(|u| u.get("total_tokens"))
                                    .and_then(|v| v.as_u64())
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
                            errors::openai(
                                StatusCode::BAD_GATEWAY,
                                "Failed to reach inference backend",
                                errors::PROXY_ERROR,
                            )
                            .into_response()
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
                let governance_for_stream = state.governance.clone();
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
                                    let gov = governance_for_stream.clone();
                                    let sb = sandbox_for_flags.clone();
                                    tokio::spawn(async move {
                                        safety::report_content_flags_to_agt(&gov, &sb, &flags)
                                            .await;
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
                errors::openai(
                    StatusCode::BAD_GATEWAY,
                    "Failed to reach inference backend",
                    errors::PROXY_ERROR,
                )
                .into_response()
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
                        if let Ok(body_json) =
                            serde_json::from_slice::<serde_json::Value>(&chat_body)
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
                        errors::openai(
                            StatusCode::BAD_GATEWAY,
                            "Failed to reach inference backend",
                            errors::PROXY_ERROR,
                        )
                        .into_response()
                    }
                }
            }
            Ok((status, resp_headers, mut resp_body)) => {
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

                // Parse Foundry guardrail annotations and report to AGT governance.
                // On 200: prompt_filter_results at top level
                // On 400: error.innererror.content_filter_result
                {
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
                            let gov = state.governance.clone();
                            let sandbox = sandbox_name.to_string();
                            tokio::spawn(async move {
                                safety::report_content_flags_to_agt(&gov, &sandbox, &flags).await;
                            });
                        }

                        // AGT output pipeline: redact → scan → policy check (blocking)
                        let response_text = body_json
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("message"))
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !response_text.is_empty() {
                            // 1. Redact credentials (AGT CredentialRedactor)
                            let redacted = state.governance.redact_text(&response_text);

                            // 2. Scan for threats (AGT McpResponseScanner)
                            let (sanitized, _threats_found) =
                                state.governance.scan_response(&redacted);

                            // 3. Policy check — blocking (was fire-and-forget)
                            let action =
                                format!("output:{}", &sanitized[..sanitized.len().min(200)]);
                            if let super::inference_policy::InferenceDecision::Deny(reason) =
                                super::inference_policy::check(&state, sandbox_name, &action).await
                            {
                                tracing::warn!(sandbox = %sandbox_name, %reason,
                                    "AGT: model response blocked by output policy");
                                return (
                                    axum::http::StatusCode::FORBIDDEN,
                                    Body::from(
                                        serde_json::json!({
                                            "error": {
                                                "message": "Response blocked by output policy",
                                                "type": "content_policy_violation",
                                                "code": "content_filter"
                                            }
                                        })
                                        .to_string(),
                                    ),
                                )
                                    .into_response();
                            }

                            // 4. Rewrite body if redaction/scanning modified the text
                            if sanitized != response_text {
                                if let Ok(mut body_mut) =
                                    serde_json::from_slice::<serde_json::Value>(&resp_body)
                                {
                                    if let Some(content) = body_mut
                                        .get_mut("choices")
                                        .and_then(|c| c.get_mut(0))
                                        .and_then(|c| c.get_mut("message"))
                                        .and_then(|m| m.get_mut("content"))
                                    {
                                        *content = serde_json::Value::String(sanitized);
                                        if let Ok(new_body) = serde_json::to_vec(&body_mut) {
                                            resp_body = new_body.into();
                                        }
                                    }
                                }
                            }
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
