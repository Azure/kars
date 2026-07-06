// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `POST /v1/access-request` — the agent's in-flight capability request path.
//!
//! The sandbox is deliberately least-privilege: a tool, skill, MCP server,
//! shell command, egress host, or autonomy tier that a task turns out to need
//! may simply be absent. Rather than fail silently, the agent raises a request
//! here. It lands in a bounded, deduplicated buffer that the controller polls
//! (`GET /internal/access-requests`) and turns into a **Pending KarsApproval**
//! surfaced in the Bridge inbox. A human (end user or operator) then approves or
//! denies; only on approval does the controller perform the privileged widening.
//!
//! This endpoint is a **request, never a grant** — it mutates nothing but an
//! outbound queue, so it is safe to expose on the same-pod loopback the agent
//! already uses for inference. It cannot itself widen access.

use axum::{
    Json, Router, extract::State, http::StatusCode, response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::errors;

/// Request body. `kind` + `target` identify what's needed; `reason` justifies it.
#[derive(Debug, Deserialize)]
pub struct AccessRequestBody {
    /// `egress` | `tool` | `skill` | `mcp` | `command` | `permission` | `tier`.
    pub kind: String,
    /// The host / tool / skill / command / MCP id being requested. For `tier`,
    /// may be empty.
    #[serde(default)]
    pub target: String,
    /// Why the task needs it. Surfaced verbatim to the human approver.
    #[serde(default)]
    pub reason: String,
    /// For `kind = "tier"`, the autonomy tier being requested (1..=5).
    #[serde(default)]
    pub tier: Option<i32>,
    /// For `kind = "egress"`, the port (defaults to 443 downstream).
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Serialize)]
struct AccessRequestAck {
    status: &'static str,
    /// True when this was the first observation (a fresh inbox item will be
    /// minted); false when it coalesced onto an existing pending request.
    new: bool,
    kind: String,
    target: String,
}

const ALLOWED_KINDS: &[&str] = &[
    "egress",
    "tool",
    "skill",
    "mcp",
    "command",
    "permission",
    "tier",
];

async fn access_request_handler(
    State(state): State<AppState>,
    Json(body): Json<AccessRequestBody>,
) -> impl IntoResponse {
    let kind = body.kind.trim().to_lowercase();
    if kind.is_empty() {
        return errors::flat(StatusCode::BAD_REQUEST, "Missing 'kind' field").into_response();
    }
    if !ALLOWED_KINDS.contains(&kind.as_str()) {
        return errors::flat(
            StatusCode::BAD_REQUEST,
            "Unknown 'kind' — expected one of: egress, tool, skill, mcp, command, permission, tier",
        )
        .into_response();
    }
    let target = body.target.trim();
    // Every kind except `tier` needs a concrete target.
    if kind != "tier" && target.is_empty() {
        return errors::flat(
            StatusCode::BAD_REQUEST,
            "Missing 'target' — name the host/tool/skill/command/mcp being requested",
        )
        .into_response();
    }
    // Bound the surface: keep reasons short so the inbox stays legible and the
    // agent can't stuff arbitrary payload into a human-facing field.
    let reason: String = body.reason.trim().chars().take(512).collect();
    let tier = body.tier.filter(|t| (1..=5).contains(t));

    let is_new = state
        .access_requests
        .record(&kind, target, &reason, tier, body.port);

    if is_new {
        tracing::info!(
            sandbox = %state.sandbox_name,
            kind = %kind,
            target = %target,
            "Agent raised a capability access request (surfacing to the inbox)"
        );
    }

    (
        StatusCode::ACCEPTED,
        Json(AccessRequestAck {
            status: "queued",
            new: is_new,
            kind,
            target: target.to_string(),
        }),
    )
        .into_response()
}

/// Loopback-mounted (public) route — the agent's request ingress + status poll.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/access-request", post(access_request_handler))
        .route("/v1/access-requests", get(access_request_status))
}

/// The agent-facing view of its own requests + decisions. After raising a
/// request the agent polls this to learn whether a human approved it, so it can
/// continue (egress grants take effect automatically; the fetch simply starts
/// succeeding) rather than blindly retrying or giving up.
#[derive(Debug, Serialize)]
struct AgentRequestView {
    kind: String,
    target: String,
    reason: String,
    /// `pending` | `approved` | `denied`.
    status: String,
}

async fn access_request_status(State(state): State<AppState>) -> impl IntoResponse {
    let items: Vec<AgentRequestView> = state
        .access_requests
        .snapshot(0)
        .into_iter()
        .map(|e| AgentRequestView {
            status: e.decision.clone().unwrap_or_else(|| "pending".to_string()),
            kind: e.kind,
            target: e.target,
            reason: e.reason,
        })
        .collect();
    Json(serde_json::json!({ "requests": items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_builds() {
        let _r = routes();
    }

    #[test]
    fn allowed_kinds_cover_the_taxonomy() {
        for k in ["egress", "tool", "skill", "mcp", "command", "permission", "tier"] {
            assert!(ALLOWED_KINDS.contains(&k));
        }
    }
}
