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
use crate::copilot_auth::{
    self, COPILOT_INTEGRATION_ID, CopilotTokenCache, EDITOR_PLUGIN_VERSION, EDITOR_VERSION,
};
use crate::metrics;
use std::sync::Arc;

/// Upstream configuration for a single request.
#[derive(Clone)]
pub struct UpstreamConfig {
    pub endpoint: String,
    pub deployment: String,
    pub sandbox_name: String,
    /// Direct bearer/API key for THIS specific upstream, set when
    /// `InferencePolicy.modelPreference` routed the request to a
    /// non-default provider that carries its own dev-mode key (e.g. a
    /// GitHub Models PAT). `None` ⇒ use the router's normal auth
    /// resolution (Workload Identity / IMDS / sidecar / the single global
    /// dev key) — the pre-existing behavior, unchanged for the default
    /// provider. Never logged. See `Config::resolve_provider`.
    pub provider_api_key: Option<String>,
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
/// then inject auth + provider-specific static headers.
///
/// When `endpoint` is a GitHub Copilot URL, also injects the three static
/// headers Copilot's ingress requires (`Editor-Version`,
/// `Copilot-Integration-Id`, `Editor-Plugin-Version`). Without these,
/// Copilot returns 400 "missing required header" or routes to the wrong
/// model behind the scenes.
fn build_upstream_headers(
    request_headers: &HeaderMap,
    _auth: &WorkloadIdentityAuth,
    token: &str,
    endpoint: &str,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in request_headers.iter() {
        match name.as_str() {
            "authorization" | "api-key" | "x-api-key" => continue,
            "host" | "connection" | "transfer-encoding" | "content-length" => continue,
            // Don't pass through Copilot's own static headers from the inbound
            // request — we always emit our own canonical values below.
            "editor-version" | "copilot-integration-id" | "editor-plugin-version" => continue,
            _ => {
                headers.insert(name.clone(), value.clone());
            }
        }
    }

    // Both API-key and Entra modes use Authorization: Bearer for the unified
    // /openai/v1/ endpoint format. Azure OpenAI accepts API keys as Bearer tokens.
    // Copilot also uses Bearer (with the exchanged Copilot JWT).
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).context("Invalid token")?,
    );
    headers
        .entry("content-type")
        .or_insert(HeaderValue::from_static("application/json"));

    if is_copilot_endpoint(endpoint) {
        headers.insert("editor-version", HeaderValue::from_static(EDITOR_VERSION));
        headers.insert(
            "copilot-integration-id",
            HeaderValue::from_static(COPILOT_INTEGRATION_ID),
        );
        headers.insert(
            "editor-plugin-version",
            HeaderValue::from_static(EDITOR_PLUGIN_VERSION),
        );
        headers.insert(
            "user-agent",
            HeaderValue::from_static(copilot_auth::USER_AGENT),
        );
    }

    Ok(headers)
}

/// Returns true if the endpoint is a GitHub Copilot endpoint
/// (`https://api.githubcopilot.com`). Copilot is OpenAI-API + Anthropic-API
/// compatible *but* requires its own short-lived JWT (exchanged from the
/// user's GitHub OAuth token) and three static integration headers.
///
/// Matches the URL's parsed HOST exactly (not a substring of the whole URL
/// string) — a naive `.contains("api.githubcopilot.com")` would also match
/// an attacker-controlled endpoint like `https://api.githubcopilot.com.evil.tld`,
/// causing a real exchanged Copilot JWT to be sent to that attacker host.
pub fn is_copilot_endpoint(endpoint: &str) -> bool {
    endpoint_host(endpoint).as_deref() == Some("api.githubcopilot.com")
}

