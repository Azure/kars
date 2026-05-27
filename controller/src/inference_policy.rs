// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `InferencePolicy` CRD — Phase 2 §8 entry 4 (S4).
//!
//! Per `docs/implementation-plan.md` §8 entry 2 and `docs/competitive.md`
//! §10.3 entry 7, `InferencePolicy` is a **sandbox-side budget /
//! guardrail / safety policy** — *not* a model-router. Model selection
//! sits in Foundry. This CR expresses *which Foundry route* to prefer
//! plus token budgets, Content Safety floor levels, and fallback chain.
//!
//! Status: full schema lands in this slice; the router-side informer
//! that hot-reloads compiled profiles is **S7**
//! (`phase2-conditions-ssa-leader`). Compiled profile bytes slot into
//! the existing
//! [`inference-router::policy_envelope::PolicyEntry`] payload — no
//! parallel hot-reload core.
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! - **CRD-derive shape** (group, version, kind, namespaced, status,
//!   shortname, printcolumns): mirrors [`crate::tool_policy`] (S2) and
//!   [`crate::a2a_agent`] (S3).
//! - **`LocalObjectRef` for status ConfigMap ref**: re-used from
//!   [`crate::mcp_server`] — single struct, four semantic clients now
//!   (S1 signing/jwks, S2 profile, S3 agent-card, S4 guardrail-profile).
//! - **`Condition` vocabulary**: re-used from
//!   [`crate::status::conditions`] via the reconciler module.
//! - **Router-side runtime gate**:
//!   [`inference-router::routes::inference_policy::check`] (Phase 1)
//!   already calls `PolicyDecisionProvider::decide()` at every
//!   inference call site (chat completions, responses API, image gen,
//!   streaming output check). The router consumes the compiled profile
//!   via `PolicyEntry.payload` — wired in S7. **No router-side change
//!   in this slice.**
//!
//! ## Spec sources
//!
//! - Token-budget shape: AGT TokenBudgetTracker
//!   (`agentmesh::governance::token_budget`).
//! - Content Safety severity floors: Microsoft Content Safety
//!   `Microsoft.DefaultV2` severity levels (`Safe` / `Low` / `Medium`
//!   / `High`); the router's existing parser already lifts these from
//!   `prompt_filter_results` annotations.
//! - Model preference: `KarsSandbox.spec.providers[]` enumeration of
//!   `azure-openai` / `anthropic` / `gemini` / `bedrock` / `ollama`;
//!   here we only declare *which* route is preferred and the fallback
//!   order.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp_server::LocalObjectRef;

/// `InferencePolicy.spec` — declares per-sandbox inference-time
/// guardrails: token budgets, Content Safety severity floors, model
/// preference + fallback chain.
///
/// Resolution rule (precedence): same as ToolPolicy — most-specific
/// `appliesTo` selector wins. See `docs/api/lifecycle.md` for
/// resolution semantics. The compiled profile carries the unmodified
/// selector; precedence resolution is router-side.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kars.azure.com",
    version = "v1alpha1",
    kind = "InferencePolicy",
    namespaced,
    status = "InferencePolicyStatus",
    shortname = "ip",
    printcolumn = r#"{"name":"Sandbox","type":"string","jsonPath":".spec.appliesTo.sandboxName"}"#,
    printcolumn = r#"{"name":"DailyTokens","type":"integer","jsonPath":".spec.tokenBudget.dailyTokens"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct InferencePolicySpec {
    /// Selector: which sandboxes / inference call sites this policy
    /// applies to. AND of fields.
    pub applies_to: InferenceAppliesTo,

    /// Token-budget caps. Optional — absent ⇒ no budget enforcement.
    ///
    /// Mutually exclusive with [`Self::bundle_ref`]: when `bundleRef`
    /// is set, the budget caps come from the signed bundle. Admission
    /// CEL + reconciler defense-in-depth both reject specs that set
    /// any of `tokenBudget`/`contentSafety`/`modelPreference`/
    /// `displayName` alongside `bundleRef`.
    pub token_budget: Option<TokenBudget>,

    /// Content Safety severity floor. Optional — absent ⇒ governance
    /// minimum (router-default) applies. The VAP referenced in §7.14
    /// rejects changes that *lower* the floor below the cluster
    /// minimum. Mutually exclusive with [`Self::bundle_ref`].
    pub content_safety: Option<ContentSafetyFloor>,

    /// Model preference + fallback chain. Optional. **Not** a router:
    /// only declares preferred route order; selection is delegated to
    /// the underlying provider. Mutually exclusive with
    /// [`Self::bundle_ref`].
    pub model_preference: Option<ModelPreference>,

    /// Optional human-readable label. Mutually exclusive with
    /// [`Self::bundle_ref`] — when `bundleRef` is set, the label
    /// comes from the signed bundle.
    pub display_name: Option<String>,

    /// Signed OCI artifact reference. The controller pulls the
    /// artifact from `<registry>/<repository>@<digest>`, verifies the
    /// cosign signature against the active
    /// [`crate::signer_policy::SignerPolicy`], re-validates the JSON
    /// structure ([`crate::policy_canonical::inference::InferenceKind`]),
    /// and synthesises the effective spec by combining the CR's
    /// `appliesTo` selector with the bundle's `tokenBudget` /
    /// `contentSafety` / `modelPreference` / `displayName` content
    /// fields. The same compile + ConfigMap write path runs as for
    /// inline specs — only the source of the content bytes differs.
    ///
    /// On any verification failure the controller stamps
    /// `Ready=False / reason=AllowlistMissing|AllowlistMalformed|
    /// AllowlistUnauthorized|AllowlistSignatureVerifyFailed` (the
    /// same `FetchError` variants used by egress + tools — one
    /// error taxonomy across all signed-policy kinds).
    ///
    /// Artifact `artifactType` MUST be
    /// `application/vnd.kars.inference-policy.v1+json`; a
    /// mismatch surfaces as `Ready=False / reason=InvalidRef`.
    ///
    /// Mutually exclusive with the inline content fields
    /// (`tokenBudget`, `contentSafety`, `modelPreference`,
    /// `displayName`). `appliesTo` always comes from the CR so one
    /// signed bundle can be referenced by multiple CRs with
    /// different selectors. See
    /// `docs/internal/policy-canonical-format.md`.
    pub bundle_ref: Option<crate::crd::OciArtifactRef>,
}

