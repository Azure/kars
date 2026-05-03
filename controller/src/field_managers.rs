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
//! Naming convention: `azureclaw-<binary>/<subsystem>` — `azureclaw-controller`
//! is the binary, the path-segment after `/` selects which reconciler /
//! sub-reconciler owns the field. Mesh-peer is the only exception (legacy
//! pre-S7 string `azureclaw-mesh-peer`) — kept verbatim to avoid an SSA
//! ownership transition on existing clusters; the constant just stops the
//! string from being inlined at multiple sites.
//!
//! Adding a new constant: add the `pub const` here AND add it to the
//! `ALL_FIELD_MANAGERS` slice below so the uniqueness test catches accidental
//! collisions.

/// `ClawSandbox` reconciler — patches Namespace, ServiceAccount,
/// Deployment, Service, NetworkPolicy, ConfigMap for each sandbox.
pub const CLAWSANDBOX: &str = "azureclaw-controller/clawsandbox";

/// `ClawPairing` reconciler + `pairing` finalizer — patches the
/// `ClawPairing.status` subresource (offload-slot lifecycle).
pub const PAIRING: &str = "azureclaw-controller/pairing";

/// `MeshPeer` reconciler — handles offload, pairing, and registry sync.
/// Note: legacy string format (`azureclaw-mesh-peer`, no slash) preserved
/// so existing clusters don't transfer field ownership on upgrade.
pub const MESH_PEER: &str = "azureclaw-mesh-peer";

/// `McpServer` reconciler — JWKS Secret, ConfigMap, status conditions.
pub const MCP_SERVER: &str = "azureclaw-controller/mcp";

/// `ToolPolicy` reconciler — compiled AGT policy profile, hot-reload bundle.
pub const TOOL_POLICY: &str = "azureclaw-controller/toolpolicy";

/// `A2AAgent` reconciler — agent-card signing key Secret, compiled bundle.
pub const A2A_AGENT: &str = "azureclaw-controller/a2aagent";

/// `InferencePolicy` reconciler — compiled JSON budget/guardrail bundle.
pub const INFERENCE_POLICY: &str = "azureclaw-controller/inferencepolicy";

/// `ClawMemory` reconciler — Foundry Memory Store binding spec.
pub const CLAW_MEMORY: &str = "azureclaw-controller/clawmemory";

/// `ClawEval` reconciler — eval bundle ConfigMap + Job emission.
pub const CLAW_EVAL: &str = "azureclaw-controller/claweval";

/// `TrustGraph` reconciler (Phase F1) — verifies signed trust edges
/// and publishes a `ConfigMap` projection to `azureclaw-system`.
pub const TRUST_GRAPH: &str = "azureclaw-controller/trustgraph";

/// Reserved for the in-pod inference router so its writes don't collide
/// with the controller's. Not used by the controller itself; exposed as a
/// constant so the uniqueness invariant is enforceable across crates.
#[allow(dead_code)]
pub const ROUTER_RECONCILER: &str = "azureclaw-router";

/// Provider-bridge subsystem — `ProviderKind` selection writeback.
pub const PROVIDER_BRIDGE: &str = "azureclaw-controller/provider-bridge";

/// Mesh-provider subsystem — separate from `MESH_PEER` (which is the
/// reconciler).
pub const MESH: &str = "azureclaw-controller/mesh";

/// Reconciler-base manager (legacy/generic). Prefer the per-CRD constants
/// for forensic clarity; `RECONCILER` remains for backwards-compat with
/// the pre-S7 `providers::field_managers` callers.
pub const RECONCILER: &str = "azureclaw-controller/reconciler";

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
    ROUTER_RECONCILER,
    PROVIDER_BRIDGE,
    MESH,
    RECONCILER,
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
        // `azureclaw-controller/`, `azureclaw-router`, or be the explicit
        // mesh-peer legacy string. New additions should follow the namespaced
        // form; this test fires if a future commit adds a bare string.
        for fm in ALL_FIELD_MANAGERS {
            let ok = fm.starts_with("azureclaw-controller/")
                || fm.starts_with("azureclaw-router")
                || *fm == MESH_PEER;
            assert!(
                ok,
                "field manager {fm:?} does not follow the \
                 azureclaw-{{controller|router}}/<subsystem> convention",
            );
        }
    }

    #[test]
    fn no_bare_azureclaw_controller_string() {
        // Catch a regression where a site uses the bare string
        // "azureclaw-controller" — that's the binary identity, not a
        // subsystem manager, and reusing it across reconcilers makes
        // SSA-conflict diagnosis impossible.
        assert!(
            !ALL_FIELD_MANAGERS.contains(&"azureclaw-controller"),
            "bare 'azureclaw-controller' is reserved; use a subsystem \
             constant (CLAWSANDBOX / PAIRING / etc.)",
        );
    }

    #[test]
    fn legacy_provider_constants_match() {
        // Backwards-compat: `providers::field_managers` previously owned
        // `RECONCILER`, `MESH`, `PAIRING`, `PROVIDER_BRIDGE`. Re-exporting
        // unchanged so existing callers (and the controller's own ssa
        // helpers) keep working.
        assert_eq!(RECONCILER, "azureclaw-controller/reconciler");
        assert_eq!(MESH, "azureclaw-controller/mesh");
        assert_eq!(PAIRING, "azureclaw-controller/pairing");
        assert_eq!(PROVIDER_BRIDGE, "azureclaw-controller/provider-bridge");
    }
}
