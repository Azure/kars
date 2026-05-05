// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reverse proxy logic — forwards inference requests to Azure AI Foundry.
//!
//! Supports both buffered and SSE streaming responses.
//! - Non-streaming: buffers response, extracts token usage for metrics/budgets
//! - Streaming (SSE): pipes response bytes directly to client for low TTFT

use anyhow::{Context, Result};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use bytes::Bytes;
use reqwest::Client;
use std::time::Instant;

use crate::auth::WorkloadIdentityAuth;
use crate::metrics;
use std::sync::Arc;

/// Upstream configuration for a single request.
#[derive(Clone)]
pub struct UpstreamConfig {
    pub endpoint: String,
    pub deployment: String,
    pub sandbox_name: String,
}

/// Determine the correct token audience for the upstream endpoint.
/// Foundry project endpoints (services.ai.azure.com/api/projects/) require
/// the `https://ai.azure.com` audience. Legacy Azure OpenAI endpoints
/// (openai.azure.com) require `https://cognitiveservices.azure.com`.
fn token_audience(endpoint: &str) -> &'static str {
    if endpoint.contains("services.ai.azure.com") && endpoint.contains("/api/projects/") {
        "https://ai.azure.com"
    } else {
        "https://cognitiveservices.azure.com"
    }
}

/// Sanitize request headers — strip credentials and hop-by-hop headers,
/// then inject Azure auth.
fn build_upstream_headers(
    request_headers: &HeaderMap,
    _auth: &WorkloadIdentityAuth,
    token: &str,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in request_headers.iter() {
        match name.as_str() {
            "authorization" | "api-key" | "x-api-key" => continue,
            "host" | "connection" | "transfer-encoding" | "content-length" => continue,
            _ => {
                headers.insert(name.clone(), value.clone());
            }
        }
    }

    // Both API-key and Entra modes use Authorization: Bearer for the unified
    // /openai/v1/ endpoint format. Azure OpenAI accepts API keys as Bearer tokens.
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).context("Invalid token")?,
    );
    headers
        .entry("content-type")
        .or_insert(HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// Record Prometheus metrics from a completed request.
fn record_metrics(
    upstream: &UpstreamConfig,
    status: StatusCode,
    latency: std::time::Duration,
    response_body: &[u8],
) {
    let status_label = if status.is_success() { "ok" } else { "error" };
    metrics::INFERENCE_REQUESTS
        .with_label_values(&[
            &upstream.sandbox_name,
            &upstream.deployment,
            &status_label.to_string(),
        ])
        .inc();
    metrics::INFERENCE_LATENCY
        .with_label_values(&[&upstream.sandbox_name, &upstream.deployment])
        .observe(latency.as_secs_f64());

    if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(response_body)
        && let Some(usage) = body_json.get("usage")
    {
        if let Some(input) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
            metrics::TOKENS_USED
                .with_label_values(&[
                    &upstream.sandbox_name,
                    &upstream.deployment,
                    &"input".to_string(),
                ])
                .inc_by(input as u64);
        }
        if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
            metrics::TOKENS_USED
                .with_label_values(&[
                    &upstream.sandbox_name,
                    &upstream.deployment,
                    &"output".to_string(),
                ])
                .inc_by(output as u64);
        }
    }
}

