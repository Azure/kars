// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! POST /anthropic/v1/messages — Anthropic Messages API translation route.
//!
//! Anthropic SDK clients in sandboxes can target the router's
//! `/anthropic/v1/messages` endpoint and get a native-shaped Messages
//! response. Internally we translate to OpenAI chat completions and call
//! Foundry the same way `/v1/chat/completions` does. When Foundry exposes
//! a native Anthropic endpoint we can switch this to passthrough without
//! breaking existing sandbox code.
//!
//! Scope (v1): non-streaming, text + system prompt, basic stop_sequences,
//! usage + finish_reason mapping. Tool use, image content and streaming
//! are intentionally deferred — they fall through to Foundry as best-effort
//! pass-through fields and most callers will get a clean text reply.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures::stream::StreamExt;
use serde_json::{Value, json};

use super::AppState;
use crate::guardrails::{self, Direction, GuardrailError, GuardrailPipeline};
use crate::provider::{ProviderError, ProviderKind};
use crate::proxy;
use std::sync::Arc;

/// Framing / hop-by-hop headers that must not be copied from an
/// upstream response onto a rebuilt one — hyper re-frames the body
/// itself, and a stale `transfer-encoding: chunked` (Anthropic over
/// HTTP/1.1) makes it abort the connection without a response.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "content-length"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn deny_response(status: StatusCode, message: &str, code: &str) -> axum::response::Response {
    // Anthropic wire shape (`error.type`) plus an explicit `error.code`
    // mirroring the OpenAI routes, so a client can switch on one stable
    // `error.code` across every inference route. For non-policy errors
    // (bad request, rate limit) `type` and `code` coincide.
    deny_coded(status, message, code, code, false)
}

/// Policy denial (provider/guardrail): keeps the Anthropic-native
/// `error.type` but carries the stable machine code in `error.code`
/// and attaches the `x-kars-decision*` headers, matching the
/// chat-completions contract. Distinct `error_type` and `code` are the
/// point: a client switches on `error.code` (e.g. `guardrail_blocked`
/// vs `guardrail_misconfigured` vs `guardrail_unavailable`) while
/// `error.type` stays the Anthropic-shaped value clients already read.
fn deny_policy(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
) -> axum::response::Response {
    deny_coded(status, message, error_type, code, true)
}

fn deny_coded(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
    decision_headers: bool,
) -> axum::response::Response {
    let mut resp = (
        status,
        Json(json!({
            "type": "error",
            "error": {
                "type": error_type,
                "code": code,
                "message": message,
            }
        })),
    )
        .into_response();
    if decision_headers {
        super::chat_completions::insert_decision_headers(
            &mut resp,
            "blocked",
            "InferencePolicy",
            message,
        );
    }
    resp
}

/// HTTP status for a guardrail pipeline error: 503 misconfigured
/// (declared stage cannot be built), 502 backend outage.
fn guardrail_error_status(e: &GuardrailError) -> StatusCode {
    match e {
        GuardrailError::Config { .. } => StatusCode::SERVICE_UNAVAILABLE,
        GuardrailError::Unavailable { .. } => StatusCode::BAD_GATEWAY,
    }
}

/// Stable machine code for a provider-resolution failure, matching
/// `chat_completions::provider_error_response`.
fn provider_error_code(e: &ProviderError) -> &'static str {
    match e {
        ProviderError::Unimplemented { .. } => "provider_unimplemented",
        _ => "provider_unconfigured",
    }
}