/// Parses `endpoint` as a URL and returns its lowercased host, or `None` if
/// it isn't a valid absolute URL. Used everywhere a host needs to be
/// compared EXACTLY (never `.contains()` on the raw string — see
/// `is_copilot_endpoint` doc comment for why that's unsafe).
pub(crate) fn endpoint_host(endpoint: &str) -> Option<String> {
    reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

/// True only for a hardcoded allowlist of genuine Azure AI/OpenAI host
/// suffixes. Workload Identity / IMDS mint a REAL Entra bearer token scoped
/// to the Azure AI audience — that token must never be sent to a host we
/// haven't verified is actually Azure-owned, or a misconfigured (or
/// malicious) provider endpoint could exfiltrate a live bearer token to an
/// attacker-controlled host. `token_for_endpoint` refuses to fall back to
/// WI/IMDS for any host that doesn't match one of these suffixes.
///
/// `ends_with` (not `contains`) is deliberate: DNS resolution follows the
/// domain hierarchy, so a string that genuinely *ends with*
/// `.openai.azure.com` can only resolve into Microsoft's real Azure
/// infrastructure — an attacker cannot make their own domain end with
/// someone else's suffix while controlling where it resolves.
pub(crate) fn is_azure_ai_host(host: &str) -> bool {
    const AZURE_AI_HOST_SUFFIXES: &[&str] = &[
        ".openai.azure.com",
        ".cognitiveservices.azure.com",
        ".services.ai.azure.com",
    ];
    AZURE_AI_HOST_SUFFIXES.iter().any(|suffix| host.ends_with(suffix))
}

/// Acquire the right auth token for a given upstream request.
///
/// - GitHub Copilot endpoints → exchanged Copilot JWT (cached, refreshed proactively).
/// - `upstream.provider_api_key` set (a non-default provider with its own
///   dev-mode key, e.g. a GitHub Models PAT) → used directly.
/// - Everything else → the router's normal Azure auth: an explicit dev-mode
///   API key or the shared Entra auth-sidecar are used as-is (operator
///   already supplied that specific credential for that specific provider —
///   the pre-existing, already-accepted single-default-provider behavior).
///   The AMBIENT Workload Identity / IMDS fallback is different: it mints a
///   real Entra token automatically from the cluster's own identity, so it
///   is only used against a verified Azure AI/OpenAI host
///   (`is_azure_ai_host`) — never against an arbitrary configured endpoint
///   that happens to have no key. Refused outright otherwise: better a
///   clean 502 than silently minting a real bearer token and sending it to
///   an unverified host.
///
/// Returning `Result<String>` lets the caller surface a clean 502 if the
/// Copilot token cache is uninitialised, the GitHub token is missing, or the
/// host isn't trusted for WI/IMDS — rather than panicking inside `forward()`.
pub async fn token_for_endpoint(
    auth: &WorkloadIdentityAuth,
    copilot: Option<&CopilotTokenCache>,
    upstream: &UpstreamConfig,
) -> Result<String> {
    let endpoint = upstream.endpoint.as_str();
    if is_copilot_endpoint(endpoint) {
        match copilot {
            Some(cache) => cache.get_jwt().await,
            None => anyhow::bail!(
                "Copilot endpoint configured but no CopilotTokenCache available — \
                 set COPILOT_GITHUB_TOKEN or mount /run/secrets/copilot-github-token"
            ),
        }
    } else if let Some(key) = upstream.provider_api_key.as_deref() {
        Ok(key.to_string())
    } else {
        // The host-verification gate below only matters for the AMBIENT
        // WI/IMDS fallback — a token minted automatically from the cluster's
        // own identity, without the operator directly handling it. Explicit
        // dev-mode credentials (a single global API key, or the shared
        // Entra auth-sidecar) are operator-supplied for a SPECIFIC provider
        // they configured; using them is the pre-existing, already-accepted
        // single-default-provider behavior and isn't gated here.
        if !auth.is_api_key_mode() && !auth.is_sidecar_mode() {
            let host = endpoint_host(endpoint).unwrap_or_default();
            if !is_azure_ai_host(&host) {
                anyhow::bail!(
                    "Refusing to send a Workload Identity / IMDS token to '{host}' — it \
                     isn't a recognized Azure AI endpoint (*.openai.azure.com / \
                     *.services.ai.azure.com / *.cognitiveservices.azure.com) and this \
                     provider has no configured key/token. Connect a direct API key for \
                     this provider, or point it at a genuine Azure AI endpoint."
                );
            }
        }
        auth.get_token(token_audience(endpoint)).await
    }
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
            metrics::record_tokens(
                &upstream.sandbox_name,
                &upstream.deployment,
                "input",
                input as u64,
            );
        }
        if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_i64()) {
            metrics::record_tokens(
                &upstream.sandbox_name,
                &upstream.deployment,
                "output",
                output as u64,
            );
        }
    }
}

