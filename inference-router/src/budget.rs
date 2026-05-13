// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Token budget enforcement — per-sandbox UTC-calendar daily +
//! monthly counters with on-disk persistence and per-request cap.
//!
//! Wire contract for Slice 2b:
//!
//! * `check_budget(sandbox, daily_limit, monthly_limit)` is called
//!   BEFORE every inference call. Returns `Err` when either counter is
//!   at-or-over the corresponding limit, mapped to HTTP 429 in
//!   `chat_completions.rs`.
//! * `record_usage(sandbox, tokens)` is called AFTER every inference
//!   call with the total tokens returned by the upstream provider.
//!   Persists the updated counters to `persist_path` on a best-effort
//!   basis (errors are logged, never propagated).
//! * `check_per_request(total)` is the existing warn-only response-side
//!   check (kept for back-compat — pre-flight `max_tokens` cap lives
//!   in `chat_completions::decide_per_request_gate`).
//!
//! Reset semantics (UTC calendar):
//!
//! * Daily counter resets when the current UTC date changes vs the
//!   stored `day_key` (Unix days since epoch).
//! * Monthly counter resets when the current UTC `(year, month0)`
//!   pair changes vs the stored `month_key` (year*12 + month0).
//!
//! Limits are **passed per-call** by the caller (sourced from the
//! loaded `InferencePolicy` when present, env defaults when not).
//! This lets policy hot-reload take effect on the very next request
//! without the tracker holding stale state.
//!
//! Persistence file is JSON of shape
//! `{ "sandboxes": { "<name>": { "day_key", "daily", "month_key",
//! "monthly" } } }`. Atomic-rename pattern: write to `*.tmp`, fsync,
//! rename over the target. On startup we load on best-effort — a
//! corrupt or missing file means we start fresh (this is correct: the
//! tracker is a counter, never a source of truth, and over-counting
//! is the safer failure mode anyway).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Trait abstraction over "what time is it" so the unit tests can
/// drive the UTC calendar boundaries deterministically. Production
/// uses `SystemClock` which delegates to `chrono::Utc::now()`.
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock — `chrono::Utc::now()` straight through.
#[derive(Debug, Default, Clone)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Days since Unix epoch in UTC. Wraps far beyond u32 limits (~11.7m
/// years from epoch) so casting back is safe in practice.
fn day_key(now: DateTime<Utc>) -> u32 {
    let days = now.date_naive().num_days_from_ce();
    // Anchor at CE day 1 — we only care about deltas, the absolute
    // value isn't surfaced anywhere.
    days as u32
}

/// `year * 12 + month0` — strictly monotonic across year boundaries.
fn month_key(now: DateTime<Utc>) -> u32 {
    let y = now.year() as u32;
    let m0 = now.month0();
    y * 12 + m0
}

/// On-disk shape — one row per sandbox.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SandboxCounters {
    day_key: u32,
    daily: u64,
    month_key: u32,
    monthly: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    sandboxes: HashMap<String, SandboxCounters>,
}

/// Per-sandbox token usage tracker with UTC-calendar daily +
/// monthly reset and best-effort disk persistence.
#[derive(Clone, Debug)]
pub struct TokenBudgetTracker {
    /// Env-default daily limit (0 = unlimited). Used when the caller
    /// doesn't pass a policy override. Kept for the legacy
    /// `TOKEN_BUDGET_DAILY` path.
    daily_default: u64,
    /// Per-response token limit (0 = unlimited). Warn-only.
    per_request_limit: u64,
    /// Per-sandbox cumulative usage.
    usage: Arc<RwLock<HashMap<String, SandboxCounters>>>,
    /// `None` ⇒ in-memory only (test mode). `Some(path)` ⇒ writes on
    /// every `record_usage`. The dir must exist; we don't try to
    /// `mkdir -p`.
    persist_path: Option<PathBuf>,
    /// Injectable clock — `SystemClock` in production.
    clock: Arc<dyn Clock>,
}

