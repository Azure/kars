// kars Bridge BFF — typed view of the `KarsSREAction` CRD.
//
// CONTRACT OWNERSHIP: the `KarsSREAction` schema is owned by core kars
// (`Azure/kars`, controller/src/kars_sre_action.rs). This is a *consumer*
// projection mirroring only what the operator console needs, with matching
// group/version/kind and camelCase serde.
//
// A short-lived, single-action, operator-approved fix proposal from the
// kars-sre agent: it diagnoses a workload incident, proposes ONE typed
// remediation, and an operator approves/rejects. On approval the controller
// mints a narrowly-scoped one-shot token and executes; the BFF never touches
// the cluster directly for the actual remediation — it only ever patches
// `spec.approval`, mirroring the exact `decide_approval` pattern used for
// `KarsApproval`.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `KarsSREAction.spec` — one typed-action proposal from the kars-sre agent.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsSREAction",
    namespaced,
    status = "KarsSREActionStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct KarsSREActionSpec {
    /// The action proposed. Closed-set type + free-form params.
    pub action: SreActionSpec,
    /// One-paragraph rationale (audit-grade text, ≤2048 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Short-form "Symptom:"/"Root cause:" diagnosis (≤512 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<String>,
    /// Operator decision. `Pending` until an operator flips it.
    pub approval: SreApprovalSpec,
    /// Max age in minutes before the proposal auto-expires (default 15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<u32>,
}

/// Typed-action descriptor (closed set: `DeleteResourceQuota`,
/// `PatchDeploymentImage`, `ScaleDeployment`, `RolloutRestart`, `DeletePod`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SreActionSpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub params: BTreeMap<String, serde_json::Value>,
}

/// Operator decision payload on a `KarsSREAction`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SreApprovalSpec {
    /// `Pending`, `Approved`, or `Rejected`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `KarsSREAction.status` — controller-managed phase + observation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsSREActionStatus {
    /// `Proposed` → `Approved` → `Applied` → `Recovered`|`Failed`, or
    /// `Rejected`/`Expired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
}
