//! Requeue duration helpers with bounded random jitter.
//!
//! ## Why jitter?
//!
//! All nine `ClawSandbox` / per-CRD reconcilers in this controller use
//! `Action::requeue(Duration::from_secs(N))` to schedule the next
//! reconcile attempt — for both success requeues (poll Foundry agent
//! status, refresh policies, etc.) and error requeues (retry after a
//! transient API failure). Without jitter, every CR observed during a
//! single watcher resync schedules its retry for *exactly* the same
//! wall-clock instant, creating a thundering-herd burst against the
//! Kubernetes API server every `N` seconds. With even 200+ CRs this
//! shows up as periodic API-server CPU spikes and degraded reconcile
//! latency.
//!
//! S7.D adds bounded multiplicative jitter (±20% by default) to every
//! requeue path so the retry distribution spreads out across each
//! interval. The choice of ±20% follows the Kubernetes ecosystem
//! convention (`k8s.io/apimachinery/pkg/util/wait` uses this default
//! across kube-controller-manager and most operator SDKs).
//!
//! ## API
//!
//! The two public helpers compose as:
//! - [`with_jitter`] takes a base `Duration` and returns it ±20%.
//!   Use for arbitrary durations (e.g. polling intervals tied to
//!   external service latency).
//! - [`requeue_secs_with_jitter`] takes a base seconds count and
//!   returns a `Duration` ±20%. Sugar for the most common call site
//!   pattern in `error_policy` functions.
//!
//! Both are deterministic-test-friendly: the jitter source is
//! `rand::rng()` so callers can swap it under test by reseeding the
//! thread-local RNG. The pure jitter math is exposed as
//! [`apply_jitter_factor`] for unit tests that don't need an RNG at
//! all.

use std::time::Duration;

/// Default symmetric jitter factor. ±20% chosen to match
/// `k8s.io/apimachinery/pkg/util/wait` defaults.
pub const DEFAULT_JITTER_FACTOR: f64 = 0.2;

/// Apply a multiplicative jitter factor to a base duration. The
/// returned duration is in `[base * (1 - factor), base * (1 + factor)]`
/// where `factor ∈ [0.0, 1.0]`.
///
/// This is the pure math, exposed for unit tests. A `factor` outside
/// `[0, 1]` is clamped — defensive against accidental misuse.
#[must_use]
pub fn apply_jitter_factor(base: Duration, factor: f64, sample: f64) -> Duration {
    let factor = factor.clamp(0.0, 1.0);
    // sample ∈ [0, 1) -> spread ∈ [-1, 1)
    let spread = (sample.clamp(0.0, 1.0) * 2.0) - 1.0;
    let multiplier = (1.0 + factor * spread).max(0.0);
    let secs = base.as_secs_f64() * multiplier;
    Duration::from_secs_f64(secs.max(0.0))
}

/// Apply [`DEFAULT_JITTER_FACTOR`] to `base` using the thread-local
/// RNG. Returns a new `Duration` suitable for `Action::requeue`.
#[must_use]
pub fn with_jitter(base: Duration) -> Duration {
    use rand::Rng;
    let sample: f64 = rand::rng().random_range(0.0..1.0);
    apply_jitter_factor(base, DEFAULT_JITTER_FACTOR, sample)
}

/// Convenience wrapper: build a `Duration` from `base_secs` and apply
/// [`DEFAULT_JITTER_FACTOR`]. Mirrors the most common call site shape
/// `Action::requeue(Duration::from_secs(N))`.
#[must_use]
pub fn requeue_secs_with_jitter(base_secs: u64) -> Duration {
    with_jitter(Duration::from_secs(base_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_zero_factor_returns_base() {
        let d = apply_jitter_factor(Duration::from_secs(30), 0.0, 0.5);
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn jitter_zero_sample_subtracts_full_factor() {
        // sample=0 -> spread=-1 -> multiplier = 1 - factor.
        let d = apply_jitter_factor(Duration::from_secs(100), 0.2, 0.0);
        assert_eq!(d, Duration::from_secs_f64(80.0));
    }

    #[test]
    fn jitter_max_sample_adds_full_factor() {
        // sample=1 -> spread=+1 -> multiplier = 1 + factor.
        let d = apply_jitter_factor(Duration::from_secs(100), 0.2, 1.0);
        assert_eq!(d, Duration::from_secs_f64(120.0));
    }

    #[test]
    fn jitter_midpoint_sample_returns_base() {
        let d = apply_jitter_factor(Duration::from_secs(50), 0.2, 0.5);
        assert_eq!(d, Duration::from_secs(50));
    }

    #[test]
    fn jitter_factor_above_one_is_clamped() {
        // factor=2 would otherwise produce negative multiplier at
        // sample=0; the clamp keeps factor at 1.0 -> multiplier=0.
        let d = apply_jitter_factor(Duration::from_secs(10), 2.0, 0.0);
        assert_eq!(d, Duration::ZERO);
    }

    #[test]
    fn jitter_negative_factor_is_clamped() {
        let d = apply_jitter_factor(Duration::from_secs(10), -0.5, 0.0);
        assert_eq!(d, Duration::from_secs(10));
    }

    #[test]
    fn jitter_never_returns_negative_duration() {
        // Factor=1, sample=0 -> multiplier=0 -> exactly zero, never below.
        for sample in [0.0, 0.25, 0.5, 0.75, 0.999_999] {
            let d = apply_jitter_factor(Duration::from_secs(10), 1.0, sample);
            assert!(d >= Duration::ZERO, "sample {sample} produced {d:?}");
        }
    }

    #[test]
    fn requeue_secs_with_jitter_stays_within_default_band() {
        // Sample 50 invocations and assert all fall within ±20%.
        let base = Duration::from_secs(30);
        let lo = base.mul_f64(1.0 - DEFAULT_JITTER_FACTOR);
        let hi = base.mul_f64(1.0 + DEFAULT_JITTER_FACTOR);
        for _ in 0..50 {
            let d = requeue_secs_with_jitter(30);
            assert!(
                d >= lo && d <= hi,
                "out-of-band: {d:?} (lo={lo:?}, hi={hi:?})"
            );
        }
    }

    #[test]
    fn jitter_distribution_is_not_constant() {
        // Pull 32 samples and assert at least 3 distinct values exist.
        // With ±20% jitter on a 30s base and an RNG sample resolution
        // of f64, collisions are vanishingly unlikely; this guards
        // against an accidental stub that returns the base unchanged.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..32 {
            seen.insert(requeue_secs_with_jitter(30).as_nanos());
        }
        assert!(
            seen.len() >= 3,
            "expected diverse samples, got {} distinct: {seen:?}",
            seen.len()
        );
    }
}
