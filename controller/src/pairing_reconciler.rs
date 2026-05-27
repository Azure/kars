// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pairing reconciler — watches KarsPairing CRDs and manages lifecycle.
//!
//! Responsibilities:
//! - Set initial phase to PendingPairing on creation
//! - Expire pairings past their `expires_at` timestamp
//! - Validate token consumption (one-time bind of AMID)
//! - Provide helpers for the mesh peer module to query/update pairings

use anyhow::Result;
use futures::StreamExt;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::json;
use std::sync::Arc;
use tokio::time::Duration;

use crate::pairing::{KarsPairing, phase};

/// Shared context for the pairing reconciler.
struct PairingContext {
    client: Client,
}

/// Custom error type for the pairing reconciler.
#[derive(Debug, thiserror::Error)]
enum PairingReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

/// Main reconciliation function for KarsPairing resources.
async fn reconcile_pairing(
    pairing: Arc<KarsPairing>,
    ctx: Arc<PairingContext>,
) -> Result<Action, PairingReconcileError> {
    let name = pairing.name_any();
    let ns = pairing.namespace().unwrap_or_else(|| "kars-system".into());

    tracing::info!(pairing = %name, "Reconciling KarsPairing");

    let api: Api<KarsPairing> = Api::namespaced(ctx.client.clone(), &ns);
    let current_phase = pairing
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("");

    // If no phase set yet, initialize to PendingPairing
    if current_phase.is_empty() {
        tracing::info!(pairing = %name, "New pairing — setting phase to PendingPairing");
        let patch = json!({
            "status": {
                "phase": phase::PENDING,
                "slotsUsed": 0,
                "tokensUsed": 0,
                "offloadsCompleted": 0,
                "offloadsFailed": 0
            }
        });
        api.patch_status(
            &name,
            &PatchParams::apply(crate::field_managers::PAIRING),
            &Patch::Merge(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(60)));
    }

    // If revoked, nothing to do
    if current_phase == phase::REVOKED {
        return Ok(Action::requeue(Duration::from_secs(3600)));
    }

    // If already expired, nothing to do
    if current_phase == phase::EXPIRED {
        return Ok(Action::requeue(Duration::from_secs(3600)));
    }

    // Check expiry for PendingPairing and Active pairings
    if current_phase == phase::PENDING || current_phase == phase::ACTIVE {
        let expires_at = &pairing.spec.expires_at;
        if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(expires_at) {
            let expiry_utc = expiry.to_utc();
            let now = chrono::Utc::now();
            if now >= expiry_utc {
                tracing::info!(pairing = %name, "Pairing expired — transitioning to Expired");
                let patch = json!({
                    "status": {
                        "phase": phase::EXPIRED
                    }
                });
                api.patch_status(
                    &name,
                    &PatchParams::apply(crate::field_managers::PAIRING),
                    &Patch::Merge(patch),
                )
                .await?;
                return Ok(Action::requeue(Duration::from_secs(3600)));
            }

            // Requeue before expiry so we catch it promptly
            let until_expiry = (expiry_utc - now).num_seconds().max(10) as u64;
            let requeue_secs = until_expiry.min(300); // check at most every 5 min
            return Ok(Action::requeue(Duration::from_secs(requeue_secs)));
        }
    }

    // Default: requeue every 5 minutes
    Ok(Action::requeue(Duration::from_secs(300)))
}

fn pairing_error_policy(
    pairing: Arc<KarsPairing>,
    error: &PairingReconcileError,
    _ctx: Arc<PairingContext>,
) -> Action {
    let class = match error {
        PairingReconcileError::Kube(_) => "kube_api",
        PairingReconcileError::SerdeJson(_) => "serde",
    };
    crate::metrics::record_reconcile_error("KarsPairing", class);
    tracing::warn!(
        pairing = %pairing.name_any(),
        error = %error,
        "KarsPairing reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

/// Start the pairing reconciler controller loop.
pub async fn run(client: Client) -> Result<()> {
    let pairings: Api<KarsPairing> = Api::all(client.clone());

    // Verify CRD is installed (non-fatal — pairing is opt-in)
    match pairings.list(&ListParams::default().limit(1)).await {
        Ok(_) => {
            tracing::info!("KarsPairing CRD found — starting pairing controller");
        }
        Err(e) => {
            tracing::warn!("KarsPairing CRD not installed — pairing/federation disabled: {e}");
            // Park forever so the tokio::select! in main() does not see
            // this reconciler exit cleanly and tear the whole controller
            // down. The CRD is only optional from the controller's
            // perspective; its absence is operator config, not a fatal
            // condition.
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }

    let ctx = Arc::new(PairingContext { client });

    Controller::new(pairings, kube::runtime::watcher::Config::default())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsPairing", reconcile_pairing(x, ctx)).await
            },
            pairing_error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("Pairing reconciled {:?}", o),
                Err(e) => tracing::warn!("Pairing reconcile failed: {e:?}"),
            }
        })
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn expiry_detection() {
        // Verify chrono parsing works with our expected format
        let past = "2020-01-01T00:00:00Z";
        let parsed = chrono::DateTime::parse_from_rfc3339(past).unwrap();
        assert!(chrono::Utc::now() >= parsed);

        let future = "2099-12-31T23:59:59Z";
        let parsed = chrono::DateTime::parse_from_rfc3339(future).unwrap();
        assert!(chrono::Utc::now() < parsed);
    }
}
