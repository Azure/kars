// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, rate-limited, deduplicated ring buffer of egress attempts that
//! were *blocked* in enforce mode (S12.f). Sibling of the existing
//! learn-mode buffer for *allowed* egress observations on `Blocklist`.
//!
//! Surfaced via `GET /egress/learned/blocked`. Hostname + port only — no
//! paths, headers, query strings, or payload data are ever stored.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default capacity when the operator does not override.
pub const DEFAULT_CAPACITY: usize = 1024;
/// Default sliding-window length for per-source rate limiting.
pub const DEFAULT_RATE_WINDOW: Duration = Duration::from_secs(60);
/// Default max ring-buffer churn events per source per window.
pub const DEFAULT_RATE_PER_SOURCE: u32 = 100;

/// One blocked-attempt record. Hostname + port only — every other attribute
/// of the original request is dropped at record time.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct BlockedEntry {
    pub host: String,
    pub port: u16,
    pub source_sandbox: String,
    pub first_seen_unix: u64,
    pub last_seen_unix: u64,
    pub count: u32,
}

/// Outcome of a `record` call. The aggregate `count` on an existing entry
/// always reflects every observation, even when the ring-buffer write is
/// suppressed by the rate limiter.
#[derive(Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    /// First observation for this `(source, host, port)` key — written to
    /// the buffer.
    Recorded,
    /// Matched an existing key — `count` bumped, `last_seen_unix` updated.
    Deduplicated,
    /// Per-source sliding-window rate limit exceeded — observation dropped.
    /// No buffer mutation, no count bump.
    RateLimited,
    /// Host failed normalization (empty, IP literal, etc.) — observation
    /// rejected. Caller may safely ignore.
    Rejected,
}

/// Bounded ring buffer of blocked-attempt records.
pub struct BlockedBuffer {
    inner: Mutex<Inner>,
    capacity: usize,
    rate_limit_window: Duration,
    rate_limit_per_source: u32,
}

struct Inner {
    by_key: HashMap<(String, String, u16), BlockedEntry>,
    order: VecDeque<(String, String, u16)>,
    rate_per_source: HashMap<String, RateState>,
}

struct RateState {
    window_start: Instant,
    count: u32,
}

impl BlockedBuffer {
    pub fn new(capacity: usize, rate_limit_window: Duration, rate_limit_per_source: u32) -> Self {
        Self {
            inner: Mutex::new(Inner {
                by_key: HashMap::new(),
                order: VecDeque::new(),
                rate_per_source: HashMap::new(),
            }),
            capacity: capacity.max(1),
            rate_limit_window,
            rate_limit_per_source,
        }
    }

    /// Convenience constructor with the recommended defaults
    /// (1024 entries, 100 events / 60s per source).
    pub fn with_defaults() -> Self {
        Self::new(
            DEFAULT_CAPACITY,
            DEFAULT_RATE_WINDOW,
            DEFAULT_RATE_PER_SOURCE,
        )
    }

    /// Record a blocked egress attempt. See `RecordOutcome` for the
    /// possible results. Hostname-only — never store paths, headers, or
    /// payload data.
    pub fn record(&self, source_sandbox: &str, host: &str, port: u16) -> RecordOutcome {
        self.record_at(Instant::now(), unix_now(), source_sandbox, host, port)
    }

    /// Testable record entry point — accepts an injected clock pair.
    pub fn record_at(
        &self,
        now: Instant,
        now_unix: u64,
        source_sandbox: &str,
        host: &str,
        port: u16,
    ) -> RecordOutcome {
        let host = match normalize_host(host) {
            Some(h) => h,
            None => return RecordOutcome::Rejected,
        };
        let source = if source_sandbox.is_empty() {
            "unknown".to_string()
        } else {
            source_sandbox.to_string()
        };

        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(), // poisoned — keep going, state is still readable
        };

        let key = (source.clone(), host.clone(), port);

        // Dedup path: existing entries always bump count + last_seen, even
        // when the rate limiter would have suppressed a fresh write.
        if let Some(entry) = inner.by_key.get_mut(&key) {
            entry.last_seen_unix = now_unix;
            entry.count = entry.count.saturating_add(1);
            return RecordOutcome::Deduplicated;
        }

        // Per-source sliding-window rate limit applies only to the
        // ring-buffer churn (new keys), not to dedup updates above.
        let rate = inner
            .rate_per_source
            .entry(source.clone())
            .or_insert(RateState {
                window_start: now,
                count: 0,
            });
        if now.duration_since(rate.window_start) >= self.rate_limit_window {
            rate.window_start = now;
            rate.count = 0;
        }
        if rate.count >= self.rate_limit_per_source {
            return RecordOutcome::RateLimited;
        }
        rate.count += 1;

