//! Azure Workload Identity authentication.
//!
//! Acquires tokens via the Workload Identity token exchange flow:
//! 1. Read the projected service account token from the pod volume mount
//! 2. Exchange it for an Azure AD token via the token exchange endpoint
//! 3. Cache and refresh tokens automatically
//!
//! No API keys, no secrets, no env vars with credentials.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cached Azure AD token with expiry tracking.
struct CachedToken {
    access_token: String,
    expires_at: std::time::Instant,
}

/// Token provider using Workload Identity Federation.
pub struct WorkloadIdentityAuth {
    client: reqwest::Client,
    token_cache: Arc<RwLock<Option<CachedToken>>>,
}

impl WorkloadIdentityAuth {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Get a valid Azure AD token, refreshing if expired.
    pub async fn get_token(&self, resource: &str) -> Result<String> {
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
        let token = self.exchange_token(resource).await?;
        Ok(token)
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