/// Selector for which sandboxes / call sites an `InferencePolicy`
/// applies to. AND-combined.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InferenceAppliesTo {
    /// Exact sandbox name (`KarsSandbox.metadata.name`). Empty means
    /// any sandbox in the namespace.
    pub sandbox_name: Option<String>,

    /// Sandbox label selector. AND with `sandboxName`.
    #[serde(default)]
    pub sandbox_match_labels: std::collections::BTreeMap<String, String>,

    /// Inference action filter: one of `chat` / `responses` /
    /// `image` / `embeddings` / `*` (default). Maps to the call sites
    /// in `inference-router/src/routes/inference.rs`.
    pub action: Option<String>,
}

/// Token budgets. Carried verbatim into the compiled profile; the
/// router-side AGT `TokenBudgetTracker` enforces.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenBudget {
    /// Per-request hard cap. Single inference call exceeding this is
    /// refused before the upstream forward.
    pub per_request_tokens: Option<u64>,

    /// Daily aggregate cap (sum of input + output tokens).
    pub daily_tokens: Option<u64>,

    /// Monthly aggregate cap. Must be >= dailyTokens (admission CEL).
    pub monthly_tokens: Option<u64>,
}

/// Content Safety severity floors. Severity values match Microsoft
/// Content Safety `Microsoft.DefaultV2`: `Safe` / `Low` / `Medium` /
/// `High`. The router enforces by parsing `prompt_filter_results` from
/// model responses (Phase 1 substrate).
///
/// Each field declares the *maximum tolerated* severity for that
/// category. A response carrying a finding **above** the floor is
/// blocked. Absent field ⇒ category not policed by this CR.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentSafetyFloor {
    /// Hate speech severity floor.
    pub hate: Option<String>,
    /// Self-harm severity floor.
    pub self_harm: Option<String>,
    /// Sexual content severity floor.
    pub sexual: Option<String>,
    /// Violence severity floor.
    pub violence: Option<String>,
    /// Whether to require Prompt Shields (jailbreak / indirect
    /// injection) be enabled by the upstream. The router fails-closed
    /// if Prompt Shields are advertised by the deployment but the
    /// response lacks the corresponding annotations.
    pub require_prompt_shields: Option<bool>,
}