        let entry = BlockedEntry {
            host: host.clone(),
            port,
            source_sandbox: source.clone(),
            first_seen_unix: now_unix,
            last_seen_unix: now_unix,
            count: 1,
        };
        inner.by_key.insert(key.clone(), entry);
        inner.order.push_back(key);

        // FIFO eviction at capacity.
        while inner.order.len() > self.capacity {
            if let Some(old_key) = inner.order.pop_front() {
                inner.by_key.remove(&old_key);
            }
        }

        RecordOutcome::Recorded
    }

    /// Snapshot of up to `limit` entries, newest first by insertion order.
    pub fn snapshot(&self, limit: usize) -> Vec<BlockedEntry> {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        inner
            .order
            .iter()
            .rev()
            .take(limit)
            .filter_map(|k| inner.by_key.get(k).cloned())
            .collect()
    }

    /// Snapshot of every entry whose `last_seen_unix >= since_unix`, newest
    /// first by last-seen. Used by `/internal/egress/blocked?since=…` to
    /// support the CLI's `--since` filter.
    ///
    /// The buffer doesn't index by time, so this performs a full scan over
    /// the deduplicated entry set. Bounded by `capacity` (1024 by default).
    pub fn snapshot_since(&self, since_unix: u64) -> Vec<BlockedEntry> {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut out: Vec<BlockedEntry> = inner
            .by_key
            .values()
            .filter(|e| e.last_seen_unix >= since_unix)
            .cloned()
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.last_seen_unix));
        out
    }

    /// Top-N most-attempted hosts whose `last_seen_unix >= since_unix`,
    /// aggregated across sandboxes + ports by hostname. Returns
    /// `(host, total_attempts)` pairs, descending by attempt count.
    ///
    /// Used by the periodic `EgressBlockedSeen` event (Slice 5b) and the
    /// `/internal/egress/blocked/top` endpoint surfaced to the plugin.
    pub fn top_hosts(&self, since_unix: u64, n: usize) -> Vec<(String, u32)> {
        if n == 0 {
            return Vec::new();
        }
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut by_host: HashMap<String, u32> = HashMap::new();
        for e in inner.by_key.values() {
            if e.last_seen_unix < since_unix {
                continue;
            }
            let slot = by_host.entry(e.host.clone()).or_insert(0);
            *slot = slot.saturating_add(e.count);
        }
        let mut out: Vec<(String, u32)> = by_host.into_iter().collect();
        // Stable secondary sort by host name so equal counts produce
        // deterministic output for tests + UI.
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(n);
        out
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.by_key.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.by_key.clear();
            g.order.clear();
            g.rate_per_source.clear();
        }
    }
}

