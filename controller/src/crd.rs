// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ClawSandbox Custom Resource Definition.
//!
//! This is the Rust representation of the ClawSandbox CRD.
//! kube-rs derives the CRD schema, API bindings, and JSON schema automatically.

use crate::mcp_server::LocalObjectRef;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// ClawSandbox spec — declares the desired state for a sandboxed OpenClaw agent.
#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "azureclaw.azure.com",
    version = "v1alpha1",
    kind = "ClawSandbox",
    namespaced,
    status = "ClawSandboxStatus",
    shortname = "cs",
    shortname = "claw",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Runtime","type":"string","jsonPath":".spec.runtime.kind"}"#,
    printcolumn = r#"{"name":"InferencePolicy","type":"string","jsonPath":".spec.inferenceRef.name"}"#,
    printcolumn = r#"{"name":"Isolation","type":"string","jsonPath":".spec.sandbox.isolation"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClawSandboxSpec {
    /// Agent runtime selector (S10.A1 multi-runtime hosting).
    ///
    /// Replaces the original `spec.openclaw` field. The `kind` discriminator
    /// selects which sibling struct (`openclaw` / `openaiAgents` /
    /// `microsoftAgentFramework` / `byo`) is required; the others must be
    /// absent. Mutual exclusion is enforced by Helm CRD CEL `x-kubernetes-
    /// validations`; the controller additionally validates shape defensively
    /// via `validate_runtime_shape` before deployment planning.
    ///
    /// Default constructed value (used in tests + when k8s defaulting is
    /// active) is `kind: OpenClaw` with an empty `OpenClawConfig`. Required
    /// on the wire — Helm CRD `required: ["runtime", "sandbox", "inference"]`.
    #[serde(default)]
    pub runtime: RuntimeSpec,

    /// Sandbox security settings
    pub sandbox: Option<SandboxConfig>,

    /// Reference to an `InferencePolicy` CR in the **same namespace** as
    /// this `ClawSandbox`. The referenced CR is the single source of
    /// truth for inference guardrails: model preference, content-safety
    /// floor, prompt-shield requirement, token budgets. The reconciler
    /// resolves the ref at apply time; if the target is missing the
    /// sandbox enters `Degraded` with reason `InferencePolicyNotFound`
    /// (no inline-fallback path post-S13).
    ///
    /// Cross-namespace refs are deliberately not supported — would be a
    /// privilege-escalation vector. See `docs/crd-precedence.md`.
    pub inference_ref: LocalObjectRef,

    /// Optional reference to a `ClawMemory` CR in the **same namespace**
    /// as this `ClawSandbox`. When set, the controller mirrors the
    /// compiled binding `ConfigMap` (`clawmemory-{name}-binding`) into
    /// the sandbox namespace and mounts it into the inference-router
    /// at [`crate::reconciler::governance_mounts::paths::MEMORY_BINDING_DIR`].
    /// The router's `memory_binding_loader` reads the file, registers
    /// the digest under `PolicyKind::Memory`, and echoes it via
    /// `GET /internal/policy-status` so the `ClawMemory` reconciler
    /// can close the principles.md §3 "Ready ⇔ router echo" loop
    /// (Slice 3a). Cross-namespace refs are deliberately not supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_ref: Option<LocalObjectRef>,

    /// Network policy
    pub network_policy: Option<NetworkPolicyConfig>,

    /// Foundry Agent Service configuration.
    /// When set, the controller creates a Foundry prompt agent on reconcile
    /// and injects FOUNDRY_AGENT_ID into the OpenClaw container.
    pub agent: Option<AgentConfig>,

    /// AGT behavioral governance (opt-in for multi-agent).
    /// When enabled, the controller injects AGT env vars and mounts policy config.
    pub governance: Option<GovernanceConfig>,

    /// Azure services accessible from the sandbox.
    /// NOTE: Schema reserved for future use. The controller does not yet create
    /// Azure role assignments for declared services. Inference via Foundry works
    /// through the inference router (no role assignment needed).
    pub azure_services: Option<Vec<AzureServiceConfig>>,

    /// Resource limits
    pub resources: Option<ResourceConfig>,

    /// A2A 1.0.0 inbound exposure (default: not exposed).
    ///
    /// **Default OFF.** When `Some`, the controller emits a Service +
    /// CiliumNetworkPolicy + a routing entry in the gateway ConfigMap.
    /// When `None` or set to a struct with `enabled: false`, no inbound
    /// A2A path exists for this sandbox.
    ///
    /// See ADR-0001 §D6 for the surgical-exposure design (allowedCallers
    /// pinning, expiresAt, advertisedSkills, minimumTrustScore, rate
    /// limit, body cap, session length, streaming flag, revoke-now).
    ///
    /// Reconciler-side enforcement lands in
    /// `phase1/a2a-controller-revocation`; this branch is schema-only.
    pub a2a: Option<A2aIngressConfig>,

    /// Upstream-protocol compatibility opt-in (Phase 1 schema-only scaffold).
    ///
    /// When `Some`, the controller will (in a future reconciler branch) accept
    /// inbound traffic in upstream wire formats (e.g. `sigs.k8s.io/agent-sandbox`
    /// SandboxClaim semantics) and translate them into the canonical
    /// AzureClaw runtime contracts before they reach the agent. The translation
    /// path is **read-only at the boundary**: AzureClaw never mutates upstream
    /// objects in cluster, only mirrors observed state and emits canonical
    /// status conditions.
    ///
    /// **Default OFF.** Schema lands now so future reconciler branches are
    /// pure wiring. No code path consumes this field yet.
    pub upstream_compatibility: Option<UpstreamCompatibilityConfig>,

    /// Operator-driven graceful pause (Phase G P1 #4).
    ///
    /// When `Some(true)`, the controller scales the sandbox Deployment
    /// to `replicas: 0` and stamps the K8s `Suspended=True` Condition
    /// with reason `SuspendedBySpec`. The namespace, NetworkPolicy,
    /// ServiceAccount, governance ConfigMaps, and any Azure
    /// federated-identity binding are preserved byte-identical, so
    /// flipping back to `Some(false)` (or unsetting) restores the
    /// agent in-place without losing state.
    ///
    /// Distinct from `Suspended=True / Reason=OverlayMode` (induced by
    /// `spec.upstreamCompatibility.sigsAgentSandbox=overlay`), which
    /// signals that an upstream CR owns the Pod entirely. Spec-level
    /// suspension is the operator-driven graceful pause; OverlayMode
    /// is the architectural reason the controller never owns the Pod
    /// at all. When both apply, OverlayMode wins (its reason is
    /// stamped, no Deployment is created either way).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended: Option<bool>,
}

