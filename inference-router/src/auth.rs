//! Azure authentication — supports both Workload Identity (AKS) and API key (local dev).
//!
//! In AKS: uses Workload Identity token exchange (federated OIDC → Azure AD token)
//! In dev: reads API key from /run/secrets/azure-openai-key (mounted by azureclaw dev)
//!
//! No API keys in the sandbox. The router handles all auth.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cached Azure AD token with expiry tracking.
struct CachedToken {
    access_token: String,
    expires_at: std::time::Instant,
}

/// Auth provider — automatically selects between API key and Workload Identity.
pub struct WorkloadIdentityAuth {
    client: reqwest::Client,
    token_cache: Arc<RwLock<Option<CachedToken>>>,
    /// API key loaded from /run/secrets/ (dev mode fallback)
    api_key: Option<String>,
}

impl WorkloadIdentityAuth {
    pub fn new() -> Self {
        // Try to load API key from secret mount (dev mode)
        let api_key = std::fs::read_to_string("/run/secrets/azure-openai-key")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if api_key.is_some() {
            tracing::info!("Auth mode: API key from /run/secrets/ (dev mode)");
        } else {
            tracing::info!("Auth mode: Workload Identity (AKS mode)");
        }

        Self {
            client: reqwest::Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
            api_key,
        }
    }

    /// Get auth header value. Returns either an API key or a bearer token.
    /// The caller should use this as the `api-key` header for Azure OpenAI.
    pub async fn get_token(&self, resource: &str) -> Result<String> {
        // Dev mode: use API key directly
        if let Some(ref key) = self.api_key {
            return Ok(key.clone());
        }

        // AKS mode: use Workload Identity token exchange
        // Check cache first
        {
            let cache = self.token_cache.read().await;
            if let Some(ref cached) = *cache {
                if cached.expires_at > std::time::Instant::now() + std::time::Duration::from_secs(60) {
                    return Ok(cached.access_token.clone());
                }
            }
        }

        // Token expired or missing — exchange
        self.exchange_token(resource).await
    }

    /// Returns true if using API key auth (dev mode), false if Workload Identity.
    pub fn is_api_key_mode(&self) -> bool {
        self.api_key.is_some()
    }

    async fn exchange_token(&self, resource: &str) -> Result<String> {
        // Read projected service account token
        let sa_token_path = std::env::var("AZURE_FEDERATED_TOKEN_FILE")
            .unwrap_or_else(|_| "/var/run/secrets/azure/tokens/azure-identity-token".into());
        let sa_token = tokio::fs::read_to_string(&sa_token_path)
            .await
            .context("Failed to read service account token — is Workload Identity configured?")?;

        let tenant_id = std::env::var("AZURE_TENANT_ID")
            .context("AZURE_TENANT_ID not set")?;
        let client_id = std::env::var("AZURE_CLIENT_ID")
            .context("AZURE_CLIENT_ID not set")?;

        let token_url = format!(
            "https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token"
        );

        let resp = self
            .client
            .post(&token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("client_id", &client_id),
                ("assertion", &sa_token),
                ("scope", &format!("{resource}/.default")),
                (
                    "client_assertion_type",
                    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                ),
                ("client_assertion", &sa_token),
            ])
            .send()
            .await
            .context("Token exchange request failed")?;

        let body: serde_json::Value = resp.json().await?;
        let access_token = body["access_token"]
            .as_str()
            .context("No access_token in response")?
            .to_string();

        // Cache it
        let mut cache = self.token_cache.write().await;
        *cache = Some(CachedToken {
            access_token: access_token.clone(),
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(3500),
        });

        Ok(access_token)
    }
}
