//! A2A trust-store cache — snapshot-style hot-reload for `kid → VerifyingKey`
//! anchors used by [`crate::a2a::card_verifier::verify_inbound_card`].
//!
//! ## Why a separate module?
//!
//! [`super::card_verifier::CardVerifierConfig`] holds a borrowed
//! `HashMap<&str, &VerifyingKey>` of trust anchors per call. That shape is
//! correct for a *pure-function* verifier but the production router has
//! to:
//!
//! 1. Source anchors from `A2AAgent` Custom Resources (one CR per
//!    federated agent, each carrying one or more keys with `kid`s).
//! 2. Hot-reload that set as CRs are added / updated / deleted by a K8s
//!    informer, *without* dropping the verification hot path into a
//!    coarse-grained mutex.
//! 3. Honour per-anchor expiry so a rotated key naturally falls out of
//!    the trust set even before the informer notifies us.
//!
//! This module is the seam that satisfies (1)–(3) for the router. It
//! does **not** itself talk to K8s; the controller-side or
//! informer-driven adapter calls [`TrustStore::replace_snapshot`] with
//! a freshly assembled [`TrustStoreSnapshot`].
//!
//! ## Concurrency model
//!
//! [`TrustStore`] holds `RwLock<Arc<TrustStoreSnapshot>>`. Readers
//! acquire a read lock for the duration of a single `Arc::clone`
//! (microseconds) and then drop the lock; subsequent verification work
//! runs against the cloned `Arc<Snapshot>` with no further
//! synchronisation. Writers (informer loop) build a fresh snapshot off
//! the lock and call [`TrustStore::replace_snapshot`], which acquires
//! the write lock for the duration of a single pointer swap.
//!
//! This gives lock-free *value* access with a brief lock around the
//! pointer swap — equivalent to `arc-swap` semantics without pulling in
//! the dependency.
//!
//! ## Expiry
//!
//! A [`TrustAnchor`] may carry a Unix-seconds `not_after` timestamp.
//! [`TrustStoreSnapshot::lookup`] performs a constant-time expiry check
//! and returns `None` for anchors whose `now >= not_after`. A separate
//! background task (not in this module) is expected to call
//! [`TrustStore::compact`] periodically to physically remove expired
//! entries; until that runs, expired entries are tombstoned but never
//! returned to callers.
//!
//! ## Anchor source provenance
//!
//! Each anchor carries an opaque [`AnchorSource`] string ("a2a-agent-cr/
//! ns/name", "static-bootstrap", etc.) so audit consumers can attribute
//! a verification decision to the K8s object that published the key.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ed25519_dalek::VerifyingKey;

/// Opaque provenance string identifying where a [`TrustAnchor`] came from.
///
/// Convention: `<source-type>/<scope>/<name>`. Examples:
/// - `a2a-agent-cr/team-alpha/billing-agent`
/// - `static-bootstrap/operator-key`
/// - `agt-trust-graph/global`
///
/// Stored verbatim; never parsed by this module.
pub type AnchorSource = String;

/// Single trust anchor: a verifying key with provenance and optional
/// expiry. The `kid` is the lookup key surface verifiers see.
#[derive(Clone, Debug)]
pub struct TrustAnchor {
    /// Key id (matches the `kid` field of incoming AgentCard signatures).
    pub kid: String,
    /// The Ed25519 verifying key.
    pub key: VerifyingKey,
    /// Optional Unix-seconds expiry. `None` ⇒ never expires (until the
    /// next snapshot replace removes it).
    pub not_after: Option<i64>,
    /// Provenance string (see [`AnchorSource`]).
    pub source: AnchorSource,
}

/// Immutable trust-anchor map. Built once via [`TrustStoreBuilder`],
/// installed via [`TrustStore::replace_snapshot`].
#[derive(Default, Clone, Debug)]
pub struct TrustStoreSnapshot {
    anchors: HashMap<String, TrustAnchor>,
    /// Monotonically-increasing generation counter for observability.
    generation: u64,
}

