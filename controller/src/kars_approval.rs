// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsApproval` CRD — the tiered, envelope-aware HITL approval primitive
//! (kars Bridge V0, Inc 4).
//!
//! A `KarsApproval` is a single human decision a `KarsTask` is waiting on: a
//! priced/external/irreversible action, a checkpoint sign-off, or a request to
//! raise a branch's autonomy tier. It is the substrate under the Bridge's
//! **steering inbox** — "you steer the mission, approve / deny / redirect,
//! *without* attaching to any agent" — and the thing that makes the autonomy
//! tiers *mean* something: at tiers 1–3 a human gates the action, and the
//! decision is itself recorded in the task's Governance Receipt.
//!
//! Like `KarsTask`, it is **independently useful on a plain kars cluster with
//! no Bridge installed**: `kubectl apply` an approval, patch `spec.decision`,
//! and the controller drives the lifecycle and stamps a verifiable record.
//!
//! ## Authority binding (controller-owned)
//!
//! An approval is bound to the **exact authority** the task held when the
//! approval became bindable: the controller copies the task's
//! `status.envelopeDigest` into `status.boundEnvelopeDigest` on first
//! observation and never changes it. If the task's envelope later drifts, the
//! pending approval goes `Stale` — you cannot grant authority against a
//! moved target. The controller is the **sole writer** of the binding, so a
//! requester cannot forge what they are asking permission for.
//!
//! ## Lifecycle
//!
//! `Pending` (awaiting bind or decision) →
//!   - `Approved` / `Denied` — a human set `spec.decision`; terminal, the
//!     decision and decider are recorded immutably.
//!   - `Expired` — undecided past `requestedAt + ttl`.
//!   - `Stale` — the bound task envelope drifted (or the task vanished) before
//!     a decision; the request no longer applies to current authority.
//!
//! A human decision wins over expiry/staleness: if a person decided, that is
//! the governance truth and it is recorded.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp_server::LocalObjectRef;

/// `.status.phase` — a human approved the request. Terminal.
pub const PHASE_APPROVED: &str = "Approved";
/// `.status.phase` — a human denied the request. Terminal.
pub const PHASE_DENIED: &str = "Denied";
/// `.status.phase` — undecided past its TTL. Terminal.
pub const PHASE_EXPIRED: &str = "Expired";
/// `.status.phase` — the bound task authority drifted before a decision.
pub const PHASE_STALE: &str = "Stale";

/// The kinds of action a `KarsApproval` can gate. Free-form `Custom` is
/// allowed so the primitive is not a closed taxonomy, but the named kinds let
/// the Bridge group and prioritise the steering inbox.
#[allow(dead_code)]
pub const ACTION_KINDS: &[&str] = &[
    "toolCall",
    "egress",
    "checkpoint",
    "tierRaise",
    "clarification",
    "irreversible",
    "custom",
];

/// `KarsApproval.spec` — a human decision a task is waiting on.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsApproval",
    namespaced,
    status = "KarsApprovalStatus",
    shortname = "cappr",
    printcolumn = r#"{"name":"Task","type":"string","jsonPath":".spec.taskRef.name"}"#,
    printcolumn = r#"{"name":"Action","type":"string","jsonPath":".spec.action.kind"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Decider","type":"string","jsonPath":".status.decider"}"#,
    printcolumn = r#"{"name":"Expires","type":"string","jsonPath":".status.expiresAt"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsApprovalSpec {
    /// The `KarsTask` this approval gates, in the **same namespace**. The
    /// controller binds the approval to this task's envelope digest.
    pub task_ref: LocalObjectRef,

    /// What needs a human decision.
    pub action: ApprovalAction,

    /// Authenticated principal that originated the request. Bridge-authored
    /// approvals populate both stable subject and display name; controller-
    /// authored agent requests may leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<ApprovalActor>,

    /// Time-to-live as an ISO-8601 duration (`PT15M`, `PT4H`, `P1D`). An
    /// undecided approval past `requestedAt + ttl` becomes `Expired`. Defaults
    /// to `PT1H` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,

    /// The human decision. Absent while the approval is pending; a person (or
    /// the Bridge acting for them) patches this to drive the terminal
    /// transition. The controller is the sole writer of `status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalDecision>,
}

