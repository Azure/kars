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
    printcolumn = r#"{"name":"Execution","type":"string","jsonPath":".status.executionPhase"}"#,
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

    /// Execution gate (plan §20). A task is *governed-but-idle* by default —
    /// validated and digested, but not running. Execution begins only on an
    /// explicit launch, mirroring the "review the package, then launch"
    /// principle: the human reviews the trust envelope, then opts in. When
    /// `execution.launch` is `true` and the envelope is valid, the controller
    /// materializes a governed `KarsSandbox` (the running agent) bounded by
    /// the envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<TaskExecution>,

    /// The **run blueprint** — the concrete, editable shape of the agent that
    /// will run this task: which harness, which model, the system prompt, the
    /// connected services (MCP) and tools it may use, the network destinations
    /// it may reach, and the sandbox isolation. This is the substance a human
    /// reviews and edits on the §20 launch package; every field here drives a
    /// real field on the materialized `InferencePolicy` / `KarsSandbox`. When a
    /// field is unset the controller falls back to a safe default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint: Option<TaskBlueprint>,

    /// Optional short label surfaced in CLI / UI listings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// The concrete, editable run blueprint reviewed on the launch package.
/// Every field maps to a real field on the materialized resources.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskBlueprint {
    /// Harness/runtime the agent runs on (`OpenClaw`, `OpenAIAgents`, `MAF`,
    /// `Hermes`, `BYO`). Drives `KarsSandbox.spec.runtime.kind`. Defaults to
    /// `OpenClaw`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,

    /// The model the agent reasons with. Drives
    /// `InferencePolicy.spec.modelPreference.primary`. Defaults from controller
    /// env when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TaskModel>,

    /// System prompt / standing instructions for the agent, in addition to the
    /// objective. Drives `KarsSandbox.spec.agent.instructions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,

    /// Tools the agent may call, expressed as the name of an existing
    /// same-namespace `ToolPolicy`. Drives `KarsSandbox.spec.governance`
    /// (`enabled: true` + `toolPolicyRef`). Composing the existing `ToolPolicy`
    /// CRD keeps the AGT profile + `appliesTo` scope authoritative rather than
    /// duplicating an allow-list here. Required whenever `mcpServers` is set —
    /// governed MCP access is meaningless without a tool policy to bound it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<String>,

    /// Connected services (MCP server names, same namespace) the mission may
    /// use. Drives `KarsSandbox.spec.governance.mcpServerRefs`. Requires
    /// `toolPolicy` to be set (governed MCP access is bounded by the tool
    /// policy).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,

    /// Network destinations the mission may reach. Drives
    /// `KarsSandbox.spec.networkPolicy.allowedEndpoints`. When non-empty the
    /// sandbox runs in strict egress mode bounded to exactly these hosts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress: Vec<TaskEgress>,

    /// Sandbox isolation level (`standard`, `enhanced`, `confidential`). Drives
    /// `KarsSandbox.spec.sandbox.isolation`. Defaults to `standard`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,

    /// Shared team memory — the name of a same-namespace `KarsMemory` the agent
    /// reads/writes. Drives `KarsSandbox.spec.memoryRef`. This is how a
    /// persistent team shares knowledge across members and over time; a short
    /// one-off task usually leaves it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

/// A model route: provider tag + deployment name.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskModel {
    /// Provider tag: `azure-openai`, `anthropic`, `gemini`, `bedrock`,
    /// `ollama`, `github-models`.
    pub provider: String,
    /// Deployment / model name as the provider advertises it.
    pub deployment: String,
}

/// A network destination the mission may reach.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskEgress {
    /// Hostname, e.g. `api.github.com`.
    pub host: String,
    /// Optional TCP port (e.g. `443`); any port when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// Execution settings for a `KarsTask`. The launch flag is the §20 gate
