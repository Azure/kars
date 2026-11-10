// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Controller-wide leader election via Kubernetes coordination Lease.
//!
//! ## Why a controller-wide gate?
//!
//! Phase 2 ships kars with `replicas: 2` for the controller
//! Deployment so a node drain / OOM doesn't take down the operator.
//! Without leader election, both replicas would run their reconciler
//! loops in parallel and race on Server-Side Apply — the SSA
//! `fieldManager` registry from S7.A keeps the writes coherent on
//! happy paths but does not protect against duplicate work, doubled
//! emit-events, doubled status patches, or doubled Foundry agent
//! creations through the inference router.
//!
//! S7.C closes that gap with a Kubernetes Lease (coordination.k8s.io/v1)
//! held by exactly one pod at a time. Reconciler tasks only spawn
//! after [`acquire_and_hold`] sends on the `ready` channel; if the
//! holder loses its lease (renewal failure), the function returns an
//! error which causes `main.rs` to exit and the pod to be restarted —
//! standard fail-fast operator pattern, identical to what
//! kube-controller-manager and other production controllers do.
//!
//! ## Mesh-peer is intentionally outside this gate
//!
//! `mesh_peer/mod.rs` already has its own Lease (`agentmesh-mesh-peer-leader`)
//! because the federation relay connection has different ownership
//! semantics than the reconciler bundle: only one pod *connects* to the
//! relay so peer-to-peer messages are not duplicated, but every replica
//! still needs the relay client running so a leader handover does not
//! drop in-flight pairings. That fine-grained gating is preserved as-is;
//! S7.C does not collapse it under the new controller-wide lease.
//!
//! ## Default-on, opt-out
//!
//! Set `LEADER_ELECTION_ENABLED=false` to disable. RBAC for
//! `coordination.k8s.io/leases` is already present on the controller's
//! ClusterRole (added in Phase 1 for mesh-peer's own lease), so no
//! manifest changes are required for existing clusters to pick this up.
//!
//! ## Decision logic is testable in isolation
//!
//! [`evaluate_lease`] is a pure function from `(Option<&LeaseSpec>, our
//! identity, now)` to a [`LeaseAction`]. The async loop in
//! [`acquire_and_hold`] just dispatches on the result. All branches of
//! the decision are unit-tested below.

use anyhow::{Context, Result, anyhow, ensure};
use chrono::{DateTime, Utc};
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use kube::{
    Api, Client,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::json;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep_until, timeout_at};

/// Default lease validity window. A lease is considered expired if its
/// `renewTime + leaseDurationSeconds <= now`.
pub const DEFAULT_LEASE_DURATION_SECS: i32 = 15;
/// Default renewal period for the holder. Should be well under
/// `DEFAULT_LEASE_DURATION_SECS` so a single failed PATCH does not lose
/// leadership.
pub const DEFAULT_RENEW_PERIOD_SECS: u64 = 5;
/// Default name of the controller-wide leader Lease in the controller's
/// own namespace. Keeping this stable across deployments avoids leader
/// flap during rolling upgrades.
pub const DEFAULT_LEASE_NAME: &str = "kars-controller-leader";

/// Configuration for [`acquire_and_hold`]. Most callers want
/// [`LeaderElectionConfig::from_env`].
#[derive(Debug, Clone)]
pub struct LeaderElectionConfig {
    pub lease_name: String,
    pub namespace: String,
    pub identity: String,
    pub lease_duration_secs: i32,
    pub renew_period_secs: u64,
}

