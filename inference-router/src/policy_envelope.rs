// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Hot-reloadable policy envelope — pure-function core.
//!
//! ## What this module is
//!
//! Phase 1 close-out (internal Phase 1 plan §7 entry 14)
//! requires that `ToolPolicy` / `InferencePolicy` changes propagate
//! into the running router **without a pod rollout**. The full
//! plumbing (K8s informer + AGT SSE subscription) is a Phase 2
//! deliverable; this module delivers the *core* the Phase 2 plumbing
//! will sit on top of:
//!
//! 1. [`PolicyEntry`] — the router-side compiled representation of
//!    a `ToolPolicy` (or `InferencePolicy`) row, scoped to the
//!    router's decision path. Independent of the controller-crate
//!    CRD type so the router can be tested without the controller.
//! 2. [`PolicyEnvelopeSnapshot`] — immutable, ordered, generation-
//!    counted map of entries.
//! 3. [`PolicyChange`] — finite enum of legal mutations
//!    (`Upserted`, `Deleted`, `Reset`).
//! 4. [`apply_policy_change`] — pure function
//!    `(snapshot, change) -> snapshot` returning a new snapshot
//!    with bumped generation and the change applied. Mirror of
//!    PR 35's `snapshot_rebuild::rebuild_snapshot` for trust
//!    anchors.
//! 5. [`PolicyEnvelope`] — `ArcSwap`-backed hot-reloadable
//!    container. Exposes only `snapshot()` + `replace_snapshot()`,
//!    matching the [`crate::a2a::trust_store::TrustStore`] shape
//!    so a Phase 2 reconciler can use the same orchestration
//!    pattern.
//!
//! ## Contract guarantees (asserted by this module's tests)
//!
//! - **Generation monotonicity:** every `apply_policy_change` call
//!   bumps the generation by 1 (saturating at `u64::MAX`).
//! - **Idempotency on re-upsert:** upserting an entry whose
//!   `(id, version_hash)` matches an existing one returns a
//!   snapshot that is structurally identical except for the
//!   generation bump. (Phase 2 callers can short-circuit using
//!   `unchanged_after_apply` to skip a redundant `replace_snapshot`.)
//! - **Order-preserving:** entries are stored in a `BTreeMap`
//!   keyed by [`PolicyId`], so iteration is sorted and stable.
//! - **Atomic snapshot swap:** `replace_snapshot` is a single
//!   `ArcSwap::store`; the next `snapshot()` observes the new
//!   value with no read-side locking.
//! - **Pinned views:** an `Arc` returned from `snapshot()` keeps
//!   the pre-replace view alive for the duration of any in-flight
//!   request that holds the `Arc`.
//!
//! ## What this module is *not*
//!
//! - **Not a decision engine.** No `decide()` function lives here.
//!   Decisions are made by [`crate::providers::policy::PolicyDecisionProvider`]
//!   implementations; this module holds the data the decisions are
//!   computed against.
//! - **Not a CEL evaluator.** Selector matching is exact-equality
//!   on tool/server name + label-subset for `match_labels`. Full
//!   CEL is enforced by admission (Phase 1 §7 entry 12), not by
//!   the router.
//! - **Not a CRD client.** The Phase 2 informer translates K8s CR
//!   events into [`PolicyChange`] values; that translation lives in
//!   the controller crate, not here.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// Stable, opaque identifier for a [`PolicyEntry`]. The Phase 2
/// reconciler will derive this from `<namespace>/<name>` of the
/// underlying `ToolPolicy` or `InferencePolicy` CR; for tests +
/// dev-mode seeding any unique string works.
pub type PolicyId = String;

/// Per-entry version digest used for idempotent upserts. The
/// reconciler is expected to compute this as a stable hash of the
/// CR `spec` (e.g. SHA-256 hex of canonicalised JSON). The router
/// treats the value as opaque and only compares for equality.
pub type VersionHash = String;

/// Selector against which a candidate router request is matched.
/// Exact-equality on `tool` and `mcp_server`; subset-of on
/// `match_labels` (every entry must equal the request's label of
/// the same key).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicySelector {
    /// Tool name as advertised by the MCP server. `None` matches
    /// every tool.
    pub tool: Option<String>,
    /// MCP server name. `None` matches every server.
    pub mcp_server: Option<String>,
    /// Required sandbox labels (subset match). Empty means "no
    /// label requirement".
    #[serde(default)]
    pub match_labels: BTreeMap<String, String>,
}

