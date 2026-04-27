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
    printcolumn = r#"{"name":"Model","type":"string","jsonPath":".spec.inference.model"}"#,
    printcolumn = r#"{"name":"Isolation","type":"string","jsonPath":".spec.sandbox.isolation"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClawSandboxSpec {
    /// OpenClaw configuration
    pub openclaw: Option<OpenClawConfig>,

    /// Sandbox security settings
    pub sandbox: Option<SandboxConfig>,

    /// Inference routing
    pub inference: Option<InferenceConfig>,

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

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InferenceConfig {
    /// azure-openai | azure-ai-foundry | self-hosted
    #[serde(default = "default_provider")]
    pub provider: String,
    pub endpoint: Option<String>,
    #[serde(default = "default_model")]
    pub model: String,
    pub fallback: Option<FallbackConfig>,
    #[serde(default = "default_true")]
    pub content_safety: bool,
    #[serde(default = "default_true")]
    pub prompt_shields: bool,
    pub token_budget: Option<TokenBudgetConfig>,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            provider: "azure-openai".into(),
            endpoint: None,
            model: "gpt-4.1".into(),
            fallback: None,
            content_safety: true,
            prompt_shields: true,
            token_budget: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct FallbackConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenBudgetConfig {
    pub daily: Option<i64>,
    pub per_request: Option<i64>,
}

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
}

impl Default for NetworkPolicyConfig {
    fn default() -> Self {
        Self {
            default_deny: true,
            approval_required: true,
            allowed_endpoints: None,
            learn_egress: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct EndpointConfig {
    pub host: String,
    pub port: Option<u16>,
    pub methods: Option<Vec<String>>,
    pub paths: Option<Vec<String>>,
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
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceConfig {
    /// Enable AGT governance (tool policy, trust, audit).
    #[serde(default)]
    pub enabled: bool,
    /// Policy profile name (references policies ConfigMap).
    #[serde(default = "default_policy")]
    pub tool_policy: String,
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
}

fn default_policy() -> String {
    "default".into()
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
fn default_provider() -> String {
    "azure-openai".into()
}
fn default_model() -> String {
    "gpt-4.1".into()
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
    fn default_model_is_gpt_4_1() {
        let cfg = InferenceConfig::default();
        assert_eq!(cfg.model, "gpt-4.1");
    }

    #[test]
    fn default_provider_is_azure_openai() {
        let cfg = InferenceConfig::default();
        assert_eq!(cfg.provider, "azure-openai");
    }

    #[test]
    fn default_inference_enables_content_safety() {
        let cfg = InferenceConfig::default();
        assert!(cfg.content_safety);
        assert!(cfg.prompt_shields);
    }

    #[test]
    fn default_inference_has_no_token_budget() {
        let cfg = InferenceConfig::default();
        assert!(cfg.token_budget.is_none());
    }

    #[test]
    fn default_network_policy_denies_all() {
        let cfg = NetworkPolicyConfig::default();
        assert!(cfg.default_deny);
        assert!(cfg.approval_required);
        assert!(cfg.allowed_endpoints.is_none());
        assert!(!cfg.learn_egress);
    }

    #[test]
    fn default_governance_config() {
        // GovernanceConfig derives Default (empty/zero), serde defaults apply on deserialization
        let cfg = GovernanceConfig::default();
        assert!(!cfg.enabled);
        // Serde default_policy() and default_trust_threshold() are used during deserialization only
        let cfg_serde: GovernanceConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(cfg_serde.tool_policy, "default");
        assert_eq!(cfg_serde.trust_threshold, 500);
        assert_eq!(cfg.tool_policy, ""); // derive Default gives empty string
    }

    #[test]
    fn sandbox_spec_fields_all_optional() {
        let spec = ClawSandboxSpec::default();
        assert!(spec.openclaw.is_none());
        assert!(spec.sandbox.is_none());
        assert!(spec.inference.is_none());
        assert!(spec.network_policy.is_none());
        assert!(spec.agent.is_none());
        assert!(spec.governance.is_none());
        assert!(spec.azure_services.is_none());
        assert!(spec.resources.is_none());
    }

    #[test]
    fn token_budget_config_defaults_to_none() {
        let cfg = TokenBudgetConfig::default();
        assert!(cfg.daily.is_none());
        assert!(cfg.per_request.is_none());
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
    fn inference_config_default_has_no_fallback() {
        let cfg = InferenceConfig::default();
        assert!(cfg.fallback.is_none());
        assert!(cfg.endpoint.is_none());
    }
}
