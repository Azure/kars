// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! mesh route handlers and router builder.
//!
//! Extracted from `routes/mod.rs` as part of the Q1 split.
//! Function bodies are byte-identical to the originals (verified by
//! `tools/item-manifest` drift-check).

use axum::Json;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bytes::Bytes;

use super::AppState;
use super::governance::agt_mesh_inbox;
use crate::errors;
use crate::mesh::MeshMetrics;

/// AGT mesh + relay routes — public (protected by E2E encryption + NetworkPolicy).
pub fn mesh_routes() -> Router<AppState> {
    Router::new()
        // Inter-agent mesh (E2E encrypted via AGT relay only — no plaintext HTTP)
        .route("/agt/mesh/inbox", get(agt_mesh_inbox))
        // AGT relay proxy (WebSocket + HTTP registry)
        // Both /agt/relay and /agt/relay/ws are accepted so callers can pass
        // either the proxy base URL or the upstream MeshClient `/ws` URL.
        .route("/agt/relay", get(agt_relay_proxy))
        .route("/agt/relay/ws", get(agt_relay_proxy))
        .route(
            "/agt/registry/{*path}",
            get(agt_registry_proxy)
                .post(agt_registry_proxy)
                .put(agt_registry_proxy)
                .delete(agt_registry_proxy),
        )
        // Blocklist (read-only, informational)
        .route("/blocklist/status", get(blocklist_status))
        .route("/blocklist/check", post(blocklist_check))
}

/// GET /agt/relay — WebSocket proxy to the self-hosted AgentMesh relay.
/// The plugin (UID 1000) can only reach localhost. The router (UID 1001) proxies
/// WebSocket connections to the relay at agentmesh-relay.agentmesh.svc.cluster.local:8765.
async fn agt_relay_proxy(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    let relay_base = std::env::var("AGT_RELAY_URL")
        .unwrap_or_else(|_| "ws://agentmesh-relay.agentmesh.svc.cluster.local:8765".into());

    // AGT upstream relay (Python FastAPI) only accepts WS upgrades on /ws.
    let needs_ws_suffix =
        !relay_base.trim_end_matches('/').ends_with("/ws") && !relay_base.contains("/ws?");
    let relay_url = if needs_ws_suffix {
        format!("{}/ws", relay_base.trim_end_matches('/'))
    } else {
        relay_base
    };

    let mesh_metrics = state.mesh_metrics.clone();
    ws.on_upgrade(move |client_socket| async move {
        relay_websocket_bridge(client_socket, &relay_url, &mesh_metrics).await;
    })
}

