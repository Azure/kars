// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pair-request handling: validates a `pair_request` against a `KarsPairing`
//! CRD, binds the requester's AMID, and produces the `PairResponse`.
//!
//! Extracted from `mesh_peer/mod.rs` to keep the run loop file under its
//! Phase 1 LOC cap. Authoritative trust is the out-of-band pairing token;
//! Signal Protocol E2E for actual offload data lives elsewhere.

use kube::{
    ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
};
use serde_json::json;

use crate::pairing::{KarsPairing, phase};

use super::{FederationMessage, IDENTITY_NAMESPACE, MeshPeerState, hex_sha256};

pub(super) async fn handle_pair_request(
    state: &MeshPeerState,
    from_amid: &str,
    secret: &str,
    pubkey_ed25519: &str,
    display_name: Option<&str>,
) -> FederationMessage {
    let token_hash = hex_sha256(secret);

    let pairings: Api<KarsPairing> = Api::namespaced(state.client.clone(), IDENTITY_NAMESPACE);
    let pairing_list = match pairings.list(&ListParams::default()).await {
        Ok(list) => list,
        Err(e) => {
            tracing::error!("Failed to list KarsPairings: {e}");
            return pair_error("Internal error — could not verify token");
        }
    };

    let matching = pairing_list
        .items
        .iter()
        .find(|p| p.spec.token_hash == token_hash);
    let pairing = match matching {
        Some(p) => p,
        None => {
            tracing::warn!(from = %from_amid, "Pair request with invalid token");
            return pair_error("Invalid pairing token");
        }
    };

    let pairing_name = pairing.name_any();
    let current_phase = pairing
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("");

    if current_phase != phase::PENDING {
        tracing::warn!(
            pairing = %pairing_name,
            phase = %current_phase,
            "Pair request for non-pending pairing"
        );
        return pair_error(&format!(
            "Pairing is {current_phase} — token already consumed or expired"
        ));
    }

    if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(&pairing.spec.expires_at)
        && chrono::Utc::now() >= expiry.to_utc()
    {
        return pair_error("Pairing token has expired");
    }

    let now = chrono::Utc::now().to_rfc3339();
    let patch = json!({
        "status": {
            "phase": phase::ACTIVE,
            "boundAmid": from_amid,
            "boundPubkeyEd25519": pubkey_ed25519,
            "pairedAt": now
        }
    });

    if let Err(e) = pairings
        .patch_status(
            &pairing_name,
            &PatchParams::apply(crate::field_managers::MESH_PEER),
            &Patch::Merge(patch),
        )
        .await
    {
        tracing::error!(pairing = %pairing_name, "Failed to update pairing status: {e}");
        return pair_error("Internal error — could not bind identity");
    }

    tracing::info!(
        pairing = %pairing_name,
        amid = %from_amid,
        display_name = %display_name.unwrap_or("—"),
        "Pairing successful — AMID bound"
    );

    FederationMessage::PairResponse {
        success: true,
        cluster_name: Some(state.cluster_name.clone()),
        controller_amid: Some(state.identity.amid.clone()),
        capabilities_granted: Some(pairing.spec.capabilities.clone()),
        slots: Some(pairing.spec.slots_max),
        token_budget: Some(pairing.spec.token_budget),
        expires_at: Some(pairing.spec.expires_at.clone()),
        error: None,
    }
}

pub(super) fn pair_error(message: &str) -> FederationMessage {
    FederationMessage::PairResponse {
        success: false,
        cluster_name: None,
        controller_amid: None,
        capabilities_granted: None,
        slots: None,
        token_budget: None,
        expires_at: None,
        error: Some(message.into()),
    }
}