impl TokenBudgetTracker {
    /// Construct an in-memory tracker (no persistence) with the
    /// system clock. Equivalent of the pre–Slice-2b constructor.
    #[must_use]
    pub fn new(daily_default: u64, per_request_limit: u64) -> Self {
        Self {
            daily_default,
            per_request_limit,
            usage: Arc::new(RwLock::new(HashMap::new())),
            persist_path: None,
            clock: Arc::new(SystemClock),
        }
    }

    /// Construct a tracker that loads previously-persisted counters
    /// from `persist_path` and writes to it on every `record_usage`.
    /// Load failures (missing file, malformed JSON) are logged and
    /// the tracker starts empty — over-counting is safer than
    /// under-counting.
    pub fn with_persistence<P: AsRef<Path>>(
        daily_default: u64,
        per_request_limit: u64,
        persist_path: P,
    ) -> Self {
        let path = persist_path.as_ref().to_path_buf();
        let initial = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedState>(&bytes) {
                Ok(state) => {
                    tracing::info!(
                        path = %path.display(),
                        sandboxes = state.sandboxes.len(),
                        "Token budget counters restored from disk"
                    );
                    state.sandboxes
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Token budget persistence file is malformed — starting empty"
                    );
                    HashMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Token budget persistence file unreadable — starting empty"
                );
                HashMap::new()
            }
        };
        Self {
            daily_default,
            per_request_limit,
            usage: Arc::new(RwLock::new(initial)),
            persist_path: Some(path),
            clock: Arc::new(SystemClock),
        }
    }

    /// Test-only constructor: inject a fake clock so the UTC-boundary
    /// reset behaviour can be asserted deterministically.
    #[cfg(test)]
    fn with_clock(
        daily_default: u64,
        per_request_limit: u64,
        clock: Arc<dyn Clock>,
        persist_path: Option<PathBuf>,
    ) -> Self {
        let initial = persist_path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<PersistedState>(&b).ok())
            .map(|s| s.sandboxes)
            .unwrap_or_default();
        Self {
            daily_default,
            per_request_limit,
            usage: Arc::new(RwLock::new(initial)),
            persist_path,
            clock,
        }
    }

    /// Pre-request gate. Returns `Err(message)` when either the daily
    /// or monthly counter is already at-or-over the corresponding
    /// limit. `daily_limit` / `monthly_limit` of `0` (or absent
    /// `None` via the helper) means "no enforcement on that axis";
    /// passing `None` for daily falls back to the env-default
    /// `daily_default`.
    pub async fn check_budget(
        &self,
        sandbox: &str,
        daily_limit: Option<u64>,
        monthly_limit: Option<u64>,
    ) -> Result<(), String> {
        let daily_effective = daily_limit.unwrap_or(self.daily_default);
        let monthly_effective = monthly_limit.unwrap_or(0);
        if daily_effective == 0 && monthly_effective == 0 {
            return Ok(());
        }

        let now = self.clock.now();
        let today = day_key(now);
        let this_month = month_key(now);

        let usage = self.usage.read().await;
        let entry = match usage.get(sandbox) {
            Some(e) => e,
            None => return Ok(()),
        };

        // Roll forward if the stored counter is from an earlier
        // day / month — those are conceptually zero now.
        let daily_used = if entry.day_key == today {
            entry.daily
        } else {
            0
        };
        let monthly_used = if entry.month_key == this_month {
            entry.monthly
        } else {
            0
        };

        if daily_effective > 0 && daily_used >= daily_effective {
            return Err(format!(
                "Daily token budget exceeded ({daily_used}/{daily_effective} tokens). \
                 Resets at the next UTC midnight."
            ));
        }
        if monthly_effective > 0 && monthly_used >= monthly_effective {
            return Err(format!(
                "Monthly token budget exceeded ({monthly_used}/{monthly_effective} tokens). \
                 Resets on the 1st of next month UTC."
            ));
        }
        Ok(())
    }

    /// Compat shim for the legacy single-limit path. New code should
    /// pass the loaded policy's limits directly via [`check_budget`].
    pub async fn check_budget_legacy(&self, sandbox: &str) -> Result<(), String> {
        self.check_budget(sandbox, None, None).await
    }

    /// Per-response cap (warn-style — current callers map this to
    /// HTTP 429 only when the request actually overshot the
    /// `per_request_limit`). Unchanged from pre-Slice-2b.
    pub fn check_per_request(&self, total_tokens: u64) -> Result<(), String> {
        if self.per_request_limit > 0 && total_tokens > self.per_request_limit {
            return Err(format!(
                "Per-request token limit exceeded ({}/{} tokens)",
                total_tokens, self.per_request_limit
            ));
        }
        Ok(())
    }

    /// Record token usage from a completed request. Updates both the
    /// daily and monthly counters with UTC-calendar rollover, then
    /// best-effort persists. Cost: one snapshot serialise + atomic
    /// rename per inference call; well within budget for AI-call
    /// latency (~100ms upstream).
    pub async fn record_usage(&self, sandbox: &str, tokens: u64) {
        let now = self.clock.now();
        let today = day_key(now);
        let this_month = month_key(now);

        let snapshot = {
            let mut usage = self.usage.write().await;
            let entry = usage.entry(sandbox.to_string()).or_insert(SandboxCounters {
                day_key: today,
                daily: 0,
                month_key: this_month,
                monthly: 0,
            });

            if entry.day_key != today {
                entry.day_key = today;
                entry.daily = 0;
            }
            if entry.month_key != this_month {
                entry.month_key = this_month;
                entry.monthly = 0;
            }
            entry.daily = entry.daily.saturating_add(tokens);
            entry.monthly = entry.monthly.saturating_add(tokens);

            if self.daily_default > 0 {
                let pct = (entry.daily as f64 / self.daily_default as f64 * 100.0) as u64;
                if pct >= 90 {
                    tracing::warn!(
                        sandbox = %sandbox,
                        used = entry.daily,
                        limit = self.daily_default,
                        "Daily token budget at {pct}% (env default)"
                    );
                }
            }

            PersistedState {
                sandboxes: usage.clone(),
            }
        };

        if let Some(ref path) = self.persist_path {
            if let Err(e) = persist_atomically(path, &snapshot) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Token budget persistence write failed (continuing in-memory)"
                );
            }
        }
    }

    /// Get current daily + monthly usage for a sandbox (or `(0, 0)`
    /// when the sandbox has never been seen). Returned values are
    /// roll-forward aware — counters from a previous day / month
    /// report as `0`.
    pub async fn get_usage(&self, sandbox: &str) -> (u64, u64) {
        let usage = self.usage.read().await;
        let now = self.clock.now();
        let today = day_key(now);
        let this_month = month_key(now);
        match usage.get(sandbox) {
            Some(entry) => {
                let d = if entry.day_key == today {
                    entry.daily
                } else {
                    0
                };
                let m = if entry.month_key == this_month {
                    entry.monthly
                } else {
                    0
                };
                (d, m)
            }
            None => (0, 0),
        }
    }
}

