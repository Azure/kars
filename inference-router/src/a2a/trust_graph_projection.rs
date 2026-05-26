// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TrustGraph projection — read-only consumer of the controller-side
//! `TrustGraph` CRD's compiled projection (Phase F1).
//!
//! ## What this module is
//!
//! A pure parser + lookup table for the JSON document the controller
//! writes to `ConfigMap[name=trustgraph-<n>-projection,
//! ns=kars-system].data["graph.json"]`. The wire shape is the
//! camelCase serialisation of `controller::trust_graph_compile::ProjectedGraph`:
//!
//! ```jsonc
//! {
//!   "vertices": [{"id": "...", "alg": "EdDSA", "publicKeyB64u": "..."}],
//!   "edges":    [{"from": "...", "to": "...", "score": 750,
//!                 "issuedAt": 1700000000, "signature": "..."}],
//!   "versionHash":     "....",
//!   "inputEdgeCount":  2
//! }
//! ```
//!
//! ## What this module is NOT
//!
//! - **Not a trust-score store.** The authoritative store is AGT's
//!   [`agentmesh::TrustManager`] — see `governance::trust_ops`. This
//!   module is a *signal source* the governance layer can opportunistically
//!   consult to bootstrap brand-new peers.
//! - **Not a verifier.** The controller has already cryptographically
//!   verified every edge before publishing the projection. The router
//!   trusts the controller's verdict (the projection ConfigMap is signed
//!   by SSA field-management; tampering is detectable by version-hash
//!   re-fetch). Only edges that survived `compile_trust_graph` reach this
//!   reader.
//! - **Not a writer.** Every method on the public type is `&self`. The
//!   only mutation path is "swap the whole projection atomically" via
//!   the loader.
//!
//! ## Module isolation
//!
//! Lives under `a2a::` to inherit the providers-only / no-credentials /
//! `forbid(unsafe_code)` posture. No `auth::*` import, no I/O —
//! the loader half (kube-client / file fetch) lives in
//! `trust_graph_loader.rs`.

use std::collections::HashMap;

use serde::Deserialize;

/// Maximum projection size accepted by the parser (1 MiB). The controller
/// caps a TrustGraph CR via apiserver's 1 MiB object limit; we mirror that
/// here as a defence against a tampered ConfigMap pointing at a bloated
/// payload.
pub const MAX_PROJECTION_BYTES: usize = 1_048_576;

/// One vertex in the projected graph — i.e. an agent identity that has
/// passed `compile_trust_graph`'s alg/key validations.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedVertex {
    pub id: String,
    pub alg: String,
    #[serde(rename = "publicKeyB64u")]
    pub public_key_b64u: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One edge in the projected graph — a controller-verified Ed25519
/// attestation `from → to` carrying an AGT-domain score [0, 1000].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedEdge {
    pub from: String,
    pub to: String,
    pub score: u32,
    pub issued_at: i64,
    #[serde(default)]
    pub not_after: Option<i64>,
    pub signature: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Wire-format root of `graph.json`. Deserialisation is fail-closed —
/// any unknown top-level field is ignored, but malformed JSON or a
/// missing required field surfaces as `ProjectionParseError::Json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireProjection {
    #[serde(default)]
    vertices: Vec<ProjectedVertex>,
    #[serde(default)]
    edges: Vec<ProjectedEdge>,
    #[serde(default)]
    version_hash: String,
    #[serde(default)]
    input_edge_count: usize,
}

/// In-memory, lookup-optimised trust-graph projection.
///
/// Indexed by `(from, to)` for O(1) edge lookup on the bootstrap path.
/// Vertices are stored alongside but not directly indexed — they exist
/// only so an operator-facing endpoint can enumerate the topology, not
/// for verification (verification happened in the controller).
#[derive(Debug, Clone, Default)]
pub struct TrustGraphProjection {
    vertices: HashMap<String, ProjectedVertex>,
    edges_by_pair: HashMap<(String, String), ProjectedEdge>,
    version_hash: String,
    input_edge_count: usize,
    edge_count: usize,
}

