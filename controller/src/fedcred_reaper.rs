// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Periodic federated-credential garbage collector.
//!
//! ## Why
//!
//! Azure caps federated identity credentials at **20 per managed identity**
//! (a hard service limit that cannot be raised). The sandbox reconciler's
//! finalizer (see `reconciler::mod`) deletes the fedcred when a `KarsSandbox`
//! is removed cleanly, but several real-world paths leak entries:
//!
//!  - `kubectl delete --force` / namespace force-deletion (skips finalizers)
//!  - Controller crash or eviction during teardown
//!  - Pre-finalizer sandboxes that existed before the cleanup logic landed
//!  - Short-lived offload sandboxes whose teardown collided with controller restarts
//!
//! Once the MI hits 20, **all new sandbox creates fail** with
//! `400 BadRequest: Too many Federated Identity Credentials`. New pods then
//! fall back to the kubelet IMDS identity instead of their own per-sandbox
//! Workload Identity, which weakens the per-sandbox blast-radius guarantee.
//!
//! ## What
//!
//! Every `FEDCRED_REAPER_INTERVAL_SECS` (default 600s):
//!
//!  1. List every fedcred whose ARM resource name starts with `kars-`.
//!  2. Compute the keep-set from live `KarsSandbox` CRs
//!     (`kars-<sandbox.name>`) plus a tiny system allowlist.
//!  3. Delete every fedcred whose subject conforms to
//!     `system:serviceaccount:kars-*:sandbox` AND is not in the keep-set.
//!
//! Operations are idempotent — `delete_federated_credential` already treats
//! 404 as success — so the reaper is safe to run on every replica without
//! coordination.
//!
//! ## What it never touches
//!
//!  - `kars-controller-sa` (controller's own WI binding)
//!  - `kars-sandbox`       (system-shared SA used by the sandbox SA template)
//!  - Any fedcred whose ARM name does not start with `kars-`
//!  - Any fedcred whose subject does not match the
//!    `system:serviceaccount:kars-*:sandbox` pattern

use crate::crd::KarsSandbox;
use crate::fedcred::FedCredManager;
use kube::api::{Api, ListParams};
use kube::{Client, ResourceExt};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// System-level fedcred names the reaper must never delete.
/// These are not associated with any `KarsSandbox` CR.
const SYSTEM_KEEPLIST: &[&str] = &["kars-controller-sa", "kars-sandbox"];

/// Prefix of fedcred ARM resource names managed by Kars.
const NAME_PREFIX: &str = "kars-";

/// Required prefix and suffix for the subject claim, so we never delete
/// fedcreds belonging to other workloads that happen to share the MI.
const SUBJECT_PREFIX: &str = "system:serviceaccount:kars-";
const SUBJECT_SUFFIX: &str = ":sandbox";

/// Default reap interval (10 minutes).
const DEFAULT_INTERVAL_SECS: u64 = 600;

