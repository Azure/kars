// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure compile / verify step for `TrustGraph` — signature
//! verification, vertex deduplication, edge validation. Mirrors the
//! split shape used by `a2a_agent_compile` / `tool_policy_compile`:
//! reconciler is the side-effecting outer loop, this module is
//! deterministic and side-effect-free so it gets dense unit-test
//! coverage.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::trust_graph::TrustEdge;
use crate::trust_graph::TrustGraphSpec;
#[cfg(test)]
use crate::trust_graph::TrustVertex;

/// Domain-separator prefix for the canonical signing payload.
/// Locked at v1 — bumping requires a coordinated CRD-version bump
/// and a peer rollout window, so it is intentionally hard to change.
pub const PAYLOAD_DOMAIN: &[u8] = b"trustgraph.v1\n";

/// Output of [`compile_trust_graph`] — the validated graph that gets
/// serialised to the projection ConfigMap.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedGraph {
    /// Verified vertices, in the order they appeared in the spec.
    /// Duplicate-id vertices are kept once (first occurrence wins);
    /// vertices whose key/alg fails to decode are dropped.
    pub vertices: Vec<ProjectedVertex>,

    /// Verified edges, in the order they appeared in the spec.
    /// Edges that fail verification are dropped — never silently
    /// promoted; the reconciler stamps `status.invalidEdges`.
    pub edges: Vec<ProjectedEdge>,

    /// SHA-256-truncated content hash of the projected graph
    /// (rendered as 16 hex chars) — change-detection token for the
    /// router-side mount and the audit log.
    pub version_hash: String,

    /// Total edges in the input spec — exposed so consumers can
    /// detect a degraded graph (`edges.len() < input_edge_count`).
    pub input_edge_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedVertex {
    pub id: String,
    pub alg: String,
    pub public_key_b64u: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedEdge {
    pub from: String,
    pub to: String,
    pub score: u32,
    pub issued_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Reasons why a single edge can fail verification. Closed set so
/// reconciler logging is log-injection-safe (no operator-supplied
/// strings interpolated into the error class).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeRejectReason {
    /// `from` did not match any (valid) vertex.
    UnknownFromVertex,
    /// `to` did not match any (valid) vertex.
    UnknownToVertex,
    /// Score outside [0, 1000].
    ScoreOutOfRange,
    /// Signature failed to base64url-decode.
    SignatureDecodeError,
    /// Signature wrong length (Ed25519 = 64 bytes after decode).
    SignatureLengthError,
    /// `notAfter` is set and `issuedAt > notAfter`.
    InvertedExpiry,
    /// Cryptographic verification failed.
    SignatureMismatch,
}

impl EdgeRejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeRejectReason::UnknownFromVertex => "unknown_from_vertex",
            EdgeRejectReason::UnknownToVertex => "unknown_to_vertex",
            EdgeRejectReason::ScoreOutOfRange => "score_out_of_range",
            EdgeRejectReason::SignatureDecodeError => "signature_decode_error",
            EdgeRejectReason::SignatureLengthError => "signature_length_error",
            EdgeRejectReason::InvertedExpiry => "inverted_expiry",
            EdgeRejectReason::SignatureMismatch => "signature_mismatch",
        }
    }
}

/// Reasons a vertex can be dropped (and any outbound edges from it
/// dropped along with it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexRejectReason {
    UnsupportedAlg,
    PublicKeyDecodeError,
    PublicKeyLengthError,
    DuplicateId,
}