/// Bidirectional WebSocket bridge: client ↔ relay.
async fn relay_websocket_bridge(
    mut client_socket: WebSocket,
    relay_url: &str,
    mesh_metrics: &std::sync::Arc<MeshMetrics>,
) {
    use futures::sink::SinkExt;
    use futures::stream::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio_tungstenite::tungstenite;

    // Connect to the upstream relay with a 30-second timeout
    let upstream = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio_tungstenite::connect_async(relay_url),
    )
    .await
    {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => {
            tracing::error!(error = %e, url = %relay_url, "Failed to connect to AGT relay");
            // Send close frame so the client gets an error instead of hanging
            let _ = client_socket.close().await;
            return;
        }
        Err(_) => {
            tracing::error!(url = %relay_url, "AGT relay connection timed out (30s)");
            let _ = client_socket.close().await;
            return;
        }
    };

    mesh_metrics
        .sessions
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(url = %relay_url, "AGT relay WebSocket proxy connected");

    let (mut client_tx, mut client_rx) = client_socket.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    // Message counters (never log content — E2E encrypted)
    let outbound_count = std::sync::Arc::new(AtomicU64::new(0));
    let outbound_bytes = std::sync::Arc::new(AtomicU64::new(0));
    let inbound_count = std::sync::Arc::new(AtomicU64::new(0));
    let inbound_bytes = std::sync::Arc::new(AtomicU64::new(0));

    let out_count = outbound_count.clone();
    let out_bytes = outbound_bytes.clone();
    let in_count = inbound_count.clone();
    let in_bytes = inbound_bytes.clone();

    let sent_metrics = mesh_metrics.clone();
    let recv_metrics = mesh_metrics.clone();

    // Channel for messages that need to be sent on the upstream socket from
    // either bridge direction (writes from `relay_to_client` for upstream
    // auto-Pong, plus all client→relay forwarding from `client_to_relay`).
    // Without this, the upstream `SinkExt` would have to be locked across
    // tasks; an mpsc keeps the writer single-owned.
    let (upstream_send_tx, mut upstream_send_rx) =
        tokio::sync::mpsc::channel::<tungstenite::Message>(32);
    let upstream_writer = tokio::spawn(async move {
        while let Some(m) = upstream_send_rx.recv().await {
            if upstream_tx.send(m).await.is_err() {
                break;
            }
        }
    });

    // Forward: client → relay (outbound encrypted messages)
    let upstream_send_tx_c2r = upstream_send_tx.clone();
    let mut client_to_relay = tokio::spawn(async move {
        while let Some(Ok(msg)) = client_rx.next().await {
            let (tung_msg, size) = match msg {
                Message::Text(ref t) => (tungstenite::Message::Text(t.to_string().into()), t.len()),
                Message::Binary(ref b) => {
                    (tungstenite::Message::Binary(b.to_vec().into()), b.len())
                }
                Message::Ping(_p) => {
                    // Axum auto-Pongs to the client; nothing to forward upstream.
                    // Do NOT propagate client Pings to relay (would corrupt the
                    // relay's pong-tracking state machine).
                    continue;
                }
                Message::Pong(_p) => {
                    // Client-side keepalive Pong; relay tracks its own ping state.
                    continue;
                }
                Message::Close(_) => break,
            };
            out_count.fetch_add(1, Ordering::Relaxed);
            out_bytes.fetch_add(size as u64, Ordering::Relaxed);
            sent_metrics.messages_sent.fetch_add(1, Ordering::Relaxed);
            // Hex-dump first 128 bytes for traffic capture / E2E encryption proof.
            // The relay only ever sees ciphertext — readable plaintext here means encryption failed.
            let raw_bytes: Vec<u8> = match &tung_msg {
                tungstenite::Message::Text(t) => t.as_bytes().to_vec(),
                tungstenite::Message::Binary(b) => b.to_vec(),
                _ => vec![],
            };
            let hex_preview: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<String>>()
                .join(" ");
            let printable: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| {
                    if b.is_ascii_graphic() || *b == b' ' {
                        *b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            tracing::debug!(
                direction = "agent->relay",
                size,
                hex = %hex_preview,
                ascii = %printable,
                "AGT relay: TRAFFIC CAPTURE (outbound frame)"
            );
            if upstream_send_tx_c2r.send(tung_msg).await.is_err() {
                break;
            }
        }
    });

    // Forward: relay → client (inbound encrypted messages)
    let upstream_send_tx_r2c = upstream_send_tx.clone();
    let mut relay_to_client = tokio::spawn(async move {
        while let Some(Ok(msg)) = upstream_rx.next().await {
            let (axum_msg, size) = match msg {
                tungstenite::Message::Text(ref t) => (Message::Text(t.to_string().into()), t.len()),
                tungstenite::Message::Binary(ref b) => {
                    (Message::Binary(b.to_vec().into()), b.len())
                }
                tungstenite::Message::Ping(p) => {
                    // CRITICAL: tokio-tungstenite client mode does NOT auto-Pong.
                    // Relay implementations may send WebSocket pings and close
                    // the agent if no Pong is received quickly.
                    // We must respond on the upstream socket directly — the
                    // axum client side is not a viable Pong source because
                    // axum strips Pong frames from the application stream.
                    let _ = upstream_send_tx_r2c
                        .send(tungstenite::Message::Pong(p.to_vec().into()))
                        .await;
                    continue;
                }
                tungstenite::Message::Pong(_p) => {
                    // Relay-side Pong; nothing for the client to do.
                    continue;
                }
                tungstenite::Message::Close(_) => break,
                _ => continue,
            };
            in_count.fetch_add(1, Ordering::Relaxed);
            in_bytes.fetch_add(size as u64, Ordering::Relaxed);
            recv_metrics
                .messages_received
                .fetch_add(1, Ordering::Relaxed);
            // Hex-dump first 128 bytes of each inbound frame for traffic capture.
            let raw_bytes: Vec<u8> = match &msg {
                tungstenite::Message::Text(t) => t.as_bytes().to_vec(),
                tungstenite::Message::Binary(b) => b.to_vec(),
                _ => vec![],
            };
            let hex_preview: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<String>>()
                .join(" ");
            let printable: String = raw_bytes
                .iter()
                .take(128)
                .map(|b| {
                    if b.is_ascii_graphic() || *b == b' ' {
                        *b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            tracing::debug!(
                direction = "relay->agent",
                size,
                hex = %hex_preview,
                ascii = %printable,
                "AGT relay: TRAFFIC CAPTURE (inbound frame)"
            );
            if client_tx.send(axum_msg).await.is_err() {
                break;
            }
        }
    });

    // Wait for either direction to close, then abort the other to prevent
    // zombie connections that trigger "Failed to send message: closed connection".
    tokio::select! {
        _ = &mut client_to_relay => { relay_to_client.abort(); },
        _ = &mut relay_to_client => { client_to_relay.abort(); },
    }
    // Drop the upstream sender channel so the writer task exits cleanly.
    drop(upstream_send_tx);
    upstream_writer.abort();

    let out_n = outbound_count.load(Ordering::Relaxed);
    let out_b = outbound_bytes.load(Ordering::Relaxed);
    let in_n = inbound_count.load(Ordering::Relaxed);
    let in_b = inbound_bytes.load(Ordering::Relaxed);
    tracing::info!(
        outbound_messages = out_n,
        outbound_bytes = out_b,
        inbound_messages = in_n,
        inbound_bytes = in_b,
        "AGT relay WebSocket proxy disconnected"
    );
}

/// GET/POST /agt/registry/* — HTTP proxy to the self-hosted AgentMesh registry.
/// Proxies all registry API calls so the plugin (UID 1000, localhost-only) can
/// reach the registry service via the router.
async fn agt_registry_proxy(
    State(state): State<AppState>,
    Path(path): Path<String>,
    query: axum::extract::RawQuery,
    headers: HeaderMap,
    method: axum::http::Method,
    body: Bytes,
) -> impl IntoResponse {
    let registry_url = std::env::var("AGT_REGISTRY_URL")
        .unwrap_or_else(|_| "http://agentmesh-registry.agentmesh.svc.cluster.local:8080".into());

    // Allowlist valid AGT registry API paths — prevent path traversal.
    // Paths arrive as the wildcard after /agt/registry/, e.g. "v1/agents".
    let valid_prefixes = ["health", "v1/"];
    let path_valid =
        valid_prefixes.iter().any(|prefix| path.starts_with(prefix)) && !path.contains("..");
    if !path_valid {
        return errors::flat(StatusCode::BAD_REQUEST, "Invalid registry path").into_response();
    }

    // AGT SDK sends fully-qualified "v1/agents", "v1/discover" etc.; forward
    // verbatim because AGT registry exposes /v1/agents, NOT /v1/v1/agents.
    let mut url = format!("{}/{}", registry_url.trim_end_matches('/'), path);
    // Forward query parameters (critical for search/lookup)
    if let Some(qs) = query.0 {
        url.push('?');
        url.push_str(&qs);
    }

    // ── Identity-claim enforcement ──────────────────────────────────────
    // Trust in AzureClaw's mesh is keyed by sandbox display_name (which is
    // K8s admission-unique within a cluster). The receiver-side KNOCK
    // handler resolves a peer AMID → display_name via the registry and
    // checks it against `parentTrustedNames`. That trust is only sound if
    // the registry guarantees a sandbox CANNOT claim another sandbox's
    // name. This block is that guarantee: every register/capabilities
    // mutation through this router is gated to identities matching the
    // local SANDBOX_NAME. Self-asserted display_name + capability claims
    // from a compromised sandbox cannot impersonate a peer.
    if method == axum::http::Method::POST && path == "v1/agents" {
        if let Err((status, msg)) = enforce_identity_claim(&path, &body) {
            tracing::warn!(
                path = %path,
                reason = %msg,
                "AGT registry proxy: identity-claim enforcement rejected mutation"
            );
            return errors::flat(status, msg).into_response();
        }
    }

    // Defense-in-depth retry. The SDK has a retry budget, but the router is
    // the single network egress for sandboxes and benefits from its own retry
    // for two reasons:
    //   1. dev port-forward (host.docker.internal:18080) sometimes returns
    //      502 for the very first connection attempt after a kubectl
    //      port-forward restart;
    //   2. the AGT relay's `proxy_relay` path next door already has retry
    //      semantics, and parity here keeps router behaviour predictable.
    //
    // Policy: same as SDK — GET/HEAD always retry; allowlisted POST paths
    // (idempotent registry endpoints) retry; transient statuses 408/429/
    // 502/503/504 + network errors. Cap 3 attempts, ~2s elapsed.
    const RETRY_STATUSES: &[u16] = &[408, 429, 502, 503, 504];
    const POST_RETRY_PATHS: &[&str] = &["v1/agents", "v1/sessions"];
    let is_idempotent = method == axum::http::Method::GET || method == axum::http::Method::HEAD;
    let post_retry = method == axum::http::Method::POST
        && POST_RETRY_PATHS
            .iter()
            .any(|p| path == *p || path.starts_with(&format!("{}?", p)));
    let retry_allowed = is_idempotent || post_retry;
    let max_attempts: u32 = if retry_allowed { 3 } else { 1 };
    let total_budget = std::time::Duration::from_millis(2000);
    let started_at = std::time::Instant::now();

    let body_clone = body.to_vec();
    let mut last_err: Option<reqwest::Error> = None;
    let mut last_response: Option<(StatusCode, axum::body::Bytes)> = None;

    for attempt in 1..=max_attempts {
        let mut req = state.client.request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
            &url,
        );
        if let Some(ct) = headers.get("content-type") {
            req = req.header("content-type", ct);
        }
        if !body_clone.is_empty() {
            req = req.body(body_clone.clone());
        }

        match req.timeout(std::time::Duration::from_secs(10)).send().await {
            Ok(resp) => {
                let status =
                    StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let resp_body = resp.bytes().await.unwrap_or_default();

                let should_retry = retry_allowed
                    && attempt < max_attempts
                    && RETRY_STATUSES.contains(&status.as_u16())
                    && started_at.elapsed() < total_budget;

                if should_retry {
                    let wait = std::time::Duration::from_millis(
                        100u64.saturating_mul(1u64 << (attempt - 1)),
                    );
                    if started_at.elapsed() + wait > total_budget {
                        last_response = Some((status, resp_body));
                        break;
                    }
                    tracing::warn!(
                        url = %url,
                        status = %status,
                        attempt,
                        max_attempts,
                        "AGT registry proxy: retrying transient status"
                    );
                    last_response = Some((status, resp_body));
                    tokio::time::sleep(wait).await;
                    continue;
                }

                // Log reputation submission failures so we can diagnose why
                // feedback_count stays 0 (the SDK silently returns false).
                if path == "registry/reputation"
                    && method == axum::http::Method::POST
                    && !status.is_success()
                {
                    let error_text = std::str::from_utf8(&resp_body).unwrap_or("<binary>");
                    tracing::warn!(
                        status = %status,
                        error = %error_text,
                        "AGT reputation submission failed"
                    );
                }

                return (
                    status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    resp_body,
                )
                    .into_response();
            }
            Err(e) => {
                let should_retry =
                    retry_allowed && attempt < max_attempts && started_at.elapsed() < total_budget;
                if should_retry {
                    let wait = std::time::Duration::from_millis(
                        100u64.saturating_mul(1u64 << (attempt - 1)),
                    );
                    if started_at.elapsed() + wait > total_budget {
                        last_err = Some(e);
                        break;
                    }
                    tracing::warn!(
                        url = %url,
                        error = %e,
                        attempt,
                        max_attempts,
                        "AGT registry proxy: retrying network error"
                    );
                    last_err = Some(e);
                    tokio::time::sleep(wait).await;
                    continue;
                }
                last_err = Some(e);
                break;
            }
        }
    }

    if let Some((status, resp_body)) = last_response {
        return (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            resp_body,
        )
            .into_response();
    }
    let err_msg = last_err
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unknown".into());
    tracing::warn!(url = %url, error = %err_msg, "AGT registry proxy failed");
    errors::flat(
        StatusCode::BAD_GATEWAY,
        format!("Registry unreachable: {}", err_msg),
    )
    .into_response()
}

/// Identity-claim enforcement for AGT registry mutations.
///
/// AzureClaw mesh trust is name-based: receivers verify peers by resolving
/// `display_name` through the registry and matching against a parent-seeded
/// trust set. That guarantee only holds if a sandbox cannot register another
/// sandbox's name. This function vets `POST /v1/agents` bodies, allowing only:
///   - `display_name` equal to the router's `SANDBOX_NAME` env (or absent)
///   - `capabilities` drawn from `{SANDBOX_NAME, "azureclaw-agent",
///     "task-execution"}` plus any prefixed with `azureclaw:` (reserved for
///     future controller-issued attestations)
///
/// Returns `Err((status, message))` if the claim must be rejected. An empty
/// body (which can occur for some capability update shapes) is allowed and
/// passes through to the upstream registry.
pub(super) fn enforce_identity_claim(path: &str, body: &Bytes) -> Result<(), (StatusCode, String)> {
    let sandbox_name = match std::env::var("SANDBOX_NAME") {
        Ok(v) if !v.is_empty() && v != "unknown" => v,
        _ => {
            // Fail closed: a router without a known SANDBOX_NAME has no way
            // to authoritatively gate identity claims. In dev/test we
            // expect SANDBOX_NAME to always be set by the controller.
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "router cannot enforce identity claims: SANDBOX_NAME not set".into(),
            ));
        }
    };

    if body.is_empty() {
        return Ok(());
    }

    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            // Don't leak bytes; let upstream registry reject the bad body.
            return Ok(());
        }
    };

    let claimed_display_name = parsed
        .get("metadata")
        .and_then(|m| m.get("display_name"))
        .and_then(|v| v.as_str())
        .or_else(|| parsed.get("display_name").and_then(|v| v.as_str()));

    if let Some(name) = claimed_display_name
        && name != sandbox_name
    {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "display_name '{}' does not match router SANDBOX_NAME '{}' — \
                 sandboxes may only register under their own admission-bound name",
                name, sandbox_name
            ),
        ));
    }

    if let Some(caps) = parsed.get("capabilities").and_then(|v| v.as_array()) {
        for cap in caps {
            let Some(s) = cap.as_str() else {
                continue;
            };
            let allowed = s == sandbox_name
                || s == "azureclaw-agent"
                || s == "task-execution"
                || s.starts_with("azureclaw:");
            if !allowed {
                return Err((
                    StatusCode::FORBIDDEN,
                    format!(
                        "capability '{}' is not permitted — sandboxes may \
                         only assert capabilities matching SANDBOX_NAME or \
                         the AzureClaw default set ({{azureclaw-agent, \
                         task-execution, azureclaw:*}})",
                        s
                    ),
                ));
            }
        }
    }

    let _ = path;
    Ok(())
}

