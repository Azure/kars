// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TrustGraph per-sandbox projection mount (Phase F2b).
//!
//! The cluster-scoped [`crate::trust_graph_reconciler`] publishes
//! **one** ConfigMap per `TrustGraph` CR into the
//! `kars-system` namespace, containing the entire signed trust
//! topology: every operator-attested edge across every agent identity.
//!
//! Mounting that cluster-wide blob into every sandbox would leak the
//! trust topology to all sandboxes (LLM06 — Sensitive Information
//! Disclosure). This module narrows the surface by:
//!
//! 1. Listing every TrustGraph projection ConfigMap in
//!    `kars-system` (label-selected to the controller's own
//!    artifact label so we don't accidentally consume an arbitrary
//!    operator-authored CM).
//! 2. Filtering each projection to **outbound edges only** — i.e.
//!    edges whose `from` matches the sandbox's identity. This is the
//!    only edge direction the F2a router-side bootstrap consults.
//! 3. Merging the filtered slices and rendering them as a fresh
//!    `<sandbox>-trustgraph-projection` ConfigMap in the sandbox's
//!    own namespace.
//! 4. Mounting that ConfigMap into the inference-router container at
//!    [`paths::TRUSTGRAPH_FILE`] and setting
//!    `TRUSTGRAPH_PROJECTION_PATH` to the same path.
//!
//! ## Identity discriminator
//!
//! For F2b the sandbox identity used in the `from` filter is the
//! sandbox **name**. This matches the F1 fixture convention and the
//! AGT TrustManager's `agent_id` value used by `update_trust`.
//! Operators who want to attribute trust to a longer DID (e.g.
//! `did:agentmesh:<sandbox>`) still get the bootstrap because the
//! projection edge `from` is taken verbatim from the operator's
//! signed `TrustGraph` CR — the controller treats it as opaque text.
//! If the operator signed an edge with `from = "alpha"` and the
//! sandbox is named `alpha`, the bootstrap fires. If the operator
//! signed it with the DID, the operator must also use the DID as the
//! sandbox identity. The two namespaces are kept consistent by the
//! existing AGT identity convention (`sandbox_name == agent_id`).

use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::api::core::v1::ConfigMap;
use kube::Client;
use kube::api::{Api, ListParams, ObjectMeta, Patch, PatchParams};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::governance_mounts;

/// Mount paths exposed inside the inference-router container.
pub mod paths {
    /// Volume mount directory.
    pub const TRUSTGRAPH_DIR: &str = "/etc/kars/trustgraph";
    /// File the router-side loader reads (matches F2a's
    /// `TRUSTGRAPH_PROJECTION_PATH` semantics).
    pub const TRUSTGRAPH_FILE: &str = "/etc/kars/trustgraph/graph.json";
    /// Volume name used inside the pod spec.
    pub const TRUSTGRAPH_VOLUME: &str = "trustgraph-projection";
    /// Env var name consumed by the router loader.
    pub const TRUSTGRAPH_ENV: &str = "TRUSTGRAPH_PROJECTION_PATH";
    /// Map key inside the published ConfigMap. Must match the
    /// router-side default expectation.
    pub const TRUSTGRAPH_DATA_KEY: &str = "graph.json";
}

/// Source-of-truth namespace for cluster-wide projections (matches
/// [`crate::trust_graph_reconciler::PROJECTION_NAMESPACE`]).
pub const SOURCE_NAMESPACE: &str = "kars-system";

/// Label used by the TrustGraph reconciler to mark its own ConfigMaps.
pub const ARTIFACT_LABEL: &str = "kars.azure.com/artifact";
pub const ARTIFACT_LABEL_VALUE: &str = "trustgraph-projection";