/// Compile a `TrustGraphSpec` into the verified projection.
///
/// The function is **pure**: deterministic, allocation-safe, no I/O.
/// Invalid vertices and edges are dropped from the projection but
/// counted; the reconciler reads the count back to set
/// `status.invalidEdges`.
pub fn compile_trust_graph(spec: &TrustGraphSpec) -> CompileResult {
    let mut vertices: Vec<ProjectedVertex> = Vec::with_capacity(spec.vertices.len());
    let mut keys: HashMap<String, VerifyingKey> = HashMap::with_capacity(spec.vertices.len());
    let mut vertex_rejects: Vec<(usize, VertexRejectReason)> = Vec::new();

    for (i, v) in spec.vertices.iter().enumerate() {
        if keys.contains_key(&v.id) {
            vertex_rejects.push((i, VertexRejectReason::DuplicateId));
            continue;
        }
        if v.alg != "EdDSA" {
            vertex_rejects.push((i, VertexRejectReason::UnsupportedAlg));
            continue;
        }
        let key_bytes = match URL_SAFE_NO_PAD.decode(v.public_key_b64u.as_bytes()) {
            Ok(b) => b,
            Err(_) => {
                vertex_rejects.push((i, VertexRejectReason::PublicKeyDecodeError));
                continue;
            }
        };
        if key_bytes.len() != 32 {
            vertex_rejects.push((i, VertexRejectReason::PublicKeyLengthError));
            continue;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&key_bytes);
        let vk = match VerifyingKey::from_bytes(&arr) {
            Ok(k) => k,
            Err(_) => {
                vertex_rejects.push((i, VertexRejectReason::PublicKeyLengthError));
                continue;
            }
        };
        keys.insert(v.id.clone(), vk);
        vertices.push(ProjectedVertex {
            id: v.id.clone(),
            alg: v.alg.clone(),
            public_key_b64u: v.public_key_b64u.clone(),
            label: v.label.clone(),
        });
    }

    let mut edges: Vec<ProjectedEdge> = Vec::with_capacity(spec.edges.len());
    let mut edge_rejects: Vec<(usize, EdgeRejectReason)> = Vec::new();

    for (i, e) in spec.edges.iter().enumerate() {
        match validate_edge(e, &keys) {
            Ok(()) => edges.push(ProjectedEdge {
                from: e.from.clone(),
                to: e.to.clone(),
                score: e.score,
                issued_at: e.issued_at,
                not_after: e.not_after,
                signature: e.signature.clone(),
                reason: e.reason.clone(),
            }),
            Err(reason) => edge_rejects.push((i, reason)),
        }
    }

    let projected = ProjectedGraph {
        vertices,
        edges,
        version_hash: String::new(),
        input_edge_count: spec.edges.len(),
    };
    let projected = with_version_hash(projected);

    CompileResult {
        projection: projected,
        vertex_rejects,
        edge_rejects,
    }
}

/// Result of [`compile_trust_graph`] — the projection plus the lists
/// of rejected entries the reconciler turns into `status.*` counts
/// and condition messages.
#[derive(Debug, Clone)]
pub struct CompileResult {
    pub projection: ProjectedGraph,
    pub vertex_rejects: Vec<(usize, VertexRejectReason)>,
    pub edge_rejects: Vec<(usize, EdgeRejectReason)>,
}

fn validate_edge(
    e: &TrustEdge,
    keys: &HashMap<String, VerifyingKey>,
) -> Result<(), EdgeRejectReason> {
    let from_key = keys
        .get(&e.from)
        .ok_or(EdgeRejectReason::UnknownFromVertex)?;
    if !keys.contains_key(&e.to) {
        return Err(EdgeRejectReason::UnknownToVertex);
    }
    if e.score > 1000 {
        return Err(EdgeRejectReason::ScoreOutOfRange);
    }
    if let Some(na) = e.not_after
        && e.issued_at > na
    {
        return Err(EdgeRejectReason::InvertedExpiry);
    }
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(e.signature.as_bytes())
        .map_err(|_| EdgeRejectReason::SignatureDecodeError)?;
    if sig_bytes.len() != 64 {
        return Err(EdgeRejectReason::SignatureLengthError);
    }
    let mut arr = [0u8; 64];
    arr.copy_from_slice(&sig_bytes);
    let sig = Signature::from_bytes(&arr);
    let payload = canonical_payload(&e.from, &e.to, e.score, e.issued_at, e.not_after);
    from_key
        .verify(&payload, &sig)
        .map_err(|_| EdgeRejectReason::SignatureMismatch)?;
    Ok(())
}