/// Look up an agent's DID from the AGT registry by searching for its sandbox name.
pub(super) async fn lookup_parent_amid(
    client: &reqwest::Client,
    registry_url: &str,
    sandbox_name: &str,
) -> Option<String> {
    let base = registry_url.trim_end_matches('/');
    lookup_parent_amid_agt(client, base, sandbox_name).await
}

/// AGT registry variant: `GET /v1/discover?capability=<name>` returns
/// `{results: [{did, capabilities, last_seen, ...}, ...]}` — display name lives
/// in the per-agent record, not in the discover hit, so we have to fetch each
/// candidate's `/v1/agents/{did}` record to disambiguate. In practice the
/// registry returns at most a handful of hits for any given sandbox name.
async fn lookup_parent_amid_agt(
    client: &reqwest::Client,
    base: &str,
    sandbox_name: &str,
) -> Option<String> {
    let resp = client
        .get(&format!(
            "{}/v1/discover?capability={}&limit=10",
            base, sandbox_name
        ))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let hits = body.get("results")?.as_array()?.clone();

    let mut best: Option<(String, String)> = None;
    for hit in hits.iter() {
        let Some(did) = hit.get("did").and_then(|v| v.as_str()) else {
            continue;
        };
        let rec_resp = client
            .get(&format!("{}/v1/agents/{}", base, did))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await;
        let display_ok = match rec_resp {
            Ok(r) if r.status().is_success() => r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|rec| {
                    rec.get("metadata")
                        .and_then(|m| m.get("display_name"))
                        .and_then(|n| n.as_str())
                        .map(|s| s == sandbox_name)
                })
                .unwrap_or(false),
            _ => false,
        };
        if !display_ok {
            continue;
        }
        let last_seen = hit
            .get("last_seen")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        match &best {
            None => best = Some((did.to_string(), last_seen)),
            Some((_, prev_seen)) if last_seen > *prev_seen => {
                best = Some((did.to_string(), last_seen));
            }
            _ => {}
        }
    }
    best.map(|(did, _)| did)
}

