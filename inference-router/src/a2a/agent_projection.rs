//! `A2AAgent` CRD → trust-anchor projection.
//!
//! Pure synchronous projection from the eventual `A2AAgent` v1alpha1
//! CRD spec into a `Vec<TrustAnchor>` ready to feed
//! [`TrustStoreBuilder`](super::trust_store::TrustStoreBuilder).
//!
//! The CRD itself lands in Phase 2 — see implementation-plan §8 item 2.
//! This module ships the projection now so the controller-side
//! informer is a one-liner when the CRD lands: read CR → call
//! [`project_anchors`] → feed to the trust store snapshot rebuild.
//!
//! ## Why ship the projection before the CRD?
//!
//! 1. The contract crystallises the CRD shape: every field this
//!    function reads is now a hard, test-locked requirement on the
//!    eventual schema. Future CRD authors cannot quietly drop or
//!    rename a field without breaking these tests.
//! 2. The conformance corpus pattern (PR 28, PR 25) wants a pure
//!    Rust function it can drive with deterministic inputs. A live
//!    informer would add async + K8s client bring-up to every test.
//! 3. The function's invariants (no duplicate kids per agent,
//!    base64url decode pin, alg pin, expiry filter) are entirely
//!    independent of K8s — so they belong in the data plane.
//!
//! ## What this is not
//!
//! - Not a watcher. The watcher (kube-rs informer in
//!   `controller/src/reconcilers/a2a_agent.rs`) is a Phase 2
//!   deliverable; it will *call* this function on each spec change.
//! - Not a verifier. Verification of inbound cards uses the
//!   resulting trust store via [`crate::a2a::card_verifier`].
//! - Not opinionated about provenance format. Callers pass an
//!   already-formatted `source` string.

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use super::signature::base64url_decode;
use super::trust_store::TrustAnchor;

/// Projected shape of `A2AAgent.spec.signingKeys[*]` from the
/// eventual CRD. Field naming follows Kubernetes camelCase via
/// serde rename.
///
/// Only fields the projection actually needs are typed here; the
/// real CRD will carry more (description, contact, etc.).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aAgentSigningKeySpec {
    /// `kid` exposed to verifying peers in inbound AgentCard
    /// signatures. Must be non-empty and unique across the spec.
    pub kid: String,

    /// Algorithm pin. Currently only `"EdDSA"` is honoured; any
    /// other value yields [`ProjectionError::UnsupportedAlg`].
    pub alg: String,

    /// Ed25519 public key, base64url-encoded with NO padding (RFC
    /// 7515 §2). 32 bytes after decode.
    #[serde(rename = "publicKeyB64u")]
    pub public_key_b64u: String,

    /// Optional Unix-seconds expiry. `None` ⇒ never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
}

/// Projected shape of `A2AAgent.spec` (the part the projection cares
/// about — the rest is owned by other reconcilers).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aAgentSpec {
    /// `metadata.namespace/metadata.name` of the CR — feeds
    /// [`TrustAnchor::source`] for provenance attribution.
    pub namespace: String,
    pub name: String,
    /// Per-CR signing keys.
    pub signing_keys: Vec<A2aAgentSigningKeySpec>,
}

/// Errors raised by [`project_anchors`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectionError {
    /// `kid` was empty.
    #[error("signingKeys[{index}].kid is empty")]
    EmptyKid { index: usize },

    /// Two entries shared the same `kid`. The CRD must enforce this
    /// at admission, but we double-check here so a malformed spec
    /// can't quietly install ambiguous anchors.
    #[error(
        "signingKeys[{index}].kid `{kid}` duplicates an earlier entry at index {previous_index}"
    )]
    DuplicateKid {
        index: usize,
        kid: String,
        previous_index: usize,
    },

    /// Algorithm not in the (currently EdDSA-only) allow-list.
    #[error("signingKeys[{index}].alg `{alg}` is not supported (expected `EdDSA`)")]
    UnsupportedAlg { index: usize, alg: String },

    /// Public key did not base64url-decode.
    #[error("signingKeys[{index}].publicKeyB64u failed base64url decode: {reason}")]
    InvalidBase64 { index: usize, reason: String },

    /// Decoded public key was not 32 bytes (Ed25519 size).
    #[error("signingKeys[{index}].publicKeyB64u decoded to {got} bytes (expected 32 for Ed25519)")]
    WrongKeyLength { index: usize, got: usize },

    /// `ed25519-dalek` rejected the bytes (e.g. small-order point).
    #[error("signingKeys[{index}].publicKeyB64u is not a valid Ed25519 point: {reason}")]
    InvalidEd25519Point { index: usize, reason: String },
}

