// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Per-CRD policy load status registry — single source of truth for what
//! the router believes it is enforcing right now.
//!
//! ## Why this exists
//!
//! `principles.md` §3 ("Honesty over scaffolding") of the
//! `crd-well-oiled-machine` plan requires that a CRD only transitions to
//! `phase=Ready` once the **router** has confirmed it loaded the
//! controller-published artifact. Until Slice 0, controllers happily
//! stamped `Ready` after writing a ConfigMap, regardless of whether the
//! sidecar router ever read it.
//!
//! Slice 0 introduced an intermediate `phase=Compiled` (artifact
//! materialized, router not yet echoing). This module is the second half
//! of that loop: it records, per `PolicyKind`, the digest the router has
//! actually loaded into memory. Slice 1b will add a controller-side
//! poller that walks `GET /internal/policy-status` and only promotes
//! `Compiled → Ready` once digests match.
//!
//! Today the AGT policy reload path
//! ([`crate::governance::Governance::load_policies_from_dir`]) is the
//! sole producer; future slices register their own
//! `PolicyKind::EgressAllowlist`, `PolicyKind::ToolPolicy`, etc. as they
//! land.
//!
//! ## Concurrency
//!
//! `Arc<PolicyStatusRegistry>` is cheaply clonable. Internally a single
//! `Mutex<HashMap<...>>` guards the per-kind entries — write traffic is
//! limited to hot-reload events (10 s mtime cadence in production), so a
//! `Mutex` is right-sized; an `RwLock` would add complexity without
//! measurable throughput benefit.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

/// Closed taxonomy of policies the router can echo back to controllers.
///
/// Adding a variant is a public-API change: the corresponding controller
/// reconciler **must** be wired in the same PR to consume the new kind
/// via `GET /internal/policy-status`. Don't add variants speculatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum PolicyKind {
    /// AGT (Agent Governance Toolkit) profile — YAML rule files mounted
    /// at `AGT_POLICY_DIR` (default `/etc/azureclaw/policies`). Produced
    /// by `ToolPolicy` reconciler (Slice 1) and by the built-in
    /// `agt-profiles/*.yaml` shipped in the sandbox image as a fallback.
    AgtProfile,
    /// `InferencePolicy` compiled profile — single JSON file mounted at
    /// `INFERENCE_POLICY_DIR` (default `/etc/azureclaw/inference`).
    /// Produced by the `InferencePolicy` reconciler (Slice 2a).
    /// Today only the `tokenBudget.perRequestTokens` axis is enforced
    /// by the router; later sub-slices add daily/monthly budgets,
    /// content-safety floors, and model failover.
    InferencePolicy,
}

impl PolicyKind {
    /// Stable wire identifier used in JSON responses. Kept in sync with
    /// the `Serialize` impl so the `kind` string is consistent across
    /// internal API + struct field debug output.
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyKind::AgtProfile => "AgtProfile",
            PolicyKind::InferencePolicy => "InferencePolicy",
        }
    }
}

/// One row of the registry — current load state for a single
/// `PolicyKind`. Serialized as-is by `GET /internal/policy-status`.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyStatusEntry {
    pub kind: PolicyKind,
    /// `sha256:<hex>` digest of the canonical bytes the router loaded.
    /// `None` when the last load attempt failed — the controller treats
    /// missing digest as "router hasn't confirmed anything yet" and
    /// keeps the CRD in `phase=Compiled`.
    pub digest: Option<String>,
    /// Filesystem path the bytes came from. Surfaced for operator
    /// debugging — when a digest mismatch occurs, the operator wants to
    /// know whether the router was looking at the right file at all.
    pub source_path: String,
    /// Wall-clock time the router materialized this state. RFC 3339 in
    /// the JSON envelope; `SystemTime` in-memory to avoid taking a
    /// `chrono` dep just for this.
    pub loaded_at: SystemTime,
    /// Most-recent error text. `Some(_)` and `digest = None` together
    /// mean the consumer is broken; `Some(_)` with `digest = Some(_)`
    /// means the last attempt failed but a previous successful load is
    /// still in effect.
    pub last_error: Option<String>,
}

