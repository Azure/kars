//! governance route handlers and router builder.
//!
//! Extracted from `routes/mod.rs` as part of the Q1 split.
//! Function bodies are byte-identical to the originals (verified by
//! `tools/item-manifest` drift-check).

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};

use super::mesh::lookup_parent_amid;
use super::{AppState, extract_admin_token};
use crate::errors;

/// AGT governance routes that expose sensitive data — require admin token.
pub fn sensitive_agt_routes() -> Router<AppState> {
    Router::new()
        // Policy evaluation
        .route("/agt/evaluate", post(agt_evaluate))
        // Trust management
        .route("/agt/trust", get(agt_trust_list))
        .route(
            "/agt/trust/{agent_id}",
            get(agt_trust_get).delete(agt_trust_delete),
        )
        // Audit log
        .route("/agt/audit", get(agt_audit))
        .route("/agt/audit/verify", get(agt_audit_verify))
        // Status (exposes trust scores, audit entries, inbox count)
        .route("/agt/status", get(agt_status))
        // Trust update (plugin pushes reputation changes to the router's trust store)
        .route("/agt/trust", post(agt_trust_update))
        // Ed25519 signing counter (plugin pushes signed/verified/rejected counts)
        .route("/agt/signing-counter", post(agt_signing_counter))
        // Dynamic rate-limit update (admin only)
        .route(
            "/agt/rate-limit",
            put(agt_rate_limit_update).get(agt_rate_limit_get),
        )
        // Registry reputation (proxied from agentmesh-registry)
        .route("/agt/reputation", get(agt_reputation))
}

async fn agt_evaluate(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let agent_id = body
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(&state.sandbox_name);
    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let extra_context = body.get("context").cloned();

    // Per-tool sliding window rate limit (AGT McpSlidingRateLimiter)
    // Extract tool name from action format "tool:exec_command" or "tool:http_fetch"
    if let Some(tool_name) = action.strip_prefix("tool:") {
        let (allowed, retry_after) = state.governance.check_tool_rate(tool_name);
        if !allowed {
            return Json(serde_json::json!({
                "allowed": false,
                "reason": format!("per-tool rate limit exceeded for '{}'", tool_name),
                "retry_after_secs": retry_after,
                "decision": "Deny"
            }))
            .into_response();
        }
    }

    let result = state
        .governance
        .evaluate(agent_id, action, extra_context.as_ref());
    Json(result).into_response()
}

/// GET /agt/trust — list all known agent trust states.
async fn agt_trust_list(State(state): State<AppState>) -> impl IntoResponse {
    let agents = state.governance.all_trust_scores();
    Json(serde_json::json!({ "agents": agents }))
}

/// GET /agt/trust/:agent_id — get trust state for a specific agent.
async fn agt_trust_get(
    State(state): State<AppState>,
    axum::extract::Path(agent_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let ts = state.governance.trust.get_trust_score(&agent_id);
    Json(serde_json::json!({
        "agent_id": ts.agent_id,
        "score": ts.score,
        "tier": crate::governance::tier_label(ts.score),
        "interactions": ts.interactions,
    }))
}

/// DELETE /agt/trust/:agent_id — remove trust state for a specific agent.
async fn agt_trust_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(agent_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    // Trust mutations require admin token even from localhost — prevents sandbox
    // (UID 1000) from forging peer trust scores via the localhost auth exemption.
    if let Some(ref expected) = state.admin_token {
        let provided = extract_admin_token(&headers);
        match provided.as_deref() {
            Some(tok) if crate::handoff::constant_time_eq(tok.as_bytes(), expected.as_bytes()) => {}
            _ => {
                tracing::warn!(
                    "DELETE /agt/trust/{} denied: missing or invalid admin token",
                    agent_id
                );
                return errors::flat(
                    StatusCode::FORBIDDEN,
                    "Admin token required for trust mutations",
                );
            }
        }
    }

    let result = state.governance.delete_trust(&agent_id);
    (StatusCode::OK, Json(result))
}

/// GET /agt/audit — get the full audit log.
async fn agt_audit(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.governance.audit_json())
}