/// Upstream-protocol compatibility (Phase 1 scaffold extended in Phase 2 S8).
///
/// Codifies §2 (Native | Translate | Overlay) of the implementation plan
/// as CRD fields. All values default to OFF — opt-in per sandbox.
///
/// **`OverlayMode` (Phase 2 S8).** When `sigs_agent_sandbox == "overlay"`,
/// the operator already manages an upstream `Sandbox` CR in the same
/// namespace; AzureClaw provides only the *overlay* (namespace + sandbox
/// ServiceAccount + Workload-Identity binding + NetworkPolicy + governance
/// ConfigMaps). The controller **skips Deployment/Service/CronJob
/// creation**: those are owned by the upstream reconciler. The
/// `upstream_sandbox_ref` field names that upstream CR. Implementation
/// plan §2 lines 269-271 + §8 entry "S8 phase2-overlaymode".
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamCompatibilityConfig {
    /// `sigs.k8s.io/agent-sandbox` SandboxClaim translation mode.
    /// Values:
    /// - `"off"` (default) — no upstream interaction; pure Native mode.
    /// - `"observe"` — mirror status only.
    /// - `"translate"` — accept SandboxClaim semantics on inbound (P1
    ///   schema-only, runtime path deferred).
    /// - `"overlay"` — operator's upstream `Sandbox` CR owns the Pod;
    ///   AzureClaw provides governance overlay only. Requires
    ///   [`upstream_sandbox_ref`] (admission-enforced).
    ///
    /// Reconciler refuses unknown strings.
    pub sigs_agent_sandbox: Option<String>,

    /// Reference to an upstream `Sandbox` CR in the same namespace.
    /// **Required when `sigs_agent_sandbox == "overlay"`.** Ignored
    /// otherwise. The controller does not watch the upstream object's
    /// status today (deferred to a future slice that adds an upstream
    /// CRD discovery / informer); operators read overlay state from
    /// the upstream CR directly. AzureClaw never mutates the upstream
    /// object — the relationship is read-only at the boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_sandbox_ref: Option<LocalObjectRef>,

    /// CNCF AI Conformance reference-mode toggle. When `true`, the
    /// reconciler emits the canonical conformance status block on the
    /// ClawSandbox object regardless of other settings. **Schema-only**;
    /// no code path consumes this yet.
    #[serde(default)]
    pub ai_conformance_reference: bool,
}

impl UpstreamCompatibilityConfig {
    /// Returns `true` if this configuration selects `OverlayMode`.
    /// Pure helper — no I/O, no logging.
    #[must_use]
    pub fn is_overlay_mode(&self) -> bool {
        self.sigs_agent_sandbox.as_deref() == Some("overlay")
    }