impl Default for BlockedBuffer {
    fn default() -> Self {
        Self::with_defaults()
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Lowercase ASCII, strip a single trailing dot, reject empty / IP literal.
fn normalize_host(host: &str) -> Option<String> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut h = trimmed.to_ascii_lowercase();
    while h.ends_with('.') {
        h.pop();
    }
    if h.is_empty() {
        return None;
    }
    // Reject IPv4/IPv6 literals — we want hostnames only. IPv6 literals may
    // arrive bracketed (e.g. "[::1]") so strip brackets before parsing.
    let parse_target = h
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(&h);
    if parse_target.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> BlockedBuffer {
        BlockedBuffer::new(8, Duration::from_secs(60), 100)
    }

    #[test]
    fn record_first_observation_returns_recorded() {
        let b = buf();
        let out = b.record("sb1", "example.com", 443);
        assert_eq!(out, RecordOutcome::Recorded);
        assert_eq!(b.len(), 1);
        let snap = b.snapshot(10);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].host, "example.com");
        assert_eq!(snap[0].port, 443);
        assert_eq!(snap[0].source_sandbox, "sb1");
        assert_eq!(snap[0].count, 1);
    }

    #[test]
    fn record_duplicate_increments_count_and_dedupes() {
        let b = buf();
        assert_eq!(b.record("sb1", "example.com", 443), RecordOutcome::Recorded);
        assert_eq!(
            b.record("sb1", "example.com", 443),
            RecordOutcome::Deduplicated
        );
        assert_eq!(
            b.record("sb1", "example.com", 443),
            RecordOutcome::Deduplicated
        );
        assert_eq!(b.len(), 1);
        let snap = b.snapshot(10);
        assert_eq!(snap[0].count, 3);
        assert!(snap[0].last_seen_unix >= snap[0].first_seen_unix);
    }

    #[test]
    fn record_distinct_hosts_creates_distinct_entries() {
        let b = buf();
        b.record("sb1", "a.example.com", 443);
        b.record("sb1", "b.example.com", 443);
        b.record("sb1", "a.example.com", 80); // distinct port also distinct
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn record_distinct_sandboxes_creates_distinct_entries() {
        let b = buf();
        b.record("sb1", "example.com", 443);
        b.record("sb2", "example.com", 443);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn rate_limit_drops_high_frequency_writes_from_one_source() {
        let b = BlockedBuffer::new(1024, Duration::from_secs(60), 5);
        let now = Instant::now();
        // 5 distinct hosts succeed; 6th and 7th hit the per-source limit.
        for i in 0..5 {
            let host = format!("h{i}.example.com");
            assert_eq!(
                b.record_at(now, 1000 + i as u64, "sb1", &host, 443),
                RecordOutcome::Recorded
            );
        }
        assert_eq!(
            b.record_at(now, 1100, "sb1", "overflow1.example.com", 443),
            RecordOutcome::RateLimited
        );
        assert_eq!(
            b.record_at(now, 1101, "sb1", "overflow2.example.com", 443),
            RecordOutcome::RateLimited
        );
        assert_eq!(b.len(), 5);
    }

    #[test]
    fn rate_limit_does_not_affect_other_sources() {
        let b = BlockedBuffer::new(1024, Duration::from_secs(60), 2);
        let now = Instant::now();
        b.record_at(now, 1000, "sb1", "a.example.com", 443);
        b.record_at(now, 1001, "sb1", "b.example.com", 443);
        assert_eq!(
            b.record_at(now, 1002, "sb1", "c.example.com", 443),
            RecordOutcome::RateLimited
        );
        // Different source has its own counter.
        assert_eq!(
            b.record_at(now, 1003, "sb2", "c.example.com", 443),
            RecordOutcome::Recorded
        );
    }

    #[test]
    fn rate_limit_window_resets_after_elapsed() {
        let b = BlockedBuffer::new(1024, Duration::from_secs(60), 1);
        let t0 = Instant::now();
        assert_eq!(
            b.record_at(t0, 1000, "sb1", "a.example.com", 443),
            RecordOutcome::Recorded
        );
        assert_eq!(
            b.record_at(t0, 1001, "sb1", "b.example.com", 443),
            RecordOutcome::RateLimited
        );
        let t1 = t0 + Duration::from_secs(61);
        assert_eq!(
            b.record_at(t1, 1100, "sb1", "c.example.com", 443),
            RecordOutcome::Recorded
        );
    }

    #[test]
    fn capacity_evicts_oldest_fifo() {
        let b = BlockedBuffer::new(3, Duration::from_secs(60), 1000);
        b.record("sb1", "a.example.com", 443);
        b.record("sb1", "b.example.com", 443);
        b.record("sb1", "c.example.com", 443);
        b.record("sb1", "d.example.com", 443); // evicts a
        assert_eq!(b.len(), 3);
        let hosts: Vec<String> = b.snapshot(10).into_iter().map(|e| e.host).collect();
        assert!(hosts.contains(&"d.example.com".into()));
        assert!(hosts.contains(&"c.example.com".into()));
        assert!(hosts.contains(&"b.example.com".into()));
        assert!(!hosts.contains(&"a.example.com".into()));
    }

    #[test]
    fn snapshot_returns_in_recency_order() {
        // Documented contract: newest first (reverse insertion order).
        let b = buf();
        b.record("sb1", "a.example.com", 443);
        b.record("sb1", "b.example.com", 443);
        b.record("sb1", "c.example.com", 443);
        let hosts: Vec<String> = b.snapshot(10).into_iter().map(|e| e.host).collect();
        assert_eq!(
            hosts,
            vec![
                "c.example.com".to_string(),
                "b.example.com".to_string(),
                "a.example.com".to_string(),
            ]
        );
    }

    #[test]
    fn hostname_normalization_lowercases() {
        let b = buf();
        b.record("sb1", "EXAMPLE.com", 443);
        b.record("sb1", "example.COM", 443);
        assert_eq!(b.len(), 1);
        assert_eq!(b.snapshot(10)[0].host, "example.com");
    }

    #[test]
    fn hostname_normalization_strips_trailing_dot() {
        let b = buf();
        b.record("sb1", "example.com.", 443);
        b.record("sb1", "example.com", 443);
        assert_eq!(b.len(), 1);
        assert_eq!(b.snapshot(10)[0].host, "example.com");
    }

    #[test]
    fn record_rejects_ip_literal_host() {
        let b = buf();
        assert_eq!(b.record("sb1", "1.2.3.4", 443), RecordOutcome::Rejected);
        assert_eq!(b.record("sb1", "[::1]", 443), RecordOutcome::Rejected);
        assert_eq!(b.record("sb1", "::1", 443), RecordOutcome::Rejected);
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn record_rejects_empty_host() {
        let b = buf();
        assert_eq!(b.record("sb1", "", 443), RecordOutcome::Rejected);
        assert_eq!(b.record("sb1", "   ", 443), RecordOutcome::Rejected);
        assert_eq!(b.record("sb1", ".", 443), RecordOutcome::Rejected);
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn empty_source_sandbox_falls_back_to_unknown() {
        let b = buf();
        assert_eq!(b.record("", "example.com", 443), RecordOutcome::Recorded);
        assert_eq!(b.snapshot(10)[0].source_sandbox, "unknown");
    }

    #[test]
    fn clear_resets_buffer() {
        let b = buf();
        b.record("sb1", "example.com", 443);
        b.record("sb1", "other.com", 443);
        assert_eq!(b.len(), 2);
        b.clear();
        assert_eq!(b.len(), 0);
        assert!(b.snapshot(10).is_empty());
    }

    // ---- Slice 5a: snapshot_since + top_hosts ----------------------------

    /// Use `record_at` to inject known timestamps so the since-filter is
    /// deterministic. The instant clock value (`now`) is unused for the
    /// filter assertions — the test only inspects `last_seen_unix`.
    fn rec_at(b: &BlockedBuffer, ts_unix: u64, sandbox: &str, host: &str, port: u16) {
        b.record_at(Instant::now(), ts_unix, sandbox, host, port);
    }

    #[test]
    fn snapshot_since_filters_by_last_seen_and_sorts_desc() {
        let b = buf();
        rec_at(&b, 100, "sb1", "old.example.com", 443);
        rec_at(&b, 200, "sb1", "mid.example.com", 443);
        rec_at(&b, 300, "sb1", "new.example.com", 443);

        // since=150 → drops old.example.com
        let snap = b.snapshot_since(150);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].host, "new.example.com"); // newest first
        assert_eq!(snap[1].host, "mid.example.com");

        // since=0 → all three
        assert_eq!(b.snapshot_since(0).len(), 3);

        // since=>max ts → none
        assert!(b.snapshot_since(9999).is_empty());
    }

    #[test]
    fn snapshot_since_returns_dedup_count_for_repeats() {
        let b = buf();
        rec_at(&b, 100, "sb1", "h.example.com", 443);
        rec_at(&b, 200, "sb1", "h.example.com", 443); // dedup; bumps last_seen
        rec_at(&b, 300, "sb1", "h.example.com", 443); // dedup again

        let snap = b.snapshot_since(0);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].count, 3);
        assert_eq!(snap[0].last_seen_unix, 300);
    }

    #[test]
    fn top_hosts_aggregates_across_sandboxes_and_ports() {
        let b = buf();
        // a.example.com from two sandboxes; total count = 1 + 1 = 2.
        rec_at(&b, 100, "sb1", "a.example.com", 443);
        rec_at(&b, 110, "sb2", "a.example.com", 443);
        // b.example.com on two ports; total count = 1 + 1 = 2.
        rec_at(&b, 120, "sb1", "b.example.com", 443);
        rec_at(&b, 130, "sb1", "b.example.com", 80);
        // c.example.com seen 3 times from one sandbox.
        rec_at(&b, 140, "sb1", "c.example.com", 443);
        rec_at(&b, 141, "sb1", "c.example.com", 443);
        rec_at(&b, 142, "sb1", "c.example.com", 443);

        let top = b.top_hosts(0, 3);
        // c (count 3) > a (2) = b (2); a sorts before b alphabetically.
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], ("c.example.com".to_string(), 3));
        assert_eq!(top[1], ("a.example.com".to_string(), 2));
        assert_eq!(top[2], ("b.example.com".to_string(), 2));
    }

    #[test]
    fn top_hosts_respects_since_filter() {
        let b = buf();
        rec_at(&b, 100, "sb1", "old.example.com", 443);
        rec_at(&b, 200, "sb1", "new.example.com", 443);

        let top = b.top_hosts(150, 10);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, "new.example.com");
    }

    #[test]
    fn top_hosts_truncates_to_n() {
        let b = BlockedBuffer::new(64, Duration::from_secs(60), 100);
        for i in 0..5 {
            rec_at(&b, 100 + i as u64, "sb1", &format!("h{i}.example.com"), 443);
        }
        assert_eq!(b.top_hosts(0, 3).len(), 3);
        assert_eq!(b.top_hosts(0, 0).len(), 0);
        assert_eq!(b.top_hosts(0, 100).len(), 5);
    }

    #[test]
    fn top_hosts_empty_buffer_returns_empty() {
        let b = buf();
        assert!(b.top_hosts(0, 10).is_empty());
    }
}