/// Forward an inference request to the appropriate Azure backend.
///
/// - **Dev mode** (API key): Azure OpenAI `/openai/deployments/{model}/{path}?api-version=...`
/// - **AKS mode** (Workload Identity / IMDS): Foundry `/openai/v1/{path}` with model in body
pub async fn forward(
    auth: &WorkloadIdentityAuth,
    client: &Client,
    upstream: &UpstreamConfig,
    method: Method,
    path: &str,
    request_headers: &HeaderMap,
    request_body: Bytes,
) -> Result<(StatusCode, HeaderMap, Bytes)> {
    let start = Instant::now();

    let (upstream_url, body) = build_upstream_url(auth, upstream, path, request_body)?;

    let mode = if auth.is_api_key_mode() {
        "dev"
    } else {
        "foundry"
    };
    tracing::info!(sandbox = %upstream.sandbox_name, model = %upstream.deployment, mode = %mode, "Forwarding inference");

    let token = auth
        .get_token(token_audience(&upstream.endpoint))
        .await
        .context("Failed to acquire auth token")?;

    let headers = build_upstream_headers(request_headers, auth, &token)?;

    tracing::info!(sandbox = %upstream.sandbox_name, url = %upstream_url, body_len = body.len(), "Sending upstream request");

    let retryable = is_idempotent(&method, path);
    let response = send_with_retry(
        client,
        &method,
        &upstream_url,
        &headers,
        body,
        retryable,
        &upstream.sandbox_name,
    )
    .await?;

    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let response_headers = response.headers().clone();

    // r6 — surface Azure-side request ids so one log line carries both our
    // trace_id (from the outer tracing span) and Azure's correlation ids.
    // `x-ms-request-id` identifies the Azure OpenAI request; `apim-request-id`
    // identifies the APIM frontend; both are what Azure support asks for.
    let azure_request_id = response_headers
        .get("x-ms-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let apim_request_id = response_headers
        .get("apim-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let response_body = response
        .bytes()
        .await
        .context("Failed to read Foundry response")?;
    let latency = start.elapsed();

    record_metrics(upstream, status, latency, &response_body);

    tracing::info!(
        sandbox = %upstream.sandbox_name,
        status = %status.as_u16(),
        latency_ms = %latency.as_millis(),
        resp_len = response_body.len(),
        azure_request_id = %azure_request_id,
        apim_request_id = %apim_request_id,
        "Foundry complete"
    );
    Ok((status, response_headers, response_body))
}

// ── Retry logic (R3) ─────────────────────────────────────────────────────────
//
// Brief Azure OpenAI blips (TCP reset, 502/503/504) previously surfaced as
// immediate 5xx to the agent. We now retry *idempotent* upstream calls with
// bounded exponential backoff. Non-idempotent calls (chat completions,
// responses, streaming) are never retried: they may have billed the caller
// or committed state on the first attempt.

/// Request methods + paths that are safe to retry. Must match Azure OpenAI
/// semantics — `POST /embeddings` is stateless-idempotent (no randomness),
/// `POST /chat/completions` and `POST /responses` are NOT (non-determinism +
/// billed tokens on every attempt).
fn is_idempotent(method: &Method, path: &str) -> bool {
    if method == Method::GET || method == Method::HEAD {
        return true;
    }
    if method == Method::POST && path.trim_end_matches('/').ends_with("/embeddings") {
        return true;
    }
    false
}

/// Decide whether to retry based on a received HTTP status. Only the three
/// "upstream degraded" classes — bad gateway, service unavailable, gateway
/// timeout — are safe to retry; 4xx signals a caller bug.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 502..=504)
}

/// Decide whether to retry based on a reqwest error.
///
/// Two classes are treated as retryable:
///
///   • `is_connect()` — TCP/TLS handshake never completed, so the
///     upstream cannot have observed the request at all. Always safe.
///
///   • `is_timeout()` — the full request/response deadline elapsed.
///     Note: this covers **any** phase, including timeouts that occur
///     after the request body has been fully sent and while we're
///     waiting for response bytes. This is only called from
///     `send_with_retry` with `retryable=true`, which the caller only
///     sets for idempotent requests (see `is_idempotent_method` +
///     `/embeddings` allowlist). For those — GET, HEAD, and the
///     deterministic POST `/embeddings` endpoint — re-sending is safe
///     regardless of when in the request cycle the timeout fired.
///
/// For any non-idempotent request (`chat/completions`, `completions`,
/// `responses`, PUT, DELETE, PATCH) the retry loop is disabled at the
/// caller, so this classifier is never consulted and a timeout mid-body
/// or mid-response produces a single failure, not a double-send.
fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout()
}

/// Send with up to `MAX_ATTEMPTS` tries + exponential backoff. Caller passes
/// `retryable=false` for non-idempotent requests to force a single attempt.
async fn send_with_retry(
    client: &Client,
    method: &Method,
    url: &str,
    headers: &HeaderMap,
    body: Bytes,
    retryable: bool,
    sandbox_name: &str,
) -> Result<reqwest::Response> {
    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFF_MS: [u64; 2] = [250, 750]; // after attempts 1 and 2

    let attempts = if retryable { MAX_ATTEMPTS } else { 1 };
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 1..=attempts {
        // RequestBuilder is not Clone, so rebuild per attempt. Body is a
        // Bytes (cheap ref-counted clone) — no real cost.
        let response = client
            .request(method.clone(), url)
            .headers(headers.clone())
            .body(body.clone())
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                if retryable && is_retryable_status(status) && attempt < attempts {
                    tracing::warn!(
                        sandbox = %sandbox_name,
                        status = %status.as_u16(),
                        attempt,
                        "Upstream returned retryable status; backing off"
                    );
                    metrics::UPSTREAM_RETRIES
                        .with_label_values(&[sandbox_name, "status"])
                        .inc();
                    tokio::time::sleep(std::time::Duration::from_millis(
                        BACKOFF_MS[(attempt - 1) as usize],
                    ))
                    .await;
                    continue;
                }
                return Ok(resp);
            }
            Err(err) => {
                if retryable && is_retryable_error(&err) && attempt < attempts {
                    tracing::warn!(
                        sandbox = %sandbox_name,
                        error = %err,
                        attempt,
                        "Upstream transport error; backing off"
                    );
                    metrics::UPSTREAM_RETRIES
                        .with_label_values(&[sandbox_name, "transport"])
                        .inc();
                    tokio::time::sleep(std::time::Duration::from_millis(
                        BACKOFF_MS[(attempt - 1) as usize],
                    ))
                    .await;
                    last_err = Some(anyhow::Error::from(err));
                    continue;
                }
                return Err(anyhow::Error::from(err).context("Foundry upstream request failed"));
            }
        }
    }

    // Exhausted all retries with transport errors. Propagate the last one.
    Err(last_err
        .unwrap_or_else(|| anyhow::anyhow!("Upstream request failed after {MAX_ATTEMPTS} attempts"))
        .context("Foundry upstream request failed after retries"))
}

