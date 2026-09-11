// kars Bridge BFF — typed view of the `KarsReceipt` CRD.
//
// CONTRACT OWNERSHIP: the `KarsReceipt` schema is owned by core kars
// (`Azure/kars`, controller/src/kars_receipt.rs). This is a *consumer*
// projection — it mirrors only the fields the Bridge surfaces in the
// Governance Receipt evidence view, with matching group/version/kind and
// camelCase serde so the wire shape is identical.
//
// Rendering a receipt does not establish validity. The BFF's explicit
// verification endpoint and `kars receipt verify` perform separate
// cryptographic checks against the controller-published trust anchor.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kars::task::LocalObjectRef;

/// `KarsReceipt.spec` — the signed Governance Receipt for one task.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "KarsReceipt",
    namespaced,
    status = "KarsReceiptStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct KarsReceiptSpec {
    /// The task this receipt attests.
    pub task_ref: LocalObjectRef,
    /// `sha256:` digest of the trust envelope the task ran under.
    pub envelope_digest: String,
    /// in-toto predicate type URI.
    pub predicate_type: String,
    /// Signing scheme, e.g. `DSSEv1+ed25519`.
    pub scheme: String,
    /// Hex SHA-256 fingerprint of the signing public key.
    pub key_id: String,
    /// The DSSE envelope: base64 in-toto Statement + Ed25519 signature(s).
    pub dsse: DsseEnvelope,
    /// Optional unsigned echo. UI claims come from the signed predicate;
    /// a nonempty contradictory echo invalidates the receipt.
    #[serde(default)]
    pub claims: Vec<ReceiptClaim>,
}

/// A DSSE envelope.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DsseEnvelope {
    /// Base64 of the in-toto Statement JSON (the signed payload).
    pub payload: String,
    /// The DSSE payload type.
    pub payload_type: String,
    /// The signatures over the PAE.
    #[serde(default)]
    pub signatures: Vec<DsseSignature>,
}

/// One DSSE signature.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DsseSignature {
    pub keyid: String,
    pub sig: String,
}

/// One claim-class assertion.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptClaim {
    /// `integrity` | `conformance` | `completeness` | `regulatory`.
    pub class: String,
    /// `PASS` | `PARTIAL` | `FAIL` | `OMITTED`.
    pub status: String,
    /// Human-readable justification, surfaced verbatim.
    pub detail: String,
}

/// `KarsReceipt.status` — informational echo (the authority is the signature).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KarsReceiptStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_task_generation: Option<i64>,
    /// Sequence number in the hash-chained receipt inclusion log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclusion_seq: Option<i64>,
    /// Hash of this receipt's inclusion-log entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclusion_entry_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclusion_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclusion_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_segment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_tree_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witnessed: Option<bool>,
}
