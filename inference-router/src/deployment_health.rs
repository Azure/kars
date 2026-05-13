// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Per-deployment health cache backing Slice 2d.2's health-aware
//! `modelPreference` failover.
//!
//! Scope: **same-provider, deployment-only failover**. The router holds
//! one Foundry/AOAI client at process start; this cache lets the
//! request hot-path walk `primary.deployment → fallback[N].deployment`
//! while skipping deployments that have been observed flapping.
//!
//! Algorithm (per deployment, keyed by deployment name string):
//! * Track consecutive 5xx/429 failures with a streak counter +
//!   `first_failure_at_ms` timestamp.
//! * A deployment is **unhealthy** when ≥ `failure_threshold`
//!   consecutive failures occur within `streak_window_ms` (the streak
//!   window restarts on any success — Slice 2d.2 ships with 3 strikes
//!   inside a 60s rolling window; both knobs are constructor args).
//! * Recovery is **single-success-based**: one 2xx clears the streak
//!   and marks the deployment healthy again.
//! * Unknown deployments are healthy by default (no observation yet
//!   means no reason to skip — defence-in-depth against a stale cache
//!   blackholing every request after a router restart).
//!
//! Hot-path constraint (per `principles.md`): `is_healthy()` is a
//! `std::sync::RwLock::read()` of a `HashMap` (cheap, parallel-safe)
//! followed by an `Arc::clone` + atomic loads of the per-entry state
//! (`AtomicU{8,64}`). No `Mutex`. Writes only happen on first-time
//! observation of a new deployment name, which is bounded by the
//! cardinality of `primary.deployment + fallback[].deployment` in
//! the loaded `InferencePolicy` (typically ≤ 5).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default consecutive-failure threshold before a deployment is marked
/// unhealthy. Picked deliberately small: three 5xx/429 in a row is a
/// strong "this endpoint is down right now" signal, but small enough
/// that a single hiccup doesn't trip the failover. Lifted to a
/// constructor argument for tests + future tuning.
pub const DEFAULT_FAILURE_THRESHOLD: u8 = 3;

/// Default rolling window the streak must complete inside before
/// `is_healthy()` flips to `false`. 60s matches the spec line in
/// `docs/internal/crd-well-oiled-machine/slice-2-inference-policy.md`.
pub const DEFAULT_STREAK_WINDOW_MS: u64 = 60_000;

/// Per-deployment failure-streak state. All counters live in atomics so
/// the hot path can read/update without a lock.
#[derive(Debug, Default)]
struct DeploymentHealth {
    /// Consecutive 5xx/429 count. Reset to 0 on the next success.
    failure_streak: AtomicU8,
    /// Unix-ms when the current failure streak began. `0` means no
    /// active streak. Used to expire stale streaks once a deployment
    /// has been idle for a full `streak_window_ms`.
    first_failure_at_ms: AtomicU64,
    /// Unix-ms of the most recent success, surfaced only for the
    /// echo-into-`/internal/policy-status` snapshot. `0` means never.
    last_success_at_ms: AtomicU64,
}

/// Process-wide registry of deployment health, keyed by the literal
/// `deployment` string the router would put in the upstream URL.
///
/// Constructed once at startup and shared via `Arc` from `AppState`.
pub struct DeploymentHealthRegistry {
    by_deployment: RwLock<HashMap<String, Arc<DeploymentHealth>>>,
    failure_threshold: u8,
    streak_window_ms: u64,
}

