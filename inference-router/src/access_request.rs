// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, deduplicated buffer of **capability access requests** raised by the
//! sandboxed agent when a task cannot proceed without something the sandbox
//! deliberately withholds — a tool, skill, MCP server, shell command, broader
//! egress, or a higher autonomy tier.
//!
//! This is the in-flight companion to two existing surfaces:
//!  - **Pre-flight** (`§20` Bridge validate) — catches *declared-but-missing*
//!    capabilities before launch.
//!  - **Blocked egress** ([`crate::egress_blocked::BlockedBuffer`]) — records
//!    hosts the forward-proxy denied.
//!
//! The agent POSTs to `/v1/access-request` (loopback only — it is a *request*,
//! never a grant; a human remains the gate). The controller polls
//! `GET /internal/access-requests`, mints a **Pending `KarsApproval`** per novel
//! request, and — only on human approval — performs the privileged widening.
//! Nothing here grants anything; the buffer is purely an outbound request queue.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Max distinct requests retained. A misbehaving agent cannot flood the inbox:
/// duplicates coalesce onto one entry and the queue is hard-capped.
const DEFAULT_CAPACITY: usize = 64;

/// One capability request. Deduplicated on `(kind, target)`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AccessRequestEntry {
    /// One of `egress`, `tool`, `skill`, `mcp`, `command`, `permission`, `tier`.
    /// Free-form so the primitive is not a closed taxonomy; the controller maps
    /// unknown kinds onto a generic `capabilityGrant` approval.
    pub kind: String,
    /// The concrete thing needed: a host, a tool name, a skill name, a command,
    /// an MCP server id, or (for `tier`) the empty string.
    pub target: String,
    /// Agent-supplied justification. Surfaced verbatim in the approval summary.
    pub reason: String,
    /// For `kind = "tier"`, the autonomy tier being requested (1..=5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<i32>,
    /// For `kind = "egress"`, the port (defaults to 443 when omitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub count: u32,
    pub first_seen_unix: u64,
    pub last_seen_unix: u64,
    /// The human's decision once made, pushed back by the controller:
    /// `approved` | `denied`. `None` while still pending. This lets the agent
    /// poll `GET /v1/access-requests`, learn its request was granted, and
    /// continue — instead of blindly retrying or giving up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at_unix: Option<u64>,
}

/// In-process, thread-safe, bounded, deduplicated request queue.
#[derive(Debug)]
pub struct AccessRequestBuffer {
    inner: Mutex<VecDeque<AccessRequestEntry>>,
    capacity: usize,
}

