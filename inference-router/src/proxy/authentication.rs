// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! A named route cannot inherit credentials from the legacy default route.

use anyhow::{Context, Result};

use super::{
    UpstreamConfig, UpstreamCredential, endpoint_host, is_azure_ai_host, is_copilot_endpoint,
    is_local_inference_host, token_audience,
};
use crate::{auth::WorkloadIdentityAuth, copilot_auth::CopilotTokenCache, provider::ProviderKind};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub enum AuthenticationProvenance {
    #[default]
    LegacyDefault,
    /// An immutable Config provider ID, never its credential.
    Named { provider_id: String },
}

pub async fn token_for_endpoint(
    auth: &WorkloadIdentityAuth,
    copilot: Option<&CopilotTokenCache>,
    upstream: &UpstreamConfig,
) -> Result<String> {
    if let AuthenticationProvenance::Named { provider_id } = &upstream.authentication {
        if is_copilot_endpoint(&upstream.endpoint) {
            let token = upstream
                .provider_api_key
                .as_deref()
                .filter(|token| !token.trim().is_empty())
                .context("named Copilot provider requires its own GitHub seat token")?;
            return copilot
                .context("Copilot token cache is unavailable")?
                .get_jwt_for_provider(provider_id, token)
                .await;
        }
        return Ok(upstream.provider_api_key.clone().unwrap_or_default());
    }
    let endpoint = upstream.endpoint.as_str();
    if is_copilot_endpoint(endpoint) {
        return copilot
            .context("default Copilot endpoint requires COPILOT_GITHUB_TOKEN")?
            .get_jwt()
            .await;
    }
    if let Some(key) = upstream.provider_api_key.as_deref() {
        return Ok(key.to_string());
    }
    if is_local_inference_host(&endpoint_host(endpoint).unwrap_or_default()) {
        return Ok(String::new());
    }
    if !auth.is_api_key_mode() && !auth.is_sidecar_mode() {
        let host = endpoint_host(endpoint).unwrap_or_default();
        anyhow::ensure!(
            is_azure_ai_host(&host),
            "Refusing to send a Workload Identity / IMDS token to '{host}': configure the provider's own credential"
        );
    }
    auth.get_token(token_audience(endpoint)).await
}

pub async fn credential_for_upstream(
    auth: &WorkloadIdentityAuth,
    copilot: Option<&CopilotTokenCache>,
    upstream: &UpstreamConfig,
) -> Result<UpstreamCredential> {
    match upstream.provider {
        ProviderKind::AzureOpenAI => {
            // Preserve the historical default local-service exclusion.
            if upstream.authentication == AuthenticationProvenance::LegacyDefault
                && is_local_inference_host(&endpoint_host(&upstream.endpoint).unwrap_or_default())
            {
                return Ok(UpstreamCredential::None);
            }
            let token = token_for_endpoint(auth, copilot, upstream).await?;
            Ok(if token.is_empty() {
                UpstreamCredential::None
            } else {
                UpstreamCredential::Bearer(token)
            })
        }
        ProviderKind::Anthropic => upstream
            .api_key
            .clone()
            .map(UpstreamCredential::AnthropicApiKey)
            .context("Anthropic provider requires its own configured credential"),
        ProviderKind::Ollama => Ok(UpstreamCredential::None),
    }
}
