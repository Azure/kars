// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `ToolPolicy` CRD — minimal scaffold per implementation-plan §7 entry 4.
//!
//! Status: **scaffold-only**. Compiles to AGT policy profiles via
//! `PolicyDecisionProvider` (in-tree `Governance` impl today; AGT-SDK-
//! backed alt under `phase1/agt-policy-provider-prod`). Wiring of the
//! commerce-cap enforcement to the router happens in
//! `phase1/a2a-1.0.0-routes-internal` for AP2 spend, and in
//! `phase1/mcp-2026-streamable-http-routes` for tool-call gating.
//!
//! ## Why a scaffold lands first
//!
//! Same rationale as `McpServer` (§7 entry 3) — schema landing decoupled
//! from data-plane wiring. Lets `A2AAgent` (and the conformance corpus
//! row "AP2 commerce") reference a stable `ToolPolicy` shape sooner.
//!
//! ## Spec sources
//!
//! - AP2 commerce: <https://ap2-protocol.org/> (canonical), shape mirrors
//!   the AAIF AP2 v0.5 cap structure.
//! - OWASP MCP Top 10 control "Excessive Agency": cap + counterparty
//!   allowlist directly map to MCP-04 and MCP-08.
//! - OWASP LLM Top 10 v2.0: LLM06 "Sensitive Information Disclosure"
//!   covered by audit.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `ToolPolicy.spec` — declares per-tool gating: rate limits, approval,
/// and AP2 commerce caps. Resolves bottom-up (most-specific selector
/// wins). See `docs/api/lifecycle.md` for resolution semantics.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "ToolPolicy",
    namespaced,
    status = "ToolPolicyStatus",
    shortname = "tp",
    printcolumn = r#"{"name":"Tool","type":"string","jsonPath":".spec.appliesTo.tool"}"#,
    printcolumn = r#"{"name":"DailyCap","type":"string","jsonPath":".spec.commerce.dailyCap"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicySpec {
    /// Selector: which tool calls this policy applies to. AND of tool name,
    /// MCP server, and sandbox label-selector.
    pub applies_to: AppliesToSelector,

    /// AP2 commerce caps. Optional — present only for tools that move
    /// money / commit purchases / sign transfers.
    pub commerce: Option<CommercePolicy>,

    /// Rate-limit configuration. Resolved by AGT RateLimiter.
    pub rate_limit: Option<RateLimitPolicy>,

    /// Approval policy: human-in-the-loop confirmation step.
    pub approval: Option<ApprovalPolicy>,

    /// Optional human-readable label.
    pub display_name: Option<String>,

    /// Customer-supplied AGT policy profile. The controller writes the
    /// raw profile bytes into the compiled ConfigMap under key
    /// `agt-profile.yaml` and the sandbox inference-router loads it via
    /// the AGT policy engine. This is the **sole** source of AGT policy
    /// profiles post-Slice-1e — the bundled
    /// `/opt/azureclaw-plugin/policies/*.yaml` fallback that older
    /// releases shipped has been removed.
    ///
    /// Two sources are supported (mutually exclusive):
    /// - `inline`: raw YAML carried in the spec (Slice 1b)
    /// - `bundleRef`: signed OCI artifact (Slice 1c.2) — the controller
    ///   pulls + cosign-verifies via
    ///   [`crate::policy_fetcher::fetch_and_verify_generic`] with
    ///   [`crate::policy_canonical::tools::ToolsKind`] and writes the
    ///   verified bytes into the same ConfigMap key. The wire contract
    ///   with the router is identical to the inline path — the router
    ///   doesn't know (or need to know) which source produced the bytes.
    ///
    /// Mutual exclusion: admission CEL (in the Helm CRD) enforces
    /// `has(inline) != has(bundleRef)`. The reconciler also checks this
    /// at runtime as a defense-in-depth measure (older clusters without
    /// the latest CRD).
    ///
    /// Per principles.md §3: when `agtProfile` is set, the controller
    /// stamps `phase=Compiled` and `Ready=False /
    /// reason=AwaitingRouterEnforcement` until the router-confirmation
    /// poller closes the loop. Fetch failures (signature, ACR, parse)
    /// surface as `Ready=False / reason=AllowlistMissing|…` — reusing
    /// the egress error taxonomy via [`crate::policy_fetcher::FetchError`].
    pub agt_profile: Option<AgtProfileSource>,
}