    /// Returns the upstream `Sandbox` CR name when in overlay mode,
    /// otherwise `None`. Centralises the "extract overlay target"
    /// logic so the reconciler does not duplicate the match.
    #[must_use]
    pub fn overlay_target_name(&self) -> Option<&str> {
        if self.is_overlay_mode() {
            self.upstream_sandbox_ref.as_ref().map(|r| r.name.as_str())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod upstream_compat_tests {
    use super::*;

    fn cfg(mode: Option<&str>, name: Option<&str>) -> UpstreamCompatibilityConfig {
        UpstreamCompatibilityConfig {
            sigs_agent_sandbox: mode.map(str::to_owned),
            upstream_sandbox_ref: name.map(|n| LocalObjectRef { name: n.into() }),
            ai_conformance_reference: false,
        }
    }

    #[test]
    fn is_overlay_mode_true_only_for_overlay_string() {
        assert!(cfg(Some("overlay"), Some("up")).is_overlay_mode());
        assert!(!cfg(Some("off"), None).is_overlay_mode());
        assert!(!cfg(Some("observe"), None).is_overlay_mode());
        assert!(!cfg(Some("translate"), None).is_overlay_mode());
        assert!(!cfg(None, None).is_overlay_mode());
        assert!(!cfg(Some("OVERLAY"), None).is_overlay_mode());
        assert!(!cfg(Some(""), None).is_overlay_mode());
    }

    #[test]
    fn overlay_target_name_extracts_only_in_overlay_mode() {
        assert_eq!(
            cfg(Some("overlay"), Some("upstream-1")).overlay_target_name(),
            Some("upstream-1")
        );
        assert_eq!(
            cfg(Some("translate"), Some("upstream-1")).overlay_target_name(),
            None
        );
        assert_eq!(cfg(Some("overlay"), None).overlay_target_name(), None);
    }

    #[test]
    fn defaults_are_native_mode() {
        let c = UpstreamCompatibilityConfig::default();
        assert!(!c.is_overlay_mode());
        assert!(c.overlay_target_name().is_none());
        assert!(c.sigs_agent_sandbox.is_none());
        assert!(c.upstream_sandbox_ref.is_none());
    }

    #[test]
    fn serde_round_trip_preserves_overlay_fields() {
        let c = cfg(Some("overlay"), Some("my-upstream"));
        let j = serde_json::to_string(&c).expect("serialise");
        let back: UpstreamCompatibilityConfig = serde_json::from_str(&j).expect("deserialise");
        assert_eq!(back.sigs_agent_sandbox.as_deref(), Some("overlay"));
        assert_eq!(
            back.upstream_sandbox_ref.as_ref().map(|r| r.name.as_str()),
            Some("my-upstream")
        );
    }

    #[test]
    fn serde_omits_upstream_ref_when_none() {
        let c = cfg(Some("off"), None);
        let j = serde_json::to_string(&c).expect("serialise");
        assert!(
            !j.contains("upstreamSandboxRef"),
            "off-mode should not serialise upstreamSandboxRef field, got {j}"
        );
    }
}

/// `ClawSandbox.spec.a2a` — inbound A2A 1.0.0 exposure block.
/// All sub-fields are admission-validated via CEL (Phase 1 §7 entry 12).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct A2aIngressConfig {
    /// Master switch. `false` (or block absent) ⇒ no inbound A2A.
    /// Setting this back to `false` triggers immediate (target < 30s)
    /// teardown of the Service + CNP + ConfigMap entry.
    #[serde(default)]
    pub enabled: bool,

    /// Required when `enabled: true`. Empty list ⇒ admission deny.
    /// Each entry pins a remote AgentCard signing key by its JWS
    /// thumbprint (RFC 7638). The router rejects calls whose card
    /// signature does not chain to one of these thumbprints.
    #[serde(default)]
    pub allowed_callers: Vec<AllowedCaller>,

    /// Required when `enabled: true`. RFC 3339 timestamp; max 30 days
    /// in the future (admission CEL). Reconciler tears down the
    /// exposure on expiry.
    pub expires_at: Option<String>,

    /// Skills advertised on this sandbox's `/.well-known/agent.json`.
    /// Anything not in this list is *not* served, even if the agent
    /// implements it. Empty ⇒ admission deny.
    #[serde(default)]
    pub advertised_skills: Vec<AdvertisedSkill>,

    /// Trust floor for inbound callers (AGT TrustManager score).
    /// Default 700. Below this, the gateway refuses the call before
    /// it touches the router.
    #[serde(default = "default_min_trust")]
    pub minimum_trust_score: u32,

    /// Per-caller rate limits enforced at the gateway layer.
    pub rate_limit: Option<A2aRateLimit>,

    /// Body cap in bytes (default 1 MiB; hard ceiling 4 MiB enforced
    /// by admission CEL).
    #[serde(default = "default_body_cap")]
    pub body_cap_bytes: u32,

    /// Session length cap (seconds; default 60). Hard ceiling 600.
    #[serde(default = "default_session_max")]
    pub session_max_seconds: u32,

    /// Allow A2A streaming responses. Default `false` (fail-closed
    /// per ADR-0001 D8).
    #[serde(default)]
    pub allow_streaming: bool,

    /// Reference to an `A2AAgent` CR in the **same namespace** whose
    /// compiled signed AgentCard the inference-router should serve at
    /// `/.well-known/agent.json`. When omitted, the controller falls
    /// back to looking for an `A2AAgent` named after the sandbox itself.
    ///
    /// Phase 3 S7 — wires the consumer side of the A2AAgent CRD. The
    /// reconciler mirrors `a2aagent-{name}-card` from the user namespace
    /// into the sandbox namespace and mounts it into the inference-router
    /// container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_ref: Option<AgentLocalRef>,
}

/// Local-namespace reference for `A2aIngressConfig.agentRef`. Mirrors the
/// shape of `crate::mcp_server::LocalObjectRef` but kept distinct so
/// the public CRD schema can evolve independently.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentLocalRef {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AllowedCaller {
    /// Human-readable name for ops dashboards.
    pub display_name: Option<String>,

    /// JWS thumbprint of the caller's AgentCard signing key
    /// (RFC 7638 — JSON Web Key Thumbprint, base64url-encoded SHA-256).
    pub jws_thumbprint: String,

    /// Optional issuer URI for the caller's identity provider. When set,
    /// the gateway requires the inbound JWS `iss` claim to match.
    pub issuer: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdvertisedSkill {
    /// Skill name (matches the skills[].name field in the AgentCard).
    pub name: String,

    /// Optional human-readable description.
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct A2aRateLimit {
    /// Requests per minute, per allowed caller.
    pub rpm: Option<u32>,

    /// Burst (token bucket).
    pub burst: Option<u32>,
}

fn default_min_trust() -> u32 {
    700
}

fn default_body_cap() -> u32 {
    1_048_576 // 1 MiB
}

fn default_session_max() -> u32 {
    60
}

// ─── Runtime selector (S10.A1 multi-runtime hosting) ─────────────────────
//
// Discriminated-union variant struct for `spec.runtime`. The `kind` enum
// selects which sibling struct is required; CEL `x-kubernetes-validations`
// in the Helm CRD enforce mutual exclusion at admission, and
// `validate_runtime_shape` (in `crd_validations`) is the controller-side
// defense-in-depth guard. Phase 2 ships exactly four variants — adding a
// fifth is a deliberate slice (Phase 3) covering image, adapter, e2e, CLI,
// docs.

/// Discriminator for [`RuntimeSpec`]. Locked to PascalCase wire values per
/// the multi-runtime naming convention (CLI flags use kebab-case; CRD field
/// names use camelCase; `kind` enum values use PascalCase).
///
/// Tier 1 (S10.A3/A4) — controller adapter shipping in Phase 2:
/// `OpenClaw`, `OpenAIAgents`, `MicrosoftAgentFramework`.
/// Tier 2 — declared roadmap; CRD-level placeholders so authors can pin  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
/// `kind:` without later schema breakage. Reconciler stamps
/// `RuntimeReady=False / AdapterMissing` until adapters land.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)] // `BYO` is the locked wire-format value (see plan.md S10 naming).
pub enum RuntimeKind {
    #[default]
    OpenClaw,
    OpenAIAgents,
    MicrosoftAgentFramework,
    SemanticKernel,
    LangGraph,
    Anthropic,
    PydanticAi,
    BYO,
}

/// Agent runtime selector. Exactly one variant struct (matching `kind`)
/// must be set; the others must be absent. See [`ClawSandboxSpec::runtime`].
#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpec {
    /// Variant discriminator: `OpenClaw | OpenAIAgents | MicrosoftAgentFramework | BYO`.
    pub kind: RuntimeKind,

    /// OpenClaw configuration. Required iff `kind == OpenClaw`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openclaw: Option<OpenClawConfig>,

    /// OpenAI Agents Python configuration. Required iff `kind == OpenAIAgents`.
    /// In Phase 2 the controller parses this variant but does not yet build
    /// a Deployment for it — `RuntimeReady=False, reason=AdapterMissing`
    /// until S10.A3 lands the adapter image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_agents: Option<OpenAIAgentsConfig>,

    /// Microsoft Agent Framework configuration. Required iff
    /// `kind == MicrosoftAgentFramework`. Phase-2 status mirrors
    /// `OpenAIAgents` — adapter ships in S10.A4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microsoft_agent_framework: Option<MicrosoftAgentFrameworkConfig>,

    /// Semantic Kernel configuration. Required iff `kind == SemanticKernel`.
    /// Tier-2 placeholder — controller stamps  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
    /// `RuntimeReady=False / AdapterMissing` until the adapter image ships.
    /// Schema is locked now to avoid a CRD breaking change later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_kernel: Option<SemanticKernelConfig>,

    /// LangGraph configuration. Required iff `kind == LangGraph`.
    /// Tier-2 placeholder — see `semantic_kernel`.  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang_graph: Option<LangGraphConfig>,

    /// Anthropic Claude Agents SDK configuration. Required iff
    /// `kind == Anthropic`. Tier-2 placeholder — see `semantic_kernel`.  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<AnthropicConfig>,

    /// Pydantic-AI runtime configuration. Required iff
    /// `kind == PydanticAi`. Adapter ships in Phase H#3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pydantic_ai: Option<PydanticAiConfig>,

    /// Bring-your-own runtime. Required iff `kind == BYO`. Image must
    /// honor the BYO contract (UID 1000, inference via `127.0.0.1:8443`,
    /// `AZURECLAW_*` env, no privileged caps). Phase 2 enforcement is
    /// warn-only via `RuntimeReady` Condition; `contractVersion` is
    /// required (no silent default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byo: Option<ByoRuntimeConfig>,
}

impl Default for RuntimeSpec {
    fn default() -> Self {
        Self {
            kind: RuntimeKind::OpenClaw,
            openclaw: Some(OpenClawConfig::default()),
            openai_agents: None,
            microsoft_agent_framework: None,
            semantic_kernel: None,
            lang_graph: None,
            anthropic: None,
            pydantic_ai: None,
            byo: None,
        }
    }
}

/// OpenAI Agents Python runtime variant.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenAIAgentsConfig {
    /// Python interpreter version (e.g. `"3.12"`). Adapter image picks the
    /// matching base; defaults to the adapter's latest-supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,
    /// Where the user's agent code comes from (OCI image or git URL).
    /// Required at runtime; CEL enforces exactly-one of `oci`/`git`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_code: Option<AgentCodeRef>,
    /// Container entrypoint. Defaults to the adapter's stock launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    /// Extra env vars merged into the adapter container. Reserved
    /// prefixes (`AGT_`, `AZURE_`, `AZURECLAW_`, …) are stripped by the
    /// reconciler — same policy as `OpenClawConfig::extra_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

/// Microsoft Agent Framework runtime variant.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MicrosoftAgentFrameworkConfig {
    /// Language flavour: `python` (default) or `dotnet`. Adapter image
    /// is selected accordingly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<MafLanguage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_code: Option<AgentCodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

/// Microsoft Agent Framework adapter language flavour.
///
/// Only `python` ships in v1.0 — the .NET flavour is a roadmap item
/// (see `docs/roadmap.md`). The single-variant enum stays in the
/// schema so adding new flavours later is a non-breaking change.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
pub enum MafLanguage {
    #[default]
    #[serde(rename = "python")]
    Python,
}

/// Semantic Kernel runtime variant (Tier-2 placeholder).  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SemanticKernelConfig {
    /// Language flavour: `python` (default), `dotnet`, or `java`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<SkLanguage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_code: Option<AgentCodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
pub enum SkLanguage {
    #[default]
    #[serde(rename = "python")]
    Python,
    #[serde(rename = "dotnet")]
    Dotnet,
    #[serde(rename = "java")]
    Java,
}

/// LangGraph runtime variant (Tier-2 placeholder).  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LangGraphConfig {
    /// Language flavour: `python` (default) or `typescript`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<LangGraphLanguage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_code: Option<AgentCodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

/// LangGraph adapter language flavour.
///
/// Both `python` and `typescript` ship as first-class v1.0 runtimes
/// (LangGraph.js for the TypeScript flavour). The image dispatched
/// is selected by `plan_langgraph` based on this field.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
pub enum LangGraphLanguage {
    #[default]
    #[serde(rename = "python")]
    Python,
    #[serde(rename = "typescript")]
    Typescript,
}

/// Anthropic Claude Agents SDK runtime variant (Tier-2 placeholder).  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
/// The Anthropic Agent SDK is currently Python-first; `pythonVersion`
/// mirrors the `OpenAIAgentsConfig` field.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicConfig {
    /// Python interpreter version (e.g. `"3.12"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_code: Option<AgentCodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

/// Pydantic-AI runtime variant (Phase H#3).
///
/// [Pydantic-AI](https://ai.pydantic.dev/) is the type-safe Python
/// agent framework from the Pydantic team. Python-only by design.
/// Wire field shape mirrors `AnthropicConfig` / `OpenAIAgentsConfig`.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PydanticAiConfig {
    /// Python interpreter version (e.g. `"3.12"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_code: Option<AgentCodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

/// Reference to user-supplied agent code. Exactly one of `oci` / `git`
/// must be set (CEL-validated in Helm CRD).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentCodeRef {
    /// Pull agent code from an OCI image (production path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci: Option<OciAgentCode>,
    /// Clone agent code from a git URL (development iteration path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitAgentCode>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct OciAgentCode {
    /// Fully-qualified OCI image reference, e.g.
    /// `myregistry.azurecr.io/agent:1.2.3`.
    pub image: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitAgentCode {
    /// Git URL (https or ssh).
    pub url: String,
    /// Branch / tag / commit SHA. Defaults to `HEAD` of default branch.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// Subdirectory inside the repo. Defaults to repo root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Bring-your-own-runtime variant. The image must honor the documented
/// BYO contract (`docs/byo-runtime-contract.md`). Phase 2 enforcement is
/// warn-only via `RuntimeReady` Condition; strict-mode admission is a
/// Phase 3 follow-up.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ByoRuntimeConfig {
    /// Container image (must declare `org.azureclaw.runtime.contract`
    /// label matching `contract_version`).
    pub image: String,
    /// Container entrypoint override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Container args override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Extra env vars (raw K8s `EnvVar` shape — supports `valueFrom`).
    /// Reserved prefixes are stripped same as openclaw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<serde_json::Value>>,
    /// BYO contract version. **Required, no default.** A silent default
    /// would let an undeclaring image appear contract-compliant.
    pub contract_version: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenClawConfig {
    pub version: Option<String>,
    pub image: Option<String>,
    pub config: Option<serde_json::Value>,
    /// Extra environment variables injected into the openclaw container as `key: value`
    /// pairs. Used by the controller to propagate offload parameters
    /// (`OFFLOAD_REQUEST_ID`, `OFFLOAD_PARENT_AMID`, `OFFLOAD_TASK`,
    /// `OFFLOAD_TIMEOUT_MINUTES`) into offload sandboxes.
    #[serde(default)]
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfig {
    /// standard | enhanced | confidential
    #[serde(default = "default_isolation")]
    pub isolation: String,
    #[serde(default = "default_seccomp")]
    pub seccomp_profile: String,
    #[serde(default = "default_selinux")]
    pub selinux_context: String,
    #[serde(default = "default_true")]
    pub read_only_root_filesystem: bool,
    #[serde(default = "default_true")]
    pub run_as_non_root: bool,
    #[serde(default)]
    pub allow_privilege_escalation: bool,
    pub writable_paths: Option<Vec<String>>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            isolation: "enhanced".into(),
            seccomp_profile: "azureclaw-strict".into(),
            // Empty = no custom SELinux type, compatible with restricted PodSecurity.
            // Custom SELinux (e.g. "azureclaw_sandbox_t") requires baseline enforcement
            // and a privileged DaemonSet to install the policy module on nodes.
            selinux_context: String::new(),
            read_only_root_filesystem: true,
            run_as_non_root: true,
            allow_privilege_escalation: false,
            writable_paths: Some(vec!["/sandbox".into(), "/tmp".into()]),
        }
    }
}

// See `crate::mcp_server::LocalObjectRef` — re-used here for sandbox refs.

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyConfig {
    #[serde(default = "default_true")]
    pub default_deny: bool,
    #[serde(default = "default_true")]
    pub approval_required: bool,
    pub allowed_endpoints: Option<Vec<EndpointConfig>>,
    /// Enable egress learn mode: observe all accessed domains (blocklist still enforced).
    /// Use `azureclaw policy learn <name>` to export the learned allowlist.
    #[serde(default)]
    pub learn_egress: bool,
    /// Reference to a signed OCI artifact containing the canonical egress
    /// allowlist. Populated by `azureclaw egress … --sign` (S12.c).
    /// **Authoritative** in S12.e — when set, the controller derives
    /// `NetworkPolicy` egress from the verified canonical artifact and
    /// inline `allowed_endpoints` is ignored (a non-empty inline that
    /// differs surfaces as `AllowlistDrift=True`).
    ///
    /// Canonical format documented at `docs/internal/policy-canonical-format.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowlist_ref: Option<OciArtifactRef>,
}

impl Default for NetworkPolicyConfig {
    fn default() -> Self {
        Self {
            default_deny: true,
            approval_required: true,
            allowed_endpoints: None,
            learn_egress: false,
            allowlist_ref: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct EndpointConfig {
    pub host: String,
    pub port: Option<u16>,
}

/// Reference to a signed OCI artifact (e.g., a sealed policy document).
///
/// Generic shape: any consumer that needs to point at a content-addressed,
/// cosign-signed OCI blob uses this struct. Keep registry-agnostic — works
/// against ACR, GHCR, or any OCI-1.1 distribution endpoint.
///
/// Verification (consumer-side) requires:
/// 1. Cryptographic validity (cosign sig present, chain valid).
/// 2. Signer identity match against a cluster `SignerPolicy` (S12.d):
///    Fulcio issuer + SAN/subject pattern.
///
/// Both checks must pass; "valid sig" alone is not authority.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OciArtifactRef {
    /// Registry hostname (e.g. `myacr.azurecr.io`, `ghcr.io`).
    pub registry: String,
    /// Repository path (e.g. `azureclaw/policies/sandbox-foo`).
    pub repository: String,
    /// Content-addressed digest, including the algorithm prefix
    /// (`sha256:abc…`). The digest covers the canonical artifact bytes —
    /// see `docs/internal/policy-canonical-format.md` for the egress-allowlist
    /// canonicalization rules.
    pub digest: String,
    /// OCI artifactType media-type, e.g.
    /// `application/vnd.azureclaw.egress-allowlist.v1+yaml`. Consumers MUST
    /// reject artifacts whose pulled `artifactType` doesn't match.
    pub artifact_type: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct AzureServiceConfig {
    /// storage | ai-search | cosmos-db | ai-foundry | keyvault | service-bus | event-hubs | sql
    pub service: String,
    pub account: Option<String>,
    pub permissions: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct ResourceConfig {
    pub requests: Option<serde_json::Value>,
    pub limits: Option<serde_json::Value>,
}

/// Foundry Agent Service configuration.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    /// System prompt / instructions for the Foundry prompt agent.
    pub instructions: Option<String>,
    /// Foundry tools to enable: file_search, web_search, code_interpreter.
    pub tools: Option<Vec<String>>,
    /// Pre-uploaded Foundry file IDs for knowledge retrieval.
    pub file_ids: Option<Vec<String>>,
}

/// AGT behavioral governance configuration.
///
/// **S13:** the `tool_policy` profile name was replaced with
/// `tool_policy_ref` — a same-namespace reference to a `ToolPolicy` CR.
/// The dedicated `ToolPolicy` CRD is the single source of truth for
/// tool-call gating (rate limit, approval, AP2 commerce caps); this
/// struct keeps only behavior knobs that aren't expressed by `ToolPolicy`
/// itself (AGT enable flag, trust threshold, trusted peers, registry mode).
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceConfig {
    /// Enable AGT governance (tool policy, trust, audit).
    #[serde(default)]
    pub enabled: bool,
    /// Reference to a `ToolPolicy` CR in the **same namespace** as this
    /// `ClawSandbox`. Required — the controller resolves the target at
    /// reconcile time; missing target → `Degraded` with reason
    /// `ToolPolicyNotFound` (no inline-fallback path post-S13). The
    /// resolved CR's `metadata.name` is used as the ConfigMap name
    /// suffix (`toolpolicy-<name>-profile`) the sandbox pod mounts at
    /// `/etc/agt/policies`. Post-Slice-1e the AGT engine reads only
    /// from that mount; the legacy bundled `AGT_POLICY_PROFILE`
    /// env-var path has been removed.
    #[serde(default)]
    pub tool_policy_ref: LocalObjectRef,
    /// Minimum trust score (0-1000) for inter-agent communication.
    #[serde(default = "default_trust_threshold")]
    pub trust_threshold: i32,
    /// Pre-seeded trusted peer AMIDs (parent-verified, not self-reported).
    /// Format: "name:AMID,name:AMID,..."
    /// Set by the spawner to let the child auto-trust its parent and siblings.
    pub trusted_peers: Option<String>,
    /// Registry mode: "global" or "local" (default: "local").
    /// Global mode enables cross-cluster mesh communication and handoff tools.
    pub registry_mode: Option<String>,
    /// Reference to an `McpServer` CR in the **same namespace** that
    /// publishes the OAuth-protected MCP endpoint this sandbox should
    /// expose. When set, the controller mirrors the
    /// `mcp-{name}-jwks` ConfigMap and `mcp-{name}-signing` Secret
    /// produced by the McpServer reconciler into the sandbox namespace
    /// and mounts them into the inference-router container.
    ///
    /// Phase 3 S7 — wires the consumer side of the McpServer CRD.
    /// Optional: when omitted, the sandbox does not expose a
    /// customer-facing MCP endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_ref: Option<LocalObjectRef>,
}

fn default_trust_threshold() -> i32 {
    500
}

/// ClawSandbox status — reflects the current observed state.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClawSandboxStatus {
    /// Pending | Creating | Running | Failed | Terminating
    pub phase: Option<String>,
    pub sandbox_pod: Option<String>,
    pub namespace: Option<String>,
    pub inference_endpoint: Option<String>,
    pub tokens_used: Option<TokensUsed>,
    pub pending_approvals: Option<i32>,
    /// Foundry Agent ID created by the controller.
    pub foundry_agent_id: Option<String>,
    /// The runtime kind the controller observed for the current
    /// `observedGeneration`. Mirrors `spec.runtime.kind` once the
    /// reconciler has accepted it; consumers should interpret this
    /// alongside `observedGeneration` to detect stale observations
    /// (per S10.A1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_kind: Option<String>,
    /// The `metadata.generation` that produced this status. Consumers
    /// compare against `metadata.generation` to detect stale observations.
    /// See `controller/src/status/conditions.rs` for semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Standard K8s Condition list. Per convention, at most one entry per
    /// `type`. Helpers in `controller::status::conditions` maintain this
    /// list (upsert by type; preserve `lastTransitionTime` across same-
    /// status reconciles).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct TokensUsed {
    pub input: Option<i64>,
    pub output: Option<i64>,
}

// Default helpers
fn default_isolation() -> String {
    "enhanced".into()
}
fn default_seccomp() -> String {
    "azureclaw-strict".into()
}
fn default_selinux() -> String {
    String::new()
}
fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_isolation_is_enhanced() {
        let cfg = SandboxConfig::default();
        assert_eq!(cfg.isolation, "enhanced");
    }

    #[test]
    fn default_seccomp_is_azureclaw_strict() {
        let cfg = SandboxConfig::default();
        assert_eq!(cfg.seccomp_profile, "azureclaw-strict");
    }

    #[test]
    fn default_selinux_context_is_empty() {
        let cfg = SandboxConfig::default();
        assert!(cfg.selinux_context.is_empty());
    }

    #[test]
    fn default_writable_paths_include_sandbox_and_tmp() {
        let cfg = SandboxConfig::default();
        let paths = cfg.writable_paths.unwrap();
        assert!(paths.contains(&"/sandbox".to_string()));
        assert!(paths.contains(&"/tmp".to_string()));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn local_object_ref_round_trips() {
        let r = LocalObjectRef {
            name: "my-policy".into(),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v.get("name").and_then(|s| s.as_str()), Some("my-policy"));
        let back: LocalObjectRef = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn local_object_ref_default_is_empty_name() {
        let r = LocalObjectRef::default();
        assert!(r.name.is_empty());
    }

    #[test]
    fn default_network_policy_denies_all() {
        let cfg = NetworkPolicyConfig::default();
        assert!(cfg.default_deny);
        assert!(cfg.approval_required);
        assert!(cfg.allowed_endpoints.is_none());
        assert!(!cfg.learn_egress);
        assert!(cfg.allowlist_ref.is_none());
    }

    #[test]
    fn allowlist_ref_round_trips_through_camel_case_json() {
        // Wire-format hygiene: K8s uses camelCase, so `allowlistRef` /
        // `artifactType` must serialize as such (S12.a contract).
        let r = OciArtifactRef {
            registry: "myacr.azurecr.io".into(),
            repository: "azureclaw/policies/sandbox-foo".into(),
            digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            artifact_type: "application/vnd.azureclaw.egress-allowlist.v1+yaml".into(),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("registry").is_some());
        assert!(v.get("repository").is_some());
        assert!(v.get("digest").is_some());
        assert!(
            v.get("artifactType").is_some(),
            "must use camelCase artifactType"
        );
        let back: OciArtifactRef = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn allowlist_ref_omitted_when_none() {
        // S12.a: existing CRs without `allowlistRef` must round-trip
        // unchanged. `skip_serializing_if = "Option::is_none"` enforces this.
        let cfg = NetworkPolicyConfig::default();
        let v = serde_json::to_value(&cfg).unwrap();
        assert!(
            v.get("allowlistRef").is_none(),
            "default NetworkPolicyConfig must not emit allowlistRef field"
        );
    }

    #[test]
    fn default_governance_config() {
        let cfg = GovernanceConfig::default();
        assert!(!cfg.enabled);
        // S13: tool_policy is now a same-namespace ref to a ToolPolicy CR.
        // Default is an empty name (a sandbox spec with governance.enabled=true
        // MUST set toolPolicyRef.name; reconciler degrades on missing ref).
        assert!(cfg.tool_policy_ref.name.is_empty());
        let cfg_serde: GovernanceConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(cfg_serde.trust_threshold, 500);
    }

    #[test]
    fn sandbox_spec_fields_all_optional() {
        let spec = ClawSandboxSpec::default();
        // S10.A1: `runtime` replaces `openclaw`; default is OpenClaw kind
        // with an empty `OpenClawConfig` (so the spec round-trips through
        // tests that previously constructed `ClawSandboxSpec::default()`).
        assert_eq!(spec.runtime.kind, RuntimeKind::OpenClaw);
        assert!(spec.runtime.openclaw.is_some());
        assert!(spec.runtime.openai_agents.is_none());
        assert!(spec.runtime.microsoft_agent_framework.is_none());
        assert!(spec.runtime.byo.is_none());
        assert!(spec.sandbox.is_none());
        // S13: `inferenceRef` is a value-type required field. Default is
        // a `LocalObjectRef` with an empty name; admission CEL rejects
        // empty-name on apply.
        assert!(spec.inference_ref.name.is_empty());
        assert!(spec.network_policy.is_none());
        assert!(spec.agent.is_none());
        assert!(spec.governance.is_none());
        assert!(spec.azure_services.is_none());
        assert!(spec.resources.is_none());
    }

    #[test]
    fn sandbox_config_security_defaults() {
        let cfg = SandboxConfig::default();
        assert!(cfg.read_only_root_filesystem);
        assert!(cfg.run_as_non_root);
        assert!(!cfg.allow_privilege_escalation);
    }

    #[test]
    fn agent_config_defaults_empty() {
        let cfg = AgentConfig::default();
        assert!(cfg.instructions.is_none());
        assert!(cfg.tools.is_none());
        assert!(cfg.file_ids.is_none());
    }

    #[test]
    fn sandbox_status_defaults_empty() {
        let status = ClawSandboxStatus::default();
        assert!(status.phase.is_none());
        assert!(status.sandbox_pod.is_none());
        assert!(status.namespace.is_none());
        assert!(status.inference_endpoint.is_none());
        assert!(status.tokens_used.is_none());
        assert!(status.pending_approvals.is_none());
        assert!(status.foundry_agent_id.is_none());
        assert!(status.observed_generation.is_none());
        assert!(status.conditions.is_empty());
    }

    #[test]
    fn sandbox_status_omits_empty_conditions_and_absent_generation_in_json() {
        let status = ClawSandboxStatus::default();
        let v = serde_json::to_value(&status).unwrap();
        assert!(
            !v.as_object().unwrap().contains_key("conditions"),
            "empty conditions must not be emitted (would reset a populated status)"
        );
        assert!(
            !v.as_object().unwrap().contains_key("observedGeneration"),
            "None observedGeneration must be absent (would wipe a real value)"
        );
    }

    #[test]
    fn inference_ref_round_trips_via_camel_case() {
        // Wire-format hygiene: K8s uses camelCase. The ref field on the
        // ClawSandboxSpec must serialize as `inferenceRef`.
        let spec = ClawSandboxSpec {
            inference_ref: LocalObjectRef {
                name: "my-sandbox-inference".into(),
            },
            ..ClawSandboxSpec::default()
        };
        let v = serde_json::to_value(&spec).unwrap();
        assert!(
            v.get("inferenceRef").is_some(),
            "must use camelCase inferenceRef"
        );
        let ir = v.get("inferenceRef").unwrap();
        assert_eq!(
            ir.get("name").and_then(|n| n.as_str()),
            Some("my-sandbox-inference")
        );
    }

    #[test]
    fn governance_tool_policy_ref_serializes_camel_case() {
        let g = GovernanceConfig {
            enabled: true,
            tool_policy_ref: LocalObjectRef { name: "tp".into() },
            trust_threshold: 500,
            trusted_peers: None,
            registry_mode: None,
            mcp_server_ref: None,
        };
        let v = serde_json::to_value(&g).unwrap();
        assert!(
            v.get("toolPolicyRef").is_some(),
            "must use camelCase toolPolicyRef"
        );
    }

    // ─── S10.A1 RuntimeSpec tests ────────────────────────────────────

    #[test]
    fn runtime_kind_serializes_to_pascal_case_literals() {
        // Wire-format guarantees — these strings appear in CRD YAML, in
        // the print column, and (post-S10.A2) in admission CEL rules. Any
        // schemars/serde change that flipped the case would break
        // operators silently.
        assert_eq!(
            serde_json::to_string(&RuntimeKind::OpenClaw).unwrap(),
            r#""OpenClaw""#
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::OpenAIAgents).unwrap(),
            r#""OpenAIAgents""#
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::MicrosoftAgentFramework).unwrap(),
            r#""MicrosoftAgentFramework""#
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::SemanticKernel).unwrap(),
            r#""SemanticKernel""#
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::LangGraph).unwrap(),
            r#""LangGraph""#
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::Anthropic).unwrap(),
            r#""Anthropic""#
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::BYO).unwrap(),
            r#""BYO""#
        );
    }

