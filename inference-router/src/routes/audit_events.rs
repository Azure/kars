//! Audit-event helpers for the `routes::handoff` module.
//!
//! Rather than pepper every handoff route with the `AuditEvent { … }`
//! literal, we give each distinct audit occurrence a named helper. This
//! keeps `handoff.rs` under its §4.2 LOC budget while the
//! `providers::AuditSink` seam is progressively adopted.
//!
//! All handoff audit emissions land here (not in `handoff.rs`). They
//! flow through the [`crate::providers::AuditSink`] trait on
//! `AppState.audit_sink`, which the in-tree
//! `impl AuditSink for Governance` writes onto the same hash-chained
//! `agentmesh::AuditLogger` the legacy `audit.log` calls used. So this
//! is a routing change, not a backend change — one chain, one set of
//! receipts, accessed through the four-seam contract.

use crate::providers::{AuditEvent, audit_now_ms};
use crate::routes::AppState;

/// Generic handoff audit helper. Appends an event through the four-seam
/// [`crate::providers::AuditSink`] trait with `verdict = "info"` and a
/// single `("detail", details)` label. Append errors are logged but
/// never fail the caller — handoff state has already mutated by the
/// time we audit it, and rejecting the request because the sink is
/// unreachable would be a denial-of-service vector against the sink.
pub async fn handoff_event(state: &AppState, action: &str, details: &str) {
    let event = AuditEvent {
        timestamp_ms: audit_now_ms(),
        principal: state.sandbox_name.to_string(),
        action: action.to_string(),
        payload_digest_hex: String::new(),
        verdict: "info".into(),
        labels: vec![("detail".into(), details.to_string())],
    };
    if let Err(e) = state.audit_sink.append(event).await {
        tracing::warn!(error = %e, action, "audit sink append failed (non-fatal)");
    }
}

/// Append a `handoff:init` audit event. Specialised helper because the
/// label key is `token_hash` (truncated to 16 chars), not `detail`.
pub async fn handoff_init(state: &AppState, sandbox: &str, token_hash: &str) {
    let event = AuditEvent {
        timestamp_ms: audit_now_ms(),
        principal: sandbox.to_string(),
        action: "handoff:init".into(),
        payload_digest_hex: String::new(),
        verdict: "success".into(),
        labels: vec![(
            "token_hash".into(),
            token_hash.get(..16).unwrap_or(token_hash).to_string(),
        )],
    };
    if let Err(e) = state.audit_sink.append(event).await {
        tracing::warn!(error = %e, "audit sink append failed for handoff:init");
    }
}
