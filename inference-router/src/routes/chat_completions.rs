// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! POST /v1/chat/completions handler — extracted from
//! `routes/inference.rs` in S15.c (Phase 2 hotspot decomposition,
//! §4.2 file-budget enforcement).
//!
//! No behavior change — body moved verbatim from inference.rs and
//! visibility raised to `pub(super)` so `inference_routes()` can
//! register it.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::stream::StreamExt;

use super::AppState;
use super::inference_translate::{chat_to_responses_body, responses_to_chat_body};
use crate::errors;
use crate::proxy;
use crate::safety;

/// POST /v1/chat/completions — the primary inference endpoint.
pub(super) async fn chat_completions(
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

    // Slice 2c latency optimisation: take **one** snapshot of the
    // loaded `InferencePolicy` at the top of the handler and reuse
    // it for every downstream enforcement axis (daily/monthly tokens,
    // perRequestTokens cap, contentSafety floor — both buffered and
    // streaming branches). Previously the handler took three
    // independent `RwLock::read().await`s per request; this
    // consolidates them to one, removing two awaits from every
    // forwarded request on the hot path.
    let policy = crate::inference_policy_loader::current_snapshot(&state.inference_policy).await;

    // Check token budget before forwarding — daily/monthly limits
    // come from the loaded `InferencePolicy` (Slice 2b) with the
    // env-driven `TOKEN_BUDGET_DAILY` as fallback.
    if let Err(msg) = state
        .budget
        .check_budget(sandbox_name, policy.daily_tokens, policy.monthly_tokens)
        .await
    {
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

    // Slice 2a — `InferencePolicy.tokenBudget.perRequestTokens`
    // enforcement. Reject pre-forward when the client explicitly
    // asks for more output tokens than the policy permits. This is
    // a **defence-in-depth fast-fail**: it does not estimate prompt
    // tokens (that's deferred to 2c when contentSafety / token
    // counting backends land), it only catches the obvious case
    // where `max_tokens > policy.perRequestTokens`. The post-response
    // `record_usage` + warn-only check at the bottom of this handler
    // remains in place for the implicit `max_tokens=null` path
    // (router does not yet reject mid-stream).
    if let Some(cap) = policy.per_request_tokens
        && let Some(requested) = extract_requested_max_tokens(&body)
        && let PerRequestGate::Reject { requested, cap } =
            decide_per_request_gate(Some(cap), Some(requested))
    {
        tracing::warn!(
            sandbox = %sandbox_name,
            requested,
            cap,
            digest = %policy.digest,
            "InferencePolicy perRequestTokens exceeded — rejecting"
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {
                    "message": format!(
                        "Requested max_tokens={requested} exceeds InferencePolicy \
                         tokenBudget.perRequestTokens={cap}"
                    ),
                    "type": "token_budget_exceeded",
                    "code": "per_request_tokens_exceeded"
                }
            })),
        )
            .into_response();
    }

    // Forward to Foundry
    let mut upstream = state.upstream_config(sandbox_name);
    // Slice 2d.1: honour `InferencePolicy.modelPreference.primary.deployment`.
    crate::routes::apply_model_preference_override(&mut upstream, &policy);

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
            let copilot = state.copilot.clone();
            let client = state.client.clone();
            let upstream = upstream.clone();
            let headers = headers.clone();
            let budget = state.budget.clone();
            let sandbox_owned = sandbox_name.to_string();

            tokio::spawn(async move {
                // Send keepalive comments every 5 seconds while waiting
                let forward_fut = proxy::forward(
                    &auth,
                    Some(&copilot),
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
            Some(&state.copilot),
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
        // Slice 2c: clone the contentSafety floor out of the snapshot
        // taken at the top of the handler so the inner `.map` closure
        // stays sync (the `Bytes -> Result<Bytes,_>` map cannot
        // `.await` an `RwLock::read()`). The snapshot was already
        // taken once at the top of the handler — no extra lock here.
        // Snapshotting at the start of the call means a controller
        // mirror landing a new ConfigMap mid-stream is not honoured
        // for the in-flight request; acceptable since policies change
        // rarely vs. single-request lifetime.
        let stream_floor = policy.content_safety.clone();
        match proxy::forward_stream(
            state.auth.clone(),
            Some(state.copilot.clone()),
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
                        Some(&state.copilot),
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
                // Slice 2c: once a floor violation is detected in the
                // first chunk, all subsequent chunks are replaced
                // with empty bytes so the model's actual content
                // never reaches the client. The first chunk itself
                // is rewritten into an SSE `error` frame followed by
                // `data: [DONE]` so the streaming client sees a
                // structured failure rather than a silently truncated
                // stream.
                let stream_blocked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let floor_for_stream = stream_floor.clone();
                let wrapped = stream.map(move |chunk| {
                    use std::sync::atomic::Ordering;
                    if stream_blocked.load(Ordering::Relaxed) {
                        // A previous chunk tripped the floor — swallow
                        // every byte that follows.
                        return Ok::<Bytes, _>(Bytes::new());
                    }
                    if let Ok(ref bytes) = chunk {
                        if let Ok(text) = std::str::from_utf8(bytes) {
                            // Check first chunk for Foundry guardrail annotations
                            if !checked_flags.load(Ordering::Relaxed)
                                && text.contains("prompt_filter_results")
                            {
                                checked_flags.store(true, Ordering::Relaxed);
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
                                // Slice 2c floor enforcement on the
                                // first chunk: extract the embedded
                                // `data: { ... }` JSON and run the
                                // same `enforce_floor` used on the
                                // non-streaming path. Identical
                                // decision logic on both paths means
                                // attackers cannot probe streaming
                                // vs. buffered to bypass the floor.
                                if floor_for_stream.is_active()
                                    && let Some(violation) =
                                        safety::first_data_line_violation(text, &floor_for_stream)
                                {
                                    stream_blocked.store(true, Ordering::Relaxed);
                                    tracing::warn!(
                                        sandbox = %sandbox_for_flags,
                                        code = violation.code(),
                                        "InferencePolicy contentSafety floor (stream): {}",
                                        violation.message()
                                    );
                                    let sse_err = format!(
                                        "data: {}\n\ndata: [DONE]\n\n",
                                        serde_json::json!({
                                            "error": {
                                                "message": violation.message(),
                                                "type": "content_policy_violation",
                                                "code": violation.code()
                                            }
                                        })
                                    );
                                    return Ok::<Bytes, _>(Bytes::from(sse_err));
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
        // Buffered — extract token usage for budget tracking.
        // Slice 2d.2: route through the health-aware failover walker
        // so a 5xx/429 against `primary.deployment` transparently
        // retries against `fallback[N].deployment`. The 400-→-
        // Responses-API recovery further down still runs against the
        // *successful* upstream's deployment.
        let result = crate::failover::forward_with_failover(
            &state.auth,
            Some(&state.copilot),
            &state.client,
            &state.deployment_health,
            &upstream,
            &policy,
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
                    Some(&state.copilot),
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

                        // Slice 2c: InferencePolicy contentSafety floor.
                        // Compare every parsed severity against the
                        // policy ceiling and fail-closed when Prompt
                        // Shields are required but unannotated. The
                        // `is_active` short-circuit inside
                        // `enforce_floor` keeps the hot path free when
                        // no floor is configured. The 403 carries a
                        // distinct `code` so operators can tell
                        // InferencePolicy-blocked apart from
                        // Foundry-blocked (`content_filter`) in audit
                        // logs and client error handlers.
                        //
                        // Latency: the floor came from the single
                        // `current_snapshot()` read taken at the top
                        // of the handler — no extra lock acquisition
                        // here.
                        if let Some(violation) =
                            safety::enforce_floor(&body_json, &policy.content_safety)
                        {
                            tracing::warn!(
                                sandbox = %sandbox_name,
                                code = violation.code(),
                                "InferencePolicy contentSafety floor: {}",
                                violation.message()
                            );
                            return (
                                axum::http::StatusCode::FORBIDDEN,
                                Body::from(
                                    serde_json::json!({
                                        "error": {
                                            "message": violation.message(),
                                            "type": "content_policy_violation",
                                            "code": violation.code()
                                        }
                                    })
                                    .to_string(),
                                ),
                            )
                                .into_response();
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

/// Slice 2a — outcome of the per-request token cap gate. Returning an
/// enum (rather than `bool`) keeps the call site explicit about the
/// rejection path and makes the unit tests symmetric across the
/// allow / reject branches.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PerRequestGate {
    /// Either no policy is loaded, the policy doesn't set a cap, or
    /// the client did not specify `max_tokens` / `max_completion_tokens`
    /// — let the request through. The post-response budget tracker
    /// still observes usage.
    Allow,
    /// Client explicitly requested more output tokens than the policy
    /// allows. Both values surface in the 429 body so the caller can
    /// adjust without inspecting CRDs.
    Reject { requested: u64, cap: u64 },
}

/// Pure decision used by `chat_completions` to gate
/// `tokenBudget.perRequestTokens`. Kept free of `AppState` so unit
/// tests can exhaust the truth table without spinning up a full
/// router fixture.
pub(super) fn decide_per_request_gate(cap: Option<u64>, requested: Option<u64>) -> PerRequestGate {
    match (cap, requested) {
        (Some(c), Some(r)) if r > c => PerRequestGate::Reject {
            requested: r,
            cap: c,
        },
        _ => PerRequestGate::Allow,
    }
}

/// Extract the requested completion-tokens budget from the chat
/// completions request body. Both `max_tokens` (legacy) and
/// `max_completion_tokens` (o-series models) are honoured —
/// `max_completion_tokens` wins when both are set, matching upstream
/// OpenAI semantics. Returns `None` when the body is unparseable or
/// neither field is present (in which case the gate defaults to
/// Allow — pre-flight cannot estimate prompt tokens in Slice 2a).
pub(super) fn extract_requested_max_tokens(body: &[u8]) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("max_completion_tokens")
        .and_then(|x| x.as_u64())
        .or_else(|| v.get("max_tokens").and_then(|x| x.as_u64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_allow_when_no_policy_cap() {
        assert_eq!(
            decide_per_request_gate(None, Some(10_000)),
            PerRequestGate::Allow
        );
    }

    #[test]
    fn gate_allow_when_request_omits_max_tokens() {
        assert_eq!(
            decide_per_request_gate(Some(4_096), None),
            PerRequestGate::Allow
        );
    }

    #[test]
    fn gate_allow_when_requested_under_cap() {
        assert_eq!(
            decide_per_request_gate(Some(4_096), Some(4_096)),
            PerRequestGate::Allow
        );
        assert_eq!(
            decide_per_request_gate(Some(4_096), Some(1_000)),
            PerRequestGate::Allow
        );
    }

    #[test]
    fn gate_reject_when_requested_over_cap() {
        assert_eq!(
            decide_per_request_gate(Some(4_096), Some(4_097)),
            PerRequestGate::Reject {
                requested: 4_097,
                cap: 4_096,
            }
        );
        assert_eq!(
            decide_per_request_gate(Some(100), Some(1_000_000)),
            PerRequestGate::Reject {
                requested: 1_000_000,
                cap: 100,
            }
        );
    }

    #[test]
    fn extract_max_tokens_prefers_max_completion_tokens() {
        // OpenAI o-series semantics: max_completion_tokens supersedes
        // max_tokens. If both are present, the newer field wins so we
        // do not under-gate.
        let body = br#"{"max_tokens": 100, "max_completion_tokens": 9999}"#;
        assert_eq!(extract_requested_max_tokens(body), Some(9_999));
    }

    #[test]
    fn extract_max_tokens_falls_back_to_legacy_field() {
        let body = br#"{"max_tokens": 2048}"#;
        assert_eq!(extract_requested_max_tokens(body), Some(2_048));
    }

    #[test]
    fn extract_max_tokens_handles_absent_field() {
        let body = br#"{"messages": []}"#;
        assert_eq!(extract_requested_max_tokens(body), None);
    }

    #[test]
    fn extract_max_tokens_handles_unparseable_body() {
        assert_eq!(extract_requested_max_tokens(b"not json"), None);
    }

    #[test]
    fn extract_max_tokens_ignores_non_integer_values() {
        // Defensive: a stringified or float value should not crash.
        let body = br#"{"max_tokens": "1024"}"#;
        assert_eq!(extract_requested_max_tokens(body), None);
        let body2 = br#"{"max_tokens": 1024.5}"#;
        // serde_json::Value::as_u64 returns None for non-integer
        // numbers, so we fall through to Allow.
        assert_eq!(extract_requested_max_tokens(body2), None);
    }
}
