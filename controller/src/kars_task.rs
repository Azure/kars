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

    /// A **requested promotion** — a target autonomy tier this mission wants to
    /// operate at (§12). When greater than `envelope.tier`, the controller opens
    /// a human `KarsApproval` (a `tierRaise`); only on approval does the
    /// controller widen this task's envelope to the requested tier. Promotion is
    /// always human-approved and ledgered, and widening is controller-only (a
    /// non-controller principal cannot raise the envelope — enforced by the
    /// envelope-write VAP), so a mission cannot self-escalate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_tier: Option<i32>,

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

    /// Per-task retention override, in seconds, counted from the moment this
    /// task's deliverable landed (`status.deliveredAt`). When the effective TTL
    /// (this override, else the cluster-wide default read from the
    /// `kars-retention-policy` ConfigMap) elapses, the controller deletes this
    /// KarsTask — mirroring Kubernetes' `Job.spec.ttlSecondsAfterFinished`. Only
    /// a DELIVERED (terminal) task is ever auto-deleted; a task still running
    /// is never touched regardless of TTL. `0` disables retention for this
    /// task specifically (keep forever) even if a cluster default is set.
    /// Unset inherits the cluster default (which itself defaults to "never").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_ttl_seconds: Option<i64>,
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

    /// Explicit egress posture: `strict` or `learning`. This is separate from
    /// the endpoint list so `strict` with an empty list means deny all external
    /// hosts rather than being indistinguishable from Learn mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_mode: Option<String>,

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

    /// Names of approved `KarsSkill` PACKAGES to install into the sandbox. Each
    /// is a `karsskill-<name>` ConfigMap (SKILL.md + scripts) the reconciler
    /// mounts into the agent's skills dir. Surfaced to the reconciler via the
    /// sandbox annotation `kars.azure.com/skills`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
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
        .or_else(|| spec.envelope.tool_policy_ref.as_ref().map(|r| r.name.as_str()))
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
    parent.iter().any(|p| {
        p.host == child.host && (p.port.is_none() || p.port == child.port)
    })
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

    /// RFC3339 timestamp of the moment this task's deliverable first landed
    /// (the `kars-mission-output-<name>` ConfigMap was observed) — stamped
    /// ONCE, write-once like `envelope_digest`, and never touched again. This
    /// is the anchor the retention-TTL reconciler counts from; a task with no
    /// `deliveredAt` is still in flight and is never auto-deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
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
            requested_tier: None,
            execution: None,
            blueprint: None,
            display_name: Some("payments-bugfix".into()),
            retention_ttl_seconds: None,
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

    // ── Effective-authority attenuation (tools + egress the sandbox enforces) ──

    fn spec_with(
        envelope: TaskEnvelope,
        tool_policy: Option<&str>,
        egress: Vec<TaskEgress>,
    ) -> KarsTaskSpec {
        KarsTaskSpec {
            objective: "x".into(),
            envelope,
            parent_ref: None,
            requested_tier: None,
            execution: None,
            blueprint: Some(TaskBlueprint {
                tool_policy: tool_policy.map(str::to_string),
                egress,
                ..Default::default()
            }),
            display_name: None,
            retention_ttl_seconds: None,
        }
    }

    fn eg(host: &str, port: Option<u16>) -> TaskEgress {
        TaskEgress { host: host.into(), port }
    }

    /// A child envelope that strictly attenuates `parent_envelope()` on every
    /// numeric axis, so attenuation tests isolate the tool/egress axes.
    fn child_envelope() -> TaskEnvelope {
        TaskEnvelope {
            tier: 4,
            budget: Some(TaskBudget {
                tokens: Some(100_000),
                usd_micros: Some(5_000_000),
            }),
            tool_policy_ref: Some(LocalObjectRef { name: "strict-tools".into() }),
            egress_allowlist_ref: None,
            delegation_depth: 2,
            authority_ceiling: 4,
        }
    }

    #[test]
    fn effective_tool_policy_prefers_blueprint_then_envelope() {
        // Blueprint wins when set.
        let s = spec_with(parent_envelope(), Some("bp-tools"), vec![]);
        assert_eq!(effective_tool_policy(&s), Some("bp-tools"));
        // Falls back to the envelope ref when the blueprint omits it.
        let s2 = spec_with(parent_envelope(), None, vec![]);
        assert_eq!(effective_tool_policy(&s2), Some("strict-tools"));
    }

    #[test]
    fn child_egress_must_be_subset_of_parent() {
        let parent = spec_with(
            parent_envelope(),
            Some("strict-tools"),
            vec![eg("api.github.com", Some(443)), eg("pkg.go.dev", None)],
        );
        // Child within the parent's allow-list (exact + any-port host) → ok.
        let ok = spec_with(
            child_envelope(),
            Some("strict-tools"),
            vec![eg("api.github.com", Some(443)), eg("pkg.go.dev", Some(443))],
        );
        assert!(spec_attenuation_violations(&ok, &parent).is_empty());
        // Child reaching a host the parent never allowed → rejected.
        let bad = spec_with(
            child_envelope(),
            Some("strict-tools"),
            vec![eg("evil.example.com", Some(443))],
        );
        let v = spec_attenuation_violations(&bad, &parent);
        assert!(matches!(
            v.as_slice(),
            [EnvelopeViolation::EgressNotSubset { host, .. }] if host == "evil.example.com"
        ));
    }

    #[test]
    fn empty_parent_egress_permits_no_child_egress() {
        let parent = spec_with(parent_envelope(), Some("strict-tools"), vec![]);
        let bad = spec_with(
            child_envelope(),
            Some("strict-tools"),
            vec![eg("api.github.com", Some(443))],
        );
        let v = spec_attenuation_violations(&bad, &parent);
        assert!(v.iter().any(|x| matches!(x, EnvelopeViolation::EgressNotSubset { .. })));
    }

    #[test]
    fn child_tool_policy_must_match_parent_effective() {
        let parent = spec_with(parent_envelope(), Some("strict-tools"), vec![]);
        // Different effective tool policy than the parent → rejected.
        let bad = spec_with(child_envelope(), Some("loose-tools"), vec![]);
        let v = spec_attenuation_violations(&bad, &parent);
        assert!(v.iter().any(|x| matches!(
            x,
            EnvelopeViolation::PolicyMismatch { axis: PolicyAxis::ToolPolicy, .. }
        )));
    }
}