/// Convert Anthropic Messages-shaped JSON to OpenAI chat-completions-shaped JSON.
///
/// Best-effort: tolerates missing fields, returns the same body unchanged on
/// JSON parse failure.
pub(super) fn anthropic_to_openai(req: &Value) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    // Anthropic `system` -> OpenAI prepended system message
    match req.get("system") {
        Some(Value::String(s)) if !s.is_empty() => {
            messages.push(json!({ "role": "system", "content": s }));
        }
        Some(Value::Array(parts)) => {
            let txt: String = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            if !txt.is_empty() {
                messages.push(json!({ "role": "system", "content": txt }));
            }
        }
        _ => {}
    }

    // Anthropic `messages[]`: each item has role + content (string or list of parts).
    if let Some(arr) = req.get("messages").and_then(|v| v.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content_val = m.get("content").cloned().unwrap_or(Value::Null);
            let text = match content_val {
                Value::String(s) => s,
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| {
                        match p.get("type").and_then(|t| t.as_str()) {
                            Some("text") => p
                                .get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_string()),
                            Some("tool_result") => p
                                .get("content")
                                .and_then(|c| c.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| p.get("content").map(|c| c.to_string())),
                            // tool_use / image / etc dropped in v1 — callers wanting
                            // them should target /v1/chat/completions directly.
                            _ => None,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            messages.push(json!({ "role": role, "content": text }));
        }
    }

    let mut out = json!({ "messages": messages });

    // Pass-through-with-rename common fields.
    if let Some(v) = req.get("model") {
        out["model"] = v.clone();
    }
    if let Some(v) = req.get("max_tokens") {
        out["max_tokens"] = v.clone();
    }
    if let Some(v) = req.get("temperature") {
        out["temperature"] = v.clone();
    }
    if let Some(v) = req.get("top_p") {
        out["top_p"] = v.clone();
    }
    // Anthropic `stop_sequences` -> OpenAI `stop`
    if let Some(v) = req.get("stop_sequences") {
        out["stop"] = v.clone();
    }
    // Anthropic always wants stream off in v1 of this route.
    out["stream"] = json!(false);

    out
}

/// Convert an OpenAI chat-completion response back to Anthropic Messages shape.
pub(super) fn openai_to_anthropic(resp: &Value, requested_model: &str) -> Value {
    let choice0 = resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or(Value::Null);

    let text = choice0
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let finish = choice0
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("stop");
    let stop_reason = match finish {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "content_filter" => "end_turn",
        "tool_calls" => "tool_use",
        other => other,
    };

    let id = resp
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("msg_unknown")
        .to_string();

    let model = resp
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or(requested_model)
        .to_string();

    let prompt_tokens = resp
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .cloned()
        .unwrap_or(json!(0));
    let completion_tokens = resp
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .cloned()
        .unwrap_or(json!(0));

    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{ "type": "text", "text": text }],
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": prompt_tokens,
            "output_tokens": completion_tokens,
        }
    })
}