/// Canonical signed-bytes layout. **Locked** at v1 — see
/// [`PAYLOAD_DOMAIN`].
pub fn canonical_payload(
    from: &str,
    to: &str,
    score: u32,
    issued_at: i64,
    not_after: Option<i64>,
) -> Vec<u8> {
    let na = not_after.map(|n| n.to_string()).unwrap_or_default();
    let mut buf =
        Vec::with_capacity(PAYLOAD_DOMAIN.len() + from.len() + to.len() + 32 + na.len() + 5);
    buf.extend_from_slice(PAYLOAD_DOMAIN);
    buf.extend_from_slice(from.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(to.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(score.to_string().as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(issued_at.to_string().as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(na.as_bytes());
    buf.push(b'\n');
    buf
}

fn with_version_hash(mut g: ProjectedGraph) -> ProjectedGraph {
    let json = serde_json::to_vec(&serde_json::json!({
        "vertices": g.vertices,
        "edges": g.edges,
    }))
    .expect("ProjectedGraph fields are always serializable");
    let digest = Sha256::digest(&json);
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    g.version_hash = out;
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn b64u(b: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(b)
    }

    fn make_signer(seed: u8) -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pk = sk.verifying_key();
        (sk, b64u(pk.as_bytes()))
    }

    fn signed_edge(
        sk: &SigningKey,
        from: &str,
        to: &str,
        score: u32,
        issued_at: i64,
        not_after: Option<i64>,
    ) -> TrustEdge {
        let payload = canonical_payload(from, to, score, issued_at, not_after);
        let sig = sk.sign(&payload);
        TrustEdge {
            from: from.into(),
            to: to.into(),
            score,
            issued_at,
            not_after,
            signature: b64u(&sig.to_bytes()),
            reason: None,
        }
    }

    #[test]
    fn empty_graph_compiles_to_empty_projection() {
        let spec = TrustGraphSpec::default();
        let r = compile_trust_graph(&spec);
        assert!(r.projection.vertices.is_empty());
        assert!(r.projection.edges.is_empty());
        assert!(r.vertex_rejects.is_empty());
        assert!(r.edge_rejects.is_empty());
        assert_eq!(r.projection.input_edge_count, 0);
        assert_eq!(r.projection.version_hash.len(), 16);
    }

    #[test]
    fn valid_signed_edge_round_trips() {
        let (sk_a, pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b,
                    label: None,
                },
            ],
            edges: vec![signed_edge(&sk_a, "a", "b", 800, 1_700_000_000, None)],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.projection.vertices.len(), 2);
        assert_eq!(r.projection.edges.len(), 1);
        assert!(r.edge_rejects.is_empty());
    }

    #[test]
    fn tampered_score_rejects_edge() {
        let (sk_a, pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let mut e = signed_edge(&sk_a, "a", "b", 500, 1_700_000_000, None);
        e.score = 501;
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b,
                    label: None,
                },
            ],
            edges: vec![e],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.projection.edges.len(), 0);
        assert_eq!(r.edge_rejects.len(), 1);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::SignatureMismatch);
    }

    #[test]
    fn unknown_from_vertex_rejects_edge() {
        let (sk_a, _pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let spec = TrustGraphSpec {
            vertices: vec![TrustVertex {
                id: "b".into(),
                alg: "EdDSA".into(),
                public_key_b64u: pk_b,
                label: None,
            }],
            edges: vec![signed_edge(&sk_a, "a", "b", 500, 1_700_000_000, None)],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.edge_rejects.len(), 1);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::UnknownFromVertex);
    }

    #[test]
    fn unknown_to_vertex_rejects_edge() {
        let (sk_a, pk_a) = make_signer(1);
        let spec = TrustGraphSpec {
            vertices: vec![TrustVertex {
                id: "a".into(),
                alg: "EdDSA".into(),
                public_key_b64u: pk_a,
                label: None,
            }],
            edges: vec![signed_edge(&sk_a, "a", "ghost", 500, 1_700_000_000, None)],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.edge_rejects.len(), 1);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::UnknownToVertex);
    }

    #[test]
    fn out_of_range_score_rejects_edge() {
        // score > 1000 is rejected by the validator before signature check.
        let (sk_a, pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let e = signed_edge(&sk_a, "a", "b", 9999, 1_700_000_000, None);
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b,
                    label: None,
                },
            ],
            edges: vec![e],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::ScoreOutOfRange);
    }

    #[test]
    fn inverted_expiry_rejects_edge() {
        let (sk_a, pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let e = signed_edge(&sk_a, "a", "b", 500, 2_000_000_000, Some(1_000_000_000));
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b,
                    label: None,
                },
            ],
            edges: vec![e],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::InvertedExpiry);
    }

    #[test]
    fn duplicate_vertex_id_drops_second_occurrence() {
        let (_, pk_a) = make_signer(1);
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a.clone(),
                    label: Some("first".into()),
                },
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: Some("second".into()),
                },
            ],
            edges: vec![],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.projection.vertices.len(), 1);
        assert_eq!(r.projection.vertices[0].label.as_deref(), Some("first"));
        assert_eq!(r.vertex_rejects.len(), 1);
        assert_eq!(r.vertex_rejects[0].1, VertexRejectReason::DuplicateId);
    }

    #[test]
    fn unsupported_alg_drops_vertex() {
        let spec = TrustGraphSpec {
            vertices: vec![TrustVertex {
                id: "a".into(),
                alg: "RS256".into(),
                public_key_b64u: "AAAA".into(),
                label: None,
            }],
            edges: vec![],
        };
        let r = compile_trust_graph(&spec);
        assert!(r.projection.vertices.is_empty());
        assert_eq!(r.vertex_rejects[0].1, VertexRejectReason::UnsupportedAlg);
    }

    #[test]
    fn bad_pubkey_length_drops_vertex() {
        let spec = TrustGraphSpec {
            vertices: vec![TrustVertex {
                id: "a".into(),
                alg: "EdDSA".into(),
                public_key_b64u: b64u(&[0u8; 16]),
                label: None,
            }],
            edges: vec![],
        };
        let r = compile_trust_graph(&spec);
        assert!(r.projection.vertices.is_empty());
        assert_eq!(
            r.vertex_rejects[0].1,
            VertexRejectReason::PublicKeyLengthError
        );
    }

    #[test]
    fn bad_signature_length_rejects_edge() {
        let (_, pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let e = TrustEdge {
            from: "a".into(),
            to: "b".into(),
            score: 500,
            issued_at: 1_700_000_000,
            not_after: None,
            signature: b64u(&[0u8; 32]),
            reason: None,
        };
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b,
                    label: None,
                },
            ],
            edges: vec![e],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::SignatureLengthError);
    }

    #[test]
    fn version_hash_is_deterministic_and_stable_across_runs() {
        let (sk_a, pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b,
                    label: None,
                },
            ],
            edges: vec![signed_edge(&sk_a, "a", "b", 600, 1_700_000_000, None)],
        };
        let h1 = compile_trust_graph(&spec).projection.version_hash;
        let h2 = compile_trust_graph(&spec).projection.version_hash;
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn payload_domain_separator_prevents_replay() {
        // An attacker who controls a signature over `from||to||...`
        // (without the `trustgraph.v1\n` prefix) cannot replay it as
        // a TrustGraph edge. We model this by signing the *unprefixed*
        // payload and showing the validator rejects it.
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = b64u(sk.verifying_key().as_bytes());
        let unprefixed: Vec<u8> = b"a\nb\n500\n1700000000\n\n".to_vec();
        let bad_sig = sk.sign(&unprefixed);
        let edge = TrustEdge {
            from: "a".into(),
            to: "b".into(),
            score: 500,
            issued_at: 1_700_000_000,
            not_after: None,
            signature: b64u(&bad_sig.to_bytes()),
            reason: None,
        };
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk.clone(),
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk,
                    label: None,
                },
            ],
            edges: vec![edge],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::SignatureMismatch);
    }

    #[test]
    fn cross_vertex_signature_rejected() {
        // Edge claims `from = a` but is signed by `b`'s key. Must reject.
        let (_, pk_a) = make_signer(1);
        let (sk_b, pk_b) = make_signer(2);
        let e = signed_edge(&sk_b, "a", "b", 500, 1_700_000_000, None);
        let spec = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a,
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b,
                    label: None,
                },
            ],
            edges: vec![e],
        };
        let r = compile_trust_graph(&spec);
        assert_eq!(r.edge_rejects[0].1, EdgeRejectReason::SignatureMismatch);
    }

    #[test]
    fn version_hash_changes_when_graph_changes() {
        let (sk_a, pk_a) = make_signer(1);
        let (_, pk_b) = make_signer(2);
        let v1 = TrustGraphSpec {
            vertices: vec![
                TrustVertex {
                    id: "a".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_a.clone(),
                    label: None,
                },
                TrustVertex {
                    id: "b".into(),
                    alg: "EdDSA".into(),
                    public_key_b64u: pk_b.clone(),
                    label: None,
                },
            ],
            edges: vec![signed_edge(&sk_a, "a", "b", 100, 1_700_000_000, None)],
        };
        let v2 = TrustGraphSpec {
            vertices: v1.vertices.clone(),
            edges: vec![signed_edge(&sk_a, "a", "b", 999, 1_700_000_000, None)],
        };
        let h1 = compile_trust_graph(&v1).projection.version_hash;
        let h2 = compile_trust_graph(&v2).projection.version_hash;
        assert_ne!(h1, h2);
    }
}
