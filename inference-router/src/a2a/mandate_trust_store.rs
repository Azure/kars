//! Mandate-issuer trust store — type-safe wrapper around
//! [`crate::a2a::trust_store::TrustStore`] for AP2 mandate signing
//! keys.
//!
//! ## Why a distinct type
//!
//! AP2 `IntentMandate` signatures and A2A agent-card signatures both
//! verify against `HashMap<&str, &VerifyingKey>`, but they are
//! **conceptually different trust paths**:
//!
//! - The **A2A trust path** authenticates *agents* — the
//!   `A2AAgent` CRD declares which kids represent each peer agent.
//! - The **mandate trust path** authenticates *commerce issuers* —
//!   a future `MandateIssuer` CRD will declare which kids may sign
//!   `IntentMandate`s on behalf of a billable principal.
//!
//! Reusing the same `TrustStore` instance across both paths would
//! mean an A2A agent compromised at the agent layer could
//! immediately sign mandates against the commerce ledger, and vice
//! versa. A distinct *type* (not just a distinct instance) makes
//! this swap a compile-time error.
//!
//! [`MandateTrustStore`] is therefore a newtype-style wrapper that
//! exposes only the operations the AP2 path needs:
//!
//! - [`MandateTrustStore::snapshot`] — `Arc`-shared point-in-time view.
//! - [`MandateTrustStore::replace_snapshot`] — hot-reload.
//! - [`MandateTrustStoreSnapshot::as_verifier_keys`] — projection
//!   for [`crate::a2a::mandate_signing::verify_mandate`].
//!
//! The `add` / `build` / `lookup` operations are intentionally
//! **not** re-exposed: callers should use [`crate::a2a::trust_store::TrustStoreBuilder`]
//! and [`MandateTrustStoreSnapshot::from_inner`] to assemble a
//! snapshot, then publish via [`MandateTrustStore::replace_snapshot`].
//! This keeps the read API minimal (verifier-side) while the
//! reconciler-side write API stays in the underlying module.
//!
//! ## Hot-reload contract
//!
//! Identical to [`crate::a2a::trust_store::TrustStore`]: a
//! `replace_snapshot` call is observed by the very next
//! `snapshot()` call. There is no caching layer between them.
//! Phase 2's `MandateIssuer` informer reconciler will rebuild the
//! snapshot on every CR event using
//! [`crate::a2a::snapshot_rebuild::rebuild_snapshot`] (works
//! unmodified — it returns an inner [`crate::a2a::trust_store::TrustStoreSnapshot`]
//! which `MandateTrustStoreSnapshot::from_inner` wraps).

use std::collections::HashMap;
use std::sync::Arc;

use ed25519_dalek::VerifyingKey;

use crate::a2a::trust_store::{TrustStore, TrustStoreSnapshot};

/// Immutable, type-safe wrapper around a [`TrustStoreSnapshot`] that
/// represents *mandate-issuer* trust, not A2A *agent* trust.
#[derive(Debug, Default)]
pub struct MandateTrustStoreSnapshot {
    inner: TrustStoreSnapshot,
}

impl MandateTrustStoreSnapshot {
    /// Wrap an underlying [`TrustStoreSnapshot`] as a mandate-trust
    /// snapshot. The reconciler typically obtains the inner snapshot
    /// from [`crate::a2a::snapshot_rebuild::rebuild_snapshot`].
    #[must_use]
    pub fn from_inner(inner: TrustStoreSnapshot) -> Self {
        Self { inner }
    }

    /// Generation counter forwarded from the inner snapshot. Used by
    /// reconciler observability to assert monotonicity.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }

    /// Number of trusted mandate-issuer kids in this snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when zero issuers are trusted — the production AP2
    /// path is fail-closed by default.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Project the snapshot to the `HashMap<&str, &VerifyingKey>`
    /// shape required by
    /// [`crate::a2a::mandate_signing::verify_mandate`]. Anchors
    /// whose `not_after` has elapsed (`now >= not_after`) are
    /// filtered out — strict comparison, identical semantics to
    /// [`TrustStoreSnapshot::as_verifier_keys`].
    #[must_use]
    pub fn as_verifier_keys(&self, now: i64) -> HashMap<&str, &VerifyingKey> {
        self.inner.as_verifier_keys(now)
    }

    /// Borrow the inner snapshot. Reserved for the rebuild
    /// orchestrator and tests; production code should consume the
    /// snapshot via [`Self::as_verifier_keys`].
    #[must_use]
    pub fn inner(&self) -> &TrustStoreSnapshot {
        &self.inner
    }
}

/// Hot-reloadable mandate-issuer trust store. Exposes only the read
/// + replace surface the AP2 path needs.
#[derive(Debug, Default)]
pub struct MandateTrustStore {
    inner: TrustStore,
}

impl MandateTrustStore {
    /// New, empty store. The AP2 verifier called against an empty
    /// store fails closed (`UnknownKid`), so this constructor
    /// produces a safe-by-default value.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Atomically swap the snapshot. The next [`Self::snapshot`]
    /// call observes the new value.
    pub fn replace_snapshot(&self, next: MandateTrustStoreSnapshot) {
        self.inner.replace_snapshot(next.inner);
    }

    /// Cheap `Arc`-clone of the current snapshot. Holds a stable
    /// view across multiple `as_verifier_keys` calls even if a
    /// concurrent `replace_snapshot` lands.
    #[must_use]
    pub fn snapshot(&self) -> Arc<MandateTrustStoreSnapshotView> {
        Arc::new(MandateTrustStoreSnapshotView {
            inner: self.inner.snapshot(),
        })
    }
}