/// POST /anthropic/v1/messages — Anthropic-shape inference, internally
/// translated to Foundry chat completions.
pub(super) async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let sandbox_name: String = headers
        .get("x-kars-sandbox")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 63
                && v.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && v.as_bytes()[0].is_ascii_alphanumeric()
        })
        .unwrap_or("unknown")
        .to_string();
    let sandbox_name = sandbox_name.as_str();

    let req_json: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return deny_response(
                StatusCode::BAD_REQUEST,
                &format!("Invalid JSON body: {e}"),
                "invalid_request_error",
            );
        }
    };

    // Governance gate (same hook as /v1/chat/completions).
    {
        let model = req_json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let action = format!("inference:anthropic_messages:{model}");
        if let super::inference_policy::InferenceDecision::Deny(reason) =
            super::inference_policy::check(&state, sandbox_name, &action).await
        {
            tracing::warn!(sandbox = %sandbox_name, %reason, "AGT policy DENIED inference (anthropic)");
            return deny_response(
                StatusCode::FORBIDDEN,
                &format!("Blocked by governance policy: {reason}"),
                "permission_error",
            );
        }
    }

    // Token budget check — daily/monthly limits sourced from the
    // loaded `InferencePolicy` (Slice 2b); env-default fallback.
    // Latency: one snapshot read per request (mirrors chat_completions).
    let policy = crate::inference_policy_loader::current_snapshot(&state.inference_policy).await;
    if let Err(msg) = state
        .budget
        .check_budget(sandbox_name, policy.daily_tokens, policy.monthly_tokens)
        .await
    {
        tracing::warn!(sandbox = %sandbox_name, "Token budget exceeded (anthropic): {msg}");
        return deny_response(StatusCode::TOO_MANY_REQUESTS, &msg, "rate_limit_error");
    }

    let requested_model = req_json
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut upstream = state.upstream_config(sandbox_name);
    // Slice 2d.1: honour `InferencePolicy.modelPreference.primary.deployment`.
    crate::routes::apply_model_preference_override(&mut upstream, &policy);

    // Retarget at the policy-selected provider (fails closed).
    if let Err(e) = crate::routes::apply_provider_resolution(&state, &mut upstream, &policy) {
        tracing::warn!(
            target: "inference.audit",
            sandbox = %sandbox_name,
            inference_policy_digest = %policy.digest,
            decision = "deny",
            gate = "provider_resolution",
            error = %e,
            "InferencePolicy provider could not be resolved (anthropic route)"
        );
        let status = match e {
            ProviderError::Unimplemented { .. } => StatusCode::NOT_IMPLEMENTED,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        return deny_policy(status, &e.to_string(), "api_error", provider_error_code(&e));
    }

    // Guardrail pipeline; a declared-but-unbuildable stage blocks.
    let guardrail_pipeline =
        match super::chat_completions::build_guardrail_pipeline(&state, &policy) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    target: "inference.audit",
                    sandbox = %sandbox_name,
                    inference_policy_digest = %policy.digest,
                    decision = "deny",
                    gate = "guardrail_config",
                    error = %e,
                    "guardrail pipeline could not be built (anthropic route) — failing closed"
                );
                return deny_policy(
                    guardrail_error_status(&e),
                    &e.to_string(),
                    "api_error",
                    e.code(),
                );
            }
        };
    if let Some(ref p) = guardrail_pipeline
        && p.covers(Direction::Input)
    {
        let input_text = guardrails::extract_anthropic_input_text(&req_json);
        match p.scan(&input_text, Direction::Input).await {
            Ok(None) => {}
            Ok(Some(v)) => {
                tracing::warn!(
                    target: "inference.audit",
                    sandbox = %sandbox_name,
                    inference_policy_digest = %policy.digest,
                    decision = "deny",
                    gate = "guardrail_input",
                    categories = ?v.categories,
                    "guardrail pipeline blocked request (anthropic route)"
                );
                return deny_policy(
                    StatusCode::FORBIDDEN,
                    &v.message(),
                    "content_policy_violation",
                    v.code(),
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "inference.audit",
                    sandbox = %sandbox_name,
                    inference_policy_digest = %policy.digest,
                    decision = "deny",
                    gate = "guardrail_input",
                    error = %e,
                    "guardrail pipeline unavailable (anthropic route) — failing closed"
                );
                return deny_policy(
                    guardrail_error_status(&e),
                    &e.to_string(),
                    "api_error",
                    e.code(),
                );
            }
        }
    }

    // Native Messages pass-through (provider: anthropic, or Copilot's
    // native /v1/messages) — no translation.
    if upstream.provider == ProviderKind::Anthropic
        || proxy::is_copilot_endpoint(&upstream.endpoint)
    {
        return forward_anthropic_passthrough(
            state,
            sandbox_name,
            headers,
            body,
            upstream,
            guardrail_pipeline,
            policy.digest.clone(),
        )
        .await;
    }

    // Translate Anthropic -> OpenAI chat completions request shape.
    let openai_body = anthropic_to_openai(&req_json);
    let openai_bytes: Bytes = match serde_json::to_vec(&openai_body) {
        Ok(v) => v.into(),
        Err(e) => {
            return deny_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Translation failed: {e}"),
                "api_error",
            );
        }
    };

    // Force JSON content-type on upstream call; strip Anthropic-specific
    // headers (anthropic-version, x-api-key) that Foundry would reject.
    let mut upstream_headers = HeaderMap::new();
    for (name, value) in headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if n == "x-api-key" || n == "anthropic-version" || n == "anthropic-beta" {
            continue;
        }
        upstream_headers.insert(name.clone(), value.clone());
    }

    let result = proxy::forward(
        &state.auth,
        Some(&state.copilot),
        &state.client,
        &upstream,
        axum::http::Method::POST,
        "chat/completions",
        &upstream_headers,
        openai_bytes,
    )
    .await;

    match result {
        Ok((status, _resp_headers, resp_body)) => {
            if !status.is_success() {
                // Pass through Foundry error verbatim — it's already JSON.
                return (status, [("content-type", "application/json")], resp_body).into_response();
            }

            // Track usage tokens for budget (mirrors chat_completions handler).
            if let Ok(body_json) = serde_json::from_slice::<Value>(&resp_body)
                && let Some(total) = body_json
                    .get("usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|t| t.as_u64())
            {
                state.budget.record_usage(sandbox_name, total).await;
            }

            // Translate response back to Anthropic shape.
            let openai_resp: Value = match serde_json::from_slice(&resp_body) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(sandbox = %sandbox_name, error = %e, "Could not parse Foundry response as JSON");
                    return deny_response(
                        StatusCode::BAD_GATEWAY,
                        "Upstream returned non-JSON response",
                        "api_error",
                    );
                }
            };
            let anthropic_resp = openai_to_anthropic(&openai_resp, &requested_model);

            // Guardrail output scan (buffered, translated path).
            if let Some(p) = guardrail_pipeline
                .as_ref()
                .filter(|p| p.covers(Direction::Output))
            {
                let text = guardrails::extract_anthropic_output_text(&anthropic_resp);
                match p.scan(&text, Direction::Output).await {
                    Ok(None) => {}
                    Ok(Some(v)) => {
                        tracing::warn!(
                            target: "inference.audit",
                            sandbox = %sandbox_name,
                            inference_policy_digest = %policy.digest,
                            decision = "deny",
                            gate = "guardrail_output",
                            categories = ?v.categories,
                            "guardrail pipeline blocked translated response (anthropic route)"
                        );
                        return deny_policy(
                            StatusCode::FORBIDDEN,
                            &v.message(),
                            "content_policy_violation",
                            v.code(),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "inference.audit",
                            sandbox = %sandbox_name,
                            inference_policy_digest = %policy.digest,
                            decision = "deny",
                            gate = "guardrail_output",
                            error = %e,
                            "guardrail pipeline unavailable (anthropic route) — failing closed"
                        );
                        return deny_policy(
                            guardrail_error_status(&e),
                            &e.to_string(),
                            "api_error",
                            e.code(),
                        );
                    }
                }
            }

            (StatusCode::OK, Json(anthropic_resp)).into_response()
        }
        Err(e) => {
            tracing::warn!(sandbox = %sandbox_name, error = %e, "Anthropic upstream call failed");
            deny_response(
                StatusCode::BAD_GATEWAY,
                &format!("Upstream error: {e}"),
                "api_error",
            )
        }
    }
}

