// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Stable Server-Side Apply field managers for every controller-emitted patch.
//!
//! Per `docs/implementation-plan.md` §10.4 #1 and the S7 Phase 2 slice
//! (`phase2-conditions-ssa-leader`): every SSA write must carry a stable
//! `fieldManager` so the API server's field-ownership tracking is
//! deterministic across controller restarts, version bumps, and multi-replica
//! HA. A second SSA helper or duplicated string literal is a §0.2 #8 hard-rule
//! violation — central registry here, single source of truth for the whole
//! controller crate.
//!
//! Naming convention: `kars-<binary>/<subsystem>` — `kars-controller`
//! is the binary, the path-segment after `/` selects which reconciler /
//! sub-reconciler owns the field. Mesh-peer is the only exception (legacy
//! pre-S7 string `kars-mesh-peer`) — kept verbatim to avoid an SSA
//! ownership transition on existing clusters; the constant just stops the
//! string from being inlined at multiple sites.
//!
//! Adding a new constant: add the `pub const` here AND add it to the
//! `ALL_FIELD_MANAGERS` slice below so the uniqueness test catches accidental
//! collisions.

/// `KarsSandbox` reconciler — patches Namespace, ServiceAccount,
/// Deployment, Service, NetworkPolicy, ConfigMap for each sandbox.
pub const CLAWSANDBOX: &str = "kars-controller/karssandbox";

/// `KarsPairing` reconciler + `pairing` finalizer — patches the
/// `KarsPairing.status` subresource (offload-slot lifecycle).
pub const PAIRING: &str = "kars-controller/pairing";

/// `MeshPeer` reconciler — handles offload, pairing, and registry sync.
/// Note: legacy string format (`kars-mesh-peer`, no slash) preserved
/// so existing clusters don't transfer field ownership on upgrade.
pub const MESH_PEER: &str = "kars-mesh-peer";

/// `McpServer` reconciler — JWKS Secret, ConfigMap, status conditions.
pub const MCP_SERVER: &str = "kars-controller/mcp";

/// `ToolPolicy` reconciler — compiled AGT policy profile, hot-reload bundle.
pub const TOOL_POLICY: &str = "kars-controller/toolpolicy";

/// `A2AAgent` reconciler — agent-card signing key Secret, compiled bundle.
pub const A2A_AGENT: &str = "kars-controller/a2aagent";

/// `InferencePolicy` reconciler — compiled JSON budget/guardrail bundle.
pub const INFERENCE_POLICY: &str = "kars-controller/inferencepolicy";

/// `KarsMemory` reconciler — Foundry Memory Store binding spec.
pub const CLAW_MEMORY: &str = "kars-controller/karsmemory";

/// `KarsEval` reconciler — eval bundle ConfigMap + Job emission.
pub const CLAW_EVAL: &str = "kars-controller/karseval";

/// `TrustGraph` reconciler (Phase F1) — verifies signed trust edges
/// and publishes a `ConfigMap` projection to `kars-system`.
pub const TRUST_GRAPH: &str = "kars-controller/trustgraph";

/// Per-sandbox TrustGraph projection mount (Phase F2b) — applies the
/// filtered slice ConfigMap into each sandbox namespace. Distinct
/// from `TRUST_GRAPH` so SSA ownership of cluster-wide projections
/// vs per-sandbox slices stays separable.
pub const TRUSTGRAPH_MOUNT: &str = "kars-controller/trustgraph-mount";

/// Reserved for the in-pod inference router so its writes don't collide
/// with the controller's. Not used by the controller itself; exposed as a
/// constant so the uniqueness invariant is enforceable across crates.
#[allow(dead_code)]
pub const ROUTER_RECONCILER: &str = "kars-router";

/// Provider-bridge subsystem — `ProviderKind` selection writeback.
pub const PROVIDER_BRIDGE: &str = "kars-controller/provider-bridge";

