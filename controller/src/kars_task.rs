// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsTask` CRD — the task-as-trust-envelope primitive (kars Bridge V0).
//!
//! A `KarsTask` is a typed unit of governed agent work that carries its
//! **trust envelope**: the autonomy tier, resource budget, tool/egress
//! allow-list references, and the delegation limits (`delegationDepth`,
//! `authorityCeiling`) that bound how authority may propagate when an agent
//! spawns a sub-agent.
//!
//! This is the substrate primitive underneath kars Bridge. It is, by design,
//! **independently useful on a plain kars cluster with no Bridge installed**:
//! `kubectl apply` a `KarsTask` and the controller stamps a stable
//! `status.envelopeDigest` and lifecycle phase. Capability-attenuating
//! delegation (a child task whose envelope is a verified strict subset of its
//! parent) builds on this type in the next slice; the Governance Receipt
//! composes its envelope digest + lineage.
//!
//! ## Autonomy tier (1..5)
//!
//! The `tier` field adopts the industry-consensus five-level autonomy
//! taxonomy (NIST AI RMF Agentic Profile / IEEE 7007 / ISO SC 42):
//!
//! - **1 — Manual / assistance:** the agent proposes; a human performs every
//!   priced or external action.
//! - **2 — Shared:** the agent acts on low-risk steps; everything else is
//!   human-gated (HITL).
//! - **3 — Conditional:** routine actions are autonomous; exceptions escalate
//!   to a human.
//! - **4 — Supervised:** autonomous with periodic human checkpoints + audit.
//! - **5 — Full:** autonomous within the envelope, bounded by budget + TTL.
//!
//! Higher tiers grant more authority. The envelope's `authorityCeiling`
//! caps the tier any *descendant* task may hold, and is itself `<= tier`.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mcp_server::LocalObjectRef;

/// Lowest valid autonomy tier.
pub const TIER_MIN: i32 = 1;
/// Highest valid autonomy tier.
pub const TIER_MAX: i32 = 5;

/// `KarsTask.spec` — a governed unit of work plus its trust envelope.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsTask",
    namespaced,
    status = "KarsTaskStatus",
    shortname = "ctask",
    printcolumn = r#"{"name":"Tier","type":"integer","jsonPath":".spec.envelope.tier"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Depth","type":"integer","jsonPath":".spec.envelope.delegationDepth"}"#,
    printcolumn = r#"{"name":"EnvelopeDigest","type":"string","jsonPath":".status.envelopeDigest"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsTaskSpec {
    /// Human-readable statement of the task to be performed. This is the
    /// instruction a task-giver writes; the agent fleet works to satisfy it.
    pub objective: String,

    /// The trust envelope that governs this task and bounds any delegation.
    pub envelope: TaskEnvelope,

    /// Optional short label surfaced in CLI / UI listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// The trust envelope carried by a `KarsTask`.
///
/// Every field is a *ceiling*: a child task minted by delegation may
/// attenuate (narrow) any of these but never amplify them. The subset
/// relation over envelopes is the heart of capability-attenuating
/// delegation (next slice).
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskEnvelope {
    /// Autonomy tier (1..5). See the module docs for the taxonomy.
    pub tier: i32,

    /// Optional resource budget for the whole task subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<TaskBudget>,

    /// Optional reference to a same-namespace `ToolPolicy` CR that bounds
    /// which tools/MCP servers this task (and its descendants) may call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy_ref: Option<LocalObjectRef>,

    /// Optional reference to a same-namespace `EgressAllowlist`-style CR that
    /// bounds the network destinations this task (and its descendants) may
    /// reach through the inference router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_allowlist_ref: Option<LocalObjectRef>,

    /// Remaining number of delegation hops this task may still spawn. A child
    /// task is minted with `delegationDepth = parent.delegationDepth - 1`;
    /// at `0` no further delegation is permitted. Must be `>= 0`.
    #[serde(default)]
    pub delegation_depth: i32,

    /// The maximum autonomy tier any *descendant* task may hold. Must be in
    /// `1..5` and `<= tier` — a task can never authorize a child to act with
    /// more authority than it holds itself.
    pub authority_ceiling: i32,
}

impl Default for TaskEnvelope {
    fn default() -> Self {
        // A safe default envelope: lowest autonomy, no delegation, no budget.
        Self {
            tier: TIER_MIN,
            budget: None,
            tool_policy_ref: None,
            egress_allowlist_ref: None,
            delegation_depth: 0,
            authority_ceiling: TIER_MIN,
        }
    }
}

