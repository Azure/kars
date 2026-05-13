// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! egress route handlers and router builder.
//!
//! Extracted from `routes/mod.rs` as part of the Q1 split.
//! Function bodies are byte-identical to the originals (verified by
//! `tools/item-manifest` drift-check).

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};

use super::AppState;
use crate::errors;

pub fn egress_routes() -> Router<AppState> {
    // Slice 5c.1 removed the in-process approval surface:
    //   POST /egress/approve, POST /egress/deny, GET /egress/pending,
    //   and POST /egress/enforce (it mutated the runtime allowlist
    //   bypassing the signed bundle). The allowlist is now exclusively
    //   sourced from the controller-published, cosign-verified bundle
    //   mounted at `${EGRESS_ALLOWLIST_DIR}/allowlist.json`. The
    //   slice-5a `BlockedBuffer` (`GET /egress/blocked` via
    //   `/egress/learned/blocked`) remains the observability surface.
    //   Operator-driven runtime approvals will land in Slice 5c.2 as a
    //   separate `EgressApproval` CRD.
    Router::new()
        .route("/egress/learn", post(egress_learn_toggle))
        .route("/egress/learned", get(egress_learned))
        .route("/egress/learned/clear", post(egress_learned_clear))
        .route("/egress/learned/blocked", get(egress_learned_blocked))
        .route("/egress/fetch", post(egress_fetch))
        .route("/egress/allowlist", get(egress_allowlist))
}

/// GET /egress/learned/blocked — list blocked egress attempts captured in
/// enforce mode. S12.f: sibling of `/egress/learned` (allowed-observation
/// learn buffer). Hostname + port only — no paths, headers, or payload data.
/// RBAC: mounted in the `protected` router that already gates `/egress/*`
/// behind `Authorization: Bearer <admin-token>` — no new auth path.
async fn egress_learned_blocked(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let limit = headers
        .get("x-azureclaw-limit")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(256)
        .min(4096);
    let entries = state.blocked_egress.snapshot(limit);
    Json(serde_json::json!({
        "total": state.blocked_egress.len(),
        "count": entries.len(),
        "entries": entries,
    }))
}

/// GET /egress/learned — list all domains observed during learn mode.
async fn egress_learned(State(state): State<AppState>) -> impl IntoResponse {
    let domains = state.blocklist.get_learned_domains().await;
    Json(serde_json::json!({
        "learn_mode": state.blocklist.is_learn_mode(),
        "count": domains.len(),
        "domains": domains,
    }))
}

/// POST /egress/learn — toggle learn mode at runtime.
async fn egress_learn_toggle(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    state.blocklist.set_learn_mode(enabled);
    Json(serde_json::json!({
        "learn_mode": enabled,
    }))
}

/// POST /egress/learned/clear — clear learned domains (after export/review).
async fn egress_learned_clear(State(state): State<AppState>) -> impl IntoResponse {
    state.blocklist.clear_learned().await;
    Json(serde_json::json!({
        "status": "cleared",
        "learn_mode": state.blocklist.is_learn_mode(),
    }))
}