/// Update running token counts from one Anthropic SSE event.
/// `message_start` carries `message.usage.input_tokens` (and an initial
/// `output_tokens`); each `message_delta` carries the cumulative
/// `usage.output_tokens`.
fn update_anthropic_usage(ev: &Value, input: &mut u64, output: &mut u64) {
    match ev.get("type").and_then(|t| t.as_str()) {
        Some("message_start") => {
            if let Some(usage) = ev.get("message").and_then(|m| m.get("usage")) {
                if let Some(i) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                    *input = i;
                }
                if let Some(o) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                    *output = o;
                }
            }
        }
        Some("message_delta") => {
            if let Some(o) = ev
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
            {
                *output = o;
            }
        }
        _ => {}
    }
}

/// Shared streaming-usage accumulator. The observer (inside the guard)
/// writes it as `usage` events arrive; the finalizer (outside the
/// guard) reads it once at the terminal.
#[derive(Default)]
struct StreamUsage {
    input: u64,
    output: u64,
}

/// Observe token usage on a streamed Anthropic passthrough. Wraps the
/// RAW upstream stream (INSIDE the guard) and updates the shared
/// [`StreamUsage`] as it sees `message_start` / `message_delta` events.
/// Every byte passes through unchanged; nothing is recorded here.
///
/// Recording is deliberately split out to [`finalize_stream_usage`]
/// (which wraps OUTSIDE the guard) because on a mid-stream guardrail
/// cut the guard stops polling its inner stream — so an inner tap's
/// terminal branch would never run and usage would be lost. The outer
/// finalizer still sees the guard's terminal and records what the
/// observer accumulated up to the cut.
fn observe_stream_usage<E>(
    stream: futures::stream::BoxStream<'static, Result<Bytes, E>>,
    usage: Arc<std::sync::Mutex<StreamUsage>>,
) -> futures::stream::BoxStream<'static, Result<Bytes, E>>
where
    E: Send + 'static,
{
    struct Ctx<E> {
        inner: futures::stream::BoxStream<'static, Result<Bytes, E>>,
        usage: Arc<std::sync::Mutex<StreamUsage>>,
        // Trailing incomplete UTF-8 bytes carried across a chunk
        // boundary, mirroring the guardrail SSE state so a split
        // multi-byte code point is not corrupted before parsing.
        byte_carry: Vec<u8>,
        line_carry: String,
    }
    fn scan_lines<E>(ctx: &mut Ctx<E>, text: &str) {
        for line in text.lines() {
            let Some(payload) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Value>(payload) {
                let mut u = ctx.usage.lock().unwrap();
                let StreamUsage { input, output } = &mut *u;
                update_anthropic_usage(&ev, input, output);
            }
        }
    }
    let ctx = Ctx {
        inner: stream,
        usage,
        byte_carry: Vec::new(),
        line_carry: String::new(),
    };
    futures::stream::unfold(ctx, |mut ctx| async move {
        match ctx.inner.next().await {
            Some(Ok(chunk)) => {
                let mut bytes = std::mem::take(&mut ctx.byte_carry);
                bytes.extend_from_slice(&chunk);
                let decodable = match std::str::from_utf8(&bytes) {
                    Ok(_) => bytes.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => bytes.len(),
                };
                ctx.byte_carry = bytes.split_off(decodable);
                ctx.line_carry.push_str(&String::from_utf8_lossy(&bytes));
                if let Some(idx) = ctx.line_carry.rfind('\n') {
                    let complete = ctx.line_carry[..=idx].to_string();
                    ctx.line_carry = ctx.line_carry[idx + 1..].to_string();
                    scan_lines(&mut ctx, &complete);
                }
                Some((Ok(chunk), ctx))
            }
            Some(Err(e)) => Some((Err(e), ctx)),
            None => {
                if !ctx.byte_carry.is_empty() {
                    let tail = std::mem::take(&mut ctx.byte_carry);
                    ctx.line_carry.push_str(&String::from_utf8_lossy(&tail));
                }
                let tail = std::mem::take(&mut ctx.line_carry);
                scan_lines(&mut ctx, &tail);
                None
            }
        }
    })
    .boxed()
}