/// Forward an inference request to the appropriate Azure backend.
///
/// - **Dev mode** (API key): Azure OpenAI `/openai/deployments/{model}/{path}?api-version=...`
/// - **AKS mode** (Workload Identity / IMDS): Foundry `/openai/v1/{path}` with model in body
#[allow(clippy::too_many_arguments)]
pub async fn forward(
    auth: &WorkloadIdentityAuth,
    copilot: Option<&CopilotTokenCache>,
    client: &Client,
    upstream: &UpstreamConfig,
    method: Method,
    path: &str,
    request_headers: &HeaderMap,
    request_body: Bytes,
) -> Result<(StatusCode, HeaderMap, Bytes)> {
    let start = Instant::now();

    let (upstream_url, body) = build_upstream_url(auth, upstream, path, request_body)?;

    let mode = if is_copilot_endpoint(&upstream.endpoint) {
        "copilot"
    } else if auth.is_api_key_mode() {
        "dev"
    } else {
        "foundry"
    };
    tracing::info!(sandbox = %upstream.sandbox_name, model = %upstream.deployment, mode = %mode, "Forwarding inference");

    let token = token_for_endpoint(auth, copilot, upstream)
        .await
        .context("Failed to acquire auth token")?;

    let headers = build_upstream_headers(request_headers, auth, &token, &upstream.endpoint)?;

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
    copilot: Option<Arc<CopilotTokenCache>>,
    client: Client,
    upstream: UpstreamConfig,
    path: &str,
    request_headers: HeaderMap,
    request_body: Bytes,
) -> Result<(
    StatusCode,
    HeaderMap,
    futures::stream::BoxStream<'static, Result<Bytes, reqwest::Error>>,
)> {
    // Inject stream_options.include_usage into request body so the final
    // SSE chunk contains a `usage` object with token counts. ONLY for
    // chat/completions — Anthropic Messages API (/v1/messages) rejects
    // `stream_options: Extra inputs are not permitted` (req_vrtx_*),
    // and the OpenAI Responses API (/v1/responses) also rejects
    // `Unknown parameter: 'stream_options.include_usage'` on Azure
    // Foundry (chat/completions accepts it, responses does not).
    // Anthropic streams already include usage in their `message_delta`
    // events; Responses streams include usage in the terminating event.
    let skip_usage_injection = path.contains("messages") || path.contains("responses");
    let body_with_usage = if skip_usage_injection {
        request_body
    } else {
        inject_stream_usage(request_body)
    };
    let (upstream_url, body) = build_upstream_url(&auth, &upstream, path, body_with_usage)?;

    tracing::info!(sandbox = %upstream.sandbox_name, model = %upstream.deployment, mode = "stream", "Forwarding SSE stream");

    let token = token_for_endpoint(&auth, copilot.as_deref(), &upstream)
        .await
        .context("Failed to acquire auth token")?;
    let headers = build_upstream_headers(&request_headers, &auth, &token, &upstream.endpoint)?;

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

    // On non-success, the upstream body is a short JSON error (not an SSE
    // stream). Eagerly drain it, log the contents (capped), and forward as a
    // single chunk so callers see the actual reason. Without this we only
    // see "status=413" and have to guess at causes (token cap? bytes cap?
    // schema break?). Cap at 4 KiB so a misbehaving upstream can't blow logs.
    if !status.is_success() {
        let body_bytes = response.bytes().await.unwrap_or_default();
        let preview = String::from_utf8_lossy(&body_bytes);
        let preview_trimmed: String = preview.chars().take(2048).collect();
        tracing::warn!(
            sandbox = %upstream.sandbox_name,
            status = %status.as_u16(),
            body_len = body_bytes.len(),
            body = %preview_trimmed,
            "Upstream returned non-success status"
        );
        let stream = futures::stream::once(async move { Ok::<_, reqwest::Error>(body_bytes) });
        return Ok((status, response_headers, stream.boxed()));
    }

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
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                    // OpenAI Responses API SSE events nest usage under
                    // `response.usage` on the final `response.completed`
                    // event; Chat Completions SSE puts it at the top
                    // level. Probe both shapes.
                    let usage = v.get("usage").or_else(|| v.get("response")?.get("usage"));
                    if let Some(usage) = usage {
                        // Record latency (stream complete)
                        let latency = start.elapsed();
                        metrics::INFERENCE_LATENCY
                            .with_label_values(&[&sandbox_name, &model])
                            .observe(latency.as_secs_f64());
                        // Token usage. OpenAI Chat Completions uses
                        // prompt_tokens/completion_tokens; OpenAI
                        // Responses uses input_tokens/output_tokens;
                        // Anthropic Messages (native /v1/messages) also
                        // uses input_tokens/output_tokens. Accept all.
                        let input_tokens = usage
                            .get("prompt_tokens")
                            .and_then(|v| v.as_i64())
                            .or_else(|| usage.get("input_tokens").and_then(|v| v.as_i64()));
                        let output_tokens = usage
                            .get("completion_tokens")
                            .and_then(|v| v.as_i64())
                            .or_else(|| usage.get("output_tokens").and_then(|v| v.as_i64()));
                        if let Some(input) = input_tokens {
                            metrics::record_tokens(&sandbox_name, &model, "input", input as u64);
                        }
                        if let Some(output) = output_tokens {
                            metrics::record_tokens(&sandbox_name, &model, "output", output as u64);
                        }
                    }
                }
            }
        }
        chunk
    });

    Ok((status, response_headers, metered.boxed()))
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
///
/// Matches the parsed HOST exactly — see `is_copilot_endpoint`'s doc comment
/// for why `.contains()` on the raw URL string is unsafe (URL-path/query
/// spoofing, e.g. `https://evil.tld/?models.github.ai`, or a subdomain like
/// `models.github.ai.evil.tld`, would otherwise also match).
fn is_github_models_endpoint(endpoint: &str) -> bool {
    matches!(
        endpoint_host(endpoint).as_deref(),
        Some("models.github.ai") | Some("models.inference.ai.azure.com")
    )
}