impl LeaderElectionConfig {
    /// Build a config from environment variables.
    ///
    /// `POD_NAMESPACE` (downward API) — namespace where the Lease lives.
    /// Falls back to `kars-system` so the machinery works in dev /
    /// kind clusters where the downward API may not be wired into the
    /// deployment.
    /// `POD_NAME` — holder identity. Falls back to `HOSTNAME` then to
    /// `kars-controller-<pid>`.
    /// `LEADER_ELECTION_LEASE_NAME` — override lease name.
    pub fn from_env() -> Self {
        let namespace = std::env::var("POD_NAMESPACE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "kars-system".to_string());
        let identity = std::env::var("POD_NAME")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| format!("kars-controller-{}", std::process::id()));
        let lease_name = std::env::var("LEADER_ELECTION_LEASE_NAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_LEASE_NAME.to_string());
        Self {
            lease_name,
            namespace,
            identity,
            lease_duration_secs: DEFAULT_LEASE_DURATION_SECS,
            renew_period_secs: DEFAULT_RENEW_PERIOD_SECS,
        }
    }
}

/// Decision returned by [`evaluate_lease`].
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum LeaseAction {
    /// We do not currently hold the lease and may take it (either it
    /// does not exist or its current holder's renewTime expired).
    Acquire,
    /// We currently hold the lease and should refresh `renewTime`.
    Renew,
    /// Lease is held by `holder` and not yet expired. We must back
    /// off and try again later. The contained string is the holder's
    /// identity, surfaced for logging only.
    Yield(String),
}

/// Pure decision function: given the current Lease spec, our identity,
/// and the wall clock, decide whether to Acquire / Renew / Yield.
///
/// A spec with no `renewTime` is treated as expired.
#[must_use]
pub fn evaluate_lease(
    spec: Option<&LeaseSpec>,
    our_identity: &str,
    now: DateTime<Utc>,
) -> LeaseAction {
    let Some(spec) = spec else {
        return LeaseAction::Acquire;
    };
    let holder = spec.holder_identity.as_deref().unwrap_or("");
    let duration_secs = spec
        .lease_duration_seconds
        .unwrap_or(DEFAULT_LEASE_DURATION_SECS);
    let expired = match spec.renew_time.as_ref() {
        Some(t) => {
            let renew_secs = t.0.as_second();
            let now_secs = now.timestamp();
            now_secs - renew_secs >= i64::from(duration_secs)
        }
        None => true,
    };

    if expired {
        LeaseAction::Acquire
    } else if holder == our_identity {
        LeaseAction::Renew
    } else {
        LeaseAction::Yield(holder.to_string())
    }
}

/// Acquire the lease and keep renewing it forever. Sends on `ready` the
/// first time we successfully acquire so the caller can spawn its
/// reconciler bundle. Returns an error when renewal fails (callers
/// should propagate so the pod restarts).
///
/// The async loop dispatches on [`evaluate_lease`] and performs the
/// corresponding I/O (CREATE / PATCH / sleep). On any K8s API error
/// during a Yield-backoff we keep polling — transient API outages must
/// not cause us to crash, only to wait. On a Renew failure after we
/// already hold leadership, however, we return immediately: losing the
/// lease while reconcilers are running is a hard fail-stop condition.
pub async fn acquire_and_hold(
    client: Client,
    cfg: LeaderElectionConfig,
    ready: oneshot::Sender<()>,
) -> Result<()> {
    let leases: Api<Lease> = Api::namespaced(client.clone(), &cfg.namespace);
    let period = Duration::from_secs(cfg.renew_period_secs);
    let validity = Duration::from_secs(u64::try_from(cfg.lease_duration_secs)?);
    let renew_window = validity
        .checked_sub(period)
        .filter(|window| !window.is_zero() && !period.is_zero())
        .context("leader-election: renewal period must be positive and below lease validity")?;
    let mut ready = Some(ready);
    let mut held = false;
    let mut held_uid = None;
    let mut renew_deadline = None;
    loop {
        // Stop before another replica may acquire; a hung API request must not
        // leave the old reconciler bundle running past its lease.
        let deadline = renew_deadline.unwrap_or_else(|| Instant::now() + renew_window);
        ensure!(
            Instant::now() < deadline,
            "leader-election: renewal deadline exceeded"
        );
        let fetched = timeout_at(deadline, async {
            match leases.get(&cfg.lease_name).await {
                Ok(lease) => Ok(Some(lease)),
                Err(kube::Error::Api(error)) if error.code == 404 => Ok(None),
                Err(error) => Err(anyhow!(error)),
            }
        })
        .await
        .context("leader-election: lease GET deadline exceeded")
        .and_then(|result| result);
        let existing = match fetched {
            Ok(lease) => lease,
            Err(e) if !held => {
                tracing::warn!(
                    error = %e,
                    "leader-election: transient API error while polling lease; backing off",
                );
                tokio::time::sleep(period).await;
                continue;
            }
            Err(e) => {
                return Err(e).context("leader-election: lease GET failed while holder");
            }
        };

        let action = evaluate_lease(
            existing.as_ref().and_then(|l| l.spec.as_ref()),
            &cfg.identity,
            Utc::now(),
        );
        if held {
            ensure!(
                Instant::now() < deadline
                    && action == LeaseAction::Renew
                    && existing
                        .as_ref()
                        .and_then(|lease| lease.metadata.uid.as_ref())
                        == held_uid.as_ref(),
                "leader-election: controller lease expired, disappeared, changed owner or was replaced"
            );
        }
        match action {
            LeaseAction::Acquire | LeaseAction::Renew => {
                let started = Instant::now();
                let written = timeout_at(
                    deadline,
                    patch_or_create_lease(&leases, &cfg, Utc::now(), existing.as_ref()),
                )
                .await
                .context("leader-election: lease write deadline exceeded")
                .and_then(|result| result);
                let lease = match written {
                    Ok(lease) => lease,
                    Err(error) if !held => {
                        tracing::warn!(%error, "leader-election: lease acquisition failed; will retry");
                        tokio::time::sleep(period).await;
                        continue;
                    }
                    Err(error) => {
                        return Err(error).context("leader-election: lease renewal failed");
                    }
                };
                ensure!(
                    Instant::now() < deadline,
                    "leader-election: lease write completed too late"
                );
                held_uid = Some(
                    lease
                        .metadata
                        .uid
                        .filter(|uid| !uid.is_empty())
                        .context("leader-election: written lease has no UID")?,
                );
                ensure!(
                    lease
                        .spec
                        .as_ref()
                        .and_then(|spec| spec.holder_identity.as_deref())
                        == Some(cfg.identity.as_str()),
                    "leader-election: written lease has a different owner"
                );
                renew_deadline = Some(started + renew_window);
                held = true;
                if let Some(tx) = ready.take() {
                    let _ = tx.send(());
                    tracing::info!(
                        lease = %cfg.lease_name,
                        identity = %cfg.identity,
                        "leader-election: acquired controller lease",
                    );
                }
            }
            LeaseAction::Yield(holder) => {
                tracing::debug!(
                    current_holder = %holder,
                    "leader-election: another replica holds the controller lease; waiting",
                );
            }
        }
        let next_poll = Instant::now() + period;
        sleep_until(renew_deadline.map_or(next_poll, |deadline| next_poll.min(deadline))).await;
    }
}

/// Create an absent lease or conditionally update the exact observed lease.
async fn patch_or_create_lease(
    leases: &Api<Lease>,
    cfg: &LeaderElectionConfig,
    now: DateTime<Utc>,
    observed: Option<&Lease>,
) -> Result<Lease> {
    let now_rfc3339 = now.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string();
    if observed.is_none() {
        let lease: Lease = serde_json::from_value(json!({
            "apiVersion": "coordination.k8s.io/v1",
            "kind": "Lease",
            "metadata": {
                "name": cfg.lease_name,
                "namespace": cfg.namespace,
            },
            "spec": {
                "holderIdentity": cfg.identity,
                "leaseDurationSeconds": cfg.lease_duration_secs,
                "acquireTime": now_rfc3339,
                "renewTime": now_rfc3339,
            }
        }))?;
        leases
            .create(&PostParams::default(), &lease)
            .await
            .map_err(|e| anyhow!(e))
    } else {
        let observed = observed.context("leader-election: missing observed lease")?;
        let uid = observed
            .metadata
            .uid
            .as_deref()
            .filter(|uid| !uid.is_empty())
            .context("leader-election: observed lease has no UID")?;
        let version = observed
            .metadata
            .resource_version
            .as_deref()
            .filter(|rv| !rv.is_empty())
            .context("leader-election: observed lease has no resourceVersion")?;
        let patch = json!({
            "metadata": {"uid": uid, "resourceVersion": version},
            "spec": {
                "holderIdentity": cfg.identity,
                "leaseDurationSeconds": cfg.lease_duration_secs,
                "renewTime": now_rfc3339,
            }
        });
        leases
            .patch(
                &cfg.lease_name,
                &PatchParams::default(),
                &Patch::Merge(patch),
            )
            .await
            .map_err(|e| anyhow!(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;

    fn micro_time(secs: i64) -> MicroTime {
        MicroTime(k8s_openapi::jiff::Timestamp::from_second(secs).unwrap())
    }

    fn spec_with(holder: &str, renew_secs_ago: i64, duration: i32) -> LeaseSpec {
        LeaseSpec {
            holder_identity: Some(holder.to_string()),
            lease_duration_seconds: Some(duration),
            renew_time: Some(micro_time(1_700_000_000 - renew_secs_ago)),
            acquire_time: Some(micro_time(1_700_000_000 - renew_secs_ago)),
            ..Default::default()
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn missing_spec_means_acquire() {
        assert_eq!(evaluate_lease(None, "us", now()), LeaseAction::Acquire);
    }

    #[test]
    fn we_hold_fresh_lease_means_renew() {
        let spec = spec_with("us", 1, 15);
        assert_eq!(evaluate_lease(Some(&spec), "us", now()), LeaseAction::Renew);
    }

    #[test]
    fn our_expired_lease_requires_fresh_acquisition() {
        let spec = spec_with("us", 60, 15);
        assert_eq!(
            evaluate_lease(Some(&spec), "us", now()),
            LeaseAction::Acquire
        );
    }

    #[test]
    fn lease_expires_at_its_validity_boundary() {
        for holder in ["us", "them"] {
            assert_eq!(
                evaluate_lease(Some(&spec_with(holder, 15, 15)), "us", now()),
                LeaseAction::Acquire
            );
        }
    }

    #[test]
    fn other_holder_fresh_lease_means_yield() {
        let spec = spec_with("them", 1, 15);
        assert_eq!(
            evaluate_lease(Some(&spec), "us", now()),
            LeaseAction::Yield("them".into())
        );
    }

    #[test]
    fn other_holder_expired_lease_means_acquire() {
        // 60s past, lease window is 15s -> expired, take over.
        let spec = spec_with("them", 60, 15);
        assert_eq!(
            evaluate_lease(Some(&spec), "us", now()),
            LeaseAction::Acquire
        );
    }

    #[test]
    fn missing_renew_time_is_treated_as_expired() {
        let spec = LeaseSpec {
            holder_identity: Some("them".into()),
            lease_duration_seconds: Some(15),
            renew_time: None,
            ..Default::default()
        };
        assert_eq!(
            evaluate_lease(Some(&spec), "us", now()),
            LeaseAction::Acquire
        );
    }

    #[test]
    fn empty_holder_identity_with_fresh_renew_yields_to_unknown_peer() {
        // Defensive: an empty holder_identity with a fresh renewTime is
        // a malformed Lease. We treat it as held by some unknown peer
        // and back off rather than racing them.
        let spec = LeaseSpec {
            holder_identity: Some(String::new()),
            lease_duration_seconds: Some(15),
            renew_time: Some(micro_time(1_700_000_000 - 1)),
            ..Default::default()
        };
        assert_eq!(
            evaluate_lease(Some(&spec), "us", now()),
            LeaseAction::Yield(String::new())
        );
    }
}

#[cfg(test)]
#[path = "leader_election_io_tests.rs"]
mod io_tests;
