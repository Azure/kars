//! ClawSandbox Custom Resource Definition.
//!
//! This is the Rust representation of the ClawSandbox CRD.
//! kube-rs derives the CRD schema, API bindings, and JSON schema automatically.

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
    /// through the inference router sidecar (no role assignment needed).
    pub azure_services: Option<Vec<AzureServiceConfig>>,

    /// Resource limits
    pub resources: Option<ResourceConfig>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct OpenClawConfig {
    pub version: Option<String>,
    pub image: Option<String>,
    pub config: Option<serde_json::Value>,
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
        let cfg_serde: GovernanceConfig =
            serde_json::from_value(serde_json::json!({})).unwrap();
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
    }

    #[test]
    fn inference_config_default_has_no_fallback() {
        let cfg = InferenceConfig::default();
        assert!(cfg.fallback.is_none());
        assert!(cfg.endpoint.is_none());
    }
}