/// Build the upstream URL and optionally inject model into request body.
///
/// Routing rules:
///  - GitHub Copilot (`api.githubcopilot.com`): no path rewrite; OpenClaw
///    sends OpenAI-shape to `/chat/completions` and Anthropic-shape to
///    `/v1/messages`. We forward those paths unchanged.
///  - GitHub Models: no path rewrite either — OpenAI-compat under root.
///  - Foundry / Azure OpenAI (`is_azure_ai_host`): prepend `/openai/v1/`
///    (the unified endpoint format that works with both API-key and Entra
///    auth).
///  - Everything else (any other host — a "Custom" wizard endpoint, a
///    local in-cluster model deployed via the local-inference wizard,
///    Ollama, a standalone vLLM/llama.cpp server, ...): no path rewrite.
///    This used to be the Azure branch's default too (the `else` here was
///    unconditional), which silently broke any genuinely custom
///    OpenAI-compatible endpoint — confirmed live: a local AIKit/llama.cpp
///    deployment got `/openai/v1/chat/completions` prepended and 404'd,
///    since it only serves the plain `/v1/chat/completions` path. Only
///    hosts we've actually verified need the Azure-specific prefix get it;
///    every other custom endpoint is assumed to be plain OpenAI-compatible,
///    which is the more common and more conservative default for an
///    endpoint we don't recognize.
fn build_upstream_url(
    _auth: &WorkloadIdentityAuth,
    upstream: &UpstreamConfig,
    path: &str,
    request_body: Bytes,
) -> Result<(String, Bytes)> {
    let needs_azure_prefix = endpoint_host(&upstream.endpoint)
        .map(|host| is_azure_ai_host(&host))
        .unwrap_or(false);
    let url = if !needs_azure_prefix
        || is_github_models_endpoint(&upstream.endpoint)
        || is_copilot_endpoint(&upstream.endpoint)
    {
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
        // Azure Foundry's /v1/responses strict schema validator rejects
        // requests whose `input[]` contains items of `{type: "reasoning",
        // encrypted_content: "..."}` from a prior turn (returns 400
        // `invalid_payload` "data does not match the expected schema").
        // OpenAI's own Responses API accepts these — Azure tightened the
        // schema. Hermes (and any OpenAI Codex Responses client) replays
        // the reasoning blob by default for stateless continuity, then
        // only learns to disable it when the upstream returns
        // `invalid_encrypted_content`. Azure's `invalid_payload` doesn't
        // trigger that retry, so the client hangs in a loop. Strip the
        // reasoning items pre-emptively so the request schema is valid
        // and tool-calling continues without Hermes ever having to retry.
        //
        // `include: ["reasoning.encrypted_content"]` is also dropped —
        // requesting reasoning encryption on a stripped input is a no-op
        // for output and Azure rejects it on the input side anyway.
        if path.trim_start_matches('/').starts_with("responses")
            && !is_github_models_endpoint(&upstream.endpoint)
            && !is_copilot_endpoint(&upstream.endpoint)
            && let Some(obj) = body_json.as_object_mut()
        {
            if let Some(inputs) = obj.get_mut("input").and_then(|v| v.as_array_mut()) {
                inputs
                    .retain(|item| item.get("type").and_then(|t| t.as_str()) != Some("reasoning"));
            }
            if let Some(include) = obj.get_mut("include").and_then(|v| v.as_array_mut()) {
                include.retain(|s| s.as_str() != Some("reasoning.encrypted_content"));
            }
        }
        // Migrate legacy manual extended thinking to adaptive thinking for the
        // models that reject it. See `rewrite_unsupported_thinking`.
        rewrite_unsupported_thinking(&mut body_json);
        serde_json::to_vec(&body_json)?.into()
    } else {
        request_body
    };
    Ok((url, body))
}