/// Wire shape mirrored from
/// `controller::trust_graph_compile::ProjectedGraph`. We need
/// `Deserialize` here (the compile module only derives `Serialize`),
/// hence the duplicate type. They share the same JSON shape — every
/// field has matching name + camelCase rename.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireProjection {
    #[serde(default)]
    vertices: Vec<WireVertex>,
    #[serde(default)]
    edges: Vec<WireEdge>,
    #[serde(default)]
    version_hash: String,
    #[serde(default)]
    input_edge_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireVertex {
    id: String,
    alg: String,
    public_key_b64u: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEdge {
    from: String,
    to: String,
    score: u32,
    issued_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_after: Option<i64>,
    signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Outcome of a single mount operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountOutcome {
    /// Per-sandbox projection ConfigMap was applied successfully and
    /// the pod spec was updated. Carries the version hash for audit.
    Mounted { version_hash: String, edges: usize },
    /// No source projections matched the controller-managed label —
    /// no per-sandbox ConfigMap was written; pod spec is left
    /// unchanged. Caller may emit a warning event.
    NoSource,
    /// Source projections existed but none contained an edge whose
    /// `from` matched this sandbox. An empty per-sandbox ConfigMap
    /// is published anyway so the volume mount is consistent across
    /// pod restarts (otherwise a deleted edge would leave a stale
    /// mount referring to a missing CM).
    EmptySlice,
}

/// Filter the cluster-wide projection list down to the slice that
/// matters for `sandbox_name`. Pure function — no I/O, fully unit
/// tested.
fn build_per_sandbox_slice(raw_documents: &[String], sandbox_name: &str) -> WireProjection {
    let mut included_edges: Vec<WireEdge> = Vec::new();
    let mut referenced_vertex_ids: BTreeSet<String> = BTreeSet::new();
    let mut all_vertices: BTreeMap<String, WireVertex> = BTreeMap::new();
    let mut version_hashes: Vec<String> = Vec::new();
    let mut total_input_edges: usize = 0;

    for raw in raw_documents {
        let parsed: WireProjection = match serde_json::from_str(raw) {
            Ok(p) => p,
            Err(_) => continue, // controller validates upstream; skip malformed.
        };
        for v in &parsed.vertices {
            all_vertices
                .entry(v.id.clone())
                .or_insert_with(|| v.clone());
        }
        for e in parsed.edges {
            // Drop self-edges defensively (controller already does, but
            // this filter is on the trust boundary).
            if e.from == e.to {
                continue;
            }
            // Outbound edges only — F2a's bootstrap path is
            // `direct_edge(sandbox_name, peer)`. Inbound edges are
            // operator-private trust intelligence and must NOT leak
            // into the sandbox.
            if e.from == sandbox_name {
                referenced_vertex_ids.insert(e.from.clone());
                referenced_vertex_ids.insert(e.to.clone());
                included_edges.push(e);
            }
        }
        if !parsed.version_hash.is_empty() {
            version_hashes.push(parsed.version_hash);
        }
        total_input_edges = total_input_edges.saturating_add(parsed.input_edge_count);
    }

    // Restrict the published vertex set to the ones referenced by the
    // included edges. This minimises the topology disclosure to "the
    // sandbox itself + its declared trustees."
    let mut vertices: Vec<WireVertex> = referenced_vertex_ids
        .into_iter()
        .filter_map(|id| all_vertices.remove(&id))
        .collect();
    // Stable order (BTreeSet already gives deterministic order).
    vertices.sort_by(|a, b| a.id.cmp(&b.id));

    // Compose a per-sandbox version hash by concatenating the source
    // hashes — change-detection token for the router-side mount.
    let mut composite = version_hashes.join(",");
    if composite.len() > 64 {
        composite.truncate(64);
    }

    let edge_count = included_edges.len();
    WireProjection {
        vertices,
        edges: included_edges,
        version_hash: composite,
        input_edge_count: total_input_edges.min(edge_count.max(1)).max(edge_count),
    }
}

/// Apply the per-sandbox projection ConfigMap and inject the volume
/// mount + env-var into `pod_spec`.
///
/// `pod_spec` is the JSON object that becomes the Deployment's
/// `template.spec` — the same handle the existing
/// [`governance_mounts::inject_configmap_mount`] calls operate on.
///
/// On any I/O error this function returns `Err(kube::Error)` and the
/// caller may decide whether to requeue. The pod spec is **left
/// unchanged on error** — the F2a router will simply load nothing
/// (env var unset) and behave identically to pre-F2.
pub async fn ensure_trustgraph_mount(
    client: &Client,
    sandbox_ns: &str,
    sandbox_name: &str,
    pod_spec: &mut Value,
) -> Result<MountOutcome, kube::Error> {
    let src_api: Api<ConfigMap> = Api::namespaced(client.clone(), SOURCE_NAMESPACE);
    let lp = ListParams::default().labels(&format!("{ARTIFACT_LABEL}={ARTIFACT_LABEL_VALUE}"));

    let cms = src_api.list(&lp).await?;
    let raw_docs: Vec<String> = cms
        .items
        .into_iter()
        .filter_map(|cm| {
            cm.data
                .and_then(|m| m.get(paths::TRUSTGRAPH_DATA_KEY).cloned())
        })
        .collect();

    if raw_docs.is_empty() {
        return Ok(MountOutcome::NoSource);
    }

    let slice = build_per_sandbox_slice(&raw_docs, sandbox_name);
    let edge_count = slice.edges.len();
    let version_hash = slice.version_hash.clone();
    let body = serde_json::to_string_pretty(&slice).unwrap_or_else(|_| "{}".to_string());

    let cm_name = format!("{sandbox_name}-trustgraph-projection");
    let dst_api: Api<ConfigMap> = Api::namespaced(client.clone(), sandbox_ns);

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/managed-by".into(),
        "kars-controller".into(),
    );
    labels.insert("kars.azure.com/sandbox".into(), sandbox_name.into());
    labels.insert(ARTIFACT_LABEL.into(), "trustgraph-per-sandbox".into());

    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "kars.azure.com/trustgraph-version-hash".into(),
        version_hash.clone(),
    );
    annotations.insert(
        "kars.azure.com/trustgraph-edge-count".into(),
        edge_count.to_string(),
    );

    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.clone()),
            namespace: Some(sandbox_ns.into()),
            labels: Some(labels),
            annotations: Some(annotations),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            paths::TRUSTGRAPH_DATA_KEY.to_string(),
            body,
        )])),
        ..Default::default()
    };
    let pp = PatchParams::apply(crate::field_managers::TRUSTGRAPH_MOUNT).force();
    dst_api.patch(&cm_name, &pp, &Patch::Apply(&cm)).await?;

    governance_mounts::inject_configmap_mount(
        pod_spec,
        "inference-router",
        &cm_name,
        paths::TRUSTGRAPH_VOLUME,
        paths::TRUSTGRAPH_DIR,
        Some((paths::TRUSTGRAPH_ENV, paths::TRUSTGRAPH_FILE)),
    );

    if edge_count == 0 {
        Ok(MountOutcome::EmptySlice)
    } else {
        Ok(MountOutcome::Mounted {
            version_hash,
            edges: edge_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc_with(from: &str, to: &str, score: u32) -> String {
        // Signature payloads are only used when a verifier is present;
        // the build_per_sandbox_slice filter doesn't verify, so any
        // valid-looking 64-byte b64url placeholder works for tests.
        let sig = "A".repeat(86);
        json!({
            "vertices": [
                {"id": from, "alg": "EdDSA", "publicKeyB64u": "x".repeat(43)},
                {"id": to,   "alg": "EdDSA", "publicKeyB64u": "y".repeat(43)},
            ],
            "edges": [{
                "from": from, "to": to, "score": score,
                "issuedAt": 1700000000, "signature": sig
            }],
            "versionHash": "abc1230000000000",
            "inputEdgeCount": 1,
        })
        .to_string()
    }

    #[test]
    fn slice_includes_only_outbound_edges() {
        let docs = vec![
            doc_with("alpha", "beta", 700),
            doc_with("alpha", "gamma", 800),
            doc_with("beta", "alpha", 900), // inbound — must be excluded
            doc_with("gamma", "delta", 1000), // unrelated — must be excluded
        ];
        let slice = build_per_sandbox_slice(&docs, "alpha");
        assert_eq!(slice.edges.len(), 2);
        for e in &slice.edges {
            assert_eq!(e.from, "alpha");
        }
        // Vertex set: alpha + the two outbound peers.
        let ids: BTreeSet<_> = slice.vertices.iter().map(|v| v.id.clone()).collect();
        assert!(ids.contains("alpha"));
        assert!(ids.contains("beta"));
        assert!(ids.contains("gamma"));
        assert!(!ids.contains("delta"));
    }

    #[test]
    fn slice_drops_self_edges() {
        let docs = vec![doc_with("alpha", "alpha", 1000)];
        let slice = build_per_sandbox_slice(&docs, "alpha");
        assert!(slice.edges.is_empty());
    }

    #[test]
    fn slice_empty_when_no_outbound_edges() {
        let docs = vec![doc_with("beta", "alpha", 500)];
        let slice = build_per_sandbox_slice(&docs, "alpha");
        assert!(slice.edges.is_empty());
        assert!(slice.vertices.is_empty());
    }

    #[test]
    fn slice_skips_malformed_documents() {
        let docs = vec!["{ not json".to_string(), doc_with("alpha", "beta", 700)];
        let slice = build_per_sandbox_slice(&docs, "alpha");
        assert_eq!(slice.edges.len(), 1);
    }

    #[test]
    fn slice_merges_multiple_source_documents() {
        let docs = vec![
            doc_with("alpha", "beta", 700),
            doc_with("alpha", "gamma", 800),
        ];
        let slice = build_per_sandbox_slice(&docs, "alpha");
        assert_eq!(slice.edges.len(), 2);
        assert!(!slice.version_hash.is_empty());
    }

    #[test]
    fn slice_version_hash_truncates_at_64_chars() {
        // Many source docs → composite hash must be capped.
        let docs: Vec<String> = (0..20)
            .map(|i| {
                let to = format!("peer{i}");
                doc_with("alpha", &to, 700)
            })
            .collect();
        let slice = build_per_sandbox_slice(&docs, "alpha");
        assert!(slice.version_hash.len() <= 64);
    }

    #[test]
    fn paths_match_router_loader_expectations() {
        // Pin: F2a's TrustGraphProjection loader reads
        // TRUSTGRAPH_PROJECTION_PATH; F2b sets the env var to the
        // same path it mounts. Drift here = silent breakage.
        assert_eq!(paths::TRUSTGRAPH_FILE, "/etc/kars/trustgraph/graph.json");
        assert_eq!(paths::TRUSTGRAPH_ENV, "TRUSTGRAPH_PROJECTION_PATH");
        assert_eq!(paths::TRUSTGRAPH_DATA_KEY, "graph.json");
    }

    #[test]
    fn slice_serialises_back_to_router_compatible_json() {
        // Round trip — what we'd write into the per-sandbox CM must
        // be parseable by the F2a router parser.
        let docs = vec![doc_with("alpha", "beta", 700)];
        let slice = build_per_sandbox_slice(&docs, "alpha");
        let body = serde_json::to_string(&slice).unwrap();
        // Re-parse with serde Value to confirm the camelCase keys.
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("vertices").is_some());
        assert!(v.get("edges").is_some());
        assert!(v.get("versionHash").is_some());
        assert!(v.get("inputEdgeCount").is_some());
        // Edge fields: camelCase issuedAt, no snake-case slip.
        let edge = &v["edges"][0];
        assert!(edge.get("issuedAt").is_some());
        assert!(edge.get("issued_at").is_none());
    }
}