/// Atomic write helper: serialize → tmp → fsync → rename. Failures
/// surface to the caller, which logs and continues (counter
/// in-memory remains authoritative for the current process).
fn persist_atomically(target: &Path, state: &PersistedState) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state).map_err(std::io::Error::other)?;
    let tmp = target.with_extension("tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Test clock holding a fixed `DateTime<Utc>` (mutable via
    /// `set`). Lets us cross UTC midnight + month boundary
    /// deterministically without touching system time.
    #[derive(Debug)]
    struct FixedClock(std::sync::Mutex<DateTime<Utc>>);
    impl FixedClock {
        fn new(t: DateTime<Utc>) -> Self {
            Self(std::sync::Mutex::new(t))
        }
        fn set(&self, t: DateTime<Utc>) {
            *self.0.lock().unwrap() = t;
        }
    }
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    fn tracker_at(
        t: DateTime<Utc>,
        daily_default: u64,
        persist: Option<PathBuf>,
    ) -> (TokenBudgetTracker, Arc<FixedClock>) {
        let clock = Arc::new(FixedClock::new(t));
        let tracker = TokenBudgetTracker::with_clock(daily_default, 0, clock.clone(), persist);
        (tracker, clock)
    }

    #[tokio::test]
    async fn unlimited_budget_always_allows() {
        let (tracker, _) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap(),
            0,
            None,
        );
        assert!(tracker.check_budget("s", None, None).await.is_ok());
        tracker.record_usage("s", 1_000_000).await;
        assert!(tracker.check_budget("s", None, None).await.is_ok());
    }

    #[tokio::test]
    async fn daily_limit_from_policy_overrides_env_default() {
        // Env default cap = 1M (very permissive); policy cap = 100
        // (strict). After recording 150 tokens we're under env but
        // over policy. Passing `Some(100)` MUST block — proves the
        // caller's override wins over `daily_default`.
        let (tracker, _) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap(),
            1_000_000,
            None,
        );
        tracker.record_usage("s", 150).await;
        assert!(
            tracker.check_budget("s", Some(100), None).await.is_err(),
            "policy override (100) must apply even when env default (1M) would allow"
        );
        // No override + plenty of headroom = OK.
        assert!(tracker.check_budget("s", None, None).await.is_ok());
    }

    #[tokio::test]
    async fn daily_limit_blocks_when_exceeded() {
        let (tracker, _) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap(),
            0,
            None,
        );
        assert!(tracker.check_budget("s", Some(100), None).await.is_ok());
        tracker.record_usage("s", 100).await;
        let err = tracker
            .check_budget("s", Some(100), None)
            .await
            .unwrap_err();
        assert!(err.contains("Daily"));
        assert!(err.contains("UTC midnight"));
    }

    #[tokio::test]
    async fn monthly_limit_blocks_when_exceeded() {
        let (tracker, _) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap(),
            0,
            None,
        );
        tracker.record_usage("s", 1_000).await;
        let err = tracker
            .check_budget("s", Some(10_000), Some(500))
            .await
            .unwrap_err();
        assert!(err.contains("Monthly"));
        assert!(err.contains("1st of next month UTC"));
    }

    #[tokio::test]
    async fn daily_counter_resets_at_utc_midnight() {
        let (tracker, clock) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 13, 23, 59, 0).unwrap(),
            0,
            None,
        );
        tracker.record_usage("s", 500).await;
        assert!(tracker.check_budget("s", Some(500), None).await.is_err());

        // Advance one minute → crosses UTC midnight.
        clock.set(Utc.with_ymd_and_hms(2026, 5, 14, 0, 0, 30).unwrap());
        assert!(
            tracker.check_budget("s", Some(500), None).await.is_ok(),
            "daily counter must roll over at UTC midnight"
        );
        let (d, m) = tracker.get_usage("s").await;
        assert_eq!(d, 0, "daily reads as 0 after rollover (roll-forward)");
        assert_eq!(m, 500, "monthly preserves across daily rollover");
    }

    #[tokio::test]
    async fn monthly_counter_resets_on_first_of_next_month() {
        let (tracker, clock) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 31, 23, 59, 0).unwrap(),
            0,
            None,
        );
        tracker.record_usage("s", 1_000).await;
        assert!(tracker.check_budget("s", None, Some(1_000)).await.is_err());

        clock.set(Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 30).unwrap());
        assert!(
            tracker.check_budget("s", None, Some(1_000)).await.is_ok(),
            "monthly counter must roll over on UTC 1st-of-month"
        );
    }

    #[tokio::test]
    async fn year_boundary_rolls_both_counters() {
        let (tracker, clock) = tracker_at(
            Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 0).unwrap(),
            0,
            None,
        );
        tracker.record_usage("s", 1_000).await;
        clock.set(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 30).unwrap());
        let (d, m) = tracker.get_usage("s").await;
        assert_eq!((d, m), (0, 0));
    }

    #[tokio::test]
    async fn counters_are_per_sandbox() {
        let (tracker, _) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap(),
            0,
            None,
        );
        tracker.record_usage("a", 100).await;
        assert!(tracker.check_budget("a", Some(100), None).await.is_err());
        assert!(tracker.check_budget("b", Some(100), None).await.is_ok());
    }

    #[tokio::test]
    async fn per_request_limit_blocks_large_responses() {
        let tracker = TokenBudgetTracker::new(0, 50);
        assert!(tracker.check_per_request(49).is_ok());
        assert!(tracker.check_per_request(50).is_ok());
        assert!(tracker.check_per_request(51).is_err());
    }

    #[tokio::test]
    async fn persistence_roundtrip_survives_restart() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("budgets.json");
        let t0 = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();

        let (tracker, _) = tracker_at(t0, 0, Some(path.clone()));
        tracker.record_usage("s", 12_345).await;
        // Drop tracker, simulate restart.
        drop(tracker);

        let (restored, _) = tracker_at(t0, 0, Some(path.clone()));
        let (d, m) = restored.get_usage("s").await;
        assert_eq!(d, 12_345);
        assert_eq!(m, 12_345);
        // Limits derived from persisted state must still bite.
        assert!(
            restored
                .check_budget("s", Some(12_345), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn persistence_corrupt_file_starts_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("budgets.json");
        std::fs::write(&path, b"not json").unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let (tracker, _) = tracker_at(t0, 0, Some(path));
        let (d, m) = tracker.get_usage("s").await;
        assert_eq!((d, m), (0, 0));
        // And the tracker still works.
        tracker.record_usage("s", 7).await;
        assert_eq!(tracker.get_usage("s").await, (7, 7));
    }

    #[tokio::test]
    async fn persistence_missing_file_starts_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let t0 = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let (tracker, _) = tracker_at(t0, 0, Some(path));
        assert_eq!(tracker.get_usage("s").await, (0, 0));
    }

    #[tokio::test]
    async fn check_budget_legacy_uses_env_default() {
        let (tracker, _) = tracker_at(
            Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap(),
            100,
            None,
        );
        tracker.record_usage("s", 100).await;
        // Legacy callers pass no overrides → env default of 100
        // applies → should now be blocked.
        assert!(tracker.check_budget_legacy("s").await.is_err());
    }

    #[tokio::test]
    async fn record_usage_initialises_keys_to_current_period() {
        // Regression: first record_usage call must stamp the
        // CURRENT day_key / month_key on the new entry so the
        // subsequent check_budget doesn't accidentally see a zero
        // day_key (which would equal `day_key(epoch)` and roll
        // forward correctly today but mask a bug if epoch ever
        // changed).
        let t = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let (tracker, _) = tracker_at(t, 0, None);
        tracker.record_usage("s", 1).await;
        let (d, m) = tracker.get_usage("s").await;
        assert_eq!((d, m), (1, 1));
    }

    #[test]
    fn day_key_advances_by_one_per_day() {
        let a = day_key(Utc.with_ymd_and_hms(2026, 5, 13, 23, 59, 59).unwrap());
        let b = day_key(Utc.with_ymd_and_hms(2026, 5, 14, 0, 0, 0).unwrap());
        assert_eq!(b - a, 1);
    }

    #[test]
    fn month_key_advances_by_one_per_month() {
        let a = month_key(Utc.with_ymd_and_hms(2026, 5, 31, 23, 59, 59).unwrap());
        let b = month_key(Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap());
        assert_eq!(b - a, 1);
        let dec = month_key(Utc.with_ymd_and_hms(2026, 12, 31, 0, 0, 0).unwrap());
        let jan = month_key(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(jan - dec, 1);
    }
}
