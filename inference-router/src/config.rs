// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Configuration loaded from environment variables.

use anyhow::{Context, Result};

/// Registry topology mode.
///
/// - `Local` (default): registry + relay + postgres are deployed alongside the agent
///   (Docker containers in dev, in-cluster services on AKS). Handoff is unavailable.
/// - `Global`: a shared registry is deployed externally. Both local and cloud agents
///   register there, enabling identity succession and cross-host handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryMode {
    /// Self-contained — registry/relay/postgres colocated with agent.
    Local,
    /// Shared external registry — enables handoff between hosts.
    Global,
}

impl std::fmt::Display for RegistryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Global => write!(f, "global"),
        }
    }
}

pub struct Config {
    /// Port to listen on (default: 8443)
    pub port: u16,

    /// Azure AI Foundry endpoint for inference (e.g. https://my-resource.openai.azure.com/)
    /// Falls back to AZURE_OPENAI_ENDPOINT for dev-mode compatibility.
    /// Sourced from helm values, NOT from CRs. See docs/adr/0002-inference-endpoint-sourcing.md.
    pub foundry_endpoint: Option<String>,

    /// Foundry project endpoint for standalone APIs: Memory Store, Foundry IQ, Agent Service.
    /// (e.g. https://my-resource.services.ai.azure.com/api/projects/my-project)
    /// Uses https://ai.azure.com token audience.
    pub foundry_project_endpoint: Option<String>,

    /// Legacy Azure OpenAI endpoint — used as fallback if FOUNDRY_ENDPOINT is not set.
    pub azure_openai_endpoint: Option<String>,

    /// Default model name
    pub default_model: String,

    /// Enable Foundry guardrail annotation parsing (default: true).
    /// When true, the router reads prompt_filter_results from Foundry
    /// responses and reports content flags to the AGT governance engine.
    #[allow(dead_code)]
    pub content_safety_enabled: bool,

    /// Legacy — Foundry guardrails (DefaultV2) run Prompt Shields automatically.
    #[allow(dead_code)]
    pub prompt_shields_enabled: bool,

    /// Legacy — no longer used (Foundry guardrails replace standalone API).
    #[allow(dead_code)]
    pub content_safety_endpoint: Option<String>,

    /// Daily token budget per sandbox (0 = unlimited)
    pub token_budget_daily: u64,

    /// Per-request token limit (0 = unlimited)
    pub token_budget_per_request: u64,

    /// Registry topology mode (local or global).
    pub registry_mode: RegistryMode,

    /// Registry URL (used in both modes — local points to colocated service,
    /// global points to the shared external registry).
    pub registry_url: Option<String>,

    /// Explicit provider override (from `KARS_PROVIDER` env var).
    /// When set to `"github-copilot"`, the router treats inference as
    /// Copilot-API-bound regardless of the configured endpoint URLs.
    /// Captured at config-load time so provider detection is a pure
    /// function on the `Config` struct (testable without env hacks).
    pub provider_override: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            port: std::env::var("ROUTER_PORT")
                .unwrap_or_else(|_| "8443".into())
                .parse()
                .context("ROUTER_PORT must be a valid port number")?,

            foundry_endpoint: std::env::var("FOUNDRY_ENDPOINT")
                .ok()
                .filter(|s| !s.is_empty()),
            foundry_project_endpoint: std::env::var("FOUNDRY_PROJECT_ENDPOINT")
                .ok()
                .filter(|s| !s.is_empty()),
            azure_openai_endpoint: std::env::var("AZURE_OPENAI_ENDPOINT")
                .ok()
                .filter(|s| !s.is_empty()),

            default_model: std::env::var("DEFAULT_MODEL")
                .or_else(|_| std::env::var("AZURE_OPENAI_DEPLOYMENT"))
                .or_else(|_| std::env::var("OPENCLAW_MODEL"))
                .unwrap_or_else(|_| "gpt-4.1".into()),

            content_safety_enabled: std::env::var("CONTENT_SAFETY_ENABLED")
                .unwrap_or_else(|_| "true".into())
                .parse()
                .unwrap_or(true),

            prompt_shields_enabled: std::env::var("PROMPT_SHIELDS_ENABLED")
                .unwrap_or_else(|_| "true".into())
                .parse()
                .unwrap_or(true),

            content_safety_endpoint: std::env::var("CONTENT_SAFETY_ENDPOINT").ok(),

            token_budget_daily: std::env::var("TOKEN_BUDGET_DAILY")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap_or(0),

            token_budget_per_request: std::env::var("TOKEN_BUDGET_PER_REQUEST")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap_or(0),

