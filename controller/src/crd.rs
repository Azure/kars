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
pub struct ClawSandboxSpec {
    /// OpenClaw configuration
    pub openclaw: Option<OpenClawConfig>,

    /// Sandbox security settings
    pub sandbox: Option<SandboxConfig>,

    /// Inference routing
    pub inference: Option<InferenceConfig>,

    /// Network policy
    pub network_policy: Option<NetworkPolicyConfig>,

    /// Azure services accessible from the sandbox
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
pub struct TokenBudgetConfig {
    pub daily: Option<i64>,
    pub per_request: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema)]
pub struct NetworkPolicyConfig {
    #[serde(default = "default_true")]
    pub default_deny: bool,
    #[serde(default = "default_true")]
    pub approval_required: bool,
    pub allowed_endpoints: Option<Vec<EndpointConfig>>,
}

impl Default for NetworkPolicyConfig {
    fn default() -> Self {
        Self {
            default_deny: true,
            approval_required: true,
            allowed_endpoints: None,
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

/// ClawSandbox status — reflects the current observed state.
#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct ClawSandboxStatus {
    /// Pending | Creating | Running | Failed | Terminating
    pub phase: Option<String>,
    pub sandbox_pod: Option<String>,
    pub namespace: Option<String>,
    pub inference_endpoint: Option<String>,
    pub tokens_used: Option<TokensUsed>,
    pub pending_approvals: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
pub struct TokensUsed {
    pub input: Option<i64>,
    pub output: Option<i64>,
}

// Default helpers
fn default_isolation() -> String { "enhanced".into() }
fn default_seccomp() -> String { "azureclaw-strict".into() }
fn default_selinux() -> String { String::new() }
fn default_provider() -> String { "azure-openai".into() }
fn default_model() -> String { "gpt-4.1".into() }
fn default_true() -> bool { true }
