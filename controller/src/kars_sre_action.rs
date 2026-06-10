// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsSREAction` CRD — the typed-action proposal+execution surface
//! for the kars-sre agent (proposal §7.7 + §7.8.4).
//!
//! ## What it is
//!
//! A short-lived, single-action, operator-approved fix proposal from
//! the kars-sre agent. The agent emits one of these via its plugin
//! when it has diagnosed a workload incident and identified a typed
//! action it could take to remediate. The operator approves (or
//! rejects), and on approval the controller mints a short-lived
//! ServiceAccount token scoped to JUST the verb + resource + namespace
//! the action targets, executes via that token, and tears the binding
//! down post-execution.
//!
//! This CR is the "Slice 3" piece that turns the diagnostic-only SRE
//! agent from Slices 1+2 into an autonomous remediator (gated by the
//! operator's approval).
//!
//! ## Authority model
//!
//! The kars-sre sandbox SA (`kars-sre/sandbox`) gets a narrow `create`
//! permission on this CRD via a ClusterRole shipped in the chart.
//! Operators get `update` (to flip `.spec.approval.state`) via a
//! separate `kars:sre-approver` ClusterRole that the cluster admin
//! binds to humans / groups.
//!
//! K8s audit log is the audit surface — every approve / reject /
//! controller-issued TokenRequest is captured there.
//!
//! ## Typed actions (closed set — Slice 3)
//!
//! Per proposal §7.7.1:
//!
//! | type | schema (in `spec.action.params`) |
//! |---|---|
//! | `DeleteResourceQuota` | `{namespace, name}` — must NOT carry `kars.azure.com/managed-by=controller` |
//! | `PatchDeploymentImage` | `{namespace, name, container, image}` |
//! | `ScaleDeployment` | `{namespace, name, replicas: 0..50}` |
//! | `RolloutRestart` | `{namespace, kind∈{Deployment,StatefulSet,DaemonSet}, name}` |
//! | `DeletePod` | `{namespace, name}` |
//!
//! Slice 4+ may add `PatchConfigMapKey` etc.
//!
//! Each type maps to ONE (verb, resource, namespace) tuple at
//! reconciler-mint time. The controller refuses any action whose
//! target namespace is in the protected-resource denylist (§7.7.1):
//! `kube-system`, `kars-system`, `kars-sre`, `kube-public`,
//! `kube-node-lease`, `agentmesh`, or any namespace whose name
//! matches `kars-*` and contains a KarsSandbox with role=sre.
//!
//! ## Lifecycle
//!
//! `Proposed` (agent created; awaiting operator) →
//! `Approved` (operator flipped `spec.approval.state=Approved`) →
//! `Applied` (controller minted token, executed, torn down) →
//! `Recovered` | `Failed` (post-apply observation, set by reconciler) →
//!     also `Rejected` (operator denied) or `Expired` (>15min idle).
//!
//! The lifecycle is one-way. A new incident produces a new CR.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `KarsSREAction.spec` — declares one typed-action proposal.
///
/// The CR is namespaced; conventionally lives in `kars-sre` (the SRE
/// sandbox's own namespace) so list+watch from the SRE SA is naturally
/// scoped, but the controller accepts any namespace the operator
/// configures.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsSREAction",
    namespaced,
    status = "KarsSREActionStatus",
    shortname = "sreaction",
    printcolumn = r#"{"name":"Type","type":"string","jsonPath":".spec.action.type"}"#,
    printcolumn = r#"{"name":"Target-NS","type":"string","jsonPath":".spec.action.params.namespace"}"#,
    printcolumn = r#"{"name":"Target-Name","type":"string","jsonPath":".spec.action.params.name"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Approval","type":"string","jsonPath":".spec.approval.state"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct KarsSREActionSpec {
    /// The action the SRE agent proposes to take. Closed-set type +
    /// free-form params (validated per-type at reconcile time).
    pub action: ActionSpec,

    /// One-paragraph rationale from the agent: why this fix is the
    /// right response to the observed symptoms. Audit-grade text.
    /// Max 2048 chars; renders verbatim in `kubectl describe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,

    /// Short-form diagnosis (the "Symptom:" + "Root cause:" lines from
    /// the agent's proposal format). 1-line summary suitable for a
    /// Telegram notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<String>,

    /// Operator decision. The agent creates the CR with
    /// `approval.state="Pending"`; the operator flips it to
    /// `Approved` or `Rejected` via `kars sre approve <id>` /
    /// `kars sre reject <id>` (or directly via `kubectl edit`).
    pub approval: ApprovalSpec,

    /// Maximum age (in minutes) before the proposal auto-expires.
    /// Reconciler transitions `.status.phase=Expired` after this
    /// elapses if approval is still `Pending`. Default 15.
    /// Clamped to [1, 60] at admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<u32>,
}

/// Typed-action descriptor (closed set per proposal §7.7.1).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    /// Action type from the closed set (`DeleteResourceQuota`,
    /// `PatchDeploymentImage`, `ScaleDeployment`, `RolloutRestart`,
    /// `DeletePod`). Validated at admission via CEL.
    #[serde(rename = "type")]
    pub kind: String,

    /// Per-type params. Stored as a string-keyed map so the CRD schema
    /// emits a concrete `type: object` (apiserver rejects fields with
    /// no schema type). Values are arbitrary JSON — the reconciler
    /// validates the shape per `kind` at execute time.
    ///
    /// Required fields per type:
    ///   - DeleteResourceQuota: {namespace, name}
    ///   - PatchDeploymentImage: {namespace, name, container, image}
    ///   - ScaleDeployment: {namespace, name, replicas}
    ///   - RolloutRestart: {namespace, kind, name}
    ///   - DeletePod: {namespace, name}
    pub params: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Operator decision payload.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalSpec {
    /// `Pending` (initial), `Approved`, or `Rejected`. Flipped by an
    /// operator with the `kars:sre-approver` ClusterRole.
    pub state: String,

    /// Optional human-readable note attached to the decision (e.g.
    /// "approved by oncall — incident #4711"). Surfaces in audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `KarsSREAction.status` — controller-managed phase + observation.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsSREActionStatus {
    /// `Proposed` → `Approved` → `Applied` → `Recovered` | `Failed`.
    /// Or `Rejected` (operator denied) / `Expired` (TTL elapsed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// `metadata.generation` last reconciled. When != current, the
    /// reconciler still has work to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// Wall-clock timestamp the controller minted the writer token
    /// and executed the action (set on transition into Applied).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,

    /// Name of the one-shot ClusterRoleBinding the controller minted
    /// for the writer SA on approval. Cleaned up post-execution.
    /// Persisted in status so the cleanup reconciler can find it
    /// after a controller restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_crb_name: Option<String>,

    /// Standard k8s conditions. The reconciler stamps:
    ///   - `Available` (True iff phase=Applied/Recovered)
    ///   - `Approved` (True iff spec.approval.state=Approved)
    ///   - `Executed` (True iff the action ran via the minted token)
    ///   - `Recovered` (True iff post-apply observation passed)
    ///   - `Degraded` (True with reason if anything went wrong)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}
