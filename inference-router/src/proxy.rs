//! Reverse proxy logic — forwards inference requests to Azure AI Foundry.
//!
//! The proxy:
//! 1. Strips any credentials the sandbox may have tried to inject
//! 2. Acquires a Managed Identity token via Workload Identity / IMDS
//! 3. Injects the bearer token into the upstream request
//! 4. Forwards to Azure AI Foundry and returns the response
//! 5. Records metrics (latency, tokens, status)

use anyhow::{Context, Result};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use bytes::Bytes;
use reqwest::Client;
use std::time::Instant;

use crate::auth::WorkloadIdentityAuth;
use crate::metrics;

/// Upstream configuration for a single request.
pub struct UpstreamConfig {
    pub endpoint: String,
    pub deployment: String,
    pub sandbox_name: String,
}

/// Sanitize request headers — strip credentials and hop-by-hop headers,
/// then inject Azure auth.
fn build_upstream_headers(
    request_headers: &HeaderMap,
    auth: &WorkloadIdentityAuth,
    token: &str,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in request_headers.iter() {
        match name.as_str() {
            "authorization" | "api-key" | "x-api-key" => continue,
            "host" | "connection" | "transfer-encoding" | "content-length" => continue,
            _ => { headers.insert(name.clone(), value.clone()); }
        }
    }

    if auth.is_api_key_mode() {
        headers.insert("api-key", HeaderValue::from_str(token).context("Invalid API key")?);
    } else {
        headers.insert("authorization", HeaderValue::from_str(&format!("Bearer {token}")).context("Invalid token")?);
    }
    headers.entry("content-type").or_insert(HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// Record Prometheus metrics from a completed request.
fn record_metrics(upstream: &UpstreamConfig, status: StatusCode, latency: std::time::Duration, response_body: &[u8]) {
    let status_label = if status.is_success() { "ok" } else { "error" };
    metrics::INFERENCE_REQUESTS
        .with_label_values(&[&upstream.sandbox_name, &upstream.deployment, status_label])
        .inc();
    metrics::INFERENCE_LATENCY
        .with_label_values(&[&upstream.sandbox_name, &upstream.deployment])
        .observe(latency.as_secs_f64());

    if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(response_body) {
        if let Some(usage) = body_json.get("usage") {
            if let Some(input) = usage.get("prompt_tokens").and_then(|v| v.as_i64()) {
                metrics::TOKENS_USED
                    .with_label_values(&[&upstream.sandbox_name, &upstream.deployment, "input"])
                    .inc_by(input as u64);
            }
            if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
                metrics::TOKENS_USED
                    .with_label_values(&[&upstream.sandbox_name, &upstream.deployment, "output"])
                    .inc_by(output as u64);
            }
        }
    }
}

/// Forward an inference request to Azure AI Foundry.
///
/// Uses the Foundry `/openai/v1/{path}` endpoint which is OpenAI-compatible
/// and supports all endpoints: chat/completions, completions, embeddings, models.
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

    let upstream_url = format!(
        "{}/openai/v1/{}",
        upstream.endpoint.trim_end_matches('/'),
        path.trim_start_matches('/'),
    );

    tracing::info!(sandbox = %upstream.sandbox_name, model = %upstream.deployment, path = %path, "Forwarding to Foundry");

    // Inject model name into request body if not present
    let body = if let Ok(mut body_json) = serde_json::from_slice::<serde_json::Value>(&request_body) {
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

    let token = auth.get_token("https://cognitiveservices.azure.com").await
        .context("Failed to acquire token for Foundry")?;

    let headers = build_upstream_headers(request_headers, auth, &token)?;

    let response = client
        .request(method, &upstream_url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .context("Foundry upstream request failed")?;

    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let response_headers = response.headers().clone();
    let response_body = response.bytes().await.context("Failed to read Foundry response")?;
    let latency = start.elapsed();

    record_metrics(upstream, status, latency, &response_body);

    tracing::info!(sandbox = %upstream.sandbox_name, status = %status.as_u16(), latency_ms = %latency.as_millis(), "Foundry complete");
    Ok((status, response_headers, response_body))
}