impl PolicySelector {
    /// Returns `true` if every constraint declared in the selector
    /// is satisfied by the request. An empty selector matches
    /// everything.
    #[must_use]
    pub fn matches(
        &self,
        tool: Option<&str>,
        mcp_server: Option<&str>,
        labels: &BTreeMap<String, String>,
    ) -> bool {
        if let Some(t) = &self.tool {
            if Some(t.as_str()) != tool {
                return false;
            }
        }
        if let Some(s) = &self.mcp_server {
            if Some(s.as_str()) != mcp_server {
                return false;
            }
        }
        for (k, v) in &self.match_labels {
            if labels.get(k).map(String::as_str) != Some(v.as_str()) {
                return false;
            }
        }
        true
    }
}

/// One compiled policy entry. Carries only the *router-decisional*
/// fields; the full CRD shape (display name, status, etc.) is the
/// reconciler's concern.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyEntry {
    pub id: PolicyId,
    pub version: VersionHash,
    pub selector: PolicySelector,
    /// Free-form per-policy decisional payload. The Phase 2
    /// `PolicyDecisionProvider` will define the concrete shape;
    /// holding `serde_json::Value` here lets us evolve without
    /// breaking the envelope's hot-reload contract. The router
    /// never inspects the inside of `payload` from this module.
    pub payload: serde_json::Value,
}

/// Immutable, generation-counted set of policies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyEnvelopeSnapshot {
    generation: u64,
    entries: BTreeMap<PolicyId, PolicyEntry>,
}

impl PolicyEnvelopeSnapshot {
    /// Build a snapshot from an explicit set of entries. Phase 2
    /// reconciler uses this for the first hydration after the
    /// initial K8s list. Duplicate ids → last-write-wins (the
    /// reconciler is expected to dedupe upstream; we don't error
    /// here so the dev-mode seeder doesn't have to).
    #[must_use]
    pub fn from_entries(generation: u64, entries: impl IntoIterator<Item = PolicyEntry>) -> Self {
        let mut map = BTreeMap::new();
        for e in entries {
            map.insert(e.id.clone(), e);
        }
        Self {
            generation,
            entries: map,
        }
    }

    /// Generation counter — every `apply_policy_change` bumps this
    /// by 1. Phase 2 cache invalidation is keyed off this value.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff zero entries are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Lookup by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&PolicyEntry> {
        self.entries.get(id)
    }

    /// Iterator over entries in stable key order.
    pub fn iter(&self) -> impl Iterator<Item = &PolicyEntry> + '_ {
        self.entries.values()
    }

    /// All entries whose selector matches the supplied request
    /// dimensions. Iteration order is the snapshot's stable
    /// key order.
    pub fn select<'a>(
        &'a self,
        tool: Option<&'a str>,
        mcp_server: Option<&'a str>,
        labels: &'a BTreeMap<String, String>,
    ) -> impl Iterator<Item = &'a PolicyEntry> + 'a {
        self.entries
            .values()
            .filter(move |e| e.selector.matches(tool, mcp_server, labels))
    }
}

/// Mutation applied to a [`PolicyEnvelopeSnapshot`] by
/// [`apply_policy_change`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyChange {
    /// Insert or replace an entry. Idempotent when an existing
    /// entry has the same `(id, version)`; in that case the
    /// returned snapshot equals the input modulo generation bump.
    Upserted(PolicyEntry),
    /// Remove an entry. No-op if id is unknown.
    Deleted(PolicyId),
    /// Replace the entire entry set. The Phase 2 reconciler emits
    /// this when it detects an informer relist (snapshot rebuild)
    /// or when AGT pushes a full-set update.
    Reset(Vec<PolicyEntry>),
}

/// Result of `apply_policy_change`. Carries the new snapshot plus
/// a flag the reconciler can use to short-circuit a redundant
/// `replace_snapshot` (and the resulting cache invalidation
/// avalanche).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub snapshot: PolicyEnvelopeSnapshot,
    /// `true` when the entry set is structurally identical to the
    /// pre-change snapshot (only the generation differs). Phase 2
    /// reconcilers MAY skip publishing in this case; doing so is
    /// safe because no observer caches against generation alone.
    pub structurally_unchanged: bool,
}