impl Default for AccessRequestBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl AccessRequestBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity.min(DEFAULT_CAPACITY))),
            capacity: capacity.max(1),
        }
    }

    /// Record a request. Duplicates (same `kind` + `target`) coalesce onto the
    /// existing entry, bumping `count` + `last_seen`. Returns `true` when a NEW
    /// entry was created (the caller may log the first observation).
    pub fn record(
        &self,
        kind: &str,
        target: &str,
        reason: &str,
        tier: Option<i32>,
        port: Option<u16>,
    ) -> bool {
        let kind = kind.trim();
        let target = target.trim();
        if kind.is_empty() {
            return false;
        }
        let now = now_unix();
        let Ok(mut q) = self.inner.lock() else {
            return false;
        };
        if let Some(e) = q.iter_mut().find(|e| e.kind == kind && e.target == target) {
            e.count = e.count.saturating_add(1);
            e.last_seen_unix = now;
            // Keep the freshest reason/tier/port — the agent may refine them.
            if !reason.trim().is_empty() {
                e.reason = reason.trim().to_string();
            }
            if tier.is_some() {
                e.tier = tier;
            }
            if port.is_some() {
                e.port = port;
            }
            return false;
        }
        if q.len() >= self.capacity {
            q.pop_front();
        }
        q.push_back(AccessRequestEntry {
            kind: kind.to_string(),
            target: target.to_string(),
            reason: reason.trim().to_string(),
            tier,
            port,
            count: 1,
            first_seen_unix: now,
            last_seen_unix: now,
            decision: None,
            decided_at_unix: None,
        });
        true
    }

    /// Record a human decision (`approved` | `denied`) pushed back by the
    /// controller, keyed on `(kind, target)`. Returns `true` when a matching
    /// entry was updated. For an `egress` decision the target may be a host that
    /// was only ever auto-recorded in the blocked buffer (never POSTed here); in
    /// that case we synthesise an entry so the agent's poll still reflects it.
    pub fn set_decision(&self, kind: &str, target: &str, verdict: &str) -> bool {
        let kind = kind.trim();
        let target = target.trim();
        let verdict = verdict.trim();
        if kind.is_empty() || verdict.is_empty() {
            return false;
        }
        let now = now_unix();
        let Ok(mut q) = self.inner.lock() else {
            return false;
        };
        if let Some(e) = q.iter_mut().find(|e| e.kind == kind && e.target == target) {
            e.decision = Some(verdict.to_string());
            e.decided_at_unix = Some(now);
            return true;
        }
        // No matching request (e.g. an auto-surfaced egress block) — synthesise
        // one so the agent can still observe the decision on its next poll.
        if q.len() >= self.capacity {
            q.pop_front();
        }
        q.push_back(AccessRequestEntry {
            kind: kind.to_string(),
            target: target.to_string(),
            reason: String::new(),
            tier: None,
            port: None,
            count: 0,
            first_seen_unix: now,
            last_seen_unix: now,
            decision: Some(verdict.to_string()),
            decided_at_unix: Some(now),
        });
        true
    }

    /// Snapshot the current queue (newest last), capped at `limit`.
    #[must_use]
    pub fn snapshot(&self, limit: usize) -> Vec<AccessRequestEntry> {
        let Ok(q) = self.inner.lock() else {
            return Vec::new();
        };
        let take = if limit == 0 { q.len() } else { limit };
        q.iter().rev().take(take).rev().cloned().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|q| q.len()).unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_dedups() {
        let b = AccessRequestBuffer::new(8);
        assert!(b.record("egress", "api.example.com", "fetch docs", None, Some(443)));
        // Duplicate coalesces — not a new entry.
        assert!(!b.record(
            "egress",
            "api.example.com",
            "still need it",
            None,
            Some(443)
        ));
        assert_eq!(b.len(), 1);
        let snap = b.snapshot(0);
        assert_eq!(snap[0].count, 2);
        assert_eq!(snap[0].reason, "still need it");
    }

    #[test]
    fn distinct_kinds_and_targets_are_separate() {
        let b = AccessRequestBuffer::new(8);
        b.record("egress", "a.com", "x", None, None);
        b.record("tool", "a.com", "x", None, None);
        b.record("egress", "b.com", "x", None, None);
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn empty_kind_rejected() {
        let b = AccessRequestBuffer::new(8);
        assert!(!b.record("", "a.com", "x", None, None));
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let b = AccessRequestBuffer::new(2);
        b.record("tool", "one", "x", None, None);
        b.record("tool", "two", "x", None, None);
        b.record("tool", "three", "x", None, None);
        assert_eq!(b.len(), 2);
        let snap = b.snapshot(0);
        assert_eq!(snap[0].target, "two");
        assert_eq!(snap[1].target, "three");
    }

    #[test]
    fn tier_request_carries_tier() {
        let b = AccessRequestBuffer::new(8);
        b.record("tier", "", "need to act autonomously", Some(3), None);
        let snap = b.snapshot(0);
        assert_eq!(snap[0].tier, Some(3));
    }

    #[test]
    fn decision_updates_matching_entry() {
        let b = AccessRequestBuffer::new(8);
        b.record("egress", "pypi.org", "install dep", None, Some(443));
        assert!(b.set_decision("egress", "pypi.org", "approved"));
        let snap = b.snapshot(0);
        assert_eq!(snap[0].decision.as_deref(), Some("approved"));
        assert!(snap[0].decided_at_unix.is_some());
    }

    #[test]
    fn decision_synthesises_entry_for_unseen_egress() {
        let b = AccessRequestBuffer::new(8);
        // Never POSTed here (auto-surfaced from the blocked buffer instead).
        assert!(b.set_decision("egress", "npmjs.org", "approved"));
        let snap = b.snapshot(0);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].target, "npmjs.org");
        assert_eq!(snap[0].decision.as_deref(), Some("approved"));
    }
}