/// GET /blocklist/status — blocklist health and domain count.
async fn blocklist_status(State(state): State<AppState>) -> impl IntoResponse {
    let count = state.blocklist.len().await;
    Json(serde_json::json!({
        "enabled": count > 0 || std::env::var("BLOCKLIST_ENABLED").unwrap_or_else(|_| "true".into()) == "true",
        "domain_count": count,
        "learn_mode": state.blocklist.is_learn_mode(),
        "learned_domains": state.blocklist.learned_count().await,
    }))
}

/// POST /blocklist/check — check if a domain/URL is blocked.
async fn blocklist_check(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let input = body
        .get("domain")
        .or_else(|| body.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if input.is_empty() {
        return errors::flat(StatusCode::BAD_REQUEST, "Provide 'domain' or 'url' field")
            .into_response();
    }

    match state.blocklist.is_blocked(input).await {
        crate::blocklist::BlockResult::Blocked { reason, domain } => (
            StatusCode::OK,
            Json(serde_json::json!({
                "blocked": true,
                "domain": domain,
                "reason": reason,
            })),
        )
            .into_response(),
        crate::blocklist::BlockResult::Allowed => (
            StatusCode::OK,
            Json(serde_json::json!({
                "blocked": false,
                "domain": input,
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize all enforcement tests: they mutate the SANDBOX_NAME process
    // env, which would race under cargo's default parallel test runner.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn with_env<R>(name: &str, value: &str, f: impl FnOnce() -> R) -> R {
        let _lock = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(name).ok();
        // SAFETY: env mutation gated by ENV_GUARD; tests are the only callers.
        unsafe { std::env::set_var(name, value) };
        let result = f();
        match prev {
            Some(v) => unsafe { std::env::set_var(name, v) },
            None => unsafe { std::env::remove_var(name) },
        }
        result
    }

    #[test]
    fn enforce_accepts_matching_display_name() {
        with_env("SANDBOX_NAME", "azureclawtest", || {
            let body = Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "amid": "ABC",
                    "display_name": "azureclawtest",
                    "capabilities": ["azureclaw-agent", "task-execution", "azureclawtest"],
                }))
                .unwrap(),
            );
            assert!(enforce_identity_claim("v1/agents", &body).is_ok());
        });
    }

    #[test]
    fn enforce_rejects_squatted_display_name() {
        with_env("SANDBOX_NAME", "viz", || {
            let body = Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "amid": "ABC",
                    "display_name": "azureclawtest",
                }))
                .unwrap(),
            );
            let err = enforce_identity_claim("v1/agents", &body).unwrap_err();
            assert_eq!(err.0, StatusCode::FORBIDDEN);
            assert!(err.1.contains("display_name"));
        });
    }

    #[test]
    fn enforce_rejects_squatted_capability() {
        with_env("SANDBOX_NAME", "viz", || {
            let body = Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "amid": "ABC",
                    "display_name": "viz",
                    "capabilities": ["azureclaw-agent", "azureclawtest"],
                }))
                .unwrap(),
            );
            let err = enforce_identity_claim("v1/agents", &body).unwrap_err();
            assert_eq!(err.0, StatusCode::FORBIDDEN);
            assert!(err.1.contains("capability"));
        });
    }

    #[test]
    fn enforce_allows_default_capability_set() {
        with_env("SANDBOX_NAME", "viz", || {
            let body = Bytes::from(serde_json::to_vec(&serde_json::json!({
                "amid": "ABC",
                "display_name": "viz",
                "capabilities": ["azureclaw-agent", "task-execution", "viz", "azureclaw:internal"],
            })).unwrap());
            assert!(enforce_identity_claim("v1/agents", &body).is_ok());
        });
    }

    #[test]
    fn enforce_allows_missing_display_name() {
        // Some capability-update payloads omit display_name.
        with_env("SANDBOX_NAME", "viz", || {
            let body = Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "amid": "ABC",
                    "capabilities": ["task-execution"],
                }))
                .unwrap(),
            );
            assert!(enforce_identity_claim("v1/agents", &body).is_ok());
        });
    }

    #[test]
    fn enforce_fails_closed_without_sandbox_name() {
        let _lock = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("SANDBOX_NAME").ok();
        unsafe { std::env::remove_var("SANDBOX_NAME") };
        let body = Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "display_name": "anything",
            }))
            .unwrap(),
        );
        let err = enforce_identity_claim("v1/agents", &body).unwrap_err();
        assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
        if let Some(v) = prev {
            unsafe { std::env::set_var("SANDBOX_NAME", v) };
        }
    }

    #[test]
    fn enforce_passes_empty_body() {
        with_env("SANDBOX_NAME", "viz", || {
            assert!(enforce_identity_claim("v1/agents", &Bytes::new()).is_ok());
        });
    }

    #[test]
    fn enforce_passes_invalid_json() {
        // Invalid JSON is forwarded so upstream registry can return its own error.
        with_env("SANDBOX_NAME", "viz", || {
            let body = Bytes::from_static(b"not json");
            assert!(enforce_identity_claim("v1/agents", &body).is_ok());
        });
    }
}
