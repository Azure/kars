// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! In-tree `AuditSink` implementation on `crate::governance::Governance`.
//!
//! The four-seam trait is defined in `providers/audit.rs`; this module
//! supplies a real implementation. Like the sibling
//! [`crate::providers::policy_impl`], no wrapper type is introduced —
//! `AuditSink` is implemented directly on `Governance` and the same
//! `Arc<Governance>` coerces into `Arc<dyn AuditSink>`.
//!
//! ## Contract points
//!
//! - **Idempotency.** The agentmesh SDK's `AuditLogger::log` is *not*
//!   idempotent — every call appends. The trait contract requires
//!   idempotent `append` on identical `(timestamp_ms, principal, action,
//!   payload_digest_hex)` tuples, so this impl maintains a bounded LRU
//!   of recent tuples → receipts. Dedup is in-process only; restart
//!   resets the cache (acceptable — retries happen in the same process).
//! - **`get`.** Scans the in-memory hash-chain for a matching entry hash
//!   and translates `AuditEntry` back to `AuditEvent`. Returns `Ok(None)`
//!   on not-found.
//! - **Back-pressure.** The in-tree logger never returns `QueueFull` or
//!   `Unreachable` — everything is local `Mutex<Vec>`. The error arms
//!   exist for forward-compatibility with the AGT-SDK and remote sinks.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::governance::Governance;
use crate::providers::audit::{AuditError, AuditEvent, AuditReceipt, AuditSink, ReceiptId};

/// Maximum number of distinct `(timestamp_ms, principal, action, digest)`
/// tuples remembered for dedup. At ~200 bytes/entry this caps the cache
/// around 50 KB.
const DEDUP_CAPACITY: usize = 256;

/// Dedup key. `timestamp_ms` plus three identity fields — matches the
/// trait's idempotency contract exactly.
type DedupKey = (u64, String, String, String);

/// In-memory dedup cache. LRU-ish: insertion order is tracked via a
/// parallel `Vec<DedupKey>`; oldest entry is evicted when `DEDUP_CAPACITY`
/// is reached. Access is serialised behind a `Mutex` — the critical
/// section is a HashMap lookup plus at most one eviction, so contention
/// is bounded even under high append volume.
#[derive(Default)]
pub(crate) struct AuditDedup {
    inner: Mutex<AuditDedupState>,
}

#[derive(Default)]
struct AuditDedupState {
    map: HashMap<DedupKey, AuditReceipt>,
    order: Vec<DedupKey>,
}

impl AuditDedup {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Return the cached receipt for `key`, if any.
    fn get(&self, key: &DedupKey) -> Option<AuditReceipt> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .get(key)
            .cloned()
    }

    /// Remember `(key → receipt)`. Evicts the oldest entry when the
    /// cache is full.
    fn insert(&self, key: DedupKey, receipt: AuditReceipt) {
        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if st.map.contains_key(&key) {
            return;
        }
        if st.order.len() >= DEDUP_CAPACITY {
            let oldest = st.order.remove(0);
            st.map.remove(&oldest);
        }
        st.order.push(key.clone());
        st.map.insert(key, receipt);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .len()
    }
}

/// Current epoch in milliseconds. Falls back to 0 on clock-skew panic
/// (impossible in practice; `SystemTime::now()` can only be "before
/// epoch" on deliberately misconfigured machines).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Turn an `AuditEvent` into the `(agent_id, action, decision)` triple
/// consumed by `agentmesh::AuditLogger::log`. Labels are folded into
/// `decision` as `verdict|k=v|k=v` — preserves them in the hash chain
/// without changing the upstream crate.
fn event_to_legacy_args(event: &AuditEvent) -> (String, String, String) {
    let mut decision = event.verdict.clone();
    for (k, v) in &event.labels {
        decision.push('|');
        decision.push_str(k);
        decision.push('=');
        decision.push_str(v);
    }
    if !event.payload_digest_hex.is_empty() {
        decision.push_str("|digest=");
        decision.push_str(&event.payload_digest_hex);
    }
    (event.principal.clone(), event.action.clone(), decision)
}

/// Inverse of [`event_to_legacy_args`] for `get()`. Labels are NOT
/// reconstructed — they're squashed into `decision`, and the trait
/// contract doesn't require perfect round-trips on `get` (it's for
/// auditing, not replay).
fn legacy_entry_to_event(entry: &agentmesh::AuditEntry) -> AuditEvent {
    let timestamp_ms = parse_iso8601_ms(&entry.timestamp).unwrap_or(0);
    AuditEvent {
        timestamp_ms,
        principal: entry.agent_id.clone(),
        action: entry.action.clone(),
        payload_digest_hex: String::new(),
        verdict: entry.decision.clone(),
        labels: Vec::new(),
    }
}