/// Thread-safe registry of per-`PolicyKind` load state.
///
/// Lives at `AppState.policy_status` (single instance shared with the
/// `Governance` engine). The only mutators today are `record_success`
/// and `record_error`; future slices will add their own producers as
/// they land.
#[derive(Debug, Default)]
pub struct PolicyStatusRegistry {
    entries: Mutex<HashMap<PolicyKind, PolicyStatusEntry>>,
}

impl PolicyStatusRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `kind` was successfully loaded from `source_path`
    /// with the given canonical bytes. The digest is computed here (not
    /// at the call site) so all producers hash the same way — drift
    /// between producers would make digest comparison meaningless.
    pub fn record_success(&self, kind: PolicyKind, source_path: &str, bytes: &[u8]) {
        let digest = format!("sha256:{}", hex_digest(bytes));
        let entry = PolicyStatusEntry {
            kind,
            digest: Some(digest),
            source_path: source_path.to_string(),
            loaded_at: SystemTime::now(),
            last_error: None,
        };
        if let Ok(mut g) = self.entries.lock() {
            g.insert(kind, entry);
        }
    }

    /// Record a failed load attempt. Preserves any prior successful
    /// `digest` so the controller can tell "never loaded" from "loaded
    /// once, now broken". Both states should still keep the CRD out of
    /// `phase=Ready` — that's the controller's call, not ours.
    pub fn record_error(&self, kind: PolicyKind, source_path: &str, error: &str) {
        if let Ok(mut g) = self.entries.lock() {
            let prior_digest = g.get(&kind).and_then(|e| e.digest.clone());
            let entry = PolicyStatusEntry {
                kind,
                digest: prior_digest,
                source_path: source_path.to_string(),
                loaded_at: SystemTime::now(),
                last_error: Some(truncate_err(error)),
            };
            g.insert(kind, entry);
        }
    }

    /// Snapshot of all entries. Returns owned values — the registry
    /// lock is held only for the clone, not the duration of the caller's
    /// work.
    pub fn snapshot(&self) -> Vec<PolicyStatusEntry> {
        self.entries
            .lock()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Lookup a single entry by kind. `None` when the kind has never
    /// been recorded (i.e., the corresponding consumer hasn't run yet).
    pub fn get(&self, kind: PolicyKind) -> Option<PolicyStatusEntry> {
        self.entries.lock().ok().and_then(|g| g.get(&kind).cloned())
    }

    /// Number of distinct kinds tracked. Useful for tests and for the
    /// `/internal/policy-status` route's response metadata.
    pub fn len(&self) -> usize {
        self.entries.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Cap error strings to keep `/internal/policy-status` JSON payloads
/// bounded. AGT engine errors are usually short, but a bad YAML can
/// produce multi-kB serde errors with line numbers in the middle of
/// the file; the head is enough for triage.
fn truncate_err(err: &str) -> String {
    const MAX: usize = 512;
    if err.len() <= MAX {
        err.to_string()
    } else {
        // Truncate at a char boundary to avoid panicking on multi-byte
        // UTF-8. AGT YAML parser errors are ASCII in practice, but the
        // policy filename embedded in the error may not be.
        let mut end = MAX;
        while !err.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…[truncated]", &err[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_success_stores_digest() {
        let reg = PolicyStatusRegistry::new();
        reg.record_success(
            PolicyKind::AgtProfile,
            "/etc/azureclaw/policies/a.yaml",
            b"hello",
        );
        let e = reg.get(PolicyKind::AgtProfile).expect("entry present");
        assert_eq!(e.kind, PolicyKind::AgtProfile);
        assert_eq!(e.source_path, "/etc/azureclaw/policies/a.yaml");
        assert_eq!(
            e.digest.as_deref(),
            Some("sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"),
        );
        assert!(e.last_error.is_none());
    }

    #[test]
    fn record_success_overwrites_prior_entry_for_same_kind() {
        let reg = PolicyStatusRegistry::new();
        reg.record_success(PolicyKind::AgtProfile, "/a.yaml", b"v1");
        reg.record_success(PolicyKind::AgtProfile, "/b.yaml", b"v2");
        assert_eq!(reg.len(), 1, "same kind must collapse to one entry");
        let e = reg.get(PolicyKind::AgtProfile).unwrap();
        assert_eq!(e.source_path, "/b.yaml");
        // Different bytes → different digest.
        assert_ne!(
            e.digest.as_deref().unwrap(),
            "sha256:0e7f3eaf2e36f33b58a1f8de1f56e7e2e3b9fce5dabbf64ab36a0b9eeb0a4f50",
        );
    }

    #[test]
    fn record_error_preserves_prior_digest() {
        let reg = PolicyStatusRegistry::new();
        reg.record_success(PolicyKind::AgtProfile, "/a.yaml", b"good");
        let good_digest = reg.get(PolicyKind::AgtProfile).unwrap().digest.clone();
        assert!(good_digest.is_some());

        reg.record_error(PolicyKind::AgtProfile, "/a.yaml", "yaml parse error");
        let e = reg.get(PolicyKind::AgtProfile).unwrap();
        assert_eq!(
            e.digest, good_digest,
            "prior successful digest must survive a later error",
        );
        assert_eq!(e.last_error.as_deref(), Some("yaml parse error"));
    }

    #[test]
    fn record_error_with_no_prior_load_has_none_digest() {
        let reg = PolicyStatusRegistry::new();
        reg.record_error(PolicyKind::AgtProfile, "/a.yaml", "EACCES");
        let e = reg.get(PolicyKind::AgtProfile).unwrap();
        assert!(e.digest.is_none());
        assert_eq!(e.last_error.as_deref(), Some("EACCES"));
    }

    #[test]
    fn truncate_err_caps_long_messages() {
        let reg = PolicyStatusRegistry::new();
        let long = "x".repeat(2000);
        reg.record_error(PolicyKind::AgtProfile, "/a.yaml", &long);
        let e = reg.get(PolicyKind::AgtProfile).unwrap();
        let err = e.last_error.expect("error stored");
        assert!(
            err.len() < long.len(),
            "expected truncation, got {} bytes",
            err.len()
        );
        assert!(err.ends_with("…[truncated]"));
    }

    #[test]
    fn truncate_err_handles_multibyte_boundary() {
        // 4-byte UTF-8 char ("😀" = U+1F600) repeated to exceed cap.
        let s = "😀".repeat(200);
        let reg = PolicyStatusRegistry::new();
        reg.record_error(PolicyKind::AgtProfile, "/a.yaml", &s);
        let err = reg.get(PolicyKind::AgtProfile).unwrap().last_error.unwrap();
        // Must not panic + must be valid UTF-8 (implicit, since it's a
        // `String`). Also exercises the char-boundary backoff.
        assert!(err.ends_with("…[truncated]"));
    }

    #[test]
    fn snapshot_returns_all_entries_then_releases_lock() {
        let reg = PolicyStatusRegistry::new();
        reg.record_success(PolicyKind::AgtProfile, "/a.yaml", b"x");
        let snap = reg.snapshot();
        assert_eq!(snap.len(), 1);
        // Must be able to mutate while caller still holds snapshot.
        reg.record_success(PolicyKind::AgtProfile, "/b.yaml", b"y");
        assert_eq!(snap.len(), 1, "snapshot is a point-in-time copy");
    }

    #[test]
    fn len_and_is_empty_track_inserts() {
        let reg = PolicyStatusRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        reg.record_success(PolicyKind::AgtProfile, "/a.yaml", b"x");
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn policy_kind_as_str_matches_serialize() {
        // The route serializes `kind` via `Serialize`; downstream consumers
        // (controller poller, headlamp plugin) match on these strings.
        // Drift between `as_str()` and the serialized form would be a
        // silent contract break.
        let s = serde_json::to_string(&PolicyKind::AgtProfile).unwrap();
        assert_eq!(s, "\"AgtProfile\"");
        assert_eq!(PolicyKind::AgtProfile.as_str(), "AgtProfile");
    }
}
