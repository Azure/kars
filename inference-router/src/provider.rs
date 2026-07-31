// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Multi-provider upstream resolution.
//!
//! Maps the `InferencePolicy` provider tags (`azure-openai` /
//! `anthropic` / `ollama` / `bedrock`) onto a concrete upstream
//! target: base URL shape + auth scheme. The provider tag travels in
//! the compiled policy JSON (`spec.provider`, and
//! `spec.modelPreference.*.provider`); the *credentials and endpoints*
//! come exclusively from the router's own environment / secret mounts
//! — the agent process never sees a provider API key, exactly as with
//! the Azure Workload Identity path.
//!
//! ## Resolution precedence
//!
//! 1. `modelPreference.primary.provider`, when it parses to a known
//!    tag — an explicit route preference wins over the policy-level
//!    default.
//! 2. `spec.provider` (policy-level default).
//! 3. `azure-openai` (absent / unknown tags — matches the pre-slice
//!    behaviour where provider tags were informational-only).
//!
//! `bedrock` is recognised but not yet implemented: a policy that
//! declares it gets an explicit 501-style error instead of a silent
//! reroute to Azure — declared intent must never be silently ignored.
//!
//! ## Failure semantics
//!
//! A resolvable provider with missing router-side configuration
//! (no `ANTHROPIC_API_KEY`, no `OLLAMA_ENDPOINT`) fails the request
//! closed with a specific, operator-actionable error. Falling back to
//! the Azure upstream would send prompts to a provider the operator
//! didn't select.

use crate::config::Config;

/// Providers the router can actually forward to today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderKind {
    /// Azure OpenAI / Foundry (also GitHub Models + Copilot via
    /// endpoint detection) — the Phase 1 substrate. Default.
    #[default]
    AzureOpenAI,
    /// Anthropic Messages API — native pass-through on
    /// `/v1/messages`; auth via `x-api-key` from the router-side
    /// secret.
    Anthropic,
    /// OpenAI-compatible Ollama server — pass-through on
    /// `/v1/chat/completions`; no auth.
    Ollama,
}

impl ProviderKind {
    /// Kebab-case wire tag, matching the controller-side
    /// `InferenceProvider::as_tag`.
    #[must_use]
    pub fn as_tag(&self) -> &'static str {
        match self {
            Self::AzureOpenAI => "azure-openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
        }
    }
}

/// Why a provider tag could not be turned into a forwardable upstream.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// Tag is recognised by the CRD schema but the router has no
    /// client for it yet (`bedrock`). HTTP mapping: 501.
    #[error("provider '{tag}' is not implemented by this router build")]
    Unimplemented { tag: String },
    /// Provider needs an endpoint the router was not configured with.
    /// HTTP mapping: 503 (operator config gap, not a caller bug).
    #[error(
        "provider '{provider}' selected by InferencePolicy but {env} is not configured on the router"
    )]
    MissingEndpoint {
        provider: &'static str,
        env: &'static str,
    },
    /// Provider needs a credential the router was not configured
    /// with. HTTP mapping: 503.
    #[error(
        "provider '{provider}' selected by InferencePolicy but no credential is configured ({env} or secret mount)"
    )]
    MissingCredential {
        provider: &'static str,
        env: &'static str,
    },
}

/// Parse a policy provider tag. `Ok(None)` means "no opinion" (empty
/// or unknown tag — logged by the caller, keeps the pre-slice
/// informational-only behaviour for tags like `gemini`).
/// `Err(Unimplemented)` is reserved for tags the CRD schema accepts
/// but the router cannot serve, so declared intent fails loudly.
pub fn parse_tag(tag: &str) -> Result<Option<ProviderKind>, ProviderError> {
    match tag.trim().to_ascii_lowercase().as_str() {
        "azure-openai" => Ok(Some(ProviderKind::AzureOpenAI)),
        "anthropic" => Ok(Some(ProviderKind::Anthropic)),
        "ollama" => Ok(Some(ProviderKind::Ollama)),
        "bedrock" => Err(ProviderError::Unimplemented {
            tag: "bedrock".into(),
        }),
        _ => Ok(None),
    }
}

/// Concrete upstream target after resolution. For non-Azure providers
/// this carries the endpoint (and credential) the proxy layer needs;
/// `AzureOpenAI` keeps the env-driven endpoint already present on
/// `UpstreamConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTarget {
    AzureOpenAI,
    Anthropic { endpoint: String, api_key: String },
    Ollama { endpoint: String },
}

impl ProviderTarget {
    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::AzureOpenAI => ProviderKind::AzureOpenAI,
            Self::Anthropic { .. } => ProviderKind::Anthropic,
            Self::Ollama { .. } => ProviderKind::Ollama,
        }
    }
}

/// Resolve the effective provider for a request.
///
/// `model_pref_tag` is `modelPreference.primary.provider` (when a
/// policy carries a model preference), `policy_tag` is the top-level
/// `spec.provider`. See module docs for precedence. Unknown tags log
/// at WARN and fall through to the next precedence level.
pub fn resolve(
    policy_tag: Option<&str>,
    model_pref_tag: Option<&str>,
    config: &Config,
) -> Result<ProviderTarget, ProviderError> {
    let kind = effective_kind(policy_tag, model_pref_tag)?;
    target_for(kind, config)
}