            registry_mode: match std::env::var("AGT_REGISTRY_MODE")
                .unwrap_or_else(|_| "local".into())
                .to_lowercase()
                .as_str()
            {
                "global" => RegistryMode::Global,
                _ => RegistryMode::Local,
            },

            registry_url: std::env::var("AGT_REGISTRY_URL")
                .ok()
                .filter(|s| !s.is_empty()),

            provider_override: std::env::var("KARS_PROVIDER")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_lowercase()),
        })
    }

    /// Returns true if any configured endpoint points at GitHub Models
    /// (a free, public, OpenAI-compatible inference service backed by a
    /// GitHub PAT). When this is true, the router skips Azure-specific
    /// URL rewriting (`/openai/v1/`) and Foundry-only routes return 501
    /// instead of failing with a confusing upstream error.
    pub fn is_github_models(&self) -> bool {
        let candidates = [
            self.azure_openai_endpoint.as_deref(),
            self.foundry_endpoint.as_deref(),
            self.foundry_project_endpoint.as_deref(),
        ];
        candidates
            .iter()
            .flatten()
            .any(|e| e.contains("models.github.ai") || e.contains("models.inference.ai.azure.com"))
    }

    /// Returns true when the configured endpoint points at the GitHub
    /// Copilot API (`api.githubcopilot.com`) OR when the explicit
    /// `KARS_PROVIDER=github-copilot` env var is set.
    ///
    /// In Copilot mode the proxy:
    /// - skips the Azure `/openai/v1/` path prefix,
    /// - exchanges the GitHub OAuth/PAT for a short-lived Copilot JWT
    ///   instead of using the raw token,
    /// - injects the Copilot integration headers (`Editor-Version`,
    ///   `Copilot-Integration-Id`, `Editor-Plugin-Version`),
    /// - forwards `/v1/messages` (Anthropic shape) and
    ///   `/v1/chat/completions` (OpenAI shape) natively without translation.
    pub fn is_github_copilot(&self) -> bool {
        if self.provider_override.as_deref() == Some("github-copilot") {
            return true;
        }
        let candidates = [
            self.azure_openai_endpoint.as_deref(),
            self.foundry_endpoint.as_deref(),
            self.foundry_project_endpoint.as_deref(),
        ];
        candidates
            .iter()
            .flatten()
            .any(|e| e.contains("api.githubcopilot.com"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(endpoint: Option<&str>) -> Config {
        Config {
            port: 8443,
            foundry_endpoint: None,
            foundry_project_endpoint: None,
            azure_openai_endpoint: endpoint.map(String::from),
            default_model: "gpt-4o-mini".into(),
            content_safety_enabled: false,
            prompt_shields_enabled: false,
            content_safety_endpoint: None,
            token_budget_daily: 0,
            token_budget_per_request: 0,
            registry_mode: RegistryMode::Local,
            registry_url: None,
            provider_override: None,
        }
    }

    fn cfg_with_provider(endpoint: Option<&str>, provider: Option<&str>) -> Config {
        let mut c = cfg(endpoint);
        c.provider_override = provider.map(String::from);
        c
    }

    #[test]
    fn detects_github_models_marketplace_endpoint() {
        assert!(cfg(Some("https://models.github.ai/inference")).is_github_models());
    }

    #[test]
    fn detects_legacy_github_models_endpoint() {
        assert!(cfg(Some("https://models.inference.ai.azure.com")).is_github_models());
    }

    #[test]
    fn does_not_match_foundry_endpoint() {
        assert!(!cfg(Some("https://contoso.services.ai.azure.com")).is_github_models());
    }

    #[test]
    fn does_not_match_legacy_aoai_endpoint() {
        assert!(!cfg(Some("https://contoso.openai.azure.com")).is_github_models());
    }

    #[test]
    fn returns_false_when_no_endpoint_set() {
        assert!(!cfg(None).is_github_models());
    }

    #[test]
    fn detects_github_copilot_endpoint() {
        assert!(cfg(Some("https://api.githubcopilot.com")).is_github_copilot());
    }

    #[test]
    fn detects_github_copilot_via_provider_override() {
        assert!(cfg_with_provider(None, Some("github-copilot")).is_github_copilot());
    }

    #[test]
    fn does_not_match_foundry_endpoint_for_copilot() {
        assert!(!cfg(Some("https://contoso.services.ai.azure.com")).is_github_copilot());
    }

    #[test]
    fn does_not_match_github_models_endpoint_for_copilot() {
        assert!(!cfg(Some("https://models.github.ai/inference")).is_github_copilot());
    }

    #[test]
    fn provider_override_does_not_affect_github_models_detection() {
        let c = cfg_with_provider(
            Some("https://contoso.openai.azure.com"),
            Some("github-copilot"),
        );
        assert!(!c.is_github_models());
    }
}