/// Forward a streaming (SSE) inference request. Returns a byte stream
/// that intercepts SSE chunks to extract token usage from the final chunk.
///
/// Injects `stream_options.include_usage = true` so Azure OpenAI sends
/// a terminal chunk with `usage` data. The wrapper stream records latency
/// and token metrics transparently.
pub async fn forward_stream(
    auth: Arc<WorkloadIdentityAuth>,
    client: Client,
    upstream: UpstreamConfig,
    path: &str,
    request_headers: HeaderMap,
    request_body: Bytes,
) -> Result<(
    StatusCode,
    HeaderMap,
    impl futures::Stream<Item = Result<Bytes, reqwest::Error>>,
)> {
    // Inject stream_options.include_usage into request body so the final
    // SSE chunk contains a `usage` object with token counts.
    let body_with_usage = inject_stream_usage(request_body);
    let (upstream_url, body) = build_upstream_url(&auth, &upstream, path, body_with_usage)?;

    tracing::info!(sandbox = %upstream.sandbox_name, model = %upstream.deployment, mode = "stream", "Forwarding SSE stream");

    let token = auth
        .get_token(token_audience(&upstream.endpoint))
        .await
        .context("Failed to acquire auth token")?;
    let headers = build_upstream_headers(&request_headers, &auth, &token)?;

    let start = Instant::now();

    let response = client
        .post(&upstream_url)
        .headers(headers)
        .body(body)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .context("Streaming upstream request failed")?;

    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let response_headers = response.headers().clone();

    // r6 — log Azure correlation ids for the stream path too. Emitted at
    // stream-start because headers arrive before any bytes.
    let azure_request_id = response_headers
        .get("x-ms-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let apim_request_id = response_headers
        .get("apim-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    tracing::info!(
        sandbox = %upstream.sandbox_name,
        status = %status.as_u16(),
        azure_request_id = %azure_request_id,
        apim_request_id = %apim_request_id,
        "Foundry stream headers received"
    );

    // Record request count immediately
    let status_label = if status.is_success() { "ok" } else { "error" };
    metrics::INFERENCE_REQUESTS
        .with_label_values(&[
            &upstream.sandbox_name,
            &upstream.deployment,
            &status_label.to_string(),
        ])
        .inc();

    // Wrap the byte stream to intercept the final SSE chunk for token metrics
    let sandbox_name = upstream.sandbox_name.clone();
    let model = upstream.deployment.clone();
    let inner = response.bytes_stream();

    use futures::StreamExt;
    let metered = inner.map(move |chunk| {
        if let Ok(ref bytes) = chunk {
            // SSE chunks look like: "data: {json}\n\n"
            // The final usage chunk contains "usage":{"prompt_tokens":...}
            let text = String::from_utf8_lossy(bytes);
            for line in text.split('\n') {
                let line = line.trim();
                if !line.starts_with("data: ") || line == "data: [DONE]" {
                    continue;
                }
                let json_str = &line[6..];
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str)
                    && let Some(usage) = v.get("usage")
                {
                    // Record latency (stream complete)
                    let latency = start.elapsed();
                    metrics::INFERENCE_LATENCY
                        .with_label_values(&[&sandbox_name, &model])
                        .observe(latency.as_secs_f64());
                    // Record token usage
                    if let Some(input) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                        metrics::TOKENS_USED
                            .with_label_values(&[&sandbox_name, &model, &"input".to_string()])
                            .inc_by(input as u64);
                    }
                    if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                        metrics::TOKENS_USED
                            .with_label_values(&[&sandbox_name, &model, &"output".to_string()])
                            .inc_by(output as u64);
                    }
                }
            }
        }
        chunk
    });

    Ok((status, response_headers, metered))
}

