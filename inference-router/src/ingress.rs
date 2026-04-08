//! Ingress ACL — runtime allow/block lists for inter-agent communication.
//!
//! Complements the KNOCK trust threshold and AGT `mesh:receive` policy gate
//! with operator-managed per-agent allow/block lists.  Blocked agents are
//! rejected at the `mesh:receive` policy evaluation stage; explicitly allowed
//! agents bypass the trust threshold.
//!
//! All state is in-memory (matches the egress allowlist pattern).  Restarting
//! the router resets to the default (threshold from `AGT_TRUST_THRESHOLD` env,
//! empty allow/block lists).

use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use hyper::StatusCode;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::RwLock;

use crate::routes::AppState;

/// Runtime ingress ACL state — shared via `Arc` inside `AppState`.
pub struct IngressAcl {
    /// Agents explicitly blocked from communicating with this sandbox.
    blocked: RwLock<HashSet<String>>,
    /// Agents explicitly allowed (bypass trust threshold).
    allowed: RwLock<HashSet<String>>,
    /// Dynamic trust threshold (overrides `AGT_TRUST_THRESHOLD` env at runtime).
    threshold: RwLock<u32>,
}

impl IngressAcl {
    pub fn new(initial_threshold: u32) -> Self {
        Self {
            blocked: RwLock::new(HashSet::new()),
            allowed: RwLock::new(HashSet::new()),
            threshold: RwLock::new(initial_threshold),
        }
    }

    pub fn is_blocked(&self, agent_id: &str) -> bool {
        self.blocked.read().unwrap().contains(agent_id)
    }

    pub fn is_allowed(&self, agent_id: &str) -> bool {
        self.allowed.read().unwrap().contains(agent_id)
    }

    pub fn block(&self, agent_id: &str) {
        self.blocked.write().unwrap().insert(agent_id.to_string());
        // Remove from allowed if present (block takes precedence)
        self.allowed.write().unwrap().remove(agent_id);
    }

    pub fn unblock(&self, agent_id: &str) -> bool {
        self.blocked.write().unwrap().remove(agent_id)
    }

    pub fn allow(&self, agent_id: &str) {
        self.allowed.write().unwrap().insert(agent_id.to_string());
        // Remove from blocked if present (explicit allow overrides block)
        self.blocked.write().unwrap().remove(agent_id);
    }

    pub fn threshold(&self) -> u32 {
        *self.threshold.read().unwrap()
    }

    pub fn set_threshold(&self, t: u32) {
        *self.threshold.write().unwrap() = t;
    }

    pub fn blocked_list(&self) -> Vec<String> {
        let mut v: Vec<_> = self.blocked.read().unwrap().iter().cloned().collect();
        v.sort();
        v
    }

    pub fn allowed_list(&self) -> Vec<String> {
        let mut v: Vec<_> = self.allowed.read().unwrap().iter().cloned().collect();
        v.sort();
        v
    }
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AgentBody {
    agent_id: String,
}

#[derive(Deserialize)]
struct ThresholdBody {
    threshold: u32,
}

/// GET /ingress/status — overview of ingress controls.
async fn ingress_status(State(state): State<AppState>) -> impl IntoResponse {
    let acl = &state.ingress_acl;
    let agents = state.governance.all_trust_scores();
    Json(serde_json::json!({
        "trust_threshold": acl.threshold(),
        "known_agents": agents.len(),
        "blocked_count": acl.blocked_list().len(),
        "allowed_count": acl.allowed_list().len(),
    }))
}

/// GET /ingress/agents — all known agents with trust scores and ACL status.
async fn ingress_agents(State(state): State<AppState>) -> impl IntoResponse {
    let acl = &state.ingress_acl;
    let agents = state.governance.all_trust_scores();
    let enriched: Vec<serde_json::Value> = agents
        .into_iter()
        .map(|mut a| {
            let id = a.get("agent_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            a.as_object_mut().unwrap().insert(
                "acl".into(),
                if acl.is_blocked(&id) {
                    serde_json::json!("blocked")
                } else if acl.is_allowed(&id) {
                    serde_json::json!("allowed")
                } else {
                    serde_json::json!("default")
                },
            );
            a
        })
        .collect();
    Json(serde_json::json!({
        "agents": enriched,
        "count": enriched.len(),
        "trust_threshold": acl.threshold(),
    }))
}

/// POST /ingress/block — block an agent from communicating with this sandbox.
async fn ingress_block(
    State(state): State<AppState>,
    Json(body): Json<AgentBody>,
) -> impl IntoResponse {
    state.ingress_acl.block(&body.agent_id);
    tracing::info!(agent = %body.agent_id, "Ingress: agent blocked");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "agent_id": body.agent_id,
            "status": "blocked",
        })),
    )
}

