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
use crate::proxy;

fn deny_response(status: StatusCode, message: &str, code: &str) -> axum::response::Response {
    (
        status,
        Json(json!({
            "type": "error",
            "error": {
                "type": code,
                "message": message,
            }
        })),
    )
        .into_response()
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
        .get("x-azureclaw-sandbox")
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

    // Token budget check.
    if let Err(msg) = state.budget.check_budget(sandbox_name).await {
        tracing::warn!(sandbox = %sandbox_name, "Token budget exceeded (anthropic): {msg}");
        return deny_response(StatusCode::TOO_MANY_REQUESTS, &msg, "rate_limit_error");
    }

    let requested_model = req_json
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let upstream = state.upstream_config(sandbox_name);

    // Copilot exposes a native Anthropic Messages endpoint at /v1/messages.
    // Skip translation entirely and forward the body as-is, preserving the
    // streaming + tool_use + multi-modal contracts of the Anthropic SDK.
    if proxy::is_copilot_endpoint(&upstream.endpoint) {
        return forward_anthropic_passthrough(state, sandbox_name, headers, body, upstream).await;
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
                let body = Body::from_stream(stream.map(|c| c.map_err(std::io::Error::other)));
                let mut resp = axum::response::Response::builder().status(status);
                if let Some(h) = resp.headers_mut() {
                    for (n, v) in resp_headers.iter() {
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

                let mut resp = axum::response::Response::builder().status(status);
                if let Some(h) = resp.headers_mut() {
                    for (n, v) in resp_headers.iter() {
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
}