impl TrustStoreSnapshot {
    /// Generation counter — increments on each builder finalisation.
    /// Useful for /healthz / metrics.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of anchors (including unexpired ones).
    #[must_use]
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Whether the snapshot is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// Look up a trust anchor by `kid`, applying expiry filtering.
    ///
    /// Returns `None` when:
    /// - the `kid` is not in the snapshot, **or**
    /// - the anchor has a `not_after` and `now >= not_after`.
    #[must_use]
    pub fn lookup(&self, kid: &str, now: i64) -> Option<&TrustAnchor> {
        let anchor = self.anchors.get(kid)?;
        match anchor.not_after {
            Some(exp) if now >= exp => None,
            _ => Some(anchor),
        }
    }

    /// Project the snapshot to the borrow shape
    /// [`crate::a2a::card_verifier::CardVerifierConfig`] expects.
    ///
    /// `now` filters out expired anchors. The returned `HashMap` borrows
    /// from `self`, so the caller must keep `self` alive for the
    /// duration of the verification call.
    #[must_use]
    pub fn as_verifier_keys(&self, now: i64) -> HashMap<&str, &VerifyingKey> {
        self.anchors
            .iter()
            .filter(|(_, a)| match a.not_after {
                Some(exp) => now < exp,
                None => true,
            })
            .map(|(k, a)| (k.as_str(), &a.key))
            .collect()
    }

    /// All anchor `kid`s currently in the snapshot, expired or not.
    #[must_use]
    pub fn kids(&self) -> Vec<String> {
        self.anchors.keys().cloned().collect()
    }
}

/// Builder pattern for [`TrustStoreSnapshot`]. Rejects conflicting
/// duplicate `kid`s at build time so two informer adapters can't
/// silently shadow each other.
#[derive(Default, Debug)]
pub struct TrustStoreBuilder {
    anchors: HashMap<String, TrustAnchor>,
    generation: u64,
}

/// Build-time errors for [`TrustStoreBuilder`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum TrustStoreBuildError {
    #[error(
        "duplicate kid '{kid}': existing source '{existing_source}', new source '{new_source}'"
    )]
    DuplicateKid {
        kid: String,
        existing_source: AnchorSource,
        new_source: AnchorSource,
    },
}

impl TrustStoreBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the generation counter for the resulting snapshot.
    #[must_use]
    pub fn generation(mut self, g: u64) -> Self {
        self.generation = g;
        self
    }

    /// Add an anchor. Idempotent on identical `(kid, key, not_after,
    /// source)`; conflicting duplicate `kid` returns
    /// [`TrustStoreBuildError::DuplicateKid`].
    ///
    /// # Errors
    ///
    /// Returns `DuplicateKid` when the same `kid` is added with
    /// conflicting metadata.
    pub fn add(&mut self, anchor: TrustAnchor) -> Result<&mut Self, TrustStoreBuildError> {
        if let Some(existing) = self.anchors.get(&anchor.kid) {
            if existing.key.as_bytes() == anchor.key.as_bytes()
                && existing.not_after == anchor.not_after
                && existing.source == anchor.source
            {
                return Ok(self);
            }
            return Err(TrustStoreBuildError::DuplicateKid {
                kid: anchor.kid.clone(),
                existing_source: existing.source.clone(),
                new_source: anchor.source.clone(),
            });
        }
        self.anchors.insert(anchor.kid.clone(), anchor);
        Ok(self)
    }

    /// Finalise into an immutable snapshot.
    #[must_use]
    pub fn build(self) -> TrustStoreSnapshot {
        TrustStoreSnapshot {
            anchors: self.anchors,
            generation: self.generation,
        }
    }
}

/// Hot-reloadable trust store with snapshot-replace semantics.
#[derive(Debug)]
pub struct TrustStore {
    inner: RwLock<Arc<TrustStoreSnapshot>>,
}

impl Default for TrustStore {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Arc::new(TrustStoreSnapshot::default())),
        }
    }
}