/// Best-effort ISO-8601 → millis parser. The SDK writes
/// `chrono::Utc::now().to_rfc3339()` which `chrono` can parse back. We
/// avoid pulling `chrono` into this crate just for this — a compact
/// hand parser handles the only two shapes the SDK emits
/// (`YYYY-MM-DDTHH:MM:SS(.fff)?(Z|+HH:MM)`). Failure yields `None`,
/// which the caller maps to `0`.
fn parse_iso8601_ms(s: &str) -> Option<u64> {
    // Strip timezone — SDK always writes UTC suffixes.
    let s = s.trim_end_matches('Z');
    let s = s.split('+').next().unwrap_or(s);
    let (date, time) = s.split_once('T')?;

    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: u64 = d.next()?.parse().ok()?;
    let da: u64 = d.next()?.parse().ok()?;

    let (hms, frac) = match time.split_once('.') {
        Some((hms, frac)) => (hms, frac),
        None => (time, "0"),
    };
    let mut t = hms.split(':');
    let h: u64 = t.next()?.parse().ok()?;
    let mi: u64 = t.next()?.parse().ok()?;
    let se: u64 = t.next()?.parse().ok()?;
    let ms_frac: u64 = {
        let take = frac.get(..3).unwrap_or(frac);
        take.parse().ok()?
    };

    // Days from civil date (Howard Hinnant).
    let y = y - i64::from(mo <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * m + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    let secs = days
        .checked_mul(86_400)?
        .checked_add((h * 3600 + mi * 60 + se) as i64)?;
    let ms = (secs as i128) * 1000 + ms_frac as i128;
    u64::try_from(ms.max(0)).ok()
}

#[async_trait]
impl AuditSink for Governance {
    async fn append(&self, event: AuditEvent) -> Result<AuditReceipt, AuditError> {
        let key: DedupKey = (
            event.timestamp_ms,
            event.principal.clone(),
            event.action.clone(),
            event.payload_digest_hex.clone(),
        );
        if let Some(cached) = self.audit_dedup.get(&key) {
            return Ok(cached);
        }

        let (agent_id, action, decision) = event_to_legacy_args(&event);
        let entry = self.audit_log(&agent_id, &action, &decision);

        let prev_hash_hex = if entry.previous_hash.is_empty() {
            None
        } else {
            Some(entry.previous_hash.clone())
        };
        let receipt = AuditReceipt {
            id: ReceiptId(entry.hash.clone()),
            prev_hash_hex,
            entry_hash_hex: entry.hash.clone(),
        };
        self.audit_dedup.insert(key, receipt.clone());
        crate::metrics::AGT_AUDIT_ENTRIES.set(self.audit.entries().len() as i64);
        Ok(receipt)
    }

    async fn get(&self, id: &ReceiptId) -> Result<Option<AuditEvent>, AuditError> {
        let entries = self.audit.entries();
        Ok(entries
            .iter()
            .find(|e| e.hash == id.0)
            .map(legacy_entry_to_event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sample_event(ts: u64, action: &str) -> AuditEvent {
        AuditEvent {
            timestamp_ms: ts,
            principal: "agent-1".into(),
            action: action.into(),
            payload_digest_hex: "deadbeef".into(),
            verdict: "allow".into(),
            labels: vec![("tier".into(), "1".into())],
        }
    }

    #[tokio::test]
    async fn append_returns_receipt_with_chain_hash() {
        let gov = Governance::new("test-sandbox");
        let receipt = gov.append(sample_event(100, "tool.invoke")).await.unwrap();
        assert!(!receipt.entry_hash_hex.is_empty());
        assert_eq!(receipt.id.0, receipt.entry_hash_hex);
        // First entry: no previous hash.
        assert!(receipt.prev_hash_hex.is_none());
    }

    #[tokio::test]
    async fn second_append_links_to_first() {
        let gov = Governance::new("test-sandbox");
        let first = gov.append(sample_event(100, "a")).await.unwrap();
        let second = gov.append(sample_event(101, "b")).await.unwrap();
        assert_eq!(
            second.prev_hash_hex.as_deref(),
            Some(first.entry_hash_hex.as_str())
        );
    }

    #[tokio::test]
    async fn identical_event_is_deduplicated() {
        let gov = Governance::new("test-sandbox");
        let r1 = gov.append(sample_event(100, "a")).await.unwrap();
        let r2 = gov.append(sample_event(100, "a")).await.unwrap();
        assert_eq!(r1.entry_hash_hex, r2.entry_hash_hex);
        // The underlying chain must have exactly one entry.
        assert_eq!(gov.audit.entries().len(), 1);
    }

    #[tokio::test]
    async fn different_timestamp_bypasses_dedup() {
        let gov = Governance::new("test-sandbox");
        gov.append(sample_event(100, "a")).await.unwrap();
        gov.append(sample_event(101, "a")).await.unwrap();
        assert_eq!(gov.audit.entries().len(), 2);
    }

    #[tokio::test]
    async fn different_digest_bypasses_dedup() {
        let gov = Governance::new("test-sandbox");
        let mut e1 = sample_event(100, "a");
        e1.payload_digest_hex = "aa".into();
        let mut e2 = sample_event(100, "a");
        e2.payload_digest_hex = "bb".into();
        gov.append(e1).await.unwrap();
        gov.append(e2).await.unwrap();
        assert_eq!(gov.audit.entries().len(), 2);
    }

    #[tokio::test]
    async fn labels_are_folded_into_decision() {
        let gov = Governance::new("test-sandbox");
        let event = sample_event(100, "a");
        gov.append(event).await.unwrap();
        let e = gov.audit.entries().into_iter().next().unwrap();
        assert!(e.decision.contains("tier=1"), "got: {}", e.decision);
        assert!(
            e.decision.contains("digest=deadbeef"),
            "got: {}",
            e.decision
        );
        assert!(e.decision.starts_with("allow"));
    }

    #[tokio::test]
    async fn get_returns_event_when_hash_matches() {
        let gov = Governance::new("test-sandbox");
        let receipt = gov.append(sample_event(100, "a")).await.unwrap();
        let fetched = gov.get(&receipt.id).await.unwrap();
        let fetched = fetched.expect("event should exist");
        assert_eq!(fetched.principal, "agent-1");
        assert_eq!(fetched.action, "a");
        // Verdict carries the folded labels+digest.
        assert!(fetched.verdict.starts_with("allow"));
    }

    #[tokio::test]
    async fn get_returns_none_for_unknown_id() {
        let gov = Governance::new("test-sandbox");
        gov.append(sample_event(100, "a")).await.unwrap();
        let missing = gov.get(&ReceiptId("nope".into())).await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn trait_object_coercion_works() {
        let sink: Arc<dyn AuditSink> =
            Arc::new(Governance::new("test-sandbox")) as Arc<dyn AuditSink>;
        let receipt = sink.append(sample_event(100, "a")).await.unwrap();
        let fetched = sink.get(&receipt.id).await.unwrap();
        assert!(fetched.is_some());
    }

    #[tokio::test]
    async fn dedup_cache_evicts_oldest_over_capacity() {
        let gov = Governance::new("test-sandbox");
        // Fill the cache beyond capacity with distinct keys.
        for i in 0..(DEDUP_CAPACITY as u64 + 5) {
            gov.append(sample_event(i, "a")).await.unwrap();
        }
        // Cache size is clamped to DEDUP_CAPACITY.
        assert_eq!(gov.audit_dedup.len(), DEDUP_CAPACITY);
        // The oldest keys must be gone: appending with `ts=0` again
        // writes a new entry (not a dedup hit).
        let entries_before = gov.audit.entries().len();
        gov.append(sample_event(0, "a")).await.unwrap();
        assert_eq!(gov.audit.entries().len(), entries_before + 1);
    }

    #[test]
    fn parse_iso8601_roundtrips_epoch_second() {
        // 2024-01-01T00:00:00Z = 1704067200_000 ms.
        assert_eq!(
            parse_iso8601_ms("2024-01-01T00:00:00Z"),
            Some(1_704_067_200_000)
        );
    }

    #[test]
    fn parse_iso8601_handles_fractional_seconds() {
        assert_eq!(
            parse_iso8601_ms("2024-01-01T00:00:00.123Z"),
            Some(1_704_067_200_123)
        );
    }

    #[test]
    fn parse_iso8601_handles_offset() {
        // +00:00 is equivalent to Z; we strip it regardless.
        assert_eq!(
            parse_iso8601_ms("2024-01-01T00:00:00+00:00"),
            Some(1_704_067_200_000)
        );
    }

    #[test]
    fn parse_iso8601_rejects_garbage() {
        assert!(parse_iso8601_ms("not-a-date").is_none());
        assert!(parse_iso8601_ms("2024-01-01").is_none());
    }

    #[test]
    fn event_to_legacy_args_flattens_cleanly() {
        let (p, a, d) = event_to_legacy_args(&sample_event(0, "x"));
        assert_eq!(p, "agent-1");
        assert_eq!(a, "x");
        assert_eq!(d, "allow|tier=1|digest=deadbeef");
    }

    #[test]
    fn event_to_legacy_args_no_labels_no_digest() {
        let mut ev = sample_event(0, "x");
        ev.labels.clear();
        ev.payload_digest_hex.clear();
        let (_, _, d) = event_to_legacy_args(&ev);
        assert_eq!(d, "allow");
    }
}
