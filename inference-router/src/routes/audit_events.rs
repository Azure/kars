//! Audit-event helpers for the `routes::handoff` module.
//!
//! Rather than pepper every handoff route with the `AuditEvent { … }`
//! literal, we give each distinct audit occurrence a named helper. This
//! keeps `handoff.rs` under its §4.2 LOC budget while the
//! `providers::AuditSink` seam is progressively adopted.
//!
//! New migrated call-sites land here, not in `handoff.rs`. The legacy
//! `state.governance.audit.log(...)` direct-field calls continue to work
//! unchanged — they're migrated incrementally, one per follow-up PR, to
//! keep blast radius small.

use crate::providers::{AuditEvent, audit_now_ms};
use crate::routes::AppState;

/// Append a `handoff:init` audit event through the four-seam
/// [`crate::providers::AuditSink`] trait. Errors are logged but do not
/// fail the caller — audit append is non-fatal here (the hand-off token
/// itself is already persisted, and rejecting it because the audit sink
/// is unreachable would be a denial-of-service vector against the sink).
pub async fn handoff_init(state: &AppState, sandbox: &str, token_hash: &str) {
    let event = AuditEvent {
        timestamp_ms: audit_now_ms(),
        principal: sandbox.to_string(),
        action: "handoff:init".into(),
        payload_digest_hex: String::new(),
        verdict: "success".into(),
        labels: vec![(
            "token_hash".into(),
            token_hash
                .get(..16)
                .unwrap_or(token_hash)
                .to_string(),
        )],
    };
    if let Err(e) = state.audit_sink.append(event).await {
        tracing::warn!(error = %e, "audit sink append failed for handoff:init");
    }
}
