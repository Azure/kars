// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Trust-store snapshot rebuild orchestration.
//!
//! Composes [`agent_projection::project_anchors`] across many
//! [`A2aAgentSpec`]s into a single
//! [`trust_store::TrustStoreSnapshot`]. This is the function the
//! Phase 2 `A2AAgent` informer reconciler will call on every event:
//! it accepts the current full set of `A2aAgentSpec`s observed in
//! the cluster, runs them through projection + the trust-store
//! builder, and returns a snapshot ready to hand to
//! [`trust_store::TrustStore::replace_snapshot`].
//!
//! ## Error semantics
//!
//! The informer must remain available even if a single `A2aAgent` CR
//! is malformed. Therefore [`rebuild_snapshot`] returns a
//! [`RebuildOutcome`] carrying:
//!
//! - the [`TrustStoreSnapshot`] built from every CR that **did**
//!   project successfully and whose kids did not collide with an
//!   already-accepted anchor, and
//! - a list of [`RebuildIssue`]s describing every projection error
//!   and every duplicate-kid conflict.
//!
//! The reconciler is expected to publish the snapshot regardless of
//! issues (so good CRs immediately take effect) and surface the
//! issues on the offending CR's `status.conditions[*]`.
//!
//! Two `A2aAgentSpec`s that contribute the *same* anchor (identical
//! kid, key, expiry, and projected source) are deduplicated silently
//! — a single agent listed twice in the informer's cache is not an
//! error. Only conflicting kids (same kid, different key/expiry)
//! produce a [`RebuildIssue::DuplicateKid`].
//!
//! ## Determinism
//!
//! `rebuild_snapshot` is deterministic over a sorted view of the
//! input. Specs are processed in ascending `(namespace, name)` order
//! so that "first-wins" conflict resolution is reproducible across
//! reconciles. Without this, two informer cache iteration orders
//! could yield different snapshots and operators would see
//! `status.conditions[*]` flap on every restart.

use crate::a2a::agent_projection::{A2aAgentSpec, ProjectionError, project_anchors};
use crate::a2a::trust_store::{TrustStoreBuildError, TrustStoreBuilder, TrustStoreSnapshot};

/// One issue surfaced by [`rebuild_snapshot`]. Each variant carries
/// enough provenance to render a `status.conditions[*]` message on
/// the offending `A2aAgent` CR.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum RebuildIssue {
    /// The spec at `(namespace, name)` failed projection.
    ///
    /// `signing_keys[index]` was the offending entry.
    #[error("projection failed for {namespace}/{name}: {source}")]
    Projection {
        namespace: String,
        name: String,
        #[source]
        source: ProjectionError,
    },

    /// Two `A2aAgentSpec`s contributed conflicting anchors for the
    /// same kid. The first-seen anchor (lexicographically smaller
    /// `(namespace, name)`) is retained; the conflicting one is
    /// dropped.
    #[error(
        "duplicate kid '{kid}': retained from {existing}, dropped from {namespace}/{name}: {source}"
    )]
    DuplicateKid {
        kid: String,
        existing: String,
        namespace: String,
        name: String,
        #[source]
        source: TrustStoreBuildError,
    },
}

/// Result of a snapshot rebuild.
#[derive(Debug)]
pub struct RebuildOutcome {
    /// The snapshot built from every successfully-projected,
    /// non-conflicting anchor. Always returned, even when `issues`
    /// is non-empty.
    pub snapshot: TrustStoreSnapshot,
    /// Issues discovered during the rebuild. The reconciler should
    /// surface these on the offending CR's status; the snapshot is
    /// still safe to publish because conflicts were resolved by
    /// "first-wins" and projection failures dropped the bad CR
    /// entirely.
    pub issues: Vec<RebuildIssue>,
}