    #[test]
    fn runtime_default_is_openclaw_with_empty_config() {
        let rt = RuntimeSpec::default();
        assert_eq!(rt.kind, RuntimeKind::OpenClaw);
        assert!(rt.openclaw.is_some());
        assert!(rt.openai_agents.is_none());
        assert!(rt.microsoft_agent_framework.is_none());
        assert!(rt.semantic_kernel.is_none());
        assert!(rt.lang_graph.is_none());
        assert!(rt.anthropic.is_none());
        assert!(rt.byo.is_none());
    }

    #[test]
    fn runtime_openclaw_round_trip() {
        let rt: RuntimeSpec = serde_json::from_value(serde_json::json!({
            "kind": "OpenClaw",
            "openclaw": {
                "image": "myregistry.azurecr.io/openclaw:1.2.3"
            }
        }))
        .unwrap();
        assert_eq!(rt.kind, RuntimeKind::OpenClaw);
        assert_eq!(
            rt.openclaw.as_ref().unwrap().image.as_deref(),
            Some("myregistry.azurecr.io/openclaw:1.2.3")
        );
    }

    #[test]
    fn runtime_openai_agents_round_trip() {
        let rt: RuntimeSpec = serde_json::from_value(serde_json::json!({
            "kind": "OpenAIAgents",
            "openaiAgents": {
                "pythonVersion": "3.12",
                "agentCode": {
                    "oci": { "image": "myregistry.azurecr.io/agent:1.0" }
                },
                "entrypoint": ["python", "-m", "agent"]
            }
        }))
        .unwrap();
        assert_eq!(rt.kind, RuntimeKind::OpenAIAgents);
        let cfg = rt.openai_agents.as_ref().unwrap();
        assert_eq!(cfg.python_version.as_deref(), Some("3.12"));
        let code = cfg.agent_code.as_ref().unwrap();
        assert!(code.oci.is_some());
        assert!(code.git.is_none());
    }