/// Run the fedcred reaper loop forever. Returns only on fatal error.
///
/// `interval_secs` can be overridden via `FEDCRED_REAPER_INTERVAL_SECS`
/// (useful for tests). Set to 0 to disable.
pub async fn run(client: Client, fedcred: Arc<FedCredManager>) -> anyhow::Result<()> {
    let interval_secs: u64 = std::env::var("FEDCRED_REAPER_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS);

    if interval_secs == 0 {
        tracing::info!("Fedcred reaper disabled (FEDCRED_REAPER_INTERVAL_SECS=0)");
        std::future::pending::<anyhow::Result<()>>().await
    } else {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the first tick (fires immediately) so we don't race controller startup.
        ticker.tick().await;
        tracing::info!(
            interval_secs,
            "Fedcred reaper started — will GC orphan federated credentials"
        );
        loop {
            ticker.tick().await;
            if let Err(e) = reap_once(&client, &fedcred).await {
                tracing::warn!("Fedcred reaper cycle failed (will retry next tick): {e}");
            }
        }
    }
}

/// Execute one reap cycle. Idempotent and safe to call concurrently.
async fn reap_once(client: &Client, fedcred: &FedCredManager) -> Result<(), String> {
    // 1. Build the keep-set from live KarsSandbox CRs.
    let sandboxes: Api<KarsSandbox> = Api::all(client.clone());
    let live = sandboxes
        .list(&ListParams::default())
        .await
        .map_err(|e| format!("list KarsSandbox failed: {e}"))?;
    let mut keep: HashSet<String> =
        HashSet::with_capacity(live.items.len() + SYSTEM_KEEPLIST.len());
    for s in live.items.iter() {
        keep.insert(format!("{NAME_PREFIX}{}", s.name_any()));
    }
    for s in SYSTEM_KEEPLIST {
        keep.insert((*s).to_string());
    }

    // 2. List all fedcreds on the MI.
    let entries = fedcred.list_federated_credentials().await?;
    let total = entries.len();

    // 3. Find and delete orphans.
    let mut deleted = 0usize;
    let mut skipped_unknown = 0usize;
    for entry in entries.iter() {
        if !entry.name.starts_with(NAME_PREFIX) {
            continue; // not ours
        }
        if keep.contains(&entry.name) {
            continue; // alive
        }
        if !entry.subject.starts_with(SUBJECT_PREFIX) || !entry.subject.ends_with(SUBJECT_SUFFIX) {
            // Defence-in-depth: refuse to delete fedcreds whose subject
            // doesn't match our convention, even if the name does.
            skipped_unknown += 1;
            tracing::warn!(
                name = %entry.name,
                subject = %entry.subject,
                "Skipping fedcred with non-conforming subject (will not delete)"
            );
            continue;
        }

        // `delete_federated_credential` takes the sandbox name (without the prefix).
        let sandbox_name = entry.name.trim_start_matches(NAME_PREFIX);
        match fedcred.delete_federated_credential(sandbox_name).await {
            Ok(()) => {
                deleted += 1;
                tracing::info!(
                    name = %entry.name,
                    subject = %entry.subject,
                    "Reaped orphan federated credential"
                );
            }
            Err(e) => {
                tracing::warn!(
                    name = %entry.name,
                    "Failed to delete orphan fedcred (will retry next cycle): {e}"
                );
            }
        }
    }

    if deleted > 0 || skipped_unknown > 0 {
        tracing::info!(
            deleted,
            kept = keep.len(),
            skipped_unknown,
            total_listed = total,
            "Fedcred reaper cycle complete"
        );
    } else {
        tracing::debug!(
            kept = keep.len(),
            total_listed = total,
            "Fedcred reaper cycle complete — no orphans"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names that match our prefix and would be considered for deletion.
    fn is_candidate(name: &str, subject: &str) -> bool {
        name.starts_with(NAME_PREFIX)
            && subject.starts_with(SUBJECT_PREFIX)
            && subject.ends_with(SUBJECT_SUFFIX)
    }

    #[test]
    fn keep_list_contains_system_entries() {
        assert!(SYSTEM_KEEPLIST.contains(&"kars-controller-sa"));
        assert!(SYSTEM_KEEPLIST.contains(&"kars-sandbox"));
    }

    #[test]
    fn rejects_non_prefixed_names() {
        assert!(!is_candidate(
            "other-team-fedcred",
            "system:serviceaccount:kars-x:sandbox",
        ));
    }

    #[test]
    fn rejects_non_conforming_subject() {
        assert!(!is_candidate(
            "kars-foo",
            "system:serviceaccount:other-ns:sandbox",
        ));
        assert!(!is_candidate(
            "kars-foo",
            "system:serviceaccount:kars-foo:other-sa",
        ));
    }

    #[test]
    fn accepts_conforming_pair() {
        assert!(is_candidate(
            "kars-akstest",
            "system:serviceaccount:kars-akstest:sandbox",
        ));
    }

    #[test]
    fn keep_set_dedups_system_and_live() {
        let mut keep: HashSet<String> = HashSet::new();
        keep.insert("kars-akstest".into());
        for s in SYSTEM_KEEPLIST {
            keep.insert((*s).to_string());
        }
        assert_eq!(keep.len(), 3);
        assert!(keep.contains("kars-akstest"));
        assert!(keep.contains("kars-controller-sa"));
        assert!(keep.contains("kars-sandbox"));
    }
}