/// POST /ingress/unblock — remove an agent from the block list.
async fn ingress_unblock(
    State(state): State<AppState>,
    Json(body): Json<AgentBody>,
) -> impl IntoResponse {
    let removed = state.ingress_acl.unblock(&body.agent_id);
    if removed {
        tracing::info!(agent = %body.agent_id, "Ingress: agent unblocked");
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "agent_id": body.agent_id,
            "was_blocked": removed,
        })),
    )
}

/// POST /ingress/allow — explicitly allow an agent (bypasses trust threshold).
async fn ingress_allow(
    State(state): State<AppState>,
    Json(body): Json<AgentBody>,
) -> impl IntoResponse {
    state.ingress_acl.allow(&body.agent_id);
    tracing::info!(agent = %body.agent_id, "Ingress: agent explicitly allowed");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "agent_id": body.agent_id,
            "status": "allowed",
        })),
    )
}

/// GET /ingress/blocked — list blocked agents.
async fn ingress_blocked(State(state): State<AppState>) -> impl IntoResponse {
    let blocked = state.ingress_acl.blocked_list();
    Json(serde_json::json!({
        "agents": blocked,
        "count": blocked.len(),
    }))
}

/// GET /ingress/allowed — list explicitly allowed agents.
async fn ingress_allowed(State(state): State<AppState>) -> impl IntoResponse {
    let allowed = state.ingress_acl.allowed_list();
    Json(serde_json::json!({
        "agents": allowed,
        "count": allowed.len(),
    }))
}

/// POST /ingress/threshold — update trust threshold at runtime.
async fn ingress_threshold(
    State(state): State<AppState>,
    Json(body): Json<ThresholdBody>,
) -> impl IntoResponse {
    let old = state.ingress_acl.threshold();
    let new = body.threshold.min(1000); // Clamp to valid range
    state.ingress_acl.set_threshold(new);
    tracing::info!(old_threshold = old, new_threshold = new, "Ingress: trust threshold updated");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "old_threshold": old,
            "new_threshold": new,
        })),
    )
}

/// Ingress management routes — require admin token.
pub fn ingress_routes() -> Router<AppState> {
    Router::new()
        .route("/ingress/status", get(ingress_status))
        .route("/ingress/agents", get(ingress_agents))
        .route("/ingress/block", post(ingress_block))
        .route("/ingress/unblock", post(ingress_unblock))
        .route("/ingress/allow", post(ingress_allow))
        .route("/ingress/blocked", get(ingress_blocked))
        .route("/ingress/allowed", get(ingress_allowed))
        .route("/ingress/threshold", post(ingress_threshold))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_block_and_allow() {
        let acl = IngressAcl::new(500);

        // Initially empty
        assert!(!acl.is_blocked("agent-a"));
        assert!(!acl.is_allowed("agent-a"));

        // Block an agent
        acl.block("agent-a");
        assert!(acl.is_blocked("agent-a"));
        assert!(!acl.is_allowed("agent-a"));

        // Allow overrides block
        acl.allow("agent-a");
        assert!(!acl.is_blocked("agent-a"));
        assert!(acl.is_allowed("agent-a"));

        // Block overrides allow
        acl.block("agent-a");
        assert!(acl.is_blocked("agent-a"));
        assert!(!acl.is_allowed("agent-a"));

        // Unblock
        assert!(acl.unblock("agent-a"));
        assert!(!acl.is_blocked("agent-a"));
        assert!(!acl.unblock("agent-a")); // already unblocked
    }

    #[test]
    fn acl_threshold() {
        let acl = IngressAcl::new(500);
        assert_eq!(acl.threshold(), 500);

        acl.set_threshold(750);
        assert_eq!(acl.threshold(), 750);
    }

    #[test]
    fn acl_lists_sorted() {
        let acl = IngressAcl::new(0);
        acl.block("zulu");
        acl.block("alpha");
        acl.block("mike");
        let blocked = acl.blocked_list();
        assert_eq!(blocked, vec!["alpha", "mike", "zulu"]);

        acl.allow("yankee");
        acl.allow("bravo");
        let allowed = acl.allowed_list();
        assert_eq!(allowed, vec!["bravo", "yankee"]);
    }
}