/// GET /agt/audit/verify — verify hash-chain integrity.
async fn agt_audit_verify(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.governance.audit_verify_json())
}

/// GET /agt/mesh/inbox — read received messages.
pub(super) async fn agt_mesh_inbox(State(state): State<AppState>) -> impl IntoResponse {
    let messages = state.inbox.peek().await;
    Json(serde_json::json!({
        "messages": messages,
        "count": messages.len()
    }))
}

/// GET /agt/status — overall governance status.
async fn agt_status(State(state): State<AppState>) -> impl IntoResponse {
    let inbox = state.inbox.peek().await;
    let blocklist_len = state.blocklist.len().await;

    // Get base status from native governance (policy, trust, audit, metrics)
    let mut result = state.governance.status_json();

    if let Some(obj) = result.as_object_mut() {
        obj.insert("inbox_messages".into(), serde_json::json!(inbox.len()));
        obj.insert("blocklist_domains".into(), serde_json::json!(blocklist_len));
        obj.insert(
            "egress_learn_mode".into(),
            serde_json::json!(state.blocklist.is_learn_mode()),
        );
        obj.insert(
            "egress_learned_domains".into(),
            serde_json::json!(state.blocklist.learned_count().await),
        );
        obj.insert(
            "mesh_sessions".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .sessions
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
        obj.insert(
            "mesh_messages_sent".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .messages_sent
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
        obj.insert(
            "mesh_messages_received".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .messages_received
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
        obj.insert(
            "trust_updates".into(),
            serde_json::json!(
                state
                    .mesh_metrics
                    .trust_updates
                    .load(std::sync::atomic::Ordering::Relaxed)
            ),
        );
    }

    Json(result).into_response()
}

/// POST /agt/signing-counter — plugin pushes Ed25519 signing metrics.
/// Body: { "action": "signed"|"verified"|"rejected" }
async fn agt_signing_counter(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    match action {
        "signed" => {
            state
                .governance
                .metrics
                .messages_signed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::metrics::AGT_MESSAGE_SIGNATURES
                .with_label_values(&["signed"])
                .inc();
        }
        "verified" => {
            state
                .governance
                .metrics
                .messages_verified
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::metrics::AGT_MESSAGE_SIGNATURES
                .with_label_values(&["verified"])
                .inc();
        }
        "rejected" => {
            state
                .governance
                .metrics
                .signatures_rejected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            crate::metrics::AGT_MESSAGE_SIGNATURES
                .with_label_values(&["rejected"])
                .inc();
        }
        _ => {}
    }
    Json(serde_json::json!({"ok": true}))
}

/// POST /agt/trust — plugin pushes trust updates after mesh interactions.
/// Body: { "agent_id": "peer-name", "score": 510, "interactions": 1 }
async fn agt_trust_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Trust mutations require admin token even from localhost — prevents sandbox
    // (UID 1000) from forging peer trust scores via the localhost auth exemption.
    if let Some(ref expected) = state.admin_token {
        let provided = extract_admin_token(&headers);
        match provided.as_deref() {
            Some(tok) if crate::handoff::constant_time_eq(tok.as_bytes(), expected.as_bytes()) => {}
            _ => {
                tracing::warn!("POST /agt/trust denied: missing or invalid admin token");
                return errors::flat(
                    StatusCode::FORBIDDEN,
                    "Admin token required for trust mutations",
                );
            }
        }
    }

    let agent_id = body
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let score = body.get("score").and_then(|v| v.as_u64()).unwrap_or(500) as u32;
    let interactions = body
        .get("interactions")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    state
        .mesh_metrics
        .trust_updates
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    match state.governance.update_trust(agent_id, score, interactions) {
        Ok(json) => (StatusCode::OK, Json(json)),
        Err(msg) => errors::flat(StatusCode::BAD_REQUEST, msg),
    }
}

/// GET /agt/rate-limit — current token-bucket rate limit config.
async fn agt_rate_limit_get(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "global_rate": state.governance.rate_limiter.global_rate(),
        "global_capacity": state.governance.rate_limiter.global_capacity(),
        "per_agent_rate": state.governance.rate_limiter.per_agent_rate(),
        "per_agent_capacity": state.governance.rate_limiter.per_agent_capacity(),
    }))
}