/// Mesh-provider subsystem — separate from `MESH_PEER` (which is the
/// reconciler).
pub const MESH: &str = "kars-controller/mesh";

/// Reconciler-base manager (legacy/generic). Prefer the per-CRD constants
/// for forensic clarity; `RECONCILER` remains for backwards-compat with
/// the pre-S7 `providers::field_managers` callers.
pub const RECONCILER: &str = "kars-controller/reconciler";

/// `EgressApproval` reconciler — Slice 5e.2. Owns
/// `status.{phase,conditions,expiresAt,mergedHostCount,observedGeneration}`
/// and the `kars.azure.com/egress-approval-cleanup` finalizer.
/// Distinct from the policy-lane reconcilers because the grant lane
/// has a fundamentally different lifecycle (short-TTL, auto-expires).
pub const EGRESS_APPROVAL: &str = "kars-controller/egressapproval";

/// All managers the controller emits. The §0.2 #8 "no duplication"
/// invariant is asserted in the test below: every entry here must be a
/// distinct string. Adding a constant requires extending this slice.
#[allow(dead_code)]
pub const ALL_FIELD_MANAGERS: &[&str] = &[
    CLAWSANDBOX,
    PAIRING,
    MESH_PEER,
    MCP_SERVER,
    TOOL_POLICY,
    A2A_AGENT,
    INFERENCE_POLICY,
    CLAW_MEMORY,
    CLAW_EVAL,
    TRUST_GRAPH,
    TRUSTGRAPH_MOUNT,
    ROUTER_RECONCILER,
    PROVIDER_BRIDGE,
    MESH,
    RECONCILER,
    EGRESS_APPROVAL,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_field_managers_are_unique() {
        // §0.2 #8: every SSA field manager must be unique across the
        // controller. Two sites sharing the same manager defeats SSA
        // ownership tracking for the colliding subresources.
        let set: HashSet<&&str> = ALL_FIELD_MANAGERS.iter().collect();
        assert_eq!(
            set.len(),
            ALL_FIELD_MANAGERS.len(),
            "duplicate field manager: each constant in field_managers must \
             be a distinct string. Audit additions to ALL_FIELD_MANAGERS \
             when this fires."
        );
    }

    #[test]
    fn field_managers_use_namespaced_format() {
        // Convention check — every manager should either start with
        // `kars-controller/`, `kars-router`, or be the explicit
        // mesh-peer legacy string. New additions should follow the namespaced
        // form; this test fires if a future commit adds a bare string.
        for fm in ALL_FIELD_MANAGERS {
            let ok = fm.starts_with("kars-controller/")
                || fm.starts_with("kars-router")
                || *fm == MESH_PEER;
            assert!(
                ok,
                "field manager {fm:?} does not follow the \
                 kars-{{controller|router}}/<subsystem> convention",
            );
        }
    }

    #[test]
    fn no_bare_kars_controller_string() {
        // Catch a regression where a site uses the bare string
        // "kars-controller" — that's the binary identity, not a
        // subsystem manager, and reusing it across reconcilers makes
        // SSA-conflict diagnosis impossible.
        assert!(
            !ALL_FIELD_MANAGERS.contains(&"kars-controller"),
            "bare 'kars-controller' is reserved; use a subsystem \
             constant (CLAWSANDBOX / PAIRING / etc.)",
        );
    }

    #[test]
    fn legacy_provider_constants_match() {
        // Backwards-compat: `providers::field_managers` previously owned
        // `RECONCILER`, `MESH`, `PAIRING`, `PROVIDER_BRIDGE`. Re-exporting
        // unchanged so existing callers (and the controller's own ssa
        // helpers) keep working.
        assert_eq!(RECONCILER, "kars-controller/reconciler");
        assert_eq!(MESH, "kars-controller/mesh");
        assert_eq!(PAIRING, "kars-controller/pairing");
        assert_eq!(PROVIDER_BRIDGE, "kars-controller/provider-bridge");
    }
}