/// POST /egress/fetch — audited, allowlist-checked HTTP proxy for sandbox egress.
///
/// Security model:
/// 1. Blocklist → hard deny (threat intelligence)
/// 2. Allowlist → approved domains pass through
/// 3. Unknown domain → deny + create pending approval request
/// 4. Learn mode → log + allow (discovery phase only)
/// 5. Private/internal IP → always deny (SSRF protection)
/// 6. Redirects → returned as-is (never followed)
/// 7. Response capped at 2 MB
///
/// Body: { "url": "https://...", "method": "GET"|"POST"|..., "headers": {}, "body": "..." }
/// Returns: { "status": <http_code>, "headers": {...}, "body": "..." }
async fn egress_fetch(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
    let req_body = req.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let req_headers = req.get("headers").and_then(|v| v.as_object());

    if url.is_empty() {
        return errors::flat(StatusCode::BAD_REQUEST, "Missing 'url' field").into_response();
    }

    // SSRF protection: reject requests to localhost/private IPs
    if let Ok(parsed) = reqwest::Url::parse(url) {
        if let Some(host) = parsed.host_str() {
            let is_private = match host.parse::<std::net::IpAddr>() {
                Ok(ip) => crate::forward_proxy::is_private_ip(&ip),
                Err(_) => {
                    // It's a hostname — check for common local hostnames
                    let h = host.to_lowercase();
                    h == "localhost" || h.ends_with(".local") || h.ends_with(".internal")
                }
            };
            if is_private {
                tracing::warn!(url = %url, "Egress fetch blocked: private/internal target");
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": "Cannot fetch from private/internal addresses",
                        "url": url,
                    })),
                )
                    .into_response();
            }
        }
    }

    let sandbox: &str = &state.sandbox_name;

    // Check egress access: blocklist → allowlist → pending
    if let Err(reason) = state.blocklist.check_egress(url, sandbox).await {
        tracing::warn!(url = %url, reason = %reason, "Egress fetch denied");
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({
            "error": reason,
            "url": url,
            "action": "Run 'azureclaw egress <name> --pending' to see pending requests, then 'azureclaw egress <name> --approve <domain>' to allow.",
        }))).into_response();
    }

    // Record in learn mode
    state.blocklist.record_learned(url).await;

    tracing::info!(url = %url, method = %method, "Egress fetch proxied");

    // Build and send the request
    let http_method = match method.to_uppercase().as_str() {
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        "HEAD" => reqwest::Method::HEAD,
        _ => reqwest::Method::GET,
    };

    let mut request = state.client.request(http_method, url);

    // Allowlisted request headers — block dangerous ones
    const BLOCKED_REQ_HEADERS: &[&str] = &[
        "host",
        "transfer-encoding",
        "content-length",
        "proxy-authorization",
        "proxy-connection",
    ];
    if let Some(headers) = req_headers {
        for (k, v) in headers {
            let lower = k.to_lowercase();
            if BLOCKED_REQ_HEADERS.contains(&lower.as_str()) {
                continue;
            }
            if let Some(val) = v.as_str()
                && let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            {
                request = request.header(name, val);
            }
        }
    }

    if !req_body.is_empty() {
        request = request.body(req_body.to_string());
    }

    const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024; // 2 MB

    match request
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // Strip sensitive response headers
            const STRIPPED_RESP_HEADERS: &[&str] = &[
                "set-cookie",
                "authorization",
                "x-api-key",
                "x-auth-token",
                "proxy-authenticate",
                "proxy-authorization",
                "www-authenticate",
            ];
            let resp_headers: serde_json::Map<String, serde_json::Value> = resp
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    let name = k.as_str();
                    if STRIPPED_RESP_HEADERS.contains(&name) {
                        None
                    } else {
                        v.to_str().ok().map(|val| {
                            (name.to_string(), serde_json::Value::String(val.to_string()))
                        })
                    }
                })
                .collect();
            // Cap response body to prevent OOM
            let body_bytes = resp.bytes().await.unwrap_or_default();
            let body = if body_bytes.len() > MAX_RESPONSE_BYTES {
                let truncated = String::from_utf8_lossy(&body_bytes[..MAX_RESPONSE_BYTES]);
                format!(
                    "{}... [truncated at {} bytes]",
                    truncated, MAX_RESPONSE_BYTES
                )
            } else {
                String::from_utf8_lossy(&body_bytes).into_owned()
            };
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": status,
                    "headers": resp_headers,
                    "body": body,
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "Egress fetch failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": format!("Request failed: {}", e),
                    "url": url,
                })),
            )
                .into_response()
        }
    }
}

/// GET /egress/allowlist — list approved egress domains.
async fn egress_allowlist(State(state): State<AppState>) -> impl IntoResponse {
    let domains = state.blocklist.get_allowlist().await;
    Json(serde_json::json!({
        "count": domains.len(),
        "domains": domains,
    }))
}

// ==========================================================================