/// Build a single [`TrustStoreSnapshot`] from many `A2aAgentSpec`s.
///
/// `generation` is recorded in the resulting snapshot for
/// monotonicity assertions; reconcilers typically pass a counter
/// they bump on every event.
///
/// `source_prefix` is forwarded to [`project_anchors`]; convention
/// is `"a2a-agent-cr"`.
///
/// See module docs for error semantics and determinism guarantees.
#[must_use]
pub fn rebuild_snapshot(
    specs: &[A2aAgentSpec],
    generation: u64,
    source_prefix: &str,
) -> RebuildOutcome {
    // Process specs in deterministic (namespace, name) order.
    let mut sorted: Vec<&A2aAgentSpec> = specs.iter().collect();
    sorted.sort_by(|a, b| {
        a.namespace
            .cmp(&b.namespace)
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut builder = TrustStoreBuilder::new().generation(generation);
    let mut issues: Vec<RebuildIssue> = Vec::new();
    // (kid -> existing "namespace/name") for richer conflict messages.
    let mut kid_origin: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for spec in sorted {
        match project_anchors(spec, source_prefix) {
            Ok(anchors) => {
                let owner = format!("{}/{}", spec.namespace, spec.name);
                for anchor in anchors {
                    let kid = anchor.kid.clone();
                    match builder.add(anchor) {
                        Ok(_) => {
                            kid_origin.entry(kid).or_insert_with(|| owner.clone());
                        }
                        Err(e) => {
                            let existing = kid_origin
                                .get(&kid)
                                .cloned()
                                .unwrap_or_else(|| "<unknown>".into());
                            issues.push(RebuildIssue::DuplicateKid {
                                kid,
                                existing,
                                namespace: spec.namespace.clone(),
                                name: spec.name.clone(),
                                source: e,
                            });
                        }
                    }
                }
            }
            Err(e) => {
                issues.push(RebuildIssue::Projection {
                    namespace: spec.namespace.clone(),
                    name: spec.name.clone(),
                    source: e,
                });
            }
        }
    }

    RebuildOutcome {
        snapshot: builder.build(),
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::agent_projection::A2aAgentSigningKeySpec;
    use base64::Engine;
    use ed25519_dalek::SigningKey;

    fn vk_b64u(seed: u8) -> String {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vk.as_bytes())
    }

    fn spec(ns: &str, name: &str, keys: Vec<(&str, u8, Option<i64>)>) -> A2aAgentSpec {
        A2aAgentSpec {
            namespace: ns.into(),
            name: name.into(),
            signing_keys: keys
                .into_iter()
                .map(|(kid, seed, exp)| A2aAgentSigningKeySpec {
                    kid: kid.into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: vk_b64u(seed),
                    not_after: exp,
                })
                .collect(),
        }
    }

    #[test]
    fn empty_input_yields_empty_snapshot_no_issues() {
        let out = rebuild_snapshot(&[], 7, "a2a-agent-cr");
        assert!(out.issues.is_empty());
        assert_eq!(out.snapshot.generation(), 7);
        assert_eq!(out.snapshot.len(), 0);
    }

    #[test]
    fn many_specs_with_unique_kids_all_land_in_snapshot() {
        let specs = vec![
            spec("ns-a", "agent-1", vec![("kid-a", 1, None)]),
            spec("ns-a", "agent-2", vec![("kid-b", 2, None)]),
            spec("ns-b", "agent-3", vec![("kid-c", 3, None)]),
        ];
        let out = rebuild_snapshot(&specs, 1, "a2a-agent-cr");
        assert!(out.issues.is_empty(), "issues: {:?}", out.issues);
        assert_eq!(out.snapshot.len(), 3);
        let kids = out.snapshot.kids();
        assert!(kids.contains(&"kid-a".to_string()));
        assert!(kids.contains(&"kid-b".to_string()));
        assert!(kids.contains(&"kid-c".to_string()));
    }

    #[test]
    fn conflicting_kid_first_seen_wins_with_issue_emitted() {
        // Lexicographic order over (namespace, name): ns-a/agent-1 < ns-b/agent-2.
        // ns-a/agent-1 contributes kid-x with seed 1 → wins.
        // ns-b/agent-2 contributes kid-x with seed 2 → conflict → dropped.
        let specs = vec![
            spec("ns-b", "agent-2", vec![("kid-x", 2, None)]),
            spec("ns-a", "agent-1", vec![("kid-x", 1, None)]),
        ];
        let out = rebuild_snapshot(&specs, 0, "a2a-agent-cr");
        assert_eq!(out.snapshot.len(), 1);
        assert_eq!(out.issues.len(), 1, "exactly one duplicate-kid issue");
        match &out.issues[0] {
            RebuildIssue::DuplicateKid {
                kid,
                existing,
                namespace,
                name,
                ..
            } => {
                assert_eq!(kid, "kid-x");
                assert_eq!(existing, "ns-a/agent-1");
                assert_eq!(namespace, "ns-b");
                assert_eq!(name, "agent-2");
            }
            other => panic!("expected DuplicateKid, got {other:?}"),
        }
        // Retained anchor is the seed=1 one (ns-a/agent-1).
        let anchor = out.snapshot.lookup("kid-x", 0).expect("kid-x present");
        let expected_seed_1 = SigningKey::from_bytes(&[1u8; 32]).verifying_key();
        assert_eq!(anchor.key.as_bytes(), expected_seed_1.as_bytes());
    }

    #[test]
    fn projection_failure_drops_offending_spec_only() {
        // First spec is fine; second has unsupported alg → projection error.
        let mut bad = spec("ns-z", "agent-bad", vec![("kid-bad", 4, None)]);
        bad.signing_keys[0].alg = "RS256".into();

        let specs = vec![spec("ns-a", "agent-good", vec![("kid-good", 5, None)]), bad];
        let out = rebuild_snapshot(&specs, 0, "a2a-agent-cr");
        assert_eq!(out.snapshot.len(), 1, "good spec landed");
        assert!(out.snapshot.lookup("kid-good", 0).is_some());
        assert!(out.snapshot.lookup("kid-bad", 0).is_none());
        assert_eq!(out.issues.len(), 1);
        match &out.issues[0] {
            RebuildIssue::Projection {
                namespace, name, ..
            } => {
                assert_eq!(namespace, "ns-z");
                assert_eq!(name, "agent-bad");
            }
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_anchor_within_same_owner_is_silently_deduped() {
        // Single spec, same kid listed twice with identical metadata
        // is rejected by `project_anchors` itself
        // (`ProjectionError::DuplicateKid`) before reaching the
        // builder. We assert that path so the test corpus pins the
        // contract: in-spec duplicates → projection issue, NOT
        // builder issue.
        let dup = spec("ns-a", "dup", vec![("kid-d", 9, None), ("kid-d", 9, None)]);
        let out = rebuild_snapshot(&[dup], 0, "a2a-agent-cr");
        assert_eq!(out.snapshot.len(), 0);
        assert_eq!(out.issues.len(), 1);
        assert!(matches!(&out.issues[0], RebuildIssue::Projection { .. }));
    }

    #[test]
    fn deterministic_ordering_independent_of_input_order() {
        // Run the same conflicting-kid inputs in two different orders
        // and assert the snapshot + issues are byte-identical.
        let s1 = vec![
            spec("ns-a", "agent-1", vec![("k", 1, None)]),
            spec("ns-b", "agent-2", vec![("k", 2, None)]),
        ];
        let s2 = vec![
            spec("ns-b", "agent-2", vec![("k", 2, None)]),
            spec("ns-a", "agent-1", vec![("k", 1, None)]),
        ];
        let o1 = rebuild_snapshot(&s1, 42, "a2a-agent-cr");
        let o2 = rebuild_snapshot(&s2, 42, "a2a-agent-cr");
        assert_eq!(o1.snapshot.kids(), o2.snapshot.kids());
        assert_eq!(o1.issues, o2.issues);
        assert_eq!(o1.snapshot.generation(), o2.snapshot.generation());
        // First-wins is ns-a/agent-1 in both cases.
        let anchor = o1.snapshot.lookup("k", 0).unwrap();
        let expected = SigningKey::from_bytes(&[1u8; 32]).verifying_key();
        assert_eq!(anchor.key.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn generation_round_trips() {
        let out = rebuild_snapshot(
            &[spec("ns-a", "agent-1", vec![("k", 1, None)])],
            12345,
            "a2a-agent-cr",
        );
        assert_eq!(out.snapshot.generation(), 12345);
    }
}