fn effective_kind(
    policy_tag: Option<&str>,
    model_pref_tag: Option<&str>,
) -> Result<ProviderKind, ProviderError> {
    if let Some(tag) = model_pref_tag.filter(|t| !t.trim().is_empty()) {
        match parse_tag(tag)? {
            Some(kind) => return Ok(kind),
            None => {
                tracing::warn!(
                    tag,
                    "InferencePolicy modelPreference.primary.provider tag not recognised — \
                     falling back to spec.provider / default"
                );
            }
        }
    }
    if let Some(tag) = policy_tag.filter(|t| !t.trim().is_empty()) {
        match parse_tag(tag)? {
            Some(kind) => return Ok(kind),
            None => {
                tracing::warn!(
                    tag,
                    "InferencePolicy spec.provider tag not recognised — using azure-openai"
                );
            }
        }
    }
    Ok(ProviderKind::AzureOpenAI)
}

fn target_for(kind: ProviderKind, config: &Config) -> Result<ProviderTarget, ProviderError> {
    match kind {
        ProviderKind::AzureOpenAI => Ok(ProviderTarget::AzureOpenAI),
        ProviderKind::Anthropic => {
            let api_key =
                config
                    .anthropic_api_key
                    .clone()
                    .ok_or(ProviderError::MissingCredential {
                        provider: "anthropic",
                        env: "ANTHROPIC_API_KEY",
                    })?;
            Ok(ProviderTarget::Anthropic {
                endpoint: config.anthropic_endpoint.clone(),
                api_key,
            })
        }
        ProviderKind::Ollama => {
            let endpoint =
                config
                    .ollama_endpoint
                    .clone()
                    .ok_or(ProviderError::MissingEndpoint {
                        provider: "ollama",
                        env: "OLLAMA_ENDPOINT",
                    })?;
            Ok(ProviderTarget::Ollama { endpoint })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RegistryMode};

    fn cfg(anthropic_key: Option<&str>, ollama: Option<&str>) -> Config {
        Config {
            port: 8443,
            foundry_endpoint: None,
            foundry_project_endpoint: None,
            azure_openai_endpoint: Some("https://contoso.openai.azure.com".into()),
            default_model: "gpt-4o-mini".into(),
            content_safety_enabled: false,
            prompt_shields_enabled: false,
            content_safety_endpoint: None,
            token_budget_daily: 0,
            token_budget_per_request: 0,
            registry_mode: RegistryMode::Local,
            registry_url: None,
            provider_override: None,
            anthropic_endpoint: "https://api.anthropic.com".into(),
            anthropic_api_key: anthropic_key.map(String::from),
            ollama_endpoint: ollama.map(String::from),
            openai_moderation_endpoint: "https://api.openai.com".into(),
            openai_moderation_api_key: None,
            openai_moderation_model: "omni-moderation-latest".into(),
        }
    }

    #[test]
    fn parse_recognises_supported_tags_case_insensitively() {
        assert_eq!(
            parse_tag("azure-openai").unwrap(),
            Some(ProviderKind::AzureOpenAI)
        );
        assert_eq!(
            parse_tag("Anthropic").unwrap(),
            Some(ProviderKind::Anthropic)
        );
        assert_eq!(parse_tag(" ollama ").unwrap(), Some(ProviderKind::Ollama));
    }

    #[test]
    fn parse_returns_none_for_unknown_tags() {
        assert_eq!(parse_tag("gemini").unwrap(), None);
        assert_eq!(parse_tag("").unwrap(), None);
        assert_eq!(parse_tag("Foundry").unwrap(), None);
    }

    #[test]
    fn parse_rejects_bedrock_as_unimplemented() {
        assert!(matches!(
            parse_tag("bedrock"),
            Err(ProviderError::Unimplemented { .. })
        ));
    }

    #[test]
    fn no_tags_resolves_to_azure() {
        let t = resolve(None, None, &cfg(None, None)).unwrap();
        assert_eq!(t, ProviderTarget::AzureOpenAI);
    }

    #[test]
    fn model_pref_tag_wins_over_policy_tag() {
        let t = resolve(
            Some("anthropic"),
            Some("azure-openai"),
            &cfg(Some("sk-x"), None),
        )
        .unwrap();
        assert_eq!(t, ProviderTarget::AzureOpenAI);
    }

    #[test]
    fn unknown_model_pref_tag_falls_back_to_policy_tag() {
        let t = resolve(Some("anthropic"), Some("gemini"), &cfg(Some("sk-x"), None)).unwrap();
        assert_eq!(
            t,
            ProviderTarget::Anthropic {
                endpoint: "https://api.anthropic.com".into(),
                api_key: "sk-x".into(),
            }
        );
    }

    #[test]
    fn anthropic_without_key_fails_closed() {
        assert!(matches!(
            resolve(Some("anthropic"), None, &cfg(None, None)),
            Err(ProviderError::MissingCredential {
                provider: "anthropic",
                ..
            })
        ));
    }

    #[test]
    fn ollama_without_endpoint_fails_closed() {
        assert!(matches!(
            resolve(Some("ollama"), None, &cfg(None, None)),
            Err(ProviderError::MissingEndpoint {
                provider: "ollama",
                ..
            })
        ));
    }

    #[test]
    fn ollama_with_endpoint_resolves() {
        let t = resolve(
            Some("ollama"),
            None,
            &cfg(None, Some("http://ollama.ollama.svc:11434")),
        )
        .unwrap();
        assert_eq!(
            t,
            ProviderTarget::Ollama {
                endpoint: "http://ollama.ollama.svc:11434".into()
            }
        );
    }

    #[test]
    fn bedrock_anywhere_is_unimplemented_not_silent() {
        assert!(matches!(
            resolve(Some("bedrock"), None, &cfg(None, None)),
            Err(ProviderError::Unimplemented { .. })
        ));
        assert!(matches!(
            resolve(None, Some("bedrock"), &cfg(None, None)),
            Err(ProviderError::Unimplemented { .. })
        ));
    }
}
