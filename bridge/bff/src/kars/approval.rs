// kars Bridge BFF — typed view of the `KarsApproval` CRD.
//
// CONTRACT OWNERSHIP: the `KarsApproval` schema is owned by core kars
// (`Azure/kars`, controller/src/kars_approval.rs). This is a *consumer*
// projection mirroring only what the steering inbox needs, with matching
// group/version/kind and camelCase serde.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kars::task::LocalObjectRef;

/// `KarsApproval.spec` — a human decision a task is waiting on.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsApproval",
    namespaced,
    status = "KarsApprovalStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct KarsApprovalSpec {
    /// The task this approval gates (same namespace).
    pub task_ref: LocalObjectRef,
    /// What needs a human decision.
    pub action: ApprovalAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<ApprovalActor>,
    /// ISO-8601 TTL (`PT15M`, `PT4H`, …). Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    /// The human decision, once made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalDecision>,
}

/// The action a `KarsApproval` gates.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalAction {
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_tier: Option<i32>,
}

/// A human's decision on an approval.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecision {
    /// `approve` or `deny`.
    pub verdict: String,
    pub decider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider_subject: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decider_roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalActor {
    pub subject: String,
    pub name: String,
}

/// `KarsApproval.status` — the controller is the sole writer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsApprovalStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_envelope_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider: Option<String>,
}
