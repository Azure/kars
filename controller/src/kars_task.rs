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

    /// Optional reference to a parent `KarsTask` in the **same namespace**.
    ///
    /// When set, this task is a *delegated child*: the controller verifies
    /// that this task's `envelope` is a strict subset of the parent's
    /// (capability-attenuating delegation — a child may narrow authority but
    /// never amplify it), and mints `status.lineage` from the parent's
    /// ancestry. A child whose envelope exceeds its parent on any axis is
    /// rejected as `Degraded` and never receives an envelope digest. This is
    /// the substrate enforcement of OWASP ASI-08 (cascading authority) — done
    /// by the controller, not asked of the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_ref: Option<LocalObjectRef>,

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

    /// Verify that `self` (a proposed child envelope) is a valid
    /// **attenuation** of `parent` — i.e. it narrows or preserves authority on
    /// every axis and never amplifies it. Returns the list of violated axes;
    /// an empty list means `self` is a valid subset of `parent`.
    ///
    /// This is the pure heart of capability-attenuating delegation (Pillar A).
    /// The lattice, axis by axis:
    ///
    /// - **tier:** `child.tier <= parent.authority_ceiling`. A child may hold
    ///   at most the authority the parent is willing to delegate — not the
    ///   parent's *own* tier, but the lower ceiling the parent declared for
    ///   descendants.
    /// - **authority_ceiling:** `child.authority_ceiling <= parent.authority_ceiling`.
    ///   A child cannot widen the ceiling it in turn grants *its* descendants.
    /// - **delegation_depth:** `child.delegation_depth <= parent.delegation_depth - 1`.
    ///   Each hop consumes one level; the parent must have depth budget left.
    /// - **budget (tokens, usd):** a child cap must be present and `<=` the
    ///   parent cap whenever the parent declares one. An unbounded child under
    ///   a bounded parent is an amplification.
    /// - **tool_policy / egress_allowlist:** if the parent pins a policy ref,
    ///   the child must pin the *same* ref. (Subset *intersection* of named
    ///   policies is a future refinement; for V0 the safe rule is "inherit the
    ///   parent's exact bound or be rejected".)
    #[must_use]
    pub fn attenuation_violations(&self, parent: &TaskEnvelope) -> Vec<EnvelopeViolation> {
        let mut v = Vec::new();

        if self.tier > parent.authority_ceiling {
            v.push(EnvelopeViolation::TierExceedsParentCeiling {
                child_tier: self.tier,
                parent_ceiling: parent.authority_ceiling,
            });
        }
        if self.authority_ceiling > parent.authority_ceiling {
            v.push(EnvelopeViolation::CeilingExceedsParentCeiling {
                child_ceiling: self.authority_ceiling,
                parent_ceiling: parent.authority_ceiling,
            });
        }
        if self.delegation_depth > parent.delegation_depth - 1 {
            v.push(EnvelopeViolation::DelegationDepthExceeded {
                child_depth: self.delegation_depth,
                parent_depth: parent.delegation_depth,
            });
        }

        // Budget: a parent cap binds the whole subtree, so a child must not
        // exceed it, and must not be unbounded where the parent is bounded.
        attenuate_budget_axis(
            self.budget.as_ref().and_then(|b| b.tokens),
            parent.budget.as_ref().and_then(|b| b.tokens),
            BudgetAxis::Tokens,
            &mut v,
        );
        attenuate_budget_axis(
            self.budget.as_ref().and_then(|b| b.usd_micros),
            parent.budget.as_ref().and_then(|b| b.usd_micros),
            BudgetAxis::UsdMicros,
            &mut v,
        );

        attenuate_policy_axis(
            self.tool_policy_ref.as_ref().map(|r| r.name.as_str()),
            parent.tool_policy_ref.as_ref().map(|r| r.name.as_str()),
            PolicyAxis::ToolPolicy,
            &mut v,
        );
        attenuate_policy_axis(
            self.egress_allowlist_ref.as_ref().map(|r| r.name.as_str()),
            parent
                .egress_allowlist_ref
                .as_ref()
                .map(|r| r.name.as_str()),
            PolicyAxis::EgressAllowlist,
            &mut v,
        );

        v
    }
}

/// Which numeric budget axis a violation concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetAxis {
    Tokens,
    UsdMicros,
}

/// Which policy-reference axis a violation concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAxis {
    ToolPolicy,
    EgressAllowlist,
}

