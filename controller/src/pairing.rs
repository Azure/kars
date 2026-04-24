//! ClawPairing Custom Resource Definition.
//!
//! Represents a trust relationship between an external OpenClaw agent and this
//! AzureClaw cluster. Admin generates a one-time pairing token; the external
//! agent presents it to bind their mesh identity (AMID) to the cluster.
//!
//! Two offload modes are supported:
//! - **task**: ephemeral sandbox executes a single task, returns results, self-destructs
//! - **handoff**: full agent state migrates to cloud, runs long-term, returns on recall

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// ClawPairing spec — declares the desired pairing configuration.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "ClawPairing",
    namespaced,
    status = "ClawPairingStatus",
    shortname = "cp",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"AMID","type":"string","jsonPath":".status.boundAmid"}"#,
    printcolumn = r#"{"name":"Budget","type":"integer","jsonPath":".spec.tokenBudget"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClawPairingSpec {
    /// SHA-256 hash of the pairing token secret (hex-encoded).
    /// The plaintext token is never stored — only the hash for verification.
    pub token_hash: String,

    /// ISO 8601 timestamp when this pairing expires.
    pub expires_at: String,

    /// Maximum concurrent offload sandboxes (default: 1).
    #[serde(default = "default_slots")]
    pub slots_max: i32,

    /// Maximum total tokens the paired agent can consume across all offloads.
    #[serde(default = "default_token_budget")]
    pub token_budget: i64,

    /// Granted capabilities: "offload", "handoff", or both.
    #[serde(default = "default_capabilities")]
    pub capabilities: Vec<String>,

    /// Optional display name for admin reference.
    pub display_name: Option<String>,

    /// Inference model to use for offload sandboxes (default: from cluster config).
    pub model: Option<String>,

    /// Isolation level for offload sandboxes (standard | enhanced | confidential).
    pub isolation: Option<String>,
}

/// ClawPairing status — reflects the current observed state.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClawPairingStatus {
    /// PendingPairing | Active | Expired | Revoked
    pub phase: Option<String>,

    /// The external agent's AMID, set when the pairing is consumed.
    pub bound_amid: Option<String>,

    /// The external agent's Ed25519 signing public key (base64).
    pub bound_pubkey_ed25519: Option<String>,

    /// ISO 8601 timestamp when the external agent paired.
    pub paired_at: Option<String>,

    /// Number of offload sandbox slots currently in use.
    pub slots_used: Option<i32>,

    /// Total tokens consumed across all offloads.
    pub tokens_used: Option<i64>,

    /// ISO 8601 timestamp of the most recent offload.
    pub last_offload_at: Option<String>,

    /// Total number of completed offloads.
    pub offloads_completed: Option<i32>,

    /// Total number of failed offloads.
    pub offloads_failed: Option<i32>,

    /// Name of the currently active offload sandbox (if any).
    pub active_sandbox: Option<String>,
}

fn default_slots() -> i32 {
    1
}

fn default_token_budget() -> i64 {
    500_000
}

fn default_capabilities() -> Vec<String> {
    vec!["offload".into(), "handoff".into()]
}

/// Pairing phases.
pub mod phase {
    /// Token generated, waiting for external agent to pair.
    pub const PENDING: &str = "PendingPairing";
    /// External agent has paired, AMID is bound.
    pub const ACTIVE: &str = "Active";
    /// Pairing has passed its expiry date.
    pub const EXPIRED: &str = "Expired";
    /// Admin has manually revoked this pairing.
    pub const REVOKED: &str = "Revoked";
}

/// Release one offload slot on whichever `ClawPairing` is bound to the
/// given external requester AMID.
///
/// Looks up the pairing by `status.boundAmid`, decrements `slotsUsed`
/// (floor 0), and clears `activeSandbox`. No-op if no matching pairing
/// is found. Errors are logged and swallowed — this runs during the
/// deletion path and must not block finalizer removal.
pub async fn release_offload_slot(
    client: kube::Client,
    requester: &str,
    sandbox_name: &str,
) {
    use kube::{
        Api, ResourceExt,
        api::{ListParams, Patch, PatchParams},
    };
    let pairings_api: Api<ClawPairing> =
        Api::namespaced(client, crate::mesh_peer::IDENTITY_NAMESPACE);
    let Ok(list) = pairings_api.list(&ListParams::default()).await else {
        return;
    };
    let Some(pairing) = list.items.iter().find(|p| {
        p.status.as_ref().and_then(|s| s.bound_amid.as_deref()) == Some(requester)
    }) else {
        return;
    };
    let pairing_name = pairing.name_any();
    let slots = pairing.status.as_ref().and_then(|s| s.slots_used).unwrap_or(1);
    let patch = serde_json::json!({
        "status": {
            "slotsUsed": (slots - 1).max(0),
            "activeSandbox": serde_json::Value::Null,
        }
    });
    let _ = pairings_api
        .patch_status(
            &pairing_name,
            &PatchParams::apply("azureclaw-controller"),
            &Patch::Merge(patch),
        )
        .await;
    tracing::info!(
        sandbox = %sandbox_name,
        pairing = %pairing_name,
        "Offload slot released"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_slots_is_one() {
        let spec: ClawPairingSpec = serde_json::from_value(serde_json::json!({
            "tokenHash": "abc123",
            "expiresAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(spec.slots_max, 1);
    }

    #[test]
    fn default_token_budget_is_500k() {
        let spec: ClawPairingSpec = serde_json::from_value(serde_json::json!({
            "tokenHash": "abc123",
            "expiresAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(spec.token_budget, 500_000);
    }

    #[test]
    fn default_capabilities_include_both_modes() {
        let spec: ClawPairingSpec = serde_json::from_value(serde_json::json!({
            "tokenHash": "abc123",
            "expiresAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        assert!(spec.capabilities.contains(&"offload".to_string()));
        assert!(spec.capabilities.contains(&"handoff".to_string()));
    }

    #[test]
    fn status_defaults_empty() {
        let status = ClawPairingStatus::default();
        assert!(status.phase.is_none());
        assert!(status.bound_amid.is_none());
        assert!(status.paired_at.is_none());
        assert!(status.slots_used.is_none());
        assert!(status.tokens_used.is_none());
        assert!(status.active_sandbox.is_none());
    }

    #[test]
    fn custom_budget_and_slots() {
        let spec: ClawPairingSpec = serde_json::from_value(serde_json::json!({
            "tokenHash": "abc123",
            "expiresAt": "2026-07-14T00:00:00Z",
            "slotsMax": 3,
            "tokenBudget": 1000000,
            "capabilities": ["offload"]
        }))
        .unwrap();
        assert_eq!(spec.slots_max, 3);
        assert_eq!(spec.token_budget, 1_000_000);
        assert_eq!(spec.capabilities, vec!["offload"]);
    }

    #[test]
    fn phase_constants() {
        assert_eq!(phase::PENDING, "PendingPairing");
        assert_eq!(phase::ACTIVE, "Active");
        assert_eq!(phase::EXPIRED, "Expired");
        assert_eq!(phase::REVOKED, "Revoked");
    }

    #[test]
    fn display_name_is_optional() {
        let spec: ClawPairingSpec = serde_json::from_value(serde_json::json!({
            "tokenHash": "abc123",
            "expiresAt": "2026-07-14T00:00:00Z"
        }))
        .unwrap();
        assert!(spec.display_name.is_none());
        assert!(spec.model.is_none());
        assert!(spec.isolation.is_none());
    }
}