    #[test]
    fn runtime_microsoft_agent_framework_python_round_trip() {
        let rt: RuntimeSpec = serde_json::from_value(serde_json::json!({
            "kind": "MicrosoftAgentFramework",
            "microsoftAgentFramework": {
                "language": "python",
                "agentCode": {
                    "git": { "url": "https://github.com/contoso/agent.git", "ref": "main" }
                }
            }
        }))
        .unwrap();
        assert_eq!(rt.kind, RuntimeKind::MicrosoftAgentFramework);
        let cfg = rt.microsoft_agent_framework.as_ref().unwrap();
        assert_eq!(cfg.language, Some(MafLanguage::Python));
        let code = cfg.agent_code.as_ref().unwrap();
        assert!(code.git.is_some());
        assert_eq!(
            code.git.as_ref().unwrap().git_ref.as_deref(),
            Some("main"),
            "git ref must round-trip through the `ref` rename"
        );
    }

    #[test]
    fn runtime_byo_requires_contract_version() {
        // contractVersion is REQUIRED; a missing value must fail to deserialize
        // (silent default would defeat the declared-contract guard).
        let res: Result<RuntimeSpec, _> = serde_json::from_value(serde_json::json!({
            "kind": "BYO",
            "byo": { "image": "myregistry.azurecr.io/agent:1.0" }
        }));
        assert!(
            res.is_err(),
            "missing contractVersion must reject (rubber-duck #9)"
        );

        let rt: RuntimeSpec = serde_json::from_value(serde_json::json!({
            "kind": "BYO",
            "byo": {
                "image": "myregistry.azurecr.io/agent:1.0",
                "contractVersion": "v1"
            }
        }))
        .unwrap();
        assert_eq!(rt.kind, RuntimeKind::BYO);
        assert_eq!(rt.byo.as_ref().unwrap().contract_version, "v1");
    }