/// Read-only view of a mandate-trust snapshot, returned by
/// [`MandateTrustStore::snapshot`].
///
/// Holding this `Arc` keeps the snapshot pinned across multiple
/// `as_verifier_keys` calls; even if the underlying store is hot-
/// reloaded mid-flight, the view this `Arc` points at remains
/// valid until dropped.
#[derive(Debug)]
pub struct MandateTrustStoreSnapshotView {
    inner: Arc<TrustStoreSnapshot>,
}

impl MandateTrustStoreSnapshotView {
    /// Generation of the underlying snapshot.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }

    /// Number of trusted issuers in the view.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when zero issuers are trusted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Project to the verifier-key map (see
    /// [`MandateTrustStoreSnapshot::as_verifier_keys`]).
    #[must_use]
    pub fn as_verifier_keys(&self, now: i64) -> HashMap<&str, &VerifyingKey> {
        self.inner.as_verifier_keys(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::trust_store::{AnchorSource, TrustAnchor, TrustStoreBuilder};

    fn vk_from_seed(seed: u8) -> VerifyingKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32]).verifying_key()
    }

    fn snapshot_with_kid(kid: &str, seed: u8, not_after: Option<i64>) -> TrustStoreSnapshot {
        let mut b = TrustStoreBuilder::new().generation(1);
        b.add(TrustAnchor {
            kid: kid.into(),
            key: vk_from_seed(seed),
            not_after,
            source: AnchorSource::from("mandate-issuer-cr/test"),
        })
        .expect("first add");
        b.build()
    }

    #[test]
    fn empty_store_yields_empty_snapshot() {
        let s = MandateTrustStore::new();
        let v = s.snapshot();
        assert_eq!(v.len(), 0);
        assert!(v.is_empty());
        assert!(v.as_verifier_keys(0).is_empty());
    }

    #[test]
    fn replace_snapshot_visible_on_next_snapshot_call() {
        let s = MandateTrustStore::new();
        s.replace_snapshot(MandateTrustStoreSnapshot::from_inner(snapshot_with_kid(
            "kid-a", 1, None,
        )));
        let v = s.snapshot();
        assert_eq!(v.len(), 1);
        assert!(v.as_verifier_keys(0).contains_key("kid-a"));
    }

    #[test]
    fn replace_with_empty_snapshot_revokes_all() {
        let s = MandateTrustStore::new();
        s.replace_snapshot(MandateTrustStoreSnapshot::from_inner(snapshot_with_kid(
            "kid-a", 1, None,
        )));
        assert_eq!(s.snapshot().len(), 1);
        s.replace_snapshot(MandateTrustStoreSnapshot::default());
        assert_eq!(s.snapshot().len(), 0);
    }

    #[test]
    fn arc_view_pins_pre_replace_snapshot() {
        let s = MandateTrustStore::new();
        s.replace_snapshot(MandateTrustStoreSnapshot::from_inner(snapshot_with_kid(
            "kid-pinned",
            9,
            None,
        )));
        let pinned = s.snapshot();

        // Hot-reload to a different snapshot.
        s.replace_snapshot(MandateTrustStoreSnapshot::from_inner(snapshot_with_kid(
            "kid-fresh",
            10,
            None,
        )));

        // Pinned view still sees pre-replace state.
        assert!(pinned.as_verifier_keys(0).contains_key("kid-pinned"));
        assert!(!pinned.as_verifier_keys(0).contains_key("kid-fresh"));

        // Fresh snapshot sees post-replace state.
        let fresh = s.snapshot();
        assert!(!fresh.as_verifier_keys(0).contains_key("kid-pinned"));
        assert!(fresh.as_verifier_keys(0).contains_key("kid-fresh"));
    }

    #[test]
    fn expired_anchors_filtered_strictly() {
        // not_after == 100; at now=100, expired (strict <).
        let snap = snapshot_with_kid("kid-exp", 3, Some(100));
        let s = MandateTrustStore::new();
        s.replace_snapshot(MandateTrustStoreSnapshot::from_inner(snap));
        let v = s.snapshot();
        assert!(v.as_verifier_keys(99).contains_key("kid-exp"), "live");
        assert!(!v.as_verifier_keys(100).contains_key("kid-exp"), "expired");
        assert!(
            !v.as_verifier_keys(101).contains_key("kid-exp"),
            "long expired"
        );
    }

    #[test]
    fn generation_round_trips_through_wrapper() {
        let inner = TrustStoreBuilder::new().generation(7).build();
        let wrapped = MandateTrustStoreSnapshot::from_inner(inner);
        assert_eq!(wrapped.generation(), 7);
    }

    #[test]
    fn as_verifier_keys_is_compatible_with_verify_mandate_signature() {
        // The whole point of this wrapper is that the projection
        // type matches what `verify_mandate` accepts. We don't run
        // verify_mandate here (that's mandate_signing.rs's corpus),
        // but we assert the *type* is exactly `HashMap<&str,
        // &VerifyingKey>` so a future signature change to either
        // side would fail compilation here.
        let s = MandateTrustStore::new();
        s.replace_snapshot(MandateTrustStoreSnapshot::from_inner(snapshot_with_kid(
            "kid", 5, None,
        )));
        let view = s.snapshot();
        let map: HashMap<&str, &VerifyingKey> = view.as_verifier_keys(0);
        assert!(map.contains_key("kid"));

        // Pass the map to a function that explicitly types
        // `crate::a2a::mandate_signing::TrustedKeys` — if either
        // alias drifts, this stops compiling.
        let _: crate::a2a::mandate_signing::TrustedKeys<'_> = map;
    }
}