impl TaskEnvelope {
    /// Compute the stable digest of this envelope.
    ///
    /// The digest is a `sha256:`-prefixed hex string over the canonical JSON
    /// serialization of the envelope. serde serializes struct fields in
    /// declaration order deterministically, so the same envelope always
    /// produces the same digest across processes — the property the
    /// Governance Receipt relies on to bind a task to the authority it ran
    /// under.
    #[must_use]
    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("TaskEnvelope always serializes");
        let full = Sha256::digest(&bytes);
        // 16 bytes (32 hex chars) is ample collision resistance for an
        // authority-binding identifier while keeping status compact.
        let mut out = String::with_capacity(7 + 32);
        out.push_str("sha256:");
        for b in &full[..16] {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
        }
        out
    }
}

/// Optional resource budget for a task subtree.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskBudget {
    /// Maximum total tokens the task subtree may consume. `0`/absent means
    /// "no token cap declared" (governance still applies at the router).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,

    /// Maximum total spend in micro-USD (1e-6 USD) for the task subtree.
    /// Integer micro-USD avoids floating-point in an audit-bound field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_micros: Option<i64>,
}

/// `KarsTask.status`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsTaskStatus {
    /// One of: `Pending`, `Ready`, `Degraded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// The `.metadata.generation` most recently reconciled, so clients can
    /// tell whether `status` reflects the current `spec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// Standard K8s conditions. `Ready` is set `True` once the envelope has
    /// been validated and its digest stamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    /// `sha256:` digest of the validated trust envelope. Stable for a given
    /// envelope; recomputed whenever the spec changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_digest: Option<String>,

    /// Ancestry of this task, oldest-first: the chain of parent task names
    /// from the root delegation down to (but excluding) this task. Empty for
    /// a root task. Populated by the delegation minting path (next slice).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_envelope() -> TaskEnvelope {
        TaskEnvelope {
            tier: 3,
            budget: Some(TaskBudget {
                tokens: Some(100_000),
                usd_micros: Some(5_000_000),
            }),
            tool_policy_ref: Some(LocalObjectRef {
                name: "default-tools".into(),
            }),
            egress_allowlist_ref: None,
            delegation_depth: 2,
            authority_ceiling: 3,
        }
    }

    #[test]
    fn envelope_digest_is_deterministic() {
        let e = sample_envelope();
        assert_eq!(e.digest(), e.digest());
    }

    #[test]
    fn envelope_digest_has_sha256_prefix_and_length() {
        let d = sample_envelope().digest();
        assert!(d.starts_with("sha256:"));
        // "sha256:" (7) + 16 bytes * 2 hex chars (32) = 39.
        assert_eq!(d.len(), 39);
    }

    #[test]
    fn envelope_digest_changes_with_tier() {
        let mut a = sample_envelope();
        let before = a.digest();
        a.tier = 4;
        assert_ne!(before, a.digest());
    }

    #[test]
    fn envelope_digest_changes_with_delegation_depth() {
        let mut a = sample_envelope();
        let before = a.digest();
        a.delegation_depth += 1;
        assert_ne!(before, a.digest());
    }

    #[test]
    fn spec_roundtrips_through_camelcase_yaml() {
        let spec = KarsTaskSpec {
            objective: "fix the flaky test in payments".into(),
            envelope: sample_envelope(),
            display_name: Some("payments-bugfix".into()),
        };
        let yaml = serde_yaml::to_string(&spec).expect("serializes");
        // Envelope fields must be camelCase on the wire.
        assert!(yaml.contains("authorityCeiling:"));
        assert!(yaml.contains("delegationDepth:"));
        let back: KarsTaskSpec = serde_yaml::from_str(&yaml).expect("roundtrips");
        assert_eq!(back.envelope.tier, 3);
        assert_eq!(back.envelope.authority_ceiling, 3);
    }

    #[test]
    fn default_envelope_is_least_privilege() {
        let e = TaskEnvelope::default();
        assert_eq!(e.tier, TIER_MIN);
        assert_eq!(e.delegation_depth, 0);
        assert_eq!(e.authority_ceiling, TIER_MIN);
        assert!(e.budget.is_none());
    }
}