/// True for Anthropic models that reject manual extended thinking
/// (`thinking: {type: "enabled", budget_tokens: N}`) with a 400 and instead
/// require adaptive thinking (`thinking: {type: "adaptive"}`).
///
/// Verified against Anthropic's adaptive-thinking docs: Opus 4.7, Opus 4.8,
/// Sonnet 5, and the Fable 5 / Mythos 5 / Mythos Preview family are
/// adaptive-only. Older models (Sonnet 4.5, Opus 4.5, Haiku 4.5, and the
/// 4.6 family) still accept — and Sonnet 4.5 / Opus 4.5 / Haiku 4.5 *require*
/// — the legacy `enabled` form, so they must NOT be rewritten.
pub(crate) fn model_requires_adaptive_thinking(model: &str) -> bool {
    // Normalise `.`/`_` separators to `-` so `claude-opus-4.8`,
    // `claude_opus_4_8`, and `claude-opus-4-8` all match identically.
    let m = model.to_ascii_lowercase().replace(['.', '_'], "-");
    m.contains("opus-4-8")
        || m.contains("opus-4-7")
        || m.contains("sonnet-5")
        || m.contains("fable-5")
        || m.contains("mythos-5")
        || m.contains("mythos-preview")
}

/// Rewrite a legacy manual-extended-thinking request body into the adaptive
/// form for models that no longer accept `{type: "enabled", budget_tokens}`.
///
/// OpenClaw (and other Anthropic-Messages clients baked into sandbox images)
/// still emit `thinking: {type: "enabled", budget_tokens: N}`. Opus 4.8 and
/// its adaptive-only peers 400 on that (`"thinking.type.enabled" is not
/// supported for this model. Use "thinking.type.adaptive"...`), which strands
/// every affected agent/team run. We convert it in-place to
/// `thinking: {type: "adaptive"}` (dropping `budget_tokens`); adaptive defaults
/// to ~high effort, so reasoning depth is preserved. The transform is a no-op
/// unless the body carries an `enabled` `thinking` block AND the effective
/// model is adaptive-only, so legacy-required models are left untouched.
pub(crate) fn rewrite_unsupported_thinking(body_json: &mut serde_json::Value) {
    let Some(obj) = body_json.as_object_mut() else {
        return;
    };
    let model = obj
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    if !model_requires_adaptive_thinking(&model) {
        return;
    }
    if let Some(thinking) = obj.get_mut("thinking").and_then(|t| t.as_object_mut())
        && thinking.get("type").and_then(|t| t.as_str()) == Some("enabled")
    {
        thinking.clear();
        thinking.insert("type".into(), serde_json::Value::String("adaptive".into()));
    }
}

