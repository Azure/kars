//! `ClawMemory` CRD — Phase 2 §8 entry 5 (S5).
//!
//! Per `docs/implementation-plan.md` §3 non-compete table, `ClawMemory`
//! is **a binding/provisioning resource over Azure AI Foundry Memory
//! Store** — it *configures* FMS for a sandbox; it is **not** a
//! separate in-cluster memory backend. No in-cluster store is shipped.
//!
//! ## Scope (S5)
//!
//! This slice ships:
//!
//! 1. The CRD schema (`ClawMemory.spec`).
//! 2. The reconciler that compiles the spec to a binding JSON and
//!    publishes it as a `ConfigMap` (`clawmemory-{name}-binding`).
//! 3. CEL admission rules for shape invariants.
//! 4. Helm CRD with drift-checked schema.
//!
//! What this slice **does NOT** do (and why):
//!
//! - **Does NOT call Foundry directly from the controller.** Foundry
//!   Memory Store creation today happens via the existing CLI path
//!   (`cli/src/plugin.ts::ensureMemoryStore`), which calls Foundry
//!   through the router (which holds the Workload Identity). The
//!   controller has no Foundry credential and we explicitly do not
//!   want to give it one. A future slice (S7+) wires a sandbox-side
//!   informer that reads the binding ConfigMap and triggers the
//!   existing `ensureMemoryStore` flow on first use.
//! - **Does NOT enforce retention** at controller level. Retention is
//!   declared in spec; runtime enforcement is Foundry-side (via the
//!   `delete_scope` API on TTL or operator request) — see §10.4 #11
//!   hot-reload + S7 wiring.
//! - **Does NOT auto-delete the Foundry store on CR delete** in this
//!   slice. The finalizer cleans up the binding ConfigMap; the
//!   `deleteOnSandboxDelete` knob is preserved in the compiled binding
//!   so the runtime path can act on it. Foundry-side delete from the
//!   controller requires a router-mediated path (S7+).
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! - **CRD-derive shape** (group, version, kind, namespaced, status,
//!   shortname, printcolumns): mirrors S2/S3/S4.
//! - **`LocalObjectRef` for status ConfigMap ref**: re-used from
//!   [`crate::mcp_server`] — 5th semantic client now.
//! - **`Condition` vocabulary**: re-used from
//!   [`crate::status::conditions`] via the reconciler module.
//! - **Memory Store API surface**: existing
//!   `inference-router/src/routes/inference.rs` proxy
//!   (`/memory_stores/*`) + `cli/src/core/foundry-discovery.ts`
//!   `ensureMemoryStore` flow. **Not modified in this slice.**
//!
//! ## Memory Store auth caveat (verified in repo memory)
//!
//! Memory Store operations that internally call models (update,
//! search-with-items) require the **project's** managed identity to
//! have `Azure AI User` on the **resource group**, with token audience
//! `https://ai.azure.com/`. CRD-level admission cannot validate this;
//! the CR sets the binding shape, and runtime auth caveats remain a
//! deployment-time concern.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp_server::LocalObjectRef;

/// `ClawMemory.spec` — declares a Foundry Memory Store binding for a
/// sandbox.
///
/// Resolution rule: a sandbox can have at most one `ClawMemory` per
/// scope key; multiple `ClawMemory` CRs targeting the same sandbox
/// must declare distinct `scope` values. Conflict detection is
/// router-side (S7) — the CRD admission only validates shape.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "ClawMemory",
    namespaced,
    status = "ClawMemoryStatus",
    shortname = "cmem",
    printcolumn = r#"{"name":"Sandbox","type":"string","jsonPath":".spec.sandboxRef.name"}"#,
    printcolumn = r#"{"name":"Store","type":"string","jsonPath":".spec.storeName"}"#,
    printcolumn = r#"{"name":"Scope","type":"string","jsonPath":".spec.scope"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClawMemorySpec {
    /// Foundry Memory Store name. The runtime path
    /// (`cli/src/plugin.ts::ensureMemoryStore`) creates the store
    /// on first use if absent. CEL: non-empty,
    /// DNS-label-style (lowercase alphanumeric + dashes).
    pub store_name: String,

    /// Sandbox this binding applies to.
    pub sandbox_ref: SandboxRef,

    /// Scope key under which this sandbox writes/reads memories.
    /// Foundry Memory Store partitions data per scope. CEL:
    /// non-empty. Default convention (set by the runtime path if
    /// absent here): `agent:{sandboxName}`.
    pub scope: String,

    /// Optional retention floor in days. Runtime path may apply a
    /// `delete_scope` sweep when entries exceed this age. CEL:
    /// `> 0` when set.
    pub retention_days: Option<u32>,

    /// If true (default), the runtime path must call `delete_scope`
    /// on this scope when the sandbox is deleted or this CR is
    /// deleted. The controller-side cleanup is finalizer-only on the
    /// binding ConfigMap; Foundry-side delete via the router is
    /// wired in S7+.
    #[serde(default = "default_true")]
    pub delete_on_sandbox_delete: bool,

    /// Optional human-readable label.
    pub display_name: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Reference to a sandbox by name (within the same namespace as the
/// `ClawMemory` CR).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxRef {
    /// Sandbox name (`ClawSandbox.metadata.name`).
    pub name: String,
}

/// Status of a `ClawMemory` reconcile.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClawMemoryStatus {
    #[serde(default)]
    pub phase: Option<String>,

    /// `metadata.generation` last successfully reconciled. KEP-1623.
    #[serde(default)]
    pub observed_generation: Option<i64>,

    #[serde(default)]
    pub conditions: Option<Vec<Condition>>,

    /// Pointer to the binding `ConfigMap` produced by the reconciler.
    /// The router-side informer (S7) watches by label selector; this
    /// status field is for human / CLI consumption.
    #[serde(default)]
    pub binding_config_map_ref: Option<LocalObjectRef>,

    /// Hex-encoded sha256 prefix of the compiled binding JSON.
    #[serde(default)]
    pub version_hash: Option<String>,

    /// Last time the binding was compiled and pushed.
    #[serde(default)]
    pub last_reconciled_at: Option<String>,
}