/// The action a `KarsApproval` gates.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalAction {
    /// One of [`ACTION_KINDS`]. Not enum-constrained on the wire so the
    /// primitive stays open; the Bridge treats unknown kinds as `custom`.
    pub kind: String,

    /// One-line, human-readable statement of what the agent wants to do.
    pub summary: String,

    /// Optional longer detail (e.g. the exact tool args or egress host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,

    /// For a `tierRaise`, the autonomy tier (1..5) being requested. Surfaced
    /// so an approver sees exactly how much authority they are granting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_tier: Option<i32>,
}

impl Default for ApprovalAction {
    fn default() -> Self {
        Self {
            kind: "custom".to_string(),
            summary: String::new(),
            detail: None,
            requested_tier: None,
        }
    }
}

/// A human's decision on an approval.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecision {
    /// `approve` or `deny`.
    pub verdict: String,

    /// Identity of the human (or delegated principal) who decided. Recorded
    /// verbatim into status and, for granted approvals, into the receipt.
    pub decider: String,

    /// Stable OIDC subject of the authenticated decider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider_subject: Option<String>,

    /// Signed Bridge roles held at decision time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decider_roles: Vec<String>,

    /// Optional justification, surfaced to auditors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalActor {
    pub subject: String,
    pub name: String,
}

/// Verdict values.
pub const VERDICT_APPROVE: &str = "approve";
pub const VERDICT_DENY: &str = "deny";

/// `KarsApproval.status` — the controller is the sole writer.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsApprovalStatus {
    /// `Pending` | `Approved` | `Denied` | `Expired` | `Stale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// `metadata.generation` last reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// RFC-3339 time the controller first reconciled the request. The TTL is
    /// measured from here; re-reconciles never bump it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_at: Option<String>,

    /// RFC-3339 time the human decision was first recorded. Immutable once
    /// set — re-reconciles preserve it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,

    /// RFC-3339 expiry (`requestedAt + ttl`). Stable across re-reconciles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,

    /// The task envelope digest this approval is bound to. Set once by the
    /// controller from the task's `status.envelopeDigest`; never changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_envelope_digest: Option<String>,

    /// Echo of `spec.decision.decider` once decided, for the printer column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider: Option<String>,

    /// Standard K8s conditions; the `Decided` condition message surfaces
    /// *why* (e.g. the staleness reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,
}

/// The pure outcome of evaluating an approval — no I/O, fully unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// Not yet decided. Carries a human-readable reason (awaiting bind vs
    /// awaiting decision) for the condition message.
    Pending(&'static str),
    /// A human approved. Terminal.
    Approved { decider: String },
    /// A human denied. Terminal.
    Denied { decider: String },
    /// Undecided past TTL. Terminal.
    Expired,
    /// Bound authority drifted (or task vanished) before a decision.
    Stale(String),
}

impl ApprovalOutcome {
    /// The `.status.phase` string for this outcome.
    pub fn phase(&self) -> &'static str {
        match self {
            ApprovalOutcome::Pending(_) => crate::status::phase::PHASE_PENDING,
            ApprovalOutcome::Approved { .. } => PHASE_APPROVED,
            ApprovalOutcome::Denied { .. } => PHASE_DENIED,
            ApprovalOutcome::Expired => PHASE_EXPIRED,
            ApprovalOutcome::Stale(_) => PHASE_STALE,
        }
    }

    /// Whether this outcome is terminal (no further transition expected).
    pub fn is_terminal(&self) -> bool {
        !matches!(self, ApprovalOutcome::Pending(_))
    }
}

