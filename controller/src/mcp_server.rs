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
///
/// ## Three authoring paths
///
/// - `managed`: select a reviewed controller-owned in-cluster workload preset.
/// - Inline `url`/auth fields: register an already-running external or private
///   endpoint (no supply-chain attestation of the server workload).
/// - [`bundle_ref`](McpServerSpec::bundle_ref): reference a signed OCI policy
///   artifact (cosign-verified against the cluster `SignerPolicy`).
///
/// These source paths are mutually exclusive. `allowedTools`, `displayName`,
/// and the `allowedSandboxes` selector remain deployment-time controls for the
/// managed path. The
/// `allowedSandboxes` selector is owned exclusively by the CR — one
/// signed server bundle can be referenced by multiple `McpServer` CRs
/// with different sandbox selectors.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
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
    /// Validated by admission CEL.
    ///
    /// Optional: a signed bundle can supply this via [`bundle_ref`]. At
    /// reconcile time, exactly one of `url` or `bundle_ref.url` must
    /// resolve to a non-empty value, otherwise the reconciler stamps
    /// `Degraded=True/SpecInvalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Optional controller-managed in-cluster MCP workload.
    ///
    /// This is deliberately a closed preset enum rather than an arbitrary image
    /// field. An operator who can author an `McpServer` must not be able to turn
    /// the controller into a general-purpose privileged workload launcher.
    /// Presets are reviewed, versioned with Kars, and materialized into the
    /// dedicated managed-MCP namespace with a rootless security context.
    ///
    /// Mutually exclusive with `url` and `bundleRef`. The reconciler derives the
    /// effective in-cluster Streamable-HTTP URL from the managed Service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<ManagedMcpConfig>,

    /// OAuth 2.1 configuration. Required when `productionMode: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,

    /// When true, the router rejects calls that are not bearer-token-
    /// authenticated against `oauth.issuer` with a verified PKCE flow.
    /// `false` allows unauthenticated calls — dev-only; admission policy
    /// rejects this for non-dev tenants (mirrors `Null*` provider rule).
    ///
    /// Optional: a signed bundle can supply this. When neither inline
    /// nor bundle sets the field, the effective value defaults to
    /// `false` (back-compat with the pre-1c.5 schema).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub production_mode: Option<bool>,

    /// OAuth 2.1 scopes that the router will request when fronting calls
    /// from sandboxes to this server. The actual per-tool gating is
    /// expressed in `ToolPolicy` resources, not here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,

    /// Allow-list of tool names. Empty list means "no tools allowed";
    /// to allow all tools, use `["*"]` and lean on `ToolPolicy` for
    /// per-tool governance. Default empty — fail-closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,

    /// Selector restricting which sandboxes can reach this server.
    /// Empty = same-namespace only.
    ///
    /// **CR-owned field**: deliberately NOT part of any signed bundle.
    /// The selector is a deployment-time concern; one signed server
    /// bundle can be referenced by multiple `McpServer` CRs in
    /// different environments with different selectors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_sandboxes: Option<SandboxSelector>,

    /// Optional human-readable label for operator-TUI display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// Reference to a signed OCI artifact carrying the policy content
    /// (`url`, `oauth`, `productionMode`, `scopes`, `allowedTools`,
    /// `displayName`). Mutually exclusive with inline content fields —
    /// enforced by admission CEL and re-checked by the reconciler.
    ///
    /// Slice 1c.5 of `crd-well-oiled-machine`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_ref: Option<crate::crd::OciArtifactRef>,

    /// Slice 4d.4.1 — outbound static-bearer authentication.
    ///
    /// Name of an environment variable visible to the inference-router
    /// container whose value should be attached as
    /// `Authorization: Bearer <value>` on every outbound `tools/list`
    /// and `tools/call` request to this server.
    ///
    /// Designed to reuse pre-existing sandbox env vars without
    /// introducing new mounts. The primary intended consumer is the
    /// GitHub Copilot dev-credential path (`COPILOT_GITHUB_TOKEN`),
    /// which already contains a GitHub OAuth token that authenticates
    /// against `https://api.githubcopilot.com/mcp`.
    ///
    /// Behaviour when the named env var is unset or empty: the router
    /// records a `skipped` entry and continues — non-fatal. Other
    /// servers in the registry remain advertised. This keeps Foundry
    /// deployments (which do not set `COPILOT_GITHUB_TOKEN`) from
    /// breaking when a github MCP CR is present.
    ///
    /// OAuth on-behalf-of (where the *agent's* incoming bearer is
    /// re-used) is deferred to Slice 4d.5 and tracked as a separate
    /// CR field there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_from_env: Option<String>,
}

/// Controller-owned managed MCP workload selection.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedMcpConfig {
    pub preset: ManagedMcpPreset,
}

/// Reviewed workload recipes the controller knows how to deploy safely.
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedMcpPreset {
    /// Microsoft's official Playwright MCP server (headless Chromium).
    Playwright,
    /// The MCP reference "everything" server used for deterministic protocol
    /// and utility-tool verification.
    Everything,
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
    /// Match labels on `KarsSandbox` resources. Standard label selector
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
    /// `kars.azure.com/mcp-signing-key` with two keys:
    /// `signing-key.private` (raw 32-byte Ed25519 seed) and
    /// `signing-key.public` (raw 32-byte verifying key). Field-managed
    /// by `kars-controller/mcp`.
    #[serde(default)]
    pub signing_key_ref: Option<LocalObjectRef>,

    /// Reference to the ConfigMap caching the issuer's JWKS. Present
    /// only when `spec.productionMode == true`. Single key `jwks.json`
    /// holds the raw RFC 7517 JWKSet bytes. Field-managed by
    /// `kars-controller/mcp`.
    #[serde(default)]
    pub jwks_config_map_ref: Option<LocalObjectRef>,

    /// OCI manifest digest of the verified signed bundle, when the
    /// `bundleRef` authoring path was taken. Operator-visible proof
    /// that the signature was checked and the bundle's bytes shaped
    /// the effective server config.
    ///
    /// `None` when the inline path was used.
    ///
    /// Slice 1c.5 of `crd-well-oiled-machine`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_ref_digest: Option<String>,

    /// `Managed` when `spec.managed` materializes an in-cluster workload;
    /// otherwise `External`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    /// Effective upstream Streamable-HTTP endpoint consumed by sandbox routers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Managed Deployment name (`namespace/name`) when mode is `Managed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_ref: Option<String>,

    /// Tool names returned by the last successful upstream `tools/list` probe,
    /// after applying `allowedTools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_tools: Option<Vec<String>>,

    /// SHA-256 of the canonical discovered tool definitions. Lets operators and
    /// admission/preflight detect catalog drift without exposing credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_schema_digest: Option<String>,
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