impl TrustGraphProjection {
    /// Parse a projection JSON document. Returns
    /// [`ProjectionParseError`] on malformed JSON or oversize payloads.
    /// An empty `{}` parses to an empty projection (zero vertices, zero
    /// edges) — semantically identical to "no graph".
    pub fn from_json(raw: &str) -> Result<Self, ProjectionParseError> {
        if raw.len() > MAX_PROJECTION_BYTES {
            return Err(ProjectionParseError::Oversize {
                actual: raw.len(),
                max: MAX_PROJECTION_BYTES,
            });
        }
        let wire: WireProjection =
            serde_json::from_str(raw).map_err(|e| ProjectionParseError::Json(e.to_string()))?;

        let mut vertices = HashMap::with_capacity(wire.vertices.len());
        for v in wire.vertices {
            // Last-write-wins on duplicate vertex id — the controller's
            // compile step already de-duplicates, so this is just a
            // belt-and-braces guard against a tampered ConfigMap.
            vertices.insert(v.id.clone(), v);
        }

        let edge_count = wire.edges.len();
        let mut edges_by_pair = HashMap::with_capacity(edge_count);
        for e in wire.edges {
            edges_by_pair.insert((e.from.clone(), e.to.clone()), e);
        }
        // After de-duplication by (from, to), the lookup table may have
        // fewer entries than the input — record the post-dedup count so
        // operators can detect a tampered ConfigMap that smuggled
        // duplicate pairs.
        let edge_count = edges_by_pair.len();

        Ok(Self {
            vertices,
            edges_by_pair,
            version_hash: wire.version_hash,
            input_edge_count: wire.input_edge_count,
            edge_count,
        })
    }

    /// Empty projection — semantically identical to "no signed
    /// attestations available". Used as the default when no projection
    /// file is mounted.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Look up a single direct edge `from → to`. Returns `None` if
    /// either endpoint is unknown, no such edge exists, or the edge has
    /// expired relative to the supplied `now_unix_secs`.
    ///
    /// The expiry check is performed here — not at parse time — because
    /// the projection is loaded once and queried many times; reloading
    /// solely for expiry would be wasteful, and a freshly-expired edge
    /// silently dropping is the desired semantics.
    pub fn direct_edge(&self, from: &str, to: &str, now_unix_secs: i64) -> Option<&ProjectedEdge> {
        // Reject self-attestations defensively. The controller already
        // rejects these at admission time, but a hand-edited ConfigMap
        // could slip one in. A self-attested score MUST NOT bootstrap
        // anything — it would be a trivial forgery vector.
        if from == to {
            return None;
        }
        let edge = self
            .edges_by_pair
            .get(&(from.to_string(), to.to_string()))?;
        if let Some(not_after) = edge.not_after
            && now_unix_secs > not_after
        {
            return None;
        }
        Some(edge)
    }

    /// Enumerate the vertices for read-only operator endpoints.
    pub fn vertices(&self) -> impl Iterator<Item = &ProjectedVertex> {
        self.vertices.values()
    }

    /// Total vertex count.
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// Total verified-edge count (the controller-published number, not
    /// the input edge count).
    pub fn edge_count(&self) -> usize {
        self.edge_count
    }

    /// Number of edges in the original spec — exposed so operators can
    /// detect a degraded projection (`edge_count() < input_edge_count()`
    /// means the controller dropped some edges as invalid).
    pub fn input_edge_count(&self) -> usize {
        self.input_edge_count
    }

    /// Content-hash of the projection — used by
    /// [`metrics::TRUSTGRAPH_PROJECTION_VERSION`] to label the in-memory
    /// state and to emit a single audit event on swap.
    pub fn version_hash(&self) -> &str {
        &self.version_hash
    }

    /// Whether the projection contains any data. `true` for both
    /// `Self::default()` and `Self::from_json("{}")`.
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty() && self.edges_by_pair.is_empty()
    }
}