/// Preferred model + fallback chain. Each entry is a Foundry route
/// reference (deployment name + provider tag). The router selects
/// the first reachable entry; an entry returning 5xx / 429 falls
/// through to the next. **Selection is not load-balancing** — first
/// healthy wins, deterministically.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelPreference {
    /// Primary preferred route.
    pub primary: ModelRef,

    /// Ordered fallback routes. Tried in order on primary failure.
    #[serde(default)]
    pub fallback: Vec<ModelRef>,
}

/// Reference to a Foundry route. Combination of provider tag (one of
/// `azure-openai` / `anthropic` / `gemini` / `bedrock` / `ollama`) and
/// deployment name. The router resolves to the actual base URL using
/// its existing per-provider configuration.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// Provider tag.
    pub provider: String,

    /// Deployment name as advertised by the provider.
    pub deployment: String,
}

/// Status of an `InferencePolicy` reconcile.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InferencePolicyStatus {
    #[serde(default)]
    pub phase: Option<String>,

    /// `metadata.generation` last successfully reconciled. KEP-1623.
    #[serde(default)]
    pub observed_generation: Option<i64>,

    #[serde(default)]
    pub conditions: Option<Vec<Condition>>,

    /// Pointer to the compiled-profile `ConfigMap` produced by the
    /// reconciler. The router-side informer (S7) watches by label
    /// selector; this status field is for human / CLI consumption.
    #[serde(default)]
    pub profile_config_map_ref: Option<LocalObjectRef>,

    /// Hex-encoded sha256 prefix of the compiled profile JSON. Stable
    /// under serde round-trip (canonical key order). Same input ⇒
    /// same hash; immune to map-reordering.
    #[serde(default)]
    pub version_hash: Option<String>,

    /// Last time the policy was compiled and pushed.
    #[serde(default)]
    pub last_compiled_at: Option<String>,

    /// `sha256:<full hex>` digest the controller wrote to the
    /// compiled-profile ConfigMap and the annotation
    /// `kars.azure.com/inference-policy-digest`. Computed by
    /// `inference_policy_compile::inference_policy_digest` from the
    /// canonical bytes of `inference-policy.json`. Distinct from
    /// `version_hash` (16-byte legacy short-hash kept for
    /// `PolicyEntry.version` change-detection): this full digest is
    /// the **wire-contract value** the router echoes back via
    /// `GET /internal/policy-status`.
    ///
    /// `phase=Ready` is only stamped when this matches
    /// `loaded_digest` on every sandbox that references the policy.
    /// (Slice 2a of `crd-well-oiled-machine`.)
    #[serde(default)]
    pub compiled_digest: Option<String>,

    /// Digest most recently echoed by referencing sandbox routers
    /// via `GET /internal/policy-status`. When this equals
    /// `compiled_digest` for every referencing sandbox, the
    /// reconciler promotes `phase=Compiled → Ready` with reason
    /// `RouterEnforcing`. While mismatched (or while no sandbox
    /// references the policy), the reconciler stamps
    /// `Ready=False / AwaitingRouterEnforcement` and emits a
    /// `PolicyNotEnforced` Warning Event. (Slice 2a of
    /// `crd-well-oiled-machine`.)
    #[serde(default)]
    pub loaded_digest: Option<String>,

    /// OCI manifest digest of the verified `spec.bundleRef` artifact
    /// when the bundleRef path was taken; `None` for inline specs.
    /// Stamped after [`crate::policy_fetcher::fetch_and_verify_generic`]
    /// returns `Ok`, so its presence is operator-visible proof that
    /// the controller actually verified the signed artifact (vs.
    /// taking an inline shortcut). Distinct from `compiled_digest`
    /// (the wire-contract value the router echoes) — `bundle_ref_digest`
    /// is the supply-chain identifier. Slice 1c.3 of
    /// `crd-well-oiled-machine`.
    #[serde(default)]
    pub bundle_ref_digest: Option<String>,
}