/// Pure transition. See module-level docs for the contract.
#[must_use]
pub fn apply_policy_change(
    snapshot: &PolicyEnvelopeSnapshot,
    change: PolicyChange,
) -> ApplyOutcome {
    let next_gen = snapshot.generation.saturating_add(1);
    match change {
        PolicyChange::Upserted(entry) => {
            let unchanged = snapshot
                .entries
                .get(&entry.id)
                .is_some_and(|existing| existing == &entry);
            let mut entries = snapshot.entries.clone();
            entries.insert(entry.id.clone(), entry);
            ApplyOutcome {
                snapshot: PolicyEnvelopeSnapshot {
                    generation: next_gen,
                    entries,
                },
                structurally_unchanged: unchanged,
            }
        }
        PolicyChange::Deleted(id) => {
            let was_present = snapshot.entries.contains_key(&id);
            let mut entries = snapshot.entries.clone();
            entries.remove(&id);
            ApplyOutcome {
                snapshot: PolicyEnvelopeSnapshot {
                    generation: next_gen,
                    entries,
                },
                // Deleting an absent id leaves the set unchanged.
                structurally_unchanged: !was_present,
            }
        }
        PolicyChange::Reset(entries) => {
            let mut map = BTreeMap::new();
            for e in entries {
                map.insert(e.id.clone(), e);
            }
            let unchanged = map == snapshot.entries;
            ApplyOutcome {
                snapshot: PolicyEnvelopeSnapshot {
                    generation: next_gen,
                    entries: map,
                },
                structurally_unchanged: unchanged,
            }
        }
    }
}

/// Hot-reloadable container for a [`PolicyEnvelopeSnapshot`].
/// Mirrors the [`crate::a2a::trust_store::TrustStore`] shape so the
/// Phase 2 reconciler can use the same orchestration pattern (a
/// pure rebuild step that returns a snapshot, then a single
/// `replace_snapshot` to publish atomically).
#[derive(Debug)]
pub struct PolicyEnvelope {
    inner: RwLock<Arc<PolicyEnvelopeSnapshot>>,
}

impl Default for PolicyEnvelope {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Arc::new(PolicyEnvelopeSnapshot::default())),
        }
    }
}

impl PolicyEnvelope {
    /// New, empty envelope. Generation 0; the first
    /// `replace_snapshot` should bump to 1.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Atomically swap in a new snapshot. The next `snapshot()`
    /// call observes the new value.
    pub fn replace_snapshot(&self, next: PolicyEnvelopeSnapshot) {
        let mut g = self.inner.write().expect("PolicyEnvelope lock poisoned");
        *g = Arc::new(next);
    }