/// PUT /agt/rate-limit — update token-bucket rate limits at runtime (admin only).
///
/// Body: `{ "global_rate": 200, "global_capacity": 400, "per_agent_rate": 20, "per_agent_capacity": 40 }`
/// All fields optional — only provided fields are updated.
async fn agt_rate_limit_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(ref expected) = state.admin_token {
        let provided = extract_admin_token(&headers);
        match provided.as_deref() {
            Some(tok) if crate::handoff::constant_time_eq(tok.as_bytes(), expected.as_bytes()) => {}
            _ => {
                tracing::warn!("PUT /agt/rate-limit denied: missing or invalid admin token");
                return errors::flat(StatusCode::FORBIDDEN, "Admin token required");
            }
        }
    }

    let rl = &state.governance.rate_limiter;
    let global_rate = body["global_rate"].as_f64().unwrap_or(rl.global_rate());
    let global_capacity = body["global_capacity"]
        .as_f64()
        .unwrap_or(rl.global_capacity());
    let per_agent_rate = body["per_agent_rate"]
        .as_f64()
        .unwrap_or(rl.per_agent_rate());
    let per_agent_capacity = body["per_agent_capacity"]
        .as_f64()
        .unwrap_or(rl.per_agent_capacity());

    rl.update_rates(
        global_rate,
        global_capacity,
        per_agent_rate,
        per_agent_capacity,
    );

    tracing::info!(
        global_rate,
        global_capacity,
        per_agent_rate,
        per_agent_capacity,
        "Rate limits updated dynamically"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "global_rate": global_rate,
            "global_capacity": global_capacity,
            "per_agent_rate": per_agent_rate,
            "per_agent_capacity": per_agent_capacity,
            "message": "Rate limits updated",
        })),
    )
}

/// GET /agt/reputation — fetch this agent's reputation from the AgentMesh registry.
/// Returns the registry-computed score (session history, peer feedback, tier bonus)
/// alongside the local trust store snapshot.
async fn agt_reputation(State(state): State<AppState>) -> impl IntoResponse {
    let registry_url = std::env::var("AGT_REGISTRY_URL")
        .unwrap_or_else(|_| "http://agentmesh-registry.agentmesh.svc.cluster.local:8080".into());

    let sandbox_name: &str = &state.sandbox_name;
    let base = registry_url.trim_end_matches('/');

    // Step 1: Look up our AMID from the registry
    let amid = lookup_parent_amid(&state.client, &registry_url, sandbox_name).await;

    // Step 2: If we found our AMID, fetch reputation score
    let registry = if let Some(ref agent_amid) = amid {
        match state
            .client
            .get(&format!(
                "{}/v1/registry/reputation/score?amid={}",
                base, agent_amid
            ))
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp.json::<serde_json::Value>().await.ok(),
            Ok(resp) => {
                tracing::debug!(status = %resp.status(), "Registry reputation lookup returned non-200");
                None
            }
            Err(e) => {
                tracing::debug!(error = %e, "Registry reputation lookup failed");
                None
            }
        }
    } else {
        tracing::debug!(sandbox = %sandbox_name, "Agent not found in registry — not yet registered");
        None
    };

    // Local trust from native governance
    let local_trust = serde_json::json!(state.governance.all_trust_scores());

    Json(serde_json::json!({
        "amid": amid.as_deref().unwrap_or(sandbox_name),
        "sandbox": sandbox_name,
        "registry": registry,
        "local_trust": local_trust,
    }))
}