/// between *governed* (validated, digested, idle) and *executing* (a real
/// sandbox/agent materialized).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskExecution {
    /// When `true`, the controller materializes a governed `KarsSandbox` from
    /// this task. Defaults to `false` — review before launch.
    #[serde(default)]
    pub launch: bool,

    /// Runtime to launch the agent on. Defaults to `OpenClaw`. Must match the
    /// controller's `RuntimeKind` enum. Superseded by `blueprint.runtime` when
    /// both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
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
    /// A child's blueprint egress reaches a destination the parent does not
    /// allow — egress must be a subset of the parent's (capability attenuation
    /// applied to the *effective* network surface the sandbox enforces, not a
    /// vestigial ref).
    EgressNotSubset {
        host: String,
        port: Option<u16>,
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
            EnvelopeViolation::EgressNotSubset { host, port } => match port {
                Some(p) => write!(
                    f,
                    "egress to {host}:{p} is not permitted by the parent (egress must be a subset of the parent's)"
                ),
                None => write!(
                    f,
                    "egress to {host} is not permitted by the parent (egress must be a subset of the parent's)"
                ),
            },
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

/// The *effective* tool policy a task runs under: the blueprint's tool policy
/// when set (it composes the sandbox governance), else the envelope's
/// `toolPolicyRef`. This is the single source attenuation must check so that
/// the verified subset relation matches what `materialize` actually enforces.
#[must_use]
pub fn effective_tool_policy(spec: &KarsTaskSpec) -> Option<&str> {
    spec.blueprint
        .as_ref()
        .and_then(|b| b.tool_policy.as_deref())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            spec.envelope
                .tool_policy_ref
                .as_ref()
                .map(|r| r.name.as_str())
        })
}

/// The *effective* egress allow-list a task runs under: the blueprint's egress
/// list (which materializes to `KarsSandbox.networkPolicy.allowedEndpoints`).
/// This is the real network surface, so it is what delegation must attenuate.
#[must_use]
pub fn effective_egress(spec: &KarsTaskSpec) -> &[TaskEgress] {
    spec.blueprint
        .as_ref()
        .map(|b| b.egress.as_slice())
        .unwrap_or(&[])
}

/// Whether a child egress destination is covered by the parent's allow-list.
/// A parent entry with no port (any port) covers a child entry on the same
/// host with any port; otherwise host + port must match exactly.
fn egress_covers(parent: &[TaskEgress], child: &TaskEgress) -> bool {
    parent
        .iter()
        .any(|p| p.host == child.host && (p.port.is_none() || p.port == child.port))
}

/// Full capability-attenuation check over the whole task spec: the numeric +
/// ref envelope axes **plus** the effective tool policy and effective egress
/// the sandbox will actually enforce. This closes the gap where attenuation
/// validated the envelope while execution used the blueprint — they now share
/// one source of truth. Returns an empty vec when the child strictly attenuates
/// the parent.
#[must_use]
pub fn spec_attenuation_violations(
    child: &KarsTaskSpec,
    parent: &KarsTaskSpec,
) -> Vec<EnvelopeViolation> {
    let mut v = child.envelope.attenuation_violations(&parent.envelope);

    // Effective tool policy: same equality rule as the envelope ref axis, but
    // over the value the sandbox actually runs (blueprint-or-envelope).
    attenuate_policy_axis(
        effective_tool_policy(child),
        effective_tool_policy(parent),
        PolicyAxis::ToolPolicy,
        &mut v,
    );

    // Effective egress must be a subset of the parent's: every destination the
    // child may reach must already be permitted to the parent. An empty parent
    // allow-list (model path only) permits no extra child egress.
    let parent_egress = effective_egress(parent);
    for dest in effective_egress(child) {
        if !egress_covers(parent_egress, dest) {
            v.push(EnvelopeViolation::EgressNotSubset {
                host: dest.host.clone(),
                port: dest.port,
            });
        }
    }

    v
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

    /// Execution phase (the §20 launch lifecycle), distinct from the
    /// governance `phase`:
    /// - `Idle` — governed but not launched (the default).
    /// - `Launching` — a `KarsSandbox` has been materialized; awaiting it.
    /// - `Running` — the sandbox reports Running.
    /// - `Degraded` — the sandbox degraded (e.g. no inference endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_phase: Option<String>,

    /// Name of the `KarsSandbox` materialized for this task, when launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_ref: Option<LocalObjectRef>,

    /// Human-readable detail about the execution state — surfaced verbatim in
    /// the product so a user understands *why* (e.g. the kind/Foundry caveat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_detail: Option<String>,
}

#[cfg(test)]
#[path = "kars_task_tests.rs"]
mod tests;
