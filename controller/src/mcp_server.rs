//! `McpServer` CRD — minimal scaffold per implementation-plan §7 entry 3.
//!
//! Status: **scaffold-only** in this branch. The reconciler is wired in
//! `controller/src/main.rs` but only updates Conditions; it does not yet
//! provision a router-side OAuth 2.1 endpoint. That lands in
//! `phase1/mcp-2026-streamable-http-routes`.
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
}