/// Project an `A2AAgent` spec to a `Vec<TrustAnchor>`.
///
/// Pure / total / synchronous. The result is in spec order. Caller
/// is expected to feed each anchor into a [`TrustStoreBuilder`] —
/// this function does NOT call `add` itself so callers can
/// interleave anchors from multiple A2AAgent CRs in a single
/// snapshot rebuild.
///
/// `source_prefix` is prepended to each anchor's
/// [`TrustAnchor::source`]. Convention: `"a2a-agent-cr"`.
pub fn project_anchors(
    spec: &A2aAgentSpec,
    source_prefix: &str,
) -> Result<Vec<TrustAnchor>, ProjectionError> {
    let mut out = Vec::with_capacity(spec.signing_keys.len());
    let mut seen_kids: Vec<(usize, &str)> = Vec::with_capacity(spec.signing_keys.len());

    for (index, key) in spec.signing_keys.iter().enumerate() {
        if key.kid.is_empty() {
            return Err(ProjectionError::EmptyKid { index });
        }
        if let Some(&(previous_index, _)) = seen_kids.iter().find(|(_, k)| *k == key.kid.as_str()) {
            return Err(ProjectionError::DuplicateKid {
                index,
                kid: key.kid.clone(),
                previous_index,
            });
        }
        seen_kids.push((index, key.kid.as_str()));

        if key.alg != "EdDSA" {
            return Err(ProjectionError::UnsupportedAlg {
                index,
                alg: key.alg.clone(),
            });
        }

        let bytes =
            base64url_decode(&key.public_key_b64u).map_err(|e| ProjectionError::InvalidBase64 {
                index,
                reason: e.to_string(),
            })?;
        if bytes.len() != 32 {
            return Err(ProjectionError::WrongKeyLength {
                index,
                got: bytes.len(),
            });
        }
        let arr: [u8; 32] =
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| ProjectionError::WrongKeyLength {
                    index,
                    got: bytes.len(),
                })?;
        let vk =
            VerifyingKey::from_bytes(&arr).map_err(|e| ProjectionError::InvalidEd25519Point {
                index,
                reason: e.to_string(),
            })?;

        out.push(TrustAnchor {
            kid: key.kid.clone(),
            key: vk,
            not_after: key.not_after,
            source: format!("{source_prefix}/{}/{}", spec.namespace, spec.name),
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::signature::base64url_encode;
    use ed25519_dalek::SigningKey;

    fn vk_b64u(seed: u8) -> String {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        base64url_encode(sk.verifying_key().as_bytes())
    }

    fn spec_with_keys(keys: Vec<A2aAgentSigningKeySpec>) -> A2aAgentSpec {
        A2aAgentSpec {
            namespace: "team-alpha".into(),
            name: "billing-agent".into(),
            signing_keys: keys,
        }
    }

    fn key(kid: &str, alg: &str, b64u: String, not_after: Option<i64>) -> A2aAgentSigningKeySpec {
        A2aAgentSigningKeySpec {
            kid: kid.into(),
            alg: alg.into(),
            public_key_b64u: b64u,
            not_after,
        }
    }

    #[test]
    fn happy_path_single_key() {
        let spec = spec_with_keys(vec![key("k1", "EdDSA", vk_b64u(1), None)]);
        let anchors = project_anchors(&spec, "a2a-agent-cr").unwrap();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].kid, "k1");
        assert_eq!(anchors[0].source, "a2a-agent-cr/team-alpha/billing-agent");
        assert!(anchors[0].not_after.is_none());
    }

    #[test]
    fn happy_path_multiple_keys_preserves_order() {
        let spec = spec_with_keys(vec![
            key("k1", "EdDSA", vk_b64u(1), None),
            key("k2", "EdDSA", vk_b64u(2), Some(1_900_000_000)),
            key("k3", "EdDSA", vk_b64u(3), None),
        ]);
        let anchors = project_anchors(&spec, "a2a-agent-cr").unwrap();
        let kids: Vec<&str> = anchors.iter().map(|a| a.kid.as_str()).collect();
        assert_eq!(kids, vec!["k1", "k2", "k3"]);
        assert_eq!(anchors[1].not_after, Some(1_900_000_000));
    }

    #[test]
    fn empty_kid_rejected() {
        let spec = spec_with_keys(vec![key("", "EdDSA", vk_b64u(1), None)]);
        let err = project_anchors(&spec, "src").unwrap_err();
        assert_eq!(err, ProjectionError::EmptyKid { index: 0 });
    }

    #[test]
    fn duplicate_kid_rejected() {
        let spec = spec_with_keys(vec![
            key("k1", "EdDSA", vk_b64u(1), None),
            key("k2", "EdDSA", vk_b64u(2), None),
            key("k1", "EdDSA", vk_b64u(3), None), // dupe of index 0
        ]);
        let err = project_anchors(&spec, "src").unwrap_err();
        assert_eq!(
            err,
            ProjectionError::DuplicateKid {
                index: 2,
                kid: "k1".into(),
                previous_index: 0,
            }
        );
    }

    #[test]
    fn unsupported_alg_rejected() {
        let spec = spec_with_keys(vec![key("k1", "ES256", vk_b64u(1), None)]);
        let err = project_anchors(&spec, "src").unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::UnsupportedAlg { index: 0, .. }
        ));
    }

    #[test]
    fn algorithm_confusion_hs256_rejected() {
        let spec = spec_with_keys(vec![key("k1", "HS256", vk_b64u(1), None)]);
        let err = project_anchors(&spec, "src").unwrap_err();
        assert!(matches!(err, ProjectionError::UnsupportedAlg { .. }));
    }

    #[test]
    fn alg_none_rejected() {
        let spec = spec_with_keys(vec![key("k1", "none", vk_b64u(1), None)]);
        let err = project_anchors(&spec, "src").unwrap_err();
        assert!(matches!(err, ProjectionError::UnsupportedAlg { .. }));
    }

    #[test]
    fn invalid_base64_rejected() {
        let spec = spec_with_keys(vec![key("k1", "EdDSA", "not!valid!base64".into(), None)]);
        let err = project_anchors(&spec, "src").unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::InvalidBase64 { index: 0, .. }
        ));
    }

    #[test]
    fn wrong_key_length_rejected() {
        // 16 bytes = wrong length (Ed25519 needs 32).
        let short = base64url_encode(&[0u8; 16]);
        let spec = spec_with_keys(vec![key("k1", "EdDSA", short, None)]);
        let err = project_anchors(&spec, "src").unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::WrongKeyLength { index: 0, got: 16 }
        ));
    }

    #[test]
    fn empty_signing_keys_yields_empty_anchor_list() {
        let spec = spec_with_keys(vec![]);
        let anchors = project_anchors(&spec, "src").unwrap();
        assert!(anchors.is_empty());
    }

    #[test]
    fn source_prefix_propagates_namespace_and_name() {
        let mut spec = spec_with_keys(vec![key("k1", "EdDSA", vk_b64u(1), None)]);
        spec.namespace = "ns-x".into();
        spec.name = "agent-y".into();
        let anchors = project_anchors(&spec, "agt-trust").unwrap();
        assert_eq!(anchors[0].source, "agt-trust/ns-x/agent-y");
    }

    #[test]
    fn anchor_round_trips_into_trust_store_builder() {
        use crate::a2a::trust_store::TrustStoreBuilder;
        let spec = spec_with_keys(vec![
            key("k1", "EdDSA", vk_b64u(11), None),
            key("k2", "EdDSA", vk_b64u(12), Some(2_000_000_000)),
        ]);
        let anchors = project_anchors(&spec, "a2a-agent-cr").unwrap();
        let mut builder = TrustStoreBuilder::new();
        for a in anchors {
            builder.add(a).unwrap();
        }
        let snap = builder.build();
        assert_eq!(snap.len(), 2);
        assert!(snap.lookup("k1", 1_000_000_000).is_some());
        // k2 expires at 2e9 → at 3e9 it's filtered out.
        assert!(snap.lookup("k2", 3_000_000_000).is_none());
    }

    #[test]
    fn cross_cr_kid_collision_caught_by_builder_not_projection() {
        // Same kid across two A2AAgent CRs is a *cluster-wide*
        // duplicate; the per-CR projection cannot detect it. Verify
        // that the TrustStoreBuilder catches it on combined add.
        use crate::a2a::trust_store::{TrustStoreBuildError, TrustStoreBuilder};
        let cr_a = spec_with_keys(vec![key("shared-kid", "EdDSA", vk_b64u(20), None)]);
        let cr_b_spec = A2aAgentSpec {
            namespace: "team-beta".into(),
            name: "other-agent".into(),
            signing_keys: vec![key("shared-kid", "EdDSA", vk_b64u(21), None)],
        };
        let mut builder = TrustStoreBuilder::new();
        for a in project_anchors(&cr_a, "a2a-agent-cr").unwrap() {
            builder.add(a).unwrap();
        }
        let conflict = project_anchors(&cr_b_spec, "a2a-agent-cr").unwrap();
        let err = builder
            .add(conflict.into_iter().next().unwrap())
            .unwrap_err();
        assert!(matches!(err, TrustStoreBuildError::DuplicateKid { .. }));
    }
}
