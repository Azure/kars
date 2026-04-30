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
}