/// Evaluate an approval. Pure: the reconciler resolves the live task digest
/// and the bound digest (binding the latter on first observation) and supplies
/// them here, so all decision logic is testable without a cluster.
///
/// Precedence:
/// 1. An unbound approval is `Pending` (awaiting the task envelope).
/// 2. A bound approval whose task digest drifted (or whose task vanished) is
///    `Stale`.
/// 3. A bound, current approval past its TTL is `Expired`.
/// 4. Only a still-current request may consume a human decision. A late
///    approval can never resurrect stale or expired authority.
/// 5. Otherwise `Pending` (awaiting a decision).
pub fn evaluate(
    decision: Option<&ApprovalDecision>,
    bound_digest: Option<&str>,
    live_task_digest: Option<&str>,
    expired: bool,
) -> ApprovalOutcome {
    let current = undecided_outcome(bound_digest, live_task_digest, expired);
    if !matches!(current, ApprovalOutcome::Pending("awaiting a human decision")) {
        return current;
    }
    if let Some(d) = decision {
        return match d.verdict.as_str() {
            VERDICT_APPROVE => ApprovalOutcome::Approved {
                decider: d.decider.clone(),
            },
            VERDICT_DENY => ApprovalOutcome::Denied {
                decider: d.decider.clone(),
            },
            // An unknown verdict is treated as no decision rather than a
            // silent approval — fail closed.
            _ => current,
        };
    }
    current
}

fn undecided_outcome(
    bound_digest: Option<&str>,
    live_task_digest: Option<&str>,
    expired: bool,
) -> ApprovalOutcome {
    let Some(bound) = bound_digest else {
        return ApprovalOutcome::Pending("awaiting task envelope (not yet bindable)");
    };
    match live_task_digest {
        None => ApprovalOutcome::Stale(
            "bound task is missing or no longer Ready; request no longer applies".to_string(),
        ),
        Some(live) if live != bound => ApprovalOutcome::Stale(format!(
            "task envelope drifted since the request (bound {bound}, current {live})"
        )),
        Some(_) if expired => ApprovalOutcome::Expired,
        Some(_) => ApprovalOutcome::Pending("awaiting a human decision"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(verdict: &str) -> ApprovalDecision {
        ApprovalDecision {
            verdict: verdict.to_string(),
            decider: "alice@example.com".to_string(),
            decider_subject: Some("oidc-subject-alice".into()),
            decider_roles: vec!["operator".into()],
            reason: None,
        }
    }

    #[test]
    fn approve_is_terminal_and_records_decider() {
        let out = evaluate(Some(&decision("approve")), Some("sha256:aa"), Some("sha256:aa"), false);
        assert_eq!(out.phase(), PHASE_APPROVED);
        assert!(out.is_terminal());
        assert!(matches!(out, ApprovalOutcome::Approved { decider } if decider == "alice@example.com"));
    }

    #[test]
    fn deny_is_terminal() {
        let out = evaluate(Some(&decision("deny")), Some("sha256:aa"), Some("sha256:aa"), false);
        assert_eq!(out.phase(), PHASE_DENIED);
        assert!(out.is_terminal());
    }

    #[test]
    fn stale_or_expired_authority_beats_late_decision() {
        let out = evaluate(Some(&decision("approve")), Some("sha256:aa"), Some("sha256:bb"), true);
        assert_eq!(out.phase(), PHASE_STALE);
    }

    #[test]
    fn unknown_verdict_fails_closed_to_pending() {
        let out = evaluate(Some(&decision("maybe")), Some("sha256:aa"), Some("sha256:aa"), false);
        assert_eq!(out.phase(), crate::status::phase::PHASE_PENDING);
    }

    #[test]
    fn unbound_is_pending_awaiting_task() {
        let out = evaluate(None, None, Some("sha256:aa"), false);
        assert!(matches!(out, ApprovalOutcome::Pending(_)));
    }

    #[test]
    fn drifted_envelope_is_stale() {
        let out = evaluate(None, Some("sha256:aa"), Some("sha256:bb"), false);
        assert_eq!(out.phase(), PHASE_STALE);
        assert!(out.is_terminal());
    }

    #[test]
    fn missing_task_is_stale() {
        let out = evaluate(None, Some("sha256:aa"), None, false);
        assert_eq!(out.phase(), PHASE_STALE);
    }

    #[test]
    fn bound_current_past_ttl_is_expired() {
        let out = evaluate(None, Some("sha256:aa"), Some("sha256:aa"), true);
        assert_eq!(out.phase(), PHASE_EXPIRED);
    }

    #[test]
    fn bound_current_within_ttl_is_pending_decision() {
        let out = evaluate(None, Some("sha256:aa"), Some("sha256:aa"), false);
        assert!(matches!(out, ApprovalOutcome::Pending(m) if m.contains("decision")));
        assert!(!out.is_terminal());
    }
}