// ── Retry-logic unit tests (R3) ──────────────────────────────────────────────
//
// Full retry behaviour is exercised end-to-end in
// `tests/proxy_fake_upstream.rs`. These inline tests pin the idempotency +
// status classifier so adding a new endpoint can't silently make a
// non-idempotent request retryable.

#[cfg(test)]
mod thinking_migration_tests {
    use super::{model_requires_adaptive_thinking, rewrite_unsupported_thinking};
    use serde_json::json;

    #[test]
    fn adaptive_only_models_detected() {
        for m in [
            "claude-opus-4.8",
            "claude-opus-4-8",
            "claude_opus_4_8",
            "claude-opus-4.7",
            "claude-sonnet-5",
            "claude-fable-5",
            "claude-mythos-5",
            "claude-mythos-preview",
        ] {
            assert!(model_requires_adaptive_thinking(m), "{m} should be adaptive-only");
        }
    }

    #[test]
    fn legacy_thinking_models_not_flagged() {
        // These still accept (and some require) enabled+budget_tokens — must
        // NOT be rewritten.
        for m in [
            "claude-sonnet-4.5",
            "claude-opus-4.5",
            "claude-haiku-4.5",
            "claude-sonnet-4.6",
            "claude-opus-4.6",
            "gpt-5.4",
            "",
        ] {
            assert!(!model_requires_adaptive_thinking(m), "{m:?} must not be flagged");
        }
    }

    #[test]
    fn rewrites_enabled_to_adaptive_for_opus_4_8() {
        let mut body = json!({
            "model": "claude-opus-4.8",
            "max_tokens": 16000,
            "thinking": { "type": "enabled", "budget_tokens": 10000 }
        });
        rewrite_unsupported_thinking(&mut body);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(
            body["thinking"].get("budget_tokens").is_none(),
            "budget_tokens must be dropped for adaptive thinking"
        );
    }

    #[test]
    fn leaves_legacy_model_thinking_untouched() {
        let mut body = json!({
            "model": "claude-sonnet-4.5",
            "thinking": { "type": "enabled", "budget_tokens": 8000 }
        });
        rewrite_unsupported_thinking(&mut body);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8000);
    }

    #[test]
    fn ignores_bodies_without_enabled_thinking() {
        // Already adaptive → no-op.
        let mut adaptive = json!({"model":"claude-opus-4.8","thinking":{"type":"adaptive"}});
        rewrite_unsupported_thinking(&mut adaptive);
        assert_eq!(adaptive["thinking"]["type"], "adaptive");
        // No thinking block → no-op, no panic.
        let mut none = json!({"model":"claude-opus-4.8","messages":[]});
        rewrite_unsupported_thinking(&mut none);
        assert!(none.get("thinking").is_none());
    }
}

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