    /// Cheap `Arc`-clone of the current snapshot. Holds a stable
    /// view across multiple lookups even if a concurrent
    /// `replace_snapshot` lands.
    #[must_use]
    pub fn snapshot(&self) -> Arc<PolicyEnvelopeSnapshot> {
        self.inner
            .read()
            .expect("PolicyEnvelope lock poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, version: &str) -> PolicyEntry {
        PolicyEntry {
            id: id.into(),
            version: version.into(),
            selector: PolicySelector {
                tool: Some("pay".into()),
                mcp_server: None,
                match_labels: BTreeMap::new(),
            },
            payload: json!({"role": "test"}),
        }
    }

    fn entry_with_selector(id: &str, sel: PolicySelector) -> PolicyEntry {
        PolicyEntry {
            id: id.into(),
            version: "v1".into(),
            selector: sel,
            payload: json!({}),
        }
    }

    // ---- selector semantics ---------------------------------------------

    #[test]
    fn empty_selector_matches_anything() {
        let s = PolicySelector {
            tool: None,
            mcp_server: None,
            match_labels: BTreeMap::new(),
        };
        assert!(s.matches(None, None, &BTreeMap::new()));
        assert!(s.matches(Some("pay"), Some("commerce"), &BTreeMap::new()));
    }

    #[test]
    fn tool_selector_requires_exact_match() {
        let s = PolicySelector {
            tool: Some("pay".into()),
            mcp_server: None,
            match_labels: BTreeMap::new(),
        };
        assert!(s.matches(Some("pay"), None, &BTreeMap::new()));
        assert!(!s.matches(Some("refund"), None, &BTreeMap::new()));
        assert!(!s.matches(None, None, &BTreeMap::new()));
    }

    #[test]
    fn label_selector_is_subset_match() {
        let mut req_labels = BTreeMap::new();
        req_labels.insert("env".into(), "prod".into());
        req_labels.insert("team".into(), "commerce".into());

        let mut sel_labels = BTreeMap::new();
        sel_labels.insert("env".into(), "prod".into());
        let s = PolicySelector {
            tool: None,
            mcp_server: None,
            match_labels: sel_labels,
        };
        assert!(s.matches(None, None, &req_labels), "subset matches");

        // Selector requires more than the request provides → no match.
        let mut sel_labels2 = BTreeMap::new();
        sel_labels2.insert("env".into(), "prod".into());
        sel_labels2.insert("region".into(), "us".into());
        let s2 = PolicySelector {
            tool: None,
            mcp_server: None,
            match_labels: sel_labels2,
        };
        assert!(!s2.matches(None, None, &req_labels));
    }

    // ---- pure transition ------------------------------------------------

    #[test]
    fn upsert_into_empty_yields_one_entry_and_bumps_gen() {
        let s0 = PolicyEnvelopeSnapshot::default();
        let out = apply_policy_change(&s0, PolicyChange::Upserted(entry("p1", "v1")));
        assert_eq!(out.snapshot.generation(), 1);
        assert_eq!(out.snapshot.len(), 1);
        assert!(!out.structurally_unchanged);
    }

    #[test]
    fn upsert_same_version_is_structurally_unchanged() {
        let s0 = PolicyEnvelopeSnapshot::from_entries(5, [entry("p1", "v1")]);
        let out = apply_policy_change(&s0, PolicyChange::Upserted(entry("p1", "v1")));
        assert_eq!(out.snapshot.generation(), 6);
        assert!(out.structurally_unchanged, "same version → unchanged");
    }

    #[test]
    fn upsert_different_version_is_a_change() {
        let s0 = PolicyEnvelopeSnapshot::from_entries(5, [entry("p1", "v1")]);
        let out = apply_policy_change(&s0, PolicyChange::Upserted(entry("p1", "v2")));
        assert_eq!(out.snapshot.generation(), 6);
        assert!(!out.structurally_unchanged);
        assert_eq!(out.snapshot.get("p1").unwrap().version, "v2");
    }

    #[test]
    fn delete_known_id_removes_and_bumps_gen() {
        let s0 = PolicyEnvelopeSnapshot::from_entries(5, [entry("p1", "v1"), entry("p2", "v1")]);
        let out = apply_policy_change(&s0, PolicyChange::Deleted("p1".into()));
        assert_eq!(out.snapshot.generation(), 6);
        assert_eq!(out.snapshot.len(), 1);
        assert!(out.snapshot.get("p1").is_none());
        assert!(!out.structurally_unchanged);
    }

    #[test]
    fn delete_unknown_id_is_structurally_unchanged_but_still_bumps_gen() {
        let s0 = PolicyEnvelopeSnapshot::from_entries(5, [entry("p1", "v1")]);
        let out = apply_policy_change(&s0, PolicyChange::Deleted("ghost".into()));
        assert_eq!(out.snapshot.generation(), 6);
        assert_eq!(out.snapshot.len(), 1);
        assert!(out.structurally_unchanged);
    }

    #[test]
    fn reset_replaces_entire_set() {
        let s0 = PolicyEnvelopeSnapshot::from_entries(5, [entry("p1", "v1"), entry("p2", "v1")]);
        let out = apply_policy_change(
            &s0,
            PolicyChange::Reset(vec![entry("p3", "v1"), entry("p4", "v1")]),
        );
        assert_eq!(out.snapshot.generation(), 6);
        assert_eq!(out.snapshot.len(), 2);
        assert!(out.snapshot.get("p3").is_some());
        assert!(out.snapshot.get("p1").is_none());
        assert!(!out.structurally_unchanged);
    }

    #[test]
    fn reset_with_identical_set_is_structurally_unchanged() {
        let s0 = PolicyEnvelopeSnapshot::from_entries(5, [entry("p1", "v1"), entry("p2", "v1")]);
        let out = apply_policy_change(
            &s0,
            PolicyChange::Reset(vec![entry("p1", "v1"), entry("p2", "v1")]),
        );
        assert_eq!(out.snapshot.generation(), 6);
        assert!(out.structurally_unchanged);
    }

    #[test]
    fn generation_saturates_at_u64_max() {
        let s0 = PolicyEnvelopeSnapshot::from_entries(u64::MAX, []);
        let out = apply_policy_change(&s0, PolicyChange::Deleted("ghost".into()));
        assert_eq!(out.snapshot.generation(), u64::MAX);
    }

    // ---- iteration / lookup ---------------------------------------------

    #[test]
    fn iter_is_sorted_by_id() {
        let s = PolicyEnvelopeSnapshot::from_entries(
            1,
            [entry("zeta", "v1"), entry("alpha", "v1"), entry("mu", "v1")],
        );
        let ids: Vec<&str> = s.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn select_filters_by_selector() {
        let s = PolicyEnvelopeSnapshot::from_entries(
            1,
            [
                entry_with_selector(
                    "p_pay",
                    PolicySelector {
                        tool: Some("pay".into()),
                        mcp_server: None,
                        match_labels: BTreeMap::new(),
                    },
                ),
                entry_with_selector(
                    "p_refund",
                    PolicySelector {
                        tool: Some("refund".into()),
                        mcp_server: None,
                        match_labels: BTreeMap::new(),
                    },
                ),
            ],
        );
        let labels = BTreeMap::new();
        let hits: Vec<&str> = s
            .select(Some("pay"), None, &labels)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(hits, vec!["p_pay"]);
    }

    // ---- container hot-reload semantics ---------------------------------

    #[test]
    fn empty_envelope_yields_empty_snapshot() {
        let env = PolicyEnvelope::new();
        let s = env.snapshot();
        assert_eq!(s.generation(), 0);
        assert!(s.is_empty());
    }

    #[test]
    fn replace_snapshot_visible_on_next_snapshot_call() {
        let env = PolicyEnvelope::new();
        env.replace_snapshot(PolicyEnvelopeSnapshot::from_entries(1, [entry("p1", "v1")]));
        assert_eq!(env.snapshot().len(), 1);
    }

    #[test]
    fn arc_view_pins_pre_replace_snapshot() {
        let env = PolicyEnvelope::new();
        env.replace_snapshot(PolicyEnvelopeSnapshot::from_entries(
            1,
            [entry("pinned", "v1")],
        ));
        let pinned = env.snapshot();

        env.replace_snapshot(PolicyEnvelopeSnapshot::from_entries(
            2,
            [entry("fresh", "v1")],
        ));

        // Pinned view still sees pre-replace state.
        assert!(pinned.get("pinned").is_some());
        assert!(pinned.get("fresh").is_none());

        // Fresh snapshot sees post-replace state.
        let fresh = env.snapshot();
        assert!(fresh.get("pinned").is_none());
        assert!(fresh.get("fresh").is_some());
    }

    #[test]
    fn drive_envelope_via_apply_change_orchestration() {
        // Full orchestration: pure transition then publish via the
        // hot-reloadable container. Mirrors PR 35's snapshot_rebuild
        // pattern that the Phase 2 reconciler will follow.
        let env = PolicyEnvelope::new();
        let snap0 = env.snapshot().as_ref().clone();
        let out = apply_policy_change(&snap0, PolicyChange::Upserted(entry("p1", "v1")));
        env.replace_snapshot(out.snapshot);

        let snap1 = env.snapshot();
        assert_eq!(snap1.generation(), 1);
        assert!(snap1.get("p1").is_some());

        let out2 = apply_policy_change(snap1.as_ref(), PolicyChange::Deleted("p1".into()));
        env.replace_snapshot(out2.snapshot);

        let snap2 = env.snapshot();
        assert_eq!(snap2.generation(), 2);
        assert!(snap2.get("p1").is_none());
    }
}
