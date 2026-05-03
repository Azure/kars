// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `TrustGraph` CRD — Phase F of the §14.6 roadmap.
//!
//! Cluster-scoped CRD that promotes per-sandbox AGT trust scores to a
//! cluster-wide signed graph: vertices are agent identities (AMID-like
//! strings carrying an Ed25519 public key), edges are signed trust
//! attestations of the form "vertex `from` asserts trust `score` in
//! vertex `to`, valid until `notAfter`".
//!
//! ## Scope guardrails (per plan.md Phase F)
//!
//! - **No external attestation issuers.** All edge signatures are
//!   verified locally against vertices declared in the same `TrustGraph`
//!   CR. The controller does not call out to Sigstore Rekor, OIDC IdPs,
//!   or any network endpoint.
//! - **No transparency log publish.** Phase F is in-cluster only;
//!   public-attestation transparency is out of scope per the user's
//!   "no public posting" rule.
//! - **Reconciler validates, does not derive.** The CR is the source of
//!   truth; the reconciler verifies signatures and vertex<->edge
//!   reachability and emits a projection ConfigMap. Transitive closure
//!   is computed router-side at query time (Phase F2).
//!
//! ## Where the projection lives
//!
//! For each `TrustGraph/<name>` the reconciler writes a single
//! `ConfigMap` named `trustgraph-<name>-projection` in the
//! `azureclaw-system` namespace, keyed by `graph.json`. The router
//! mounts this ConfigMap (Phase F2) and answers
//! `trust_score(from, to)` queries against the validated graph.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `TrustGraph.spec` — the cluster-wide trust topology.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "TrustGraph",
    status = "TrustGraphStatus",
    shortname = "tg",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Vertices","type":"integer","jsonPath":".status.validVertices"}"#,
    printcolumn = r#"{"name":"ValidEdges","type":"integer","jsonPath":".status.validEdges"}"#,
    printcolumn = r#"{"name":"InvalidEdges","type":"integer","jsonPath":".status.invalidEdges"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct TrustGraphSpec {
    /// Vertex set. Each vertex declares an identity and the Ed25519
    /// public key peers must use to verify outbound edge signatures
    /// from this vertex.
    ///
    /// `id` must be unique across the spec — admission CEL enforces.
    /// At least one vertex is required (an empty graph would yield a
    /// useless projection); enforced by CEL `size(self.vertices) > 0`.
    pub vertices: Vec<TrustVertex>,

    /// Edge set. Each edge is a signed attestation from `from` to
    /// `to` carrying a trust score in [0, 1000] (the AGT trust-score
    /// domain — see `crd.rs::GovernanceConfig`).
    ///
    /// Edges where signature verification fails are reported in
    /// `status.invalidEdges` and omitted from the projection
    /// ConfigMap. The reconciler does not delete invalid edges from
    /// the spec — the operator owns the spec; the reconciler only
    /// publishes the verified subset.
    #[serde(default)]
    pub edges: Vec<TrustEdge>,
}

/// One vertex — an agent identity with its Ed25519 public key.
///
/// Field naming mirrors `A2AAgent.signingKeys` for cross-CRD
/// consistency: the same key material can be authored once in
/// `A2AAgent` and re-declared as a `TrustGraph` vertex without any
/// re-encoding step.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrustVertex {
    /// Stable identity (DNS-label-shaped string). Conventionally an
    /// AGT AMID or a fully-qualified `a2a-agent/<ns>/<name>` form.
    /// Must be unique within the CR (CEL).
    pub id: String,

    /// Algorithm pin. Currently only `"EdDSA"` is honoured; other
    /// values cause the reconciler to mark the vertex invalid and
    /// drop edges originating from it.
    pub alg: String,

    /// Ed25519 public key, base64url-encoded with NO padding (RFC
    /// 7515 §2). 32 bytes after decode.
    pub public_key_b64u: String,

    /// Optional human-readable label rendered in audit logs and the
    /// projection ConfigMap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One edge — a signed trust attestation between two vertices.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrustEdge {
    /// Source vertex `id`. Must resolve to a vertex in
    /// `spec.vertices` whose public key signed this edge.
    pub from: String,

    /// Target vertex `id`. Must resolve to a vertex in
    /// `spec.vertices`.
    pub to: String,

    /// Trust score in `[0, 1000]` — the AGT domain. CEL guards the
    /// range at admission; the reconciler additionally clamps for
    /// defensive depth.
    pub score: u32,

    /// Issuance timestamp, Unix seconds. Part of the canonical signed
    /// payload — peers verify both the signature and that
    /// `issuedAt <= now`.
    pub issued_at: i64,

    /// Optional expiry timestamp, Unix seconds. `None` means "never
    /// expires" (operator-asserted; the reconciler still flags
    /// suspiciously old `issuedAt`s in conditions but does not
    /// invalidate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,

    /// Ed25519 signature, base64url-encoded with NO padding, over the
    /// canonical bytes
    /// `b"trustgraph.v1\n" || from || b"\n" || to || b"\n" || score-as-decimal-ascii || b"\n" || issued_at-as-decimal-ascii || b"\n" || not_after-as-decimal-ascii-or-empty || b"\n"`.
    /// 64 bytes after decode.
    ///
    /// The version prefix `trustgraph.v1\n` is a domain-separator so
    /// a signature over a `TrustGraph` edge can never be replayed as
    /// an `A2AAgent` AgentCard signature.
    pub signature: String,

    /// Optional free-text reason ("auditor-attested",
    /// "transitive-1-hop", …) carried into the projection so
    /// downstream policy can pivot on the origin of the trust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `TrustGraph.status` — populated by the reconciler.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrustGraphStatus {
    /// `Pending` | `Ready` | `Degraded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    /// Standard K8s conditions (`Ready`, `Validated`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Vec<Condition>>,

    /// Number of vertices that decoded successfully (correct alg +
    /// 32-byte public key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_vertices: Option<i64>,

    /// Number of edges whose signature verified against the declared
    /// `from` vertex's public key. Only valid edges are emitted into
    /// the projection ConfigMap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_edges: Option<i64>,

    /// Number of edges that failed verification (bad signature,
    /// missing vertex, decode error, expired). Counted but not named
    /// to avoid leaking authoring detail into status; operators
    /// inspect controller logs (`error_class`) for which.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_edges: Option<i64>,

    /// `metadata.namespace/metadata.name` of the projection ConfigMap.
    /// Always `azureclaw-system/trustgraph-<name>-projection` once
    /// the reconciler has written successfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_config_map_ref: Option<crate::mcp_server::LocalObjectRef>,

    /// Generation observed at the last successful reconcile. Used by
    /// kube watcher consumers to detect stale projections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// RFC3339 timestamp of the last reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reconciled_at: Option<String>,
}