#[cfg(test)]
mod host_matching_security_tests {
    use super::{
        UpstreamConfig, endpoint_host, is_azure_ai_host, is_copilot_endpoint,
        is_github_models_endpoint,
    };

    // ── is_copilot_endpoint: exact host, not substring ──────────────────────

    #[test]
    fn copilot_endpoint_matches_real_host() {
        assert!(is_copilot_endpoint("https://api.githubcopilot.com"));
        assert!(is_copilot_endpoint("https://api.githubcopilot.com/v1/chat/completions"));
    }

    #[test]
    fn copilot_endpoint_rejects_spoofed_subdomain() {
        // The exact attack a naive `.contains()` check would have missed:
        // an attacker-controlled domain that merely CONTAINS the real
        // Copilot host as a substring.
        assert!(!is_copilot_endpoint("https://api.githubcopilot.com.evil.tld"));
        assert!(!is_copilot_endpoint("https://evil.tld/api.githubcopilot.com"));
        assert!(!is_copilot_endpoint(
            "https://evil.tld/redirect?to=api.githubcopilot.com"
        ));
    }

    #[test]
    fn copilot_endpoint_rejects_malformed_url() {
        assert!(!is_copilot_endpoint("not a url at all"));
        assert!(!is_copilot_endpoint(""));
    }

    // ── is_github_models_endpoint: same class of fix ────────────────────────

    #[test]
    fn github_models_endpoint_matches_real_hosts() {
        assert!(is_github_models_endpoint("https://models.github.ai/inference"));
        assert!(is_github_models_endpoint(
            "https://models.inference.ai.azure.com/chat/completions"
        ));
    }

    #[test]
    fn github_models_endpoint_rejects_spoofed_subdomain() {
        assert!(!is_github_models_endpoint("https://models.github.ai.evil.tld"));
        assert!(!is_github_models_endpoint("https://evil.tld/models.github.ai"));
    }

    // ── is_azure_ai_host: the WI/IMDS credential-leak gate ───────────────────

    #[test]
    fn azure_ai_host_accepts_real_azure_suffixes() {
        assert!(is_azure_ai_host("contoso.openai.azure.com"));
        assert!(is_azure_ai_host("contoso.cognitiveservices.azure.com"));
        assert!(is_azure_ai_host("contoso.services.ai.azure.com"));
    }

    #[test]
    fn azure_ai_host_rejects_attacker_controlled_host() {
        // The core scenario the fix exists for: an operator (or a
        // misconfigured / malicious "additional provider" entry) points an
        // endpoint at a completely unrelated host with no key configured —
        // this must NEVER be treated as eligible for an ambient WI/IMDS
        // token.
        assert!(!is_azure_ai_host("evil.tld"));
        assert!(!is_azure_ai_host("attacker-controlled.com"));
    }

    #[test]
    fn azure_ai_host_rejects_suffix_spoof_attempt() {
        // A domain that merely CONTAINS the suffix, but doesn't END with
        // it, must not match (e.g. the suffix appearing mid-string via a
        // crafted subdomain label).
        assert!(!is_azure_ai_host("openai.azure.com.evil.tld"));
        assert!(!is_azure_ai_host("notcontoso.openai.azure.com.attacker.io"));
    }

    #[test]
    fn endpoint_host_parses_and_lowercases() {
        assert_eq!(
            endpoint_host("https://Contoso.OpenAI.Azure.Com/v1/chat"),
            Some("contoso.openai.azure.com".to_string())
        );
        assert_eq!(endpoint_host("not a url"), None);
    }

    // ── token_for_endpoint: the end-to-end gate ──────────────────────────────
    // (WorkloadIdentityAuth in ambient/no-key mode can't be constructed
    // without live WI/IMDS env in a unit test, so we only exercise the
    // pure host-classification helpers above; the integration is covered
    // live — see the session's E2E verification notes.)