/// Record the observed streaming usage to the budget exactly once, at
/// the terminal of the (possibly guard-cut) stream. Wraps OUTSIDE the
/// guard so a mid-stream cut, an upstream error, or a clean end all
/// funnel through here. Every byte passes through unchanged.
fn finalize_stream_usage<E>(
    stream: futures::stream::BoxStream<'static, Result<Bytes, E>>,
    usage: Arc<std::sync::Mutex<StreamUsage>>,
    budget: crate::budget::TokenBudgetTracker,
    sandbox: String,
) -> futures::stream::BoxStream<'static, Result<Bytes, E>>
where
    E: Send + 'static,
{
    struct Ctx<E> {
        inner: futures::stream::BoxStream<'static, Result<Bytes, E>>,
        usage: Arc<std::sync::Mutex<StreamUsage>>,
        budget: crate::budget::TokenBudgetTracker,
        sandbox: String,
        recorded: bool,
    }
    async fn record<E>(ctx: &mut Ctx<E>) {
        if ctx.recorded {
            return;
        }
        ctx.recorded = true;
        let total = {
            let u = ctx.usage.lock().unwrap();
            u.input + u.output
        };
        if total > 0 {
            ctx.budget.record_usage(&ctx.sandbox, total).await;
        }
    }
    let ctx = Ctx {
        inner: stream,
        usage,
        budget,
        sandbox,
        recorded: false,
    };
    futures::stream::unfold(ctx, |mut ctx| async move {
        match ctx.inner.next().await {
            Some(Ok(chunk)) => Some((Ok(chunk), ctx)),
            Some(Err(e)) => {
                record(&mut ctx).await;
                Some((Err(e), ctx))
            }
            None => {
                record(&mut ctx).await;
                None
            }
        }
    })
    .boxed()
}

