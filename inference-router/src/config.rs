// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Configuration loaded from environment variables.

use anyhow::{Context, Result};
use std::collections::HashMap;

/// A named upstream provider the router can route inference calls to,
/// distinct from the single "default" endpoint fields below. Populated from
/// `KARS_PROVIDER_<TAG>_ENDPOINT` (+ optional `_API_KEY`/`_TOKEN`) env vars
/// that the controller injects — one pair per provider configured on the
/// cluster's `kars-inference-providers` Secret (see
/// `docs/adr/0002-inference-endpoint-sourcing.md`). A sandbox may have
/// several of these simultaneously (e.g. Foundry AND GitHub Copilot both
/// configured); `InferencePolicy.modelPreference.primary.provider` selects
/// which one a given request actually uses — see
/// `routes::apply_model_preference_override`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEndpoint {
    pub tag: String,
    pub endpoint: String,
    /// Direct bearer/API key for this specific provider, when dev-mode auth
    /// is used (e.g. a GitHub Models PAT, or a second Azure OpenAI resource's
    /// key). `None` means "use the router's normal auth resolution for this
    /// endpoint" (Workload Identity / IMDS / sidecar / the single global dev
    /// key) — i.e. this provider rides on the SAME auth path the default
    /// provider already uses. Never logged; only ever read once at request
    /// time by `proxy::token_for_endpoint`.
    pub api_key: Option<String>,
}

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

    /// Additional named providers this sandbox's router can route to,
    /// beyond the single "default" endpoint above — parsed from
    /// `KARS_PROVIDER_<TAG>_ENDPOINT` (+ optional `_API_KEY`/`_TOKEN`) env
    /// vars. Keyed by tag (lowercase, hyphenated — e.g. "github-models",
    /// "foundry"). See `resolve_provider`.
    pub providers: HashMap<String, ProviderEndpoint>,
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
                .unwrap_or_else(|_| "false".into())
                .parse()
                .unwrap_or(false),

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

            providers: parse_providers_from_env(std::env::vars()),
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

    /// Resolve a named provider for cross-provider routing — the mechanism
    /// `InferencePolicy.modelPreference.primary.provider` (and `fallback[]`)
    /// uses to send THIS sandbox's inference calls to a provider other than
    /// its default. Returns `None` when the tag isn't configured on this
    /// sandbox (fail-open to the caller's existing default, never a 500).
    ///
    /// `"github-copilot"` is synthesized from the presence of
    /// `COPILOT_GITHUB_TOKEN` alone (the well-known Copilot endpoint needs no
    /// separate `KARS_PROVIDER_GITHUB_COPILOT_ENDPOINT` env var, and its auth
    /// is always the JWT-exchange path in `AppState.copilot`, never a raw
    /// key) — this keeps the existing single env var as the one source of
    /// truth for "is Copilot available here", instead of requiring both a
    /// legacy and a new env var to agree.
    pub fn resolve_provider(&self, tag: &str) -> Option<ProviderEndpoint> {
        if tag.eq_ignore_ascii_case("github-copilot")
            && std::env::var("COPILOT_GITHUB_TOKEN")
                .ok()
                .filter(|s| !s.is_empty())
                .is_some()
        {
            return Some(ProviderEndpoint {
                tag: "github-copilot".to_string(),
                endpoint: "https://api.githubcopilot.com".to_string(),
                api_key: None,
            });
        }
        self.providers.get(&tag.to_ascii_lowercase()).cloned()
    }
}