    #[test]
    fn runtime_serializes_only_set_variant() {
        // Default path: kind=OpenClaw + openclaw set, others omitted.
        let rt = RuntimeSpec::default();
        let v = serde_json::to_value(&rt).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("kind").and_then(|v| v.as_str()), Some("OpenClaw"));
        assert!(obj.contains_key("openclaw"));
        assert!(
            !obj.contains_key("openaiAgents"),
            "absent variants must be omitted from wire format"
        );
        assert!(!obj.contains_key("microsoftAgentFramework"));
        assert!(!obj.contains_key("semanticKernel"));
        assert!(!obj.contains_key("langGraph"));
        assert!(!obj.contains_key("anthropic"));
        assert!(!obj.contains_key("byo"));
    }

    // ─── Tier-2 placeholder runtime variants ──────────────────────────  // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10

    #[test]
    fn runtime_semantic_kernel_round_trip() {
        let rt: RuntimeSpec = serde_json::from_value(serde_json::json!({
            "kind": "SemanticKernel",
            "semanticKernel": {
                "language": "java",
                "agentCode": {
                    "oci": { "image": "contoso.azurecr.io/sk-agent:1.0" }
                }
            }
        }))
        .unwrap();
        assert_eq!(rt.kind, RuntimeKind::SemanticKernel);
        let cfg = rt.semantic_kernel.as_ref().unwrap();
        assert_eq!(cfg.language, Some(SkLanguage::Java));
        assert!(cfg.agent_code.as_ref().unwrap().oci.is_some());
    }

    #[test]
    fn runtime_lang_graph_round_trip() {
        let rt: RuntimeSpec = serde_json::from_value(serde_json::json!({
            "kind": "LangGraph",
            "langGraph": {
                "language": "typescript",
                "agentCode": {
                    "git": { "url": "https://github.com/contoso/lg-agent.git" }
                }
            }
        }))
        .unwrap();
        assert_eq!(rt.kind, RuntimeKind::LangGraph);
        let cfg = rt.lang_graph.as_ref().unwrap();
        assert_eq!(cfg.language, Some(LangGraphLanguage::Typescript));
        assert!(cfg.agent_code.as_ref().unwrap().git.is_some());
    }

    #[test]
    fn runtime_anthropic_round_trip() {
        let rt: RuntimeSpec = serde_json::from_value(serde_json::json!({
            "kind": "Anthropic",
            "anthropic": {
                "pythonVersion": "3.12",
                "agentCode": {
                    "oci": { "image": "contoso.azurecr.io/claude-agent:1.0" }
                }
            }
        }))
        .unwrap();
        assert_eq!(rt.kind, RuntimeKind::Anthropic);
        let cfg = rt.anthropic.as_ref().unwrap();
        assert_eq!(cfg.python_version.as_deref(), Some("3.12"));
    }

    #[test]
    fn runtime_tier2_placeholders_default_to_python() {
        // ci:stub-ok: Tier-2 roadmap stake — declared CRD variant per Phase 2 plan §S10
        // Defaults of new language enums must be `python` (most-common
        // flavour for each runtime) so omitting `language` doesn't
        // surprise authors with `dotnet`/`typescript`.
        assert_eq!(SkLanguage::default(), SkLanguage::Python);
        assert_eq!(LangGraphLanguage::default(), LangGraphLanguage::Python);
    }

    #[test]
    fn status_runtime_kind_is_optional_and_omitted_when_none() {
        let status = ClawSandboxStatus::default();
        let v = serde_json::to_value(&status).unwrap();
        assert!(
            !v.as_object().unwrap().contains_key("runtimeKind"),
            "None runtimeKind must be absent (would wipe a real value via merge patch)"
        );
        assert!(status.runtime_kind.is_none());
    }
}