/// Native passthrough for Copilot's Anthropic Messages API.
///
/// No translation: forwards body verbatim to `{copilot_endpoint}/v1/messages`,
/// preserves Anthropic SDK headers (`anthropic-version`, `anthropic-beta`),
/// supports SSE streaming and returns response bytes 1:1. Tool use,
/// multi-modal content, and prompt caching all flow through unchanged.
async fn forward_anthropic_passthrough(
    state: AppState,
    sandbox_name: &str,
    headers: HeaderMap,
    body: Bytes,
    upstream: crate::proxy::UpstreamConfig,
    guardrail_pipeline: Option<Arc<GuardrailPipeline>>,
    policy_digest: String,
) -> axum::response::Response {
    let is_stream = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("stream")?.as_bool())
        .unwrap_or(false);

    // Strip the Anthropic API key — the router authenticates upstream using
    // the Copilot JWT (injected by `proxy::build_upstream_headers`). Pass the
    // anthropic-version / anthropic-beta headers through verbatim.
    let mut upstream_headers = HeaderMap::new();
    for (name, value) in headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if n == "x-api-key" || n == "authorization" {
            continue;
        }
        upstream_headers.insert(name.clone(), value.clone());
    }

    if is_stream {
        match proxy::forward_stream(
            state.auth.clone(),
            Some(state.copilot.clone()),
            state.client.clone(),
            upstream,
            "v1/messages",
            upstream_headers,
            body,
        )
        .await
        {
            Ok((status, resp_headers, stream)) => {
                // Observe usage INSIDE the guard (sees raw upstream
                // events), then record OUTSIDE the guard so a mid-stream
                // cut still bills the tokens consumed up to the cut.
                let usage = Arc::new(std::sync::Mutex::new(StreamUsage::default()));
                let observed = observe_stream_usage(stream, usage.clone());
                // Streaming output scan (Anthropic event dialect).
                let guarded = match guardrail_pipeline
                    .as_ref()
                    .filter(|p| p.covers(Direction::Output))
                {
                    Some(p) => guardrails::guard_sse_stream(
                        observed,
                        p.clone(),
                        guardrails::StreamDialect::AnthropicMessages,
                        sandbox_name.to_string(),
                        policy_digest.clone(),
                    ),
                    None => observed,
                };
                let finalized = finalize_stream_usage(
                    guarded,
                    usage,
                    state.budget.clone(),
                    sandbox_name.to_string(),
                );
                let body = Body::from_stream(finalized.map(|c| c.map_err(std::io::Error::other)));
                let mut resp = axum::response::Response::builder().status(status);
                if let Some(h) = resp.headers_mut() {
                    for (n, v) in resp_headers.iter() {
                        if is_hop_by_hop(n.as_str()) {
                            continue;
                        }
                        h.insert(n.clone(), v.clone());
                    }
                    h.insert(
                        axum::http::header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_static("text/event-stream"),
                    );
                }
                resp.body(body).unwrap_or_else(|_| {
                    deny_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to construct response",
                        "api_error",
                    )
                })
            }
            Err(e) => {
                tracing::warn!(sandbox = %sandbox_name, error = %e, "Copilot Anthropic stream failed");
                deny_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("Upstream error: {e}"),
                    "api_error",
                )
            }
        }
    } else {
        match proxy::forward(
            &state.auth,
            Some(&state.copilot),
            &state.client,
            &upstream,
            axum::http::Method::POST,
            "v1/messages",
            &upstream_headers,
            body,
        )
        .await
        {
            Ok((status, resp_headers, resp_body)) => {
                // Best-effort token usage tracking for Anthropic-shape replies.
                if status.is_success()
                    && let Ok(body_json) = serde_json::from_slice::<Value>(&resp_body)
                    && let Some(usage) = body_json.get("usage")
                {
                    let input = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let output = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let total = input + output;
                    if total > 0 {
                        state.budget.record_usage(sandbox_name, total).await;
                    }
                }

                // Guardrail output scan (buffered, Anthropic shape).
                if status.is_success()
                    && let Some(p) = guardrail_pipeline
                        .as_ref()
                        .filter(|p| p.covers(Direction::Output))
                {
                    let text = guardrails::scan_text_or_raw(
                        &resp_body,
                        guardrails::extract_anthropic_output_text,
                    );
                    match p.scan(&text, Direction::Output).await {
                        Ok(None) => {}
                        Ok(Some(v)) => {
                            tracing::warn!(
                                target: "inference.audit",
                                sandbox = %sandbox_name,
                                inference_policy_digest = %policy_digest,
                                decision = "deny",
                                gate = "guardrail_output",
                                categories = ?v.categories,
                                "guardrail pipeline blocked buffered response (anthropic route)"
                            );
                            return deny_policy(
                                StatusCode::FORBIDDEN,
                                &v.message(),
                                "content_policy_violation",
                                v.code(),
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "inference.audit",
                                sandbox = %sandbox_name,
                                inference_policy_digest = %policy_digest,
                                decision = "deny",
                                gate = "guardrail_output",
                                error = %e,
                                "guardrail pipeline unavailable (anthropic route) — failing closed"
                            );
                            return deny_policy(
                                guardrail_error_status(&e),
                                &e.to_string(),
                                "api_error",
                                e.code(),
                            );
                        }
                    }
                }

                let mut resp = axum::response::Response::builder().status(status);
                if let Some(h) = resp.headers_mut() {
                    for (n, v) in resp_headers.iter() {
                        if is_hop_by_hop(n.as_str()) {
                            continue;
                        }
                        h.insert(n.clone(), v.clone());
                    }
                    if !h.contains_key(axum::http::header::CONTENT_TYPE) {
                        h.insert(
                            axum::http::header::CONTENT_TYPE,
                            axum::http::HeaderValue::from_static("application/json"),
                        );
                    }
                }
                resp.body(Body::from(resp_body)).unwrap_or_else(|_| {
                    deny_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to construct response",
                        "api_error",
                    )
                })
            }
            Err(e) => {
                tracing::warn!(sandbox = %sandbox_name, error = %e, "Copilot Anthropic call failed");
                deny_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("Upstream error: {e}"),
                    "api_error",
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_basic_text_request() {
        let req = json!({
            "model": "claude-3-5-sonnet",
            "max_tokens": 256,
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "Hi"}
            ]
        });
        let openai = anthropic_to_openai(&req);
        assert_eq!(openai["model"], "claude-3-5-sonnet");
        assert_eq!(openai["max_tokens"], 256);
        let msgs = openai["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Hi");
    }

    #[test]
    fn flattens_content_parts() {
        let req = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Hello"},
                    {"type": "text", "text": "world"}
                ]}
            ]
        });
        let openai = anthropic_to_openai(&req);
        let msgs = openai["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], "Hello\nworld");
    }

    #[test]
    fn maps_finish_reason() {
        let resp = json!({
            "id": "chatcmpl-xyz",
            "model": "gpt-4.1",
            "choices": [{
                "message": {"role": "assistant", "content": "Hi back!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        });
        let anth = openai_to_anthropic(&resp, "claude-3-5-sonnet");
        assert_eq!(anth["type"], "message");
        assert_eq!(anth["role"], "assistant");
        assert_eq!(anth["stop_reason"], "end_turn");
        assert_eq!(anth["content"][0]["type"], "text");
        assert_eq!(anth["content"][0]["text"], "Hi back!");
        assert_eq!(anth["usage"]["input_tokens"], 5);
        assert_eq!(anth["usage"]["output_tokens"], 3);
    }

    #[test]
    fn maps_length_finish_reason_to_max_tokens() {
        let resp = json!({
            "choices": [{
                "message": {"content": "..."},
                "finish_reason": "length"
            }]
        });
        let anth = openai_to_anthropic(&resp, "x");
        assert_eq!(anth["stop_reason"], "max_tokens");
    }

    #[test]
    fn stop_sequences_become_stop() {
        let req = json!({
            "messages": [{"role": "user", "content": "x"}],
            "stop_sequences": ["END", "STOP"]
        });
        let openai = anthropic_to_openai(&req);
        assert_eq!(openai["stop"], json!(["END", "STOP"]));
    }

    #[test]
    fn usage_tap_reads_message_start_and_latest_delta() {
        let mut input = 0;
        let mut output = 0;
        update_anthropic_usage(
            &json!({"type": "message_start", "message": {"usage": {"input_tokens": 42, "output_tokens": 1}}}),
            &mut input,
            &mut output,
        );
        update_anthropic_usage(
            &json!({"type": "message_delta", "usage": {"output_tokens": 5}}),
            &mut input,
            &mut output,
        );
        update_anthropic_usage(
            &json!({"type": "message_delta", "usage": {"output_tokens": 17}}),
            &mut input,
            &mut output,
        );
        assert_eq!(input, 42, "input from message_start");
        assert_eq!(output, 17, "output is the latest cumulative delta");
    }

    #[tokio::test]
    async fn streaming_usage_is_recorded_to_budget() {
        use bytes::Bytes;
        use futures::stream::StreamExt as _;
        let budget = crate::budget::TokenBudgetTracker::new(1_000_000, 0);
        // Anthropic SSE split so a delta lands across a chunk boundary.
        let frames = [
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":30,\"output_tokens\":1}}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":",
            "12}}\n\n",
            "data: [DONE]\n\n",
        ];
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            frames.iter().map(|f| Ok(Bytes::from(*f))).collect();
        // observer (no guard here) -> finalizer, mirroring the no-output-
        // guardrail passthrough path.
        let usage = Arc::new(std::sync::Mutex::new(StreamUsage::default()));
        let observed = observe_stream_usage(futures::stream::iter(chunks).boxed(), usage.clone());
        let finalized = finalize_stream_usage(observed, usage, budget.clone(), "sbx".into());
        let _drained: Vec<_> = finalized.collect().await;
        let (used, _) = budget.get_usage("sbx").await;
        assert_eq!(used, 42, "30 input + 12 output recorded once at stream end");
    }

    #[tokio::test]
    async fn buffered_policy_denial_carries_code_and_decision_headers() {
        // Anthropic-native `error.type` is preserved, the stable machine
        // code lands in `error.code`, and the decision headers are set.
        let resp = deny_policy(
            StatusCode::FORBIDDEN,
            "blocked by guardrail",
            "content_policy_violation",
            "guardrail_blocked",
        );
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            resp.headers()
                .get("x-kars-decision")
                .and_then(|v| v.to_str().ok()),
            Some("blocked")
        );
        assert!(resp.headers().get("x-kars-decision-by").is_some());
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "content_policy_violation");
        assert_eq!(v["error"]["code"], "guardrail_blocked");
    }

    #[tokio::test]
    async fn non_policy_denial_has_no_decision_headers() {
        // A plain client error is not a policy decision: type == code and
        // no decision headers.
        let resp = deny_response(
            StatusCode::TOO_MANY_REQUESTS,
            "slow down",
            "rate_limit_error",
        );
        assert!(resp.headers().get("x-kars-decision").is_none());
        let bytes = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "rate_limit_error");
        assert_eq!(v["error"]["code"], "rate_limit_error");
    }

    /// Regression: a mid-stream guardrail cut must still bill the tokens
    /// consumed up to the cut. The observer sits inside the guard; the
    /// finalizer outside records on the guard's terminal even though the
    /// guard stopped polling its inner stream at the cut.
    #[tokio::test]
    async fn streaming_usage_recorded_when_guard_cuts_midstream() {
        use bytes::Bytes;
        use futures::stream::StreamExt as _;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Moderation that flags any text containing "BLOCKED".
        let moderation = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/moderations"))
            .respond_with(move |req: &wiremock::Request| {
                let flagged = String::from_utf8_lossy(&req.body).contains("BLOCKED");
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "results": [{ "flagged": flagged, "categories": { "illicit": flagged } }]
                }))
            })
            .mount(&moderation)
            .await;

        let mut config = crate::config::Config::from_env().expect("config");
        config.openai_moderation_endpoint = moderation.uri();
        config.openai_moderation_api_key = Some("sk-mod-test".into());
        let pipeline = Arc::new(
            GuardrailPipeline::from_stages(
                &[crate::guardrails::GuardrailStageCfg {
                    provider: "openai-moderation".into(),
                    apply_to: crate::guardrails::ApplyTo::Output,
                }],
                &config,
                &reqwest::Client::new(),
            )
            .expect("pipeline builds"),
        );

        let budget = crate::budget::TokenBudgetTracker::new(1_000_000, 0);

        // message_start carries input=30; a single content_block_delta
        // over the scan threshold forces a mid-stream scan that flags and
        // cuts BEFORE any [DONE]. The trailing message_delta never arrives.
        let big = "BLOCKED ".repeat(200); // > STREAM_SCAN_THRESHOLD_CHARS
        let frames = vec![
            Ok(Bytes::from(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":30,\"output_tokens\":1}}}\n\n".to_string(),
            )),
            Ok(Bytes::from(format!(
                "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"{big}\"}}}}\n\n"
            ))),
        ];
        let stream: futures::stream::BoxStream<'static, Result<Bytes, std::io::Error>> =
            futures::stream::iter(frames).boxed();

        let usage = Arc::new(std::sync::Mutex::new(StreamUsage::default()));
        let observed = observe_stream_usage(stream, usage.clone());
        let guarded = guardrails::guard_sse_stream(
            observed,
            pipeline,
            guardrails::StreamDialect::AnthropicMessages,
            "sbx".into(),
            "sha256:t".into(),
        );
        let finalized = finalize_stream_usage(guarded, usage, budget.clone(), "sbx".into());
        let out: String = finalized
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|c| c.ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .collect();

        assert!(out.contains("guardrail_blocked"), "guard must cut: {out}");
        let (used, _) = budget.get_usage("sbx").await;
        assert_eq!(
            used, 31,
            "input(30)+message_start output(1) billed once despite the mid-stream cut, got {used}"
        );
    }
}