/// A single way in which a child envelope failed to attenuate its parent.
/// Carries enough detail to render an actionable `Degraded` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeViolation {
    TierExceedsParentCeiling {
        child_tier: i32,
        parent_ceiling: i32,
    },
    CeilingExceedsParentCeiling {
        child_ceiling: i32,
        parent_ceiling: i32,
    },
    DelegationDepthExceeded {
        child_depth: i32,
        parent_depth: i32,
    },
    BudgetExceeded {
        axis: BudgetAxis,
        child: i64,
        parent: i64,
    },
    BudgetUnbounded {
        axis: BudgetAxis,
        parent: i64,
    },
    PolicyMismatch {
        axis: PolicyAxis,
        child: Option<String>,
        parent: String,
    },
}

impl std::fmt::Display for EnvelopeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvelopeViolation::TierExceedsParentCeiling {
                child_tier,
                parent_ceiling,
            } => write!(
                f,
                "tier {child_tier} exceeds parent authority ceiling {parent_ceiling}"
            ),
            EnvelopeViolation::CeilingExceedsParentCeiling {
                child_ceiling,
                parent_ceiling,
            } => write!(
                f,
                "authorityCeiling {child_ceiling} exceeds parent authority ceiling {parent_ceiling}"
            ),
            EnvelopeViolation::DelegationDepthExceeded {
                child_depth,
                parent_depth,
            } => write!(
                f,
                "delegationDepth {child_depth} exceeds parent budget (parent depth {parent_depth}, child must be <= {})",
                parent_depth - 1
            ),
            EnvelopeViolation::BudgetExceeded {
                axis,
                child,
                parent,
            } => write!(f, "budget {axis:?} {child} exceeds parent cap {parent}"),
            EnvelopeViolation::BudgetUnbounded { axis, parent } => write!(
                f,
                "budget {axis:?} is unbounded but parent caps it at {parent}"
            ),
            EnvelopeViolation::PolicyMismatch {
                axis,
                child,
                parent,
            } => write!(
                f,
                "{axis:?} ref {} must match parent's bound `{parent}`",
                child.as_deref().unwrap_or("<none>")
            ),
        }
    }
}

/// Compare one numeric budget axis. A parent cap binds the whole subtree.
fn attenuate_budget_axis(
    child: Option<i64>,
    parent: Option<i64>,
    axis: BudgetAxis,
    out: &mut Vec<EnvelopeViolation>,
) {
    let Some(parent_cap) = parent else {
        // Parent is unbounded on this axis — any child value is an attenuation.
        return;
    };
    match child {
        None => out.push(EnvelopeViolation::BudgetUnbounded {
            axis,
            parent: parent_cap,
        }),
        Some(c) if c > parent_cap => out.push(EnvelopeViolation::BudgetExceeded {
            axis,
            child: c,
            parent: parent_cap,
        }),
        Some(_) => {}
    }
}

