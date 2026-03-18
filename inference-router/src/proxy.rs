//! Reverse proxy logic — forwards inference requests to Azure OpenAI / AI Foundry.
//!
//! The proxy:
//! 1. Strips any credentials the sandbox may have tried to inject
//! 2. Acquires a Managed Identity token via Workload Identity
//! 3. Injects the bearer token into the upstream request
//! 4. Forwards to Azure OpenAI and streams the response back
//! 5. Records metrics (latency, tokens, status)

use anyhow::{Context, Result};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use bytes::Bytes;
use reqwest::Client;
use std::time::Instant;

use crate::auth::WorkloadIdentityAuth;
use crate::metrics;

/// Upstream Azure OpenAI configuration for a single request.
pub struct UpstreamConfig {
    pub endpoint: String,
    pub deployment: String,
    pub api_version: String,
    pub sandbox_name: String,
}

/// Forward an inference request to Azure OpenAI.
/// Returns (status_code, headers, body_bytes).
pub async fn forward_to_azure_openai(
    auth: &WorkloadIdentityAuth,
    client: &Client,
    upstream: &UpstreamConfig,
    method: Method,
    path: &str,
    request_headers: &HeaderMap,
    request_body: Bytes,
) -> Result<(StatusCode, HeaderMap, Bytes)> {
    let start = Instant::now();

    // Build upstream URL
    let upstream_url = if auth.is_api_key_mode() {
        // Azure OpenAI with API key — needs api-version query param
        format!(
            "{}/openai/deployments/{}/{}?api-version={}",
            upstream.endpoint.trim_end_matches('/'),
            upstream.deployment,
            path.trim_start_matches('/'),
            upstream.api_version,
        )
    } else {
        // Workload Identity — standard /openai/deployments/ path
        format!(
            "{}/openai/deployments/{}/{}?api-version={}",
            upstream.endpoint.trim_end_matches('/'),
            upstream.deployment,
            path.trim_start_matches('/'),
            upstream.api_version,
        )
    };

    tracing::info!(
        sandbox = %upstream.sandbox_name,
        model = %upstream.deployment,
        method = %method,
        path = %path,
        "Forwarding inference request"
    );

    // Acquire token via Workload Identity (zero credentials in sandbox)
    let token = auth
        .get_token("https://cognitiveservices.azure.com")
        .await
        .context("Failed to acquire Azure AD token")?;

    // Build upstream request — strip sandbox-injected auth, inject ours
    let mut upstream_headers = HeaderMap::new();
    // Forward safe headers only
    for (name, value) in request_headers.iter() {
        let name_str = name.as_str();
        match name_str {
            // Strip any auth the sandbox tried to inject
            "authorization" | "api-key" | "x-api-key" => continue,
            // Strip hop-by-hop headers
            "host" | "connection" | "transfer-encoding" => continue,
            _ => {
                upstream_headers.insert(name.clone(), value.clone());
            }
        }
    }

    // Inject auth — API key header for dev mode, Bearer token for AKS
    if auth.is_api_key_mode() {
        upstream_headers.insert(
            "api-key",
            HeaderValue::from_str(&token)
                .context("Invalid API key value")?,
        );
    } else {
        upstream_headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}"))
                .context("Invalid token value")?,
        );
    }

    // Ensure content-type is set
    upstream_headers
        .entry("content-type")
        .or_insert(HeaderValue::from_static("application/json"));

    // Forward the request
    let response = client
        .request(method.clone(), &upstream_url)
        .headers(upstream_headers)
        .body(request_body)
        .send()
        .await
        .context("Upstream request failed")?;

    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let response_headers = response.headers().clone();
    let response_body = response
        .bytes()
        .await
        .context("Failed to read upstream response body")?;

    let latency = start.elapsed();

    // Record metrics
    let status_label = if status.is_success() { "ok" } else { "error" };
    metrics::INFERENCE_REQUESTS
        .with_label_values(&[&upstream.sandbox_name, &upstream.deployment, status_label])
        .inc();
    metrics::INFERENCE_LATENCY
        .with_label_values(&[&upstream.sandbox_name, &upstream.deployment])
        .observe(latency.as_secs_f64());

    // Extract token usage from response body (if present)
    if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&response_body) {
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

    tracing::info!(
        sandbox = %upstream.sandbox_name,
        model = %upstream.deployment,
        status = %status.as_u16(),
        latency_ms = %latency.as_millis(),
        "Inference request completed"
    );

    Ok((status, response_headers, response_body))
}