    #[test]
    fn upstream_config_with_provider_api_key_bypasses_host_gate_entirely() {
        // Sanity: an UpstreamConfig carrying its own provider_api_key is
        // handled by the FIRST branch in token_for_endpoint (direct key),
        // never reaching the host-gate logic at all — confirmed by reading
        // the function; this test just pins the struct shape so a future
        // refactor can't silently drop the field.
        let upstream = UpstreamConfig {
            endpoint: "https://evil.tld".to_string(),
            deployment: "gpt-4o".to_string(),
            sandbox_name: "sbx".to_string(),
            provider_api_key: Some("direct-key".to_string()),
        };
        assert_eq!(upstream.provider_api_key.as_deref(), Some("direct-key"));
    }
}

#[cfg(test)]
mod build_upstream_url_tests {
    use super::{Bytes, UpstreamConfig, WorkloadIdentityAuth, build_upstream_url};

    fn upstream(endpoint: &str) -> UpstreamConfig {
        UpstreamConfig {
            endpoint: endpoint.to_string(),
            deployment: "test-model".to_string(),
            sandbox_name: "sbx".to_string(),
            provider_api_key: None,
        }
    }

    #[test]
    fn azure_ai_host_gets_openai_v1_prefix() {
        let auth = WorkloadIdentityAuth::new();
        let up = upstream("https://contoso.openai.azure.com");
        let (url, _) = build_upstream_url(&auth, &up, "/chat/completions", Bytes::new()).unwrap();
        assert_eq!(url, "https://contoso.openai.azure.com/openai/v1/chat/completions");
    }

    #[test]
    fn copilot_host_is_not_rewritten() {
        let auth = WorkloadIdentityAuth::new();
        let up = upstream("https://api.githubcopilot.com");
        let (url, _) = build_upstream_url(&auth, &up, "/chat/completions", Bytes::new()).unwrap();
        assert_eq!(url, "https://api.githubcopilot.com/chat/completions");
    }

    #[test]
    fn github_models_host_is_not_rewritten() {
        let auth = WorkloadIdentityAuth::new();
        let up = upstream("https://models.github.ai/inference");
        let (url, _) = build_upstream_url(&auth, &up, "/chat/completions", Bytes::new()).unwrap();
        assert_eq!(url, "https://models.github.ai/inference/chat/completions");
    }

    #[test]
    fn generic_custom_endpoint_is_not_rewritten() {
        // The exact regression this test guards against: a genuinely custom
        // OpenAI-compatible endpoint (a "Custom" wizard provider, a local
        // in-cluster model, Ollama, a standalone vLLM/llama.cpp server, ...)
        // previously fell into the `else` branch and got an incorrect
        // `/openai/v1/` prefix prepended, 404ing against servers that only
        // serve the plain `/v1/...` path. Confirmed live: a local AIKit/
        // llama.cpp deployment reached via
        // http://verify-e2e.kars-local-inference.svc.cluster.local 404'd
        // until this fix.
        let auth = WorkloadIdentityAuth::new();
        let up = upstream("http://verify-e2e.kars-local-inference.svc.cluster.local");
        let (url, _) = build_upstream_url(&auth, &up, "/v1/chat/completions", Bytes::new()).unwrap();
        assert_eq!(
            url,
            "http://verify-e2e.kars-local-inference.svc.cluster.local/v1/chat/completions"
        );
    }

    #[test]
    fn foundry_style_host_still_gets_prefix() {
        let auth = WorkloadIdentityAuth::new();
        let up = upstream("https://my-proj.services.ai.azure.com");
        let (url, _) = build_upstream_url(&auth, &up, "/chat/completions", Bytes::new()).unwrap();
        assert_eq!(url, "https://my-proj.services.ai.azure.com/openai/v1/chat/completions");
    }
}