/// AGT policy profile source. Exactly one of `inline` or `bundleRef`
/// must be set (admission CEL + runtime check enforce); both being
/// absent is treated as "no AGT profile" (back-compat path).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgtProfileSource {
    /// Inline AGT policy YAML body. Plain string — admission validates
    /// it is non-empty; the router parses it at load time and surfaces
    /// any syntax error via `GET /internal/policy-status`.
    pub inline: Option<String>,

    /// Signed OCI artifact reference. The controller pulls the artifact
    /// from `<registry>/<repository>@<digest>`, verifies the cosign
    /// signature against the active [`crate::signer_policy::SignerPolicy`],
    /// re-validates the AGT YAML structure
    /// ([`crate::policy_canonical::tools::ToolsKind`]), and writes the
    /// bytes into the compiled-profile ConfigMap under
    /// `agt-profile.yaml`. On any verification failure the controller
    /// stamps `Ready=False / reason=AllowlistMissing|AllowlistMalformed|
    /// AllowlistUnauthorized|AllowlistSignatureVerifyFailed` (the same
    /// `FetchError` variants used by egress — one error taxonomy
    /// across all signed-policy kinds).
    ///
    /// Artifact `artifactType` MUST be
    /// `application/vnd.azureclaw.agt-profile.v1+yaml`; a mismatch
    /// surfaces as `Ready=False / reason=InvalidRef`.
    pub bundle_ref: Option<crate::crd::OciArtifactRef>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppliesToSelector {
    /// Tool name as advertised by the MCP server. `"*"` matches all.
    pub tool: Option<String>,

    /// MCP server name (`McpServer.metadata.name`) the tool must come
    /// from. Empty means any.
    pub mcp_server: Option<String>,

    /// Sandbox label selector. AND with the other fields.
    #[serde(default)]
    pub sandbox_match_labels: std::collections::BTreeMap<String, String>,
}

/// AP2 commerce caps. Hard fail-closed: a missing or malformed value
/// is treated as a deny in the policy compile step (verified by
/// conformance-corpus negative test "AP2 cap exceeded → refuse").
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommercePolicy {
    /// Daily spend cap as ISO-4217 currency string with 2-decimal-place
    /// integer minor units, e.g. `"USD 100.00"`. Parser is strict;
    /// admission CEL rejects malformed values at apply time.
    pub daily_cap: Option<String>,

    /// Monthly spend cap. Must be >= dailyCap (admission CEL).
    pub monthly_cap: Option<String>,

    /// Counterparty allowlist. Empty = deny-all (fail-closed).
    /// Format: AP2 counterparty identifier (DID or domain).
    #[serde(default)]
    pub counterparty_allowlist: Vec<String>,

    /// Per-transfer hard cap. Even within daily/monthly, a single
    /// transfer above this is refused.
    pub per_transfer_cap: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitPolicy {
    /// Requests per second across all matching invocations.
    pub rps: Option<u32>,

    /// Burst (token bucket size).
    pub burst: Option<u32>,

    /// Window for the counter, e.g. `"1m"`, `"1h"`, `"24h"`.
    pub window: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalPolicy {
    /// Approval required: `never` | `always` | `aboveThreshold`.
    /// Default `aboveThreshold` when commerce is set.
    pub mode: Option<String>,

    /// Threshold value (currency string) above which approval is
    /// required. Only meaningful when `mode == "aboveThreshold"`.
    pub threshold: Option<String>,

    /// Approval channel reference, e.g. Telegram bot, email.
    pub channel: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicyStatus {
    #[serde(default)]
    pub phase: Option<String>,

    /// `metadata.generation` last successfully reconciled. KEP-1623.
    #[serde(default)]
    pub observed_generation: Option<i64>,

    #[serde(default)]
    pub conditions: Option<Vec<Condition>>,

    /// Last time the policy was compiled to an AGT profile and pushed.
    #[serde(default)]
    pub last_compiled_at: Option<String>,

    /// Length-prefixed sha256 digest (Slice 1a aggregate canonical
    /// format) of the AGT profile bytes the controller published to
    /// the compiled ConfigMap. Set when `spec.agtProfile.inline` OR
    /// `spec.agtProfile.bundleRef` produces a profile. The router
    /// echoes this digest on `GET /internal/policy-status` once it
    /// has loaded the profile; the controller-side confirmation
    /// poller uses it to promote `Compiled → Ready`.
    #[serde(default)]
    pub agt_profile_digest: Option<String>,

    /// Verified OCI manifest digest when the AGT profile was sourced
    /// from `spec.agtProfile.bundleRef`. Distinct from
    /// [`Self::agt_profile_digest`]: that one is the length-prefixed
    /// hash over the *content* (wire contract with the router); this
    /// one is the *OCI manifest digest* re-validated against
    /// `bundleRef.digest` after cosign verification (the supply-chain
    /// attestation). `None` when the profile came from `inline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agt_profile_bundle_digest: Option<String>,
}