/// Inject `stream_options: { include_usage: true }` into the request body
/// so Azure OpenAI includes token usage in the final SSE chunk.
fn inject_stream_usage(body: Bytes) -> Bytes {
    if let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body)
        && let Some(obj) = json.as_object_mut()
    {
        let opts = obj
            .entry("stream_options")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(opts_obj) = opts.as_object_mut() {
            opts_obj.insert("include_usage".to_string(), serde_json::json!(true));
        }
        if let Ok(bytes) = serde_json::to_vec(&json) {
            return Bytes::from(bytes);
        }
    }
    body
}

/// Returns true if the endpoint is a GitHub Models endpoint
/// (https://models.github.ai/inference or the legacy
/// https://models.inference.ai.azure.com URL). GitHub Models is OpenAI-API
/// compatible but does NOT use the Azure `/openai/v1/` URL prefix.
fn is_github_models_endpoint(endpoint: &str) -> bool {
    endpoint.contains("models.github.ai") || endpoint.contains("models.inference.ai.azure.com")
}

/// Build the upstream URL and optionally inject model into request body.
/// Uses the unified /openai/v1/ format — works with both API-key and Entra auth.
fn build_upstream_url(
    _auth: &WorkloadIdentityAuth,
    upstream: &UpstreamConfig,
    path: &str,
    request_body: Bytes,
) -> Result<(String, Bytes)> {
    // GitHub Models exposes OpenAI-compatible routes directly under the
    // endpoint (no Azure-style `/openai/v1/` prefix). Detect and skip the
    // rewrite for those endpoints; everything else goes via the unified
    // `/openai/v1/` format that works for both Azure OpenAI (api-key) and
    // Foundry project (Entra) endpoints.
    let url = if is_github_models_endpoint(&upstream.endpoint) {
        format!(
            "{}/{}",
            upstream.endpoint.trim_end_matches('/'),
            path.trim_start_matches('/'),
        )
    } else {
        format!(
            "{}/openai/v1/{}",
            upstream.endpoint.trim_end_matches('/'),
            path.trim_start_matches('/'),
        )
    };
    let body = if let Ok(mut body_json) = serde_json::from_slice::<serde_json::Value>(&request_body)
    {
        if body_json.get("model").is_none() {
            body_json.as_object_mut().unwrap().insert(
                "model".into(),
                serde_json::Value::String(upstream.deployment.clone()),
            );
        }
        serde_json::to_vec(&body_json)?.into()
    } else {
        request_body
    };
    Ok((url, body))
}

// ── Retry-logic unit tests (R3) ──────────────────────────────────────────────
//
// Full retry behaviour is exercised end-to-end in
// `tests/proxy_fake_upstream.rs`. These inline tests pin the idempotency +
// status classifier so adding a new endpoint can't silently make a
// non-idempotent request retryable.

#[cfg(test)]
mod retry_tests {
    use super::{is_idempotent, is_retryable_status};
    use axum::http::Method;
    use reqwest::StatusCode;

    #[test]
    fn get_and_head_are_idempotent() {
        assert!(is_idempotent(&Method::GET, "/anything"));
        assert!(is_idempotent(&Method::HEAD, "/anything"));
    }

    #[test]
    fn post_embeddings_is_idempotent() {
        assert!(is_idempotent(&Method::POST, "/openai/v1/embeddings"));
        assert!(is_idempotent(&Method::POST, "/openai/v1/embeddings/"));
    }

    #[test]
    fn post_chat_completions_is_not_idempotent() {
        assert!(!is_idempotent(&Method::POST, "/openai/v1/chat/completions"));
        assert!(!is_idempotent(&Method::POST, "/openai/v1/responses"));
        assert!(!is_idempotent(&Method::POST, "/openai/v1/completions"));
    }

    #[test]
    fn put_delete_patch_are_not_idempotent() {
        // Azure AI Foundry memory-store / eval APIs use PUT + DELETE. These
        // mutate server state — retries can double-apply.
        assert!(!is_idempotent(&Method::PUT, "/memory-stores/x"));
        assert!(!is_idempotent(&Method::DELETE, "/memory-stores/x"));
        assert!(!is_idempotent(&Method::PATCH, "/memory-stores/x"));
    }

    #[test]
    fn retryable_statuses_are_5xx_upstream_only() {
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::GATEWAY_TIMEOUT));
    }

    #[test]
    fn client_errors_are_not_retryable() {
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_retryable_status(StatusCode::PAYLOAD_TOO_LARGE));
    }

    #[test]
    fn server_500_and_501_are_not_retryable() {
        // 500 = upstream logic bug, 501 = method not supported. Retrying
        // won't help and may waste billed tokens.
        assert!(!is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!is_retryable_status(StatusCode::NOT_IMPLEMENTED));
    }

    #[test]
    fn success_is_not_retryable() {
        assert!(!is_retryable_status(StatusCode::OK));
        assert!(!is_retryable_status(StatusCode::CREATED));
    }
}
