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

    let response = client
        .request(method, &upstream_url)
        .headers(headers)
        .body(body)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .context("Foundry upstream request failed")?;

    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let response_headers = response.headers().clone();
    let response_body = response
        .bytes()
        .await
        .context("Failed to read Foundry response")?;
    let latency = start.elapsed();

    record_metrics(upstream, status, latency, &response_body);

    tracing::info!(sandbox = %upstream.sandbox_name, status = %status.as_u16(), latency_ms = %latency.as_millis(), resp_len = response_body.len(), "Foundry complete");
    Ok((status, response_headers, response_body))
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

/// Build the upstream URL and optionally inject model into request body.
/// Uses the unified /openai/v1/ format — works with both API-key and Entra auth.
fn build_upstream_url(
    _auth: &WorkloadIdentityAuth,
    upstream: &UpstreamConfig,
    path: &str,
    request_body: Bytes,
) -> Result<(String, Bytes)> {
    // Use the unified /openai/v1/ format for all modes.
    // Both API-key (dev) and Entra (AKS) work with Bearer auth on this endpoint.
    // This supports all models including Responses-only models like gpt-5.4-pro.
    let url = format!(
        "{}/openai/v1/{}",
        upstream.endpoint.trim_end_matches('/'),
        path.trim_start_matches('/'),
    );
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