/// Parse `KARS_PROVIDER_<TAG>_ENDPOINT` (+ optional sibling `_API_KEY` /
/// `_TOKEN`) env var pairs into a tag → `ProviderEndpoint` map.
///
/// Tag extraction: `KARS_PROVIDER_GITHUB_MODELS_ENDPOINT` → tag
/// `"github-models"` (middle segment lowercased, underscores → hyphens).
/// Generic by design — adding a new provider kind needs a controller-side
/// secret entry, never a router code change.
fn parse_providers_from_env(
    vars: impl Iterator<Item = (String, String)>,
) -> HashMap<String, ProviderEndpoint> {
    let mut endpoints: HashMap<String, String> = HashMap::new();
    let mut keys: HashMap<String, String> = HashMap::new();
    for (name, value) in vars {
        if value.trim().is_empty() {
            continue;
        }
        let Some(rest) = name.strip_prefix("KARS_PROVIDER_") else {
            continue;
        };
        if let Some(tag_part) = rest.strip_suffix("_ENDPOINT") {
            endpoints.insert(tag_to_key(tag_part), value);
        } else if let Some(tag_part) = rest
            .strip_suffix("_API_KEY")
            .or_else(|| rest.strip_suffix("_TOKEN"))
        {
            keys.insert(tag_to_key(tag_part), value);
        }
    }
    endpoints
        .into_iter()
        .map(|(tag, endpoint)| {
            let api_key = keys.remove(&tag);
            (
                tag.clone(),
                ProviderEndpoint {
                    tag,
                    endpoint,
                    api_key,
                },
            )
        })
        .collect()
}

/// `GITHUB_MODELS` → `github-models`.
fn tag_to_key(tag_part: &str) -> String {
    tag_part.to_ascii_lowercase().replace('_', "-")
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
            providers: HashMap::new(),
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

    // ── Multi-provider resolution (KARS_PROVIDER_<TAG>_*) ───────────────────

    #[test]
    fn parses_provider_endpoint_and_matching_key() {
        let vars = vec![
            (
                "KARS_PROVIDER_GITHUB_MODELS_ENDPOINT".to_string(),
                "https://models.github.ai/inference".to_string(),
            ),
            (
                "KARS_PROVIDER_GITHUB_MODELS_TOKEN".to_string(),
                "ghp_test".to_string(),
            ),
            ("UNRELATED_VAR".to_string(), "ignored".to_string()),
        ];
        let providers = parse_providers_from_env(vars.into_iter());
        let p = providers.get("github-models").expect("parsed");
        assert_eq!(p.tag, "github-models");
        assert_eq!(p.endpoint, "https://models.github.ai/inference");
        assert_eq!(p.api_key.as_deref(), Some("ghp_test"));
    }

    #[test]
    fn parses_provider_endpoint_without_key() {
        let vars = vec![(
            "KARS_PROVIDER_FOUNDRY_ENDPOINT".to_string(),
            "https://contoso.services.ai.azure.com/api/projects/x".to_string(),
        )];
        let providers = parse_providers_from_env(vars.into_iter());
        let p = providers.get("foundry").expect("parsed");
        assert_eq!(p.api_key, None);
    }

    #[test]
    fn ignores_empty_provider_env_values() {
        let vars = vec![(
            "KARS_PROVIDER_FOUNDRY_ENDPOINT".to_string(),
            "".to_string(),
        )];
        let providers = parse_providers_from_env(vars.into_iter());
        assert!(providers.is_empty());
    }

    #[test]
    fn resolve_provider_finds_configured_tag() {
        let mut c = cfg(None);
        c.providers.insert(
            "github-models".to_string(),
            ProviderEndpoint {
                tag: "github-models".to_string(),
                endpoint: "https://models.github.ai/inference".to_string(),
                api_key: Some("ghp_test".to_string()),
            },
        );
        let resolved = c.resolve_provider("github-models").expect("resolved");
        assert_eq!(resolved.endpoint, "https://models.github.ai/inference");
    }

    #[test]
    fn resolve_provider_returns_none_when_not_configured() {
        let c = cfg(None);
        assert!(c.resolve_provider("azure-openai").is_none());
    }

    #[test]
    fn resolve_provider_is_case_insensitive() {
        let mut c = cfg(None);
        c.providers.insert(
            "foundry".to_string(),
            ProviderEndpoint {
                tag: "foundry".to_string(),
                endpoint: "https://contoso.services.ai.azure.com".to_string(),
                api_key: None,
            },
        );
        assert!(c.resolve_provider("Foundry").is_some());
    }

    // Note: resolve_provider("github-copilot") depends on the
    // COPILOT_GITHUB_TOKEN process-wide env var. Mutating process env from
    // parallel unit tests races with other tests reading it (e.g.
    // copilot_auth's own tests), so that branch is verified via the E2E
    // deployment check instead of a unit test here.
}
