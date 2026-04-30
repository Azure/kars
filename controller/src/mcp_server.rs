// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `McpServer` CRD — full reconciler (Phase 2 §8 entry 1).
//!
//! Status: **full** as of `phase2/mcp-reconciler` (S1 of Phase 2).
//! The reconciler in `controller/src/mcp_server_reconciler.rs` emits an
//! Ed25519 signing-key Secret and (when `productionMode: true`) caches
//! the issuer's JWKS into a ConfigMap that the inference-router mounts
//! to gate `/mcp` with OAuth 2.1.
//!
//! Spec: <https://modelcontextprotocol.io/specification/2026-01-15>
//! OAuth 2.1: <https://www.rfc-editor.org/rfc/rfc9700>
//!
//! ## Why a scaffold lands first
//!
//! - Decouples CRD/admission landing (low risk, schema-only) from the
//!   router data-plane work (higher risk, touches `inference.rs`).
//! - Lets `ToolPolicy` and `A2AAgent` reference a stable `McpServer`
//!   shape sooner.
//! - Conformance corpus entries (§5.4 row "MCP 2026 Streamable HTTP")
//!   can be authored against the schema before routes exist.
//!
//! All field names track the MCP 2026-01-15 spec exactly so future
//! schema migrations are mechanical.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `McpServer.spec` — declares an MCP 2026 server reachable from sandboxes
/// in the same namespace (or, if `crossNamespaceAllowed: true` on the
/// server side, cluster-wide).
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "McpServer",
    namespaced,
    status = "McpServerStatus",
    shortname = "mcp",
    printcolumn = r#"{"name":"URL","type":"string","jsonPath":".spec.url"}"#,
    printcolumn = r#"{"name":"Production","type":"boolean","jsonPath":".spec.productionMode"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSpec {
    /// Server endpoint URL. MUST be `https://` when `productionMode: true`.
    /// Validated by admission CEL (Phase 1 requirement, §7 entry 12).
    pub url: String,

    /// OAuth 2.1 configuration. Required when `productionMode: true`.
    pub oauth: Option<McpOAuthConfig>,

    /// When true, the router rejects calls that are not bearer-token-
    /// authenticated against `oauth.issuer` with a verified PKCE flow.
    /// `false` allows unauthenticated calls — dev-only; admission policy
    /// rejects this for non-dev tenants (mirrors `Null*` provider rule).
    #[serde(default)]
    pub production_mode: bool,

    /// OAuth 2.1 scopes that the router will request when fronting calls
    /// from sandboxes to this server. The actual per-tool gating is
    /// expressed in `ToolPolicy` resources, not here.
    #[serde(default)]
    pub scopes: Vec<String>,

    /// Allow-list of tool names. Empty list means "no tools allowed";
    /// to allow all tools, use `["*"]` and lean on `ToolPolicy` for
    /// per-tool governance. Default empty — fail-closed.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Selector restricting which sandboxes can reach this server.
    /// Empty = same-namespace only.
    pub allowed_sandboxes: Option<SandboxSelector>,

    /// Optional human-readable label for operator-TUI display.
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuthConfig {
    /// OAuth 2.1 issuer URL (must serve a discovery document at
    /// `<issuer>/.well-known/oauth-authorization-server` or
    /// `/.well-known/openid-configuration`). Validated lazily on
    /// reconcile; failures land in `Conditions`.
    pub issuer: String,

    /// Audience claim required on bearer tokens calling this server.
    pub audience: Option<String>,

    /// Required `resource` indicator value for token exchange.
    pub resource: Option<String>,

    /// PKCE method. Always S256 in this scaffold; field exists for
    /// future protocol negotiation.
    #[serde(default = "default_pkce")]
    pub pkce: String,
}

fn default_pkce() -> String {
    "S256".to_string()
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSelector {
    /// Match labels on `ClawSandbox` resources. Standard label selector
    /// semantics; AND across keys.
    #[serde(default)]
    pub match_labels: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    /// Lifecycle phase: `Pending` | `Ready` | `Degraded` | `Unknown`.
    #[serde(default)]
    pub phase: Option<String>,

    /// `metadata.generation` last successfully reconciled. KEP-1623.
    #[serde(default)]
    pub observed_generation: Option<i64>,

    /// Standard K8s Conditions, KEP-1623 shape.
    #[serde(default)]
    pub conditions: Option<Vec<Condition>>,

    /// Last health-check timestamp (RFC 3339).
    #[serde(default)]
    pub last_probed_at: Option<String>,

    /// Reference to the Secret holding the Ed25519 signing keypair this
    /// reconciler emits. The Secret has type
    /// `azureclaw.azure.com/mcp-signing-key` with two keys:
    /// `signing-key.private` (raw 32-byte Ed25519 seed) and
    /// `signing-key.public` (raw 32-byte verifying key). Field-managed
    /// by `azureclaw-controller/mcp`.
    #[serde(default)]
    pub signing_key_ref: Option<LocalObjectRef>,

    /// Reference to the ConfigMap caching the issuer's JWKS. Present
    /// only when `spec.productionMode == true`. Single key `jwks.json`
    /// holds the raw RFC 7517 JWKSet bytes. Field-managed by
    /// `azureclaw-controller/mcp`.
    #[serde(default)]
    pub jwks_config_map_ref: Option<LocalObjectRef>,
}

/// Minimal `LocalObjectReference`-shaped struct with `name` only — the
/// emitted Secret/ConfigMap always lives in the same namespace as the
/// CR, so namespace plumbing would be redundant. Mirrors the
/// `corev1.LocalObjectReference` Kubernetes API shape.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalObjectRef {
    pub name: String,
}