impl DeploymentHealthRegistry {
    /// Default-tuned registry (3 strikes / 60s window).
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(DEFAULT_FAILURE_THRESHOLD, DEFAULT_STREAK_WINDOW_MS)
    }

    /// Custom-tuned registry — used by tests with a tight window so
    /// the cache flips deterministically inside a single test body.
    #[must_use]
    pub fn with_config(failure_threshold: u8, streak_window_ms: u64) -> Self {
        Self {
            by_deployment: RwLock::new(HashMap::new()),
            failure_threshold,
            streak_window_ms,
        }
    }

    fn entry(&self, deployment: &str) -> Arc<DeploymentHealth> {
        if let Ok(map) = self.by_deployment.read()
            && let Some(existing) = map.get(deployment)
        {
            return Arc::clone(existing);
        }
        // Upgrade to write lock — rare path (only the very first
        // observation of a never-before-seen deployment name).
        let mut map = self
            .by_deployment
            .write()
            .expect("deployment health write lock poisoned");
        Arc::clone(
            map.entry(deployment.to_string())
                .or_insert_with(|| Arc::new(DeploymentHealth::default())),
        )
    }

    /// `true` when the deployment has either never been observed or
    /// is currently inside its tolerance window. Read-side is a
    /// `RwLock::read()` + atomic loads only.
    ///
    /// A deployment crosses to unhealthy iff:
    /// * `failure_streak >= failure_threshold`, **and**
    /// * the streak began within the last `streak_window_ms`.
    ///
    /// Once the window expires without a success, the next call to
    /// `is_healthy` self-heals the entry (resetting `failure_streak`)
    /// — this prevents a long-idle deployment from staying marked
    /// unhealthy forever after a router lull.
    #[must_use]
    pub fn is_healthy(&self, deployment: &str) -> bool {
        let state = {
            let map = match self.by_deployment.read() {
                Ok(m) => m,
                Err(_) => return true,
            };
            match map.get(deployment) {
                Some(s) => Arc::clone(s),
                None => return true,
            }
        };
        let streak = state.failure_streak.load(Ordering::Relaxed);
        if streak < self.failure_threshold {
            return true;
        }
        let started_at = state.first_failure_at_ms.load(Ordering::Relaxed);
        if started_at == 0 {
            return true;
        }
        let now = now_ms();
        if now.saturating_sub(started_at) > self.streak_window_ms {
            state.failure_streak.store(0, Ordering::Relaxed);
            state.first_failure_at_ms.store(0, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Record a 5xx/429 against `deployment`. Caller is expected to
    /// pre-filter — only retryable upstream failures should land here
    /// (a 400 client error is not the deployment's fault).
    pub fn record_failure(&self, deployment: &str) {
        let state = self.entry(deployment);
        let prev = state.failure_streak.fetch_add(1, Ordering::Relaxed);
        if prev == u8::MAX {
            state.failure_streak.store(u8::MAX, Ordering::Relaxed);
        }
        let _ = state.first_failure_at_ms.compare_exchange(
            0,
            now_ms(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// Record a 2xx against `deployment`. Single-success recovery:
    /// the entire streak is cleared so the next failure starts a
    /// fresh window.
    pub fn record_success(&self, deployment: &str) {
        let state = self.entry(deployment);
        state.failure_streak.store(0, Ordering::Relaxed);
        state.first_failure_at_ms.store(0, Ordering::Relaxed);
        state.last_success_at_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Snapshot of every deployment the router has observed since
    /// startup. Used by `/internal/policy-status` to echo health into
    /// the controller's polling loop without copying the live atomics.
    #[must_use]
    pub fn snapshot(&self) -> Vec<DeploymentHealthSnapshot> {
        let map = match self.by_deployment.read() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        map.iter()
            .map(|(k, s)| DeploymentHealthSnapshot {
                deployment: k.clone(),
                healthy: {
                    // Inline the freshness check rather than re-acquiring
                    // the outer read lock for each entry.
                    let streak = s.failure_streak.load(Ordering::Relaxed);
                    let started = s.first_failure_at_ms.load(Ordering::Relaxed);
                    if streak < self.failure_threshold || started == 0 {
                        true
                    } else {
                        now_ms().saturating_sub(started) > self.streak_window_ms
                    }
                },
                failure_streak: s.failure_streak.load(Ordering::Relaxed),
                first_failure_at_ms: s.first_failure_at_ms.load(Ordering::Relaxed),
                last_success_at_ms: s.last_success_at_ms.load(Ordering::Relaxed),
            })
            .collect()
    }
}

impl Default for DeploymentHealthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// JSON-shaped snapshot used by `/internal/policy-status`. Kept
/// separate from the live atomics so the controller's polling client
/// has a stable wire contract independent of the in-process state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeploymentHealthSnapshot {
    pub deployment: String,
    pub healthy: bool,
    pub failure_streak: u8,
    /// `0` when no streak is currently active.
    pub first_failure_at_ms: u64,
    /// `0` when no success has been observed since startup.
    pub last_success_at_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_deployment_is_healthy() {
        let reg = DeploymentHealthRegistry::new();
        assert!(reg.is_healthy("gpt-4o"));
        // Snapshot does not allocate an entry for an unobserved key.
        assert!(reg.snapshot().is_empty());
    }

    #[test]
    fn single_failure_below_threshold_stays_healthy() {
        let reg = DeploymentHealthRegistry::with_config(3, 60_000);
        reg.record_failure("dep-a");
        reg.record_failure("dep-a");
        assert!(reg.is_healthy("dep-a"));
    }

    #[test]
    fn three_failures_flip_to_unhealthy() {
        let reg = DeploymentHealthRegistry::with_config(3, 60_000);
        for _ in 0..3 {
            reg.record_failure("dep-a");
        }
        assert!(!reg.is_healthy("dep-a"));
    }

    #[test]
    fn single_success_recovers_after_unhealthy() {
        let reg = DeploymentHealthRegistry::with_config(3, 60_000);
        for _ in 0..3 {
            reg.record_failure("dep-a");
        }
        assert!(!reg.is_healthy("dep-a"));
        reg.record_success("dep-a");
        assert!(reg.is_healthy("dep-a"));
    }

    #[test]
    fn streak_window_self_heals_after_expiry() {
        // 1ms window — easy to deterministically blow past.
        let reg = DeploymentHealthRegistry::with_config(3, 1);
        for _ in 0..3 {
            reg.record_failure("dep-a");
        }
        // Sleep just over the window so the next `is_healthy` self-heals.
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(reg.is_healthy("dep-a"));
        // And the streak counter is now visibly reset.
        let snap = reg
            .snapshot()
            .into_iter()
            .find(|s| s.deployment == "dep-a")
            .unwrap();
        assert_eq!(snap.failure_streak, 0);
    }

    #[test]
    fn isolated_deployments_dont_cross_contaminate() {
        let reg = DeploymentHealthRegistry::with_config(3, 60_000);
        for _ in 0..3 {
            reg.record_failure("dep-a");
        }
        assert!(!reg.is_healthy("dep-a"));
        assert!(reg.is_healthy("dep-b"));
    }

    #[test]
    fn snapshot_reflects_live_state() {
        let reg = DeploymentHealthRegistry::with_config(3, 60_000);
        reg.record_failure("dep-a");
        reg.record_success("dep-b");
        let snaps = reg.snapshot();
        let a = snaps.iter().find(|s| s.deployment == "dep-a").unwrap();
        let b = snaps.iter().find(|s| s.deployment == "dep-b").unwrap();
        assert_eq!(a.failure_streak, 1);
        assert!(a.healthy);
        assert_eq!(b.failure_streak, 0);
        assert!(b.last_success_at_ms > 0);
    }

    #[test]
    fn failure_counter_saturates_at_u8_max() {
        let reg = DeploymentHealthRegistry::with_config(3, 60_000);
        for _ in 0..300 {
            reg.record_failure("dep-a");
        }
        let snap = reg
            .snapshot()
            .into_iter()
            .find(|s| s.deployment == "dep-a")
            .unwrap();
        // Did not overflow back to a small value.
        assert_eq!(snap.failure_streak, u8::MAX);
        assert!(!reg.is_healthy("dep-a"));
    }

    #[test]
    fn first_failure_timestamp_anchors_to_streak_start() {
        let reg = DeploymentHealthRegistry::with_config(3, 60_000);
        reg.record_failure("dep-a");
        let snap1 = reg
            .snapshot()
            .into_iter()
            .find(|s| s.deployment == "dep-a")
            .unwrap();
        let anchor = snap1.first_failure_at_ms;
        std::thread::sleep(std::time::Duration::from_millis(5));
        reg.record_failure("dep-a");
        let snap2 = reg
            .snapshot()
            .into_iter()
            .find(|s| s.deployment == "dep-a")
            .unwrap();
        // Anchor is sticky — the second failure does NOT bump it.
        assert_eq!(snap2.first_failure_at_ms, anchor);
    }
}