/// Parse-time errors — fail-closed. The router treats every variant as
/// "no projection available" rather than crashing the trust path.
#[derive(Debug, thiserror::Error)]
pub enum ProjectionParseError {
    #[error("trust-graph projection JSON malformed: {0}")]
    Json(String),

    #[error("trust-graph projection oversize: {actual} bytes exceeds limit {max}")]
    Oversize { actual: usize, max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Produced by the F1 fixture generator
    /// (`/tmp/fix` standalone Rust binary, seeds `[0x11; 32]` /
    /// `[0x22; 32]`).
    const PK_A: &str = "0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc";
    const PK_B: &str = "oJql9HpnWYAv-VX43C0qFKXJnSO-l_hkEn_5ODRVpPA";
    const SIG_AB: &str =
        "5-Esp-uk11u_cGsCRdUXwxbxtCzSQsNaEzIBeFUBPZFWz9cUtr9PBw6eWFIBAAprz9kGwH2wTk3xaSnbJVQzAw";

    fn fixture_json() -> String {
        format!(
            r#"{{
              "vertices": [
                {{"id": "alpha", "alg": "EdDSA", "publicKeyB64u": "{pka}"}},
                {{"id": "beta",  "alg": "EdDSA", "publicKeyB64u": "{pkb}", "label": "ops"}}
              ],
              "edges": [
                {{"from": "alpha", "to": "beta", "score": 750,
                  "issuedAt": 1700000000, "signature": "{sig}"}}
              ],
              "versionHash": "abc123def4567890",
              "inputEdgeCount": 1
            }}"#,
            pka = PK_A,
            pkb = PK_B,
            sig = SIG_AB,
        )
    }

    #[test]
    fn parses_fixture() {
        let p = TrustGraphProjection::from_json(&fixture_json()).expect("parses");
        assert_eq!(p.vertex_count(), 2);
        assert_eq!(p.edge_count(), 1);
        assert_eq!(p.input_edge_count(), 1);
        assert_eq!(p.version_hash(), "abc123def4567890");
        assert!(!p.is_empty());
    }

    #[test]
    fn direct_edge_lookup_hits_and_misses() {
        let p = TrustGraphProjection::from_json(&fixture_json()).unwrap();

        let hit = p.direct_edge("alpha", "beta", 1_700_000_500).unwrap();
        assert_eq!(hit.score, 750);
        assert_eq!(hit.from, "alpha");
        assert_eq!(hit.to, "beta");

        // Reverse direction is *not* automatically derived — trust is
        // directional.
        assert!(p.direct_edge("beta", "alpha", 1_700_000_500).is_none());

        // Unknown vertex.
        assert!(p.direct_edge("alpha", "gamma", 1_700_000_500).is_none());
    }

    #[test]
    fn self_attestation_is_dropped() {
        // Even if a tampered ConfigMap declares an edge alpha→alpha,
        // direct_edge returns None to prevent self-bootstrap.
        let raw = format!(
            r#"{{
              "vertices": [{{"id": "alpha", "alg": "EdDSA", "publicKeyB64u": "{}"}}],
              "edges": [{{"from": "alpha", "to": "alpha", "score": 1000,
                          "issuedAt": 1700000000, "signature": "{}"}}],
              "versionHash": "0000000000000000",
              "inputEdgeCount": 1
            }}"#,
            PK_A, SIG_AB
        );
        let p = TrustGraphProjection::from_json(&raw).unwrap();
        assert!(p.direct_edge("alpha", "alpha", 1_700_000_500).is_none());
    }

    #[test]
    fn expired_edge_is_dropped() {
        let raw = format!(
            r#"{{
              "vertices": [
                {{"id": "alpha", "alg": "EdDSA", "publicKeyB64u": "{}"}},
                {{"id": "beta",  "alg": "EdDSA", "publicKeyB64u": "{}"}}
              ],
              "edges": [
                {{"from": "alpha", "to": "beta", "score": 750,
                  "issuedAt": 1700000000, "notAfter": 1700000100,
                  "signature": "{}"}}
              ],
              "versionHash": "deadbeefcafef00d",
              "inputEdgeCount": 1
            }}"#,
            PK_A, PK_B, SIG_AB
        );
        let p = TrustGraphProjection::from_json(&raw).unwrap();
        // 50s after issuedAt — within window.
        assert!(p.direct_edge("alpha", "beta", 1_700_000_050).is_some());
        // 100s after issuedAt — exactly at notAfter, still valid.
        assert!(p.direct_edge("alpha", "beta", 1_700_000_100).is_some());
        // 101s after issuedAt — past notAfter, dropped.
        assert!(p.direct_edge("alpha", "beta", 1_700_000_101).is_none());
    }

    #[test]
    fn empty_object_is_empty_projection() {
        let p = TrustGraphProjection::from_json("{}").unwrap();
        assert!(p.is_empty());
        assert_eq!(p.vertex_count(), 0);
        assert_eq!(p.edge_count(), 0);
        assert_eq!(p.version_hash(), "");
    }

    #[test]
    fn malformed_json_returns_err() {
        let err = TrustGraphProjection::from_json("not json").unwrap_err();
        match err {
            ProjectionParseError::Json(_) => {}
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn oversize_payload_rejected_before_serde() {
        let huge = "x".repeat(MAX_PROJECTION_BYTES + 1);
        match TrustGraphProjection::from_json(&huge).unwrap_err() {
            ProjectionParseError::Oversize { actual, max } => {
                assert_eq!(actual, MAX_PROJECTION_BYTES + 1);
                assert_eq!(max, MAX_PROJECTION_BYTES);
            }
            other => panic!("expected Oversize, got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_field_is_ignored() {
        let raw = r#"{"vertices": [], "edges": [], "futureField": "ignored"}"#;
        let p = TrustGraphProjection::from_json(raw).unwrap();
        assert!(p.is_empty());
    }

    #[test]
    fn duplicate_edge_pair_last_write_wins() {
        let raw = format!(
            r#"{{
              "vertices": [
                {{"id": "alpha", "alg": "EdDSA", "publicKeyB64u": "{}"}},
                {{"id": "beta",  "alg": "EdDSA", "publicKeyB64u": "{}"}}
              ],
              "edges": [
                {{"from": "alpha", "to": "beta", "score": 100,
                  "issuedAt": 1700000000, "signature": "{}"}},
                {{"from": "alpha", "to": "beta", "score": 900,
                  "issuedAt": 1700000050, "signature": "{}"}}
              ],
              "versionHash": "1111111111111111",
              "inputEdgeCount": 2
            }}"#,
            PK_A, PK_B, SIG_AB, SIG_AB
        );
        let p = TrustGraphProjection::from_json(&raw).unwrap();
        // edge_count counts unique (from,to) pairs after de-dup.
        assert_eq!(p.edge_count(), 1);
        let e = p.direct_edge("alpha", "beta", 1_700_000_500).unwrap();
        // Last entry wins.
        assert_eq!(e.score, 900);
    }

    #[test]
    fn vertex_label_optional_field_round_trips() {
        let p = TrustGraphProjection::from_json(&fixture_json()).unwrap();
        let beta = p.vertices().find(|v| v.id == "beta").unwrap();
        assert_eq!(beta.label.as_deref(), Some("ops"));
        let alpha = p.vertices().find(|v| v.id == "alpha").unwrap();
        assert!(alpha.label.is_none());
    }

    #[test]
    fn missing_optional_not_after_means_no_expiry() {
        let p = TrustGraphProjection::from_json(&fixture_json()).unwrap();
        // Far-future timestamp — fixture has no notAfter, so the edge
        // is still resolvable.
        assert!(p.direct_edge("alpha", "beta", i64::MAX).is_some());
    }
}