/// Compare one policy-reference axis. If the parent pins a ref, the child must
/// pin the same one (V0 rule; intersection semantics are a future refinement).
fn attenuate_policy_axis(
    child: Option<&str>,
    parent: Option<&str>,
    axis: PolicyAxis,
    out: &mut Vec<EnvelopeViolation>,
) {
    let Some(parent_ref) = parent else {
        // Parent pins no policy on this axis — child is free to add one.
        return;
    };
    if child != Some(parent_ref) {
        out.push(EnvelopeViolation::PolicyMismatch {
            axis,
            child: child.map(str::to_string),
            parent: parent_ref.to_string(),
        });
    }
}
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
            parent_ref: None,
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

    // ── Capability-attenuating delegation lattice (Pillar A) ──────────────

    /// A permissive parent: tier 5, ceiling 4, depth 3, generous budget.
    fn parent_envelope() -> TaskEnvelope {
        TaskEnvelope {
            tier: 5,
            budget: Some(TaskBudget {
                tokens: Some(1_000_000),
                usd_micros: Some(50_000_000),
            }),
            tool_policy_ref: Some(LocalObjectRef {
                name: "strict-tools".into(),
            }),
            egress_allowlist_ref: None,
            delegation_depth: 3,
            authority_ceiling: 4,
        }
    }

    #[test]
    fn valid_child_attenuates_on_every_axis() {
        let parent = parent_envelope();
        let child = TaskEnvelope {
            tier: 4, // <= parent ceiling 4
            budget: Some(TaskBudget {
                tokens: Some(100_000),
                usd_micros: Some(5_000_000),
            }),
            tool_policy_ref: Some(LocalObjectRef {
                name: "strict-tools".into(),
            }),
            egress_allowlist_ref: None,
            delegation_depth: 2,  // <= 3 - 1
            authority_ceiling: 3, // <= 4
        };
        assert!(
            child.attenuation_violations(&parent).is_empty(),
            "{:?}",
            child.attenuation_violations(&parent)
        );
    }

    #[test]
    fn child_tier_above_parent_ceiling_is_amplification() {
        let parent = parent_envelope();
        let child = TaskEnvelope {
            tier: 5, // parent ceiling is only 4
            authority_ceiling: 4,
            delegation_depth: 0,
            ..parent_envelope()
        };
        let v = child.attenuation_violations(&parent);
        assert!(
            v.iter()
                .any(|x| matches!(x, EnvelopeViolation::TierExceedsParentCeiling { .. }))
        );
    }

    #[test]
    fn child_ceiling_above_parent_ceiling_is_amplification() {
        let parent = parent_envelope();
        let mut child = parent_envelope();
        child.tier = 4;
        child.authority_ceiling = 5; // exceeds parent ceiling 4
        child.delegation_depth = 0;
        let v = child.attenuation_violations(&parent);
        assert!(
            v.iter()
                .any(|x| matches!(x, EnvelopeViolation::CeilingExceedsParentCeiling { .. }))
        );
    }

    #[test]
    fn delegation_depth_must_decrement() {
        let parent = parent_envelope(); // depth 3
        let mut child = parent_envelope();
        child.tier = 4;
        child.authority_ceiling = 4;
        child.delegation_depth = 3; // must be <= 2
        let v = child.attenuation_violations(&parent);
        assert!(
            v.iter()
                .any(|x| matches!(x, EnvelopeViolation::DelegationDepthExceeded { .. }))
        );
    }

    #[test]
    fn exhausted_delegation_budget_rejects_any_child() {
        let mut parent = parent_envelope();
        parent.delegation_depth = 0; // no hops left
        let mut child = parent_envelope();
        child.tier = 1;
        child.authority_ceiling = 1;
        child.delegation_depth = 0;
        let v = child.attenuation_violations(&parent);
        assert!(
            v.iter()
                .any(|x| matches!(x, EnvelopeViolation::DelegationDepthExceeded { .. }))
        );
    }

    #[test]
    fn child_budget_over_parent_cap_is_amplification() {
        let parent = parent_envelope();
        let mut child = parent_envelope();
        child.tier = 4;
        child.authority_ceiling = 3;
        child.delegation_depth = 1;
        child.budget = Some(TaskBudget {
            tokens: Some(2_000_000), // parent caps at 1M
            usd_micros: Some(1_000_000),
        });
        let v = child.attenuation_violations(&parent);
        assert!(v.iter().any(|x| matches!(
            x,
            EnvelopeViolation::BudgetExceeded {
                axis: BudgetAxis::Tokens,
                ..
            }
        )));
    }

    #[test]
    fn unbounded_child_under_bounded_parent_is_amplification() {
        let parent = parent_envelope();
        let mut child = parent_envelope();
        child.tier = 4;
        child.authority_ceiling = 3;
        child.delegation_depth = 1;
        child.budget = None; // parent bounds tokens + usd
        let v = child.attenuation_violations(&parent);
        assert!(
            v.iter()
                .any(|x| matches!(x, EnvelopeViolation::BudgetUnbounded { .. }))
        );
    }

    #[test]
    fn child_must_match_parent_pinned_tool_policy() {
        let parent = parent_envelope(); // pins strict-tools
        let mut child = parent_envelope();
        child.tier = 4;
        child.authority_ceiling = 3;
        child.delegation_depth = 1;
        child.tool_policy_ref = Some(LocalObjectRef {
            name: "looser-tools".into(),
        });
        let v = child.attenuation_violations(&parent);
        assert!(v.iter().any(|x| matches!(
            x,
            EnvelopeViolation::PolicyMismatch {
                axis: PolicyAxis::ToolPolicy,
                ..
            }
        )));
    }

    #[test]
    fn child_may_add_egress_bound_where_parent_has_none() {
        let parent = parent_envelope(); // egress_allowlist_ref None
        let mut child = parent_envelope();
        child.tier = 4;
        child.authority_ceiling = 3;
        child.delegation_depth = 1;
        child.egress_allowlist_ref = Some(LocalObjectRef {
            name: "tighter-egress".into(),
        });
        // Adding a bound where the parent had none is attenuation, not amplification.
        let v = child.attenuation_violations(&parent);
        assert!(!v.iter().any(|x| matches!(
            x,
            EnvelopeViolation::PolicyMismatch {
                axis: PolicyAxis::EgressAllowlist,
                ..
            }
        )));
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