impl TrustStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a cheap clone of the current snapshot Arc. Read lock is
    /// held only for the duration of the clone.
    #[must_use]
    pub fn snapshot(&self) -> Arc<TrustStoreSnapshot> {
        let g = self.inner.read().expect("trust-store read lock poisoned");
        Arc::clone(&g)
    }

    /// Atomically install a new snapshot. Caller is responsible for
    /// monotonic generation numbering.
    pub fn replace_snapshot(&self, next: TrustStoreSnapshot) {
        let mut g = self.inner.write().expect("trust-store write lock poisoned");
        *g = Arc::new(next);
    }

    /// Compact: rebuild the snapshot dropping anchors that have already
    /// expired (`now >= not_after`). Bumps generation by 1.
    pub fn compact(&self, now: i64) {
        let prev = self.snapshot();
        let next_gen = prev.generation.saturating_add(1);
        let kept: HashMap<String, TrustAnchor> = prev
            .anchors
            .iter()
            .filter(|(_, a)| match a.not_after {
                Some(exp) => now < exp,
                None => true,
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let next = TrustStoreSnapshot {
            anchors: kept,
            generation: next_gen,
        };
        self.replace_snapshot(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::sync::atomic::{AtomicU8, Ordering};

    static SEED_COUNTER: AtomicU8 = AtomicU8::new(1);

    fn fresh_key() -> VerifyingKey {
        let n = SEED_COUNTER.fetch_add(1, Ordering::SeqCst);
        let seed = [n; 32];
        SigningKey::from_bytes(&seed).verifying_key()
    }

    fn anchor(kid: &str, not_after: Option<i64>, source: &str) -> TrustAnchor {
        TrustAnchor {
            kid: kid.to_string(),
            key: fresh_key(),
            not_after,
            source: source.to_string(),
        }
    }

    #[test]
    fn empty_snapshot_returns_none() {
        let s = TrustStoreSnapshot::default();
        assert!(s.is_empty());
        assert!(s.lookup("any", 100).is_none());
    }

    #[test]
    fn builder_roundtrip_one_anchor() {
        let mut b = TrustStoreBuilder::new().generation(1);
        b.add(anchor("k1", None, "static")).unwrap();
        let s = b.build();
        assert_eq!(s.generation(), 1);
        assert_eq!(s.len(), 1);
        assert!(s.lookup("k1", 100).is_some());
    }

    #[test]
    fn lookup_returns_none_for_expired() {
        let mut b = TrustStoreBuilder::new();
        b.add(anchor("k1", Some(50), "src")).unwrap();
        let s = b.build();
        assert!(s.lookup("k1", 49).is_some(), "before expiry");
        assert!(s.lookup("k1", 50).is_none(), "at expiry boundary");
        assert!(s.lookup("k1", 51).is_none(), "after expiry");
    }

    #[test]
    fn never_expiring_anchor_returned_at_any_time() {
        let mut b = TrustStoreBuilder::new();
        b.add(anchor("forever", None, "src")).unwrap();
        let s = b.build();
        assert!(s.lookup("forever", 0).is_some());
        assert!(s.lookup("forever", i64::MAX).is_some());
    }

    #[test]
    fn duplicate_kid_with_same_metadata_is_idempotent() {
        let mut b = TrustStoreBuilder::new();
        let key = fresh_key();
        let a1 = TrustAnchor {
            kid: "k1".into(),
            key,
            not_after: Some(100),
            source: "static".into(),
        };
        let a2 = a1.clone();
        b.add(a1).unwrap();
        b.add(a2).unwrap();
        let s = b.build();
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn duplicate_kid_with_different_key_rejected() {
        let mut b = TrustStoreBuilder::new();
        b.add(anchor("k1", None, "src-a")).unwrap();
        let err = b.add(anchor("k1", None, "src-b")).unwrap_err();
        assert!(matches!(err, TrustStoreBuildError::DuplicateKid { .. }));
    }

    #[test]
    fn duplicate_kid_with_different_expiry_rejected() {
        let mut b = TrustStoreBuilder::new();
        let key = fresh_key();
        b.add(TrustAnchor {
            kid: "k1".into(),
            key,
            not_after: Some(100),
            source: "src".into(),
        })
        .unwrap();
        let err = b
            .add(TrustAnchor {
                kid: "k1".into(),
                key,
                not_after: Some(200),
                source: "src".into(),
            })
            .unwrap_err();
        assert!(matches!(err, TrustStoreBuildError::DuplicateKid { .. }));
    }

    #[test]
    fn as_verifier_keys_filters_expired() {
        let mut b = TrustStoreBuilder::new();
        b.add(anchor("live", Some(200), "src")).unwrap();
        b.add(anchor("dead", Some(50), "src")).unwrap();
        b.add(anchor("forever", None, "src")).unwrap();
        let s = b.build();
        let keys = s.as_verifier_keys(100);
        assert!(keys.contains_key("live"));
        assert!(!keys.contains_key("dead"));
        assert!(keys.contains_key("forever"));
    }

    #[test]
    fn store_default_is_empty_generation_zero() {
        let s = TrustStore::new();
        let snap = s.snapshot();
        assert_eq!(snap.generation(), 0);
        assert!(snap.is_empty());
    }

    #[test]
    fn replace_snapshot_publishes_new_generation() {
        let s = TrustStore::new();
        let mut b = TrustStoreBuilder::new().generation(1);
        b.add(anchor("k", None, "src")).unwrap();
        s.replace_snapshot(b.build());
        let snap = s.snapshot();
        assert_eq!(snap.generation(), 1);
        assert_eq!(snap.len(), 1);
    }

    #[test]
    fn snapshot_clone_decouples_from_replace() {
        let s = TrustStore::new();
        let mut b1 = TrustStoreBuilder::new().generation(1);
        b1.add(anchor("k", None, "src")).unwrap();
        s.replace_snapshot(b1.build());
        let pinned = s.snapshot();
        let b2 = TrustStoreBuilder::new().generation(2);
        s.replace_snapshot(b2.build());
        assert_eq!(pinned.generation(), 1, "pinned snapshot does not change");
        assert_eq!(pinned.len(), 1);
        assert_eq!(s.snapshot().generation(), 2);
        assert_eq!(s.snapshot().len(), 0);
    }

    #[test]
    fn compact_drops_expired_and_bumps_generation() {
        let s = TrustStore::new();
        let mut b = TrustStoreBuilder::new().generation(5);
        b.add(anchor("live", Some(200), "src")).unwrap();
        b.add(anchor("dead", Some(50), "src")).unwrap();
        b.add(anchor("forever", None, "src")).unwrap();
        s.replace_snapshot(b.build());
        s.compact(100);
        let snap = s.snapshot();
        assert_eq!(snap.generation(), 6);
        assert_eq!(snap.len(), 2);
        assert!(snap.lookup("live", 100).is_some());
        assert!(snap.lookup("dead", 100).is_none());
        assert!(snap.lookup("forever", 100).is_some());
    }

    #[test]
    fn snapshot_is_thread_safe_via_arc_clone() {
        use std::thread;
        let s = Arc::new(TrustStore::new());
        let mut b = TrustStoreBuilder::new().generation(1);
        b.add(anchor("k", None, "src")).unwrap();
        s.replace_snapshot(b.build());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let s = Arc::clone(&s);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let snap = s.snapshot();
                    assert!(snap.lookup("k", 100).is_some());
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn kids_returns_all_kids_including_expired() {
        let mut b = TrustStoreBuilder::new();
        b.add(anchor("a", Some(10), "src")).unwrap();
        b.add(anchor("b", None, "src")).unwrap();
        let s = b.build();
        let mut kids = s.kids();
        kids.sort();
        assert_eq!(kids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn integration_with_card_verifier_borrow_shape() {
        let mut b = TrustStoreBuilder::new().generation(1);
        b.add(anchor("k1", None, "src")).unwrap();
        b.add(anchor("k2", Some(50), "src")).unwrap();
        let snap = b.build();
        let keys_at_25 = snap.as_verifier_keys(25);
        assert_eq!(keys_at_25.len(), 2);
        let keys_at_75 = snap.as_verifier_keys(75);
        assert_eq!(keys_at_75.len(), 1);
        assert!(keys_at_75.contains_key("k1"));
    }
}
