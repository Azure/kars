//! Azure federated identity credential management via ARM REST API.
//!
//! When the controller reconciles a new ClawSandbox, it creates a federated
//! credential mapping `system:serviceaccount:{namespace}:sandbox` → the
//! AzureClaw managed identity. This allows sub-agent pods to use Workload
//! Identity for Foundry API access without manual `az` CLI intervention.

use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Configuration for federated credential creation (from env vars).
#[derive(Clone)]
pub struct FedCredConfig {
    pub subscription_id: String,
    pub tenant_id: String,
    pub client_id: String,
    pub identity_name: String,
    pub identity_resource_group: String,
    pub oidc_issuer_url: String,
    pub token_file: String,
    pub authority_host: String,
}

impl FedCredConfig {
    /// Build from environment variables injected by Workload Identity webhook.
    /// Returns None if required vars are missing (fedcred creation will be skipped).
    pub fn from_env() -> Option<Self> {
        let subscription_id = std::env::var("AZURE_SUBSCRIPTION_ID").ok()?;
        let tenant_id = std::env::var("AZURE_TENANT_ID").ok()?;
        let client_id = std::env::var("AZURE_CLIENT_ID").ok()?;
        let identity_name = std::env::var("IDENTITY_NAME").ok()?;
        let identity_resource_group = std::env::var("IDENTITY_RESOURCE_GROUP").ok()?;
        let oidc_issuer_url = std::env::var("OIDC_ISSUER_URL").ok()?;
        let token_file = std::env::var("AZURE_FEDERATED_TOKEN_FILE")
            .unwrap_or_else(|_| "/var/run/secrets/azure/tokens/azure-identity-token".into());
        let authority_host = std::env::var("AZURE_AUTHORITY_HOST")
            .unwrap_or_else(|_| "https://login.microsoftonline.com".into());

        Some(Self {
            subscription_id,
            tenant_id,
            client_id,
            identity_name,
            identity_resource_group,
            oidc_issuer_url,
            token_file,
            authority_host,
        })
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Cached ARM token with expiry tracking.
struct CachedToken {
    token: String,
    acquired_at: std::time::Instant,
}

/// Manages federated credential creation with token caching.
pub struct FedCredManager {
    config: FedCredConfig,
    http: reqwest::Client,
    cached_token: Arc<RwLock<Option<CachedToken>>>,
}

impl FedCredManager {
    pub fn new(config: FedCredConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            cached_token: Arc::new(RwLock::new(None)),
        }
    }

    /// Get an ARM access token, using cache if available (tokens valid ~1h, refresh at 50min).
    /// Tries WI token exchange first, falls back to IMDS (kubelet MI).
    async fn get_arm_token(&self) -> Result<String, String> {
        // Check cache
        {
            let cached = self.cached_token.read().await;
            if let Some(ref ct) = *cached {
                if ct.acquired_at.elapsed().as_secs() < 3000 {
                    return Ok(ct.token.clone());
                }
            }
        }

        // Try WI token exchange first
        match self.wi_arm_token().await {
            Ok(token) => return self.cache_token(token).await,
            Err(wi_err) => {
                tracing::warn!("WI ARM token failed ({wi_err}), trying IMDS fallback");
            }
        }

        // Fallback: IMDS (kubelet managed identity)
        let imds_client_id = std::env::var("IMDS_CLIENT_ID").unwrap_or_default();
        let mut url = "http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https://management.azure.com".to_string();
        if !imds_client_id.is_empty() {
            url.push_str(&format!("&client_id={imds_client_id}"));
        }

        let resp = self.http
            .get(&url)
            .header("Metadata", "true")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("IMDS ARM token request failed: {e}"))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("IMDS ARM token failed: {}", &body[..body.len().min(300)]));
        }

        let body: serde_json::Value = resp.json().await
            .map_err(|e| format!("IMDS ARM token parse failed: {e}"))?;
        let token = body["access_token"]
            .as_str()
            .ok_or_else(|| "No access_token in IMDS response".to_string())?
            .to_string();

        tracing::info!("ARM token acquired via IMDS fallback");
        self.cache_token(token).await
    }

    async fn cache_token(&self, token: String) -> Result<String, String> {
        let mut cached = self.cached_token.write().await;
        *cached = Some(CachedToken {
            token: token.clone(),
            acquired_at: std::time::Instant::now(),
        });
        Ok(token)
    }

    async fn wi_arm_token(&self) -> Result<String, String> {
        let sa_token = tokio::fs::read_to_string(&self.config.token_file)
            .await
            .map_err(|e| format!("Failed to read projected token at {}: {e}", self.config.token_file))?;

        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.config.authority_host.trim_end_matches('/'),
            self.config.tenant_id,
        );

        let resp = self.http
            .post(&url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.config.client_id),
                ("client_assertion_type", "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
                ("client_assertion", sa_token.trim()),
                ("scope", "https://management.azure.com/.default"),
            ])
            .send()
            .await
            .map_err(|e| format!("Token exchange request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Token exchange failed ({status}): {}", &body[..body.len().min(300)]));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse token response: {e}"))?;

        Ok(token_resp.access_token)
    }

    /// Create or update a federated identity credential for a sandbox namespace.
    /// Idempotent — PUT creates or updates.
    pub async fn ensure_federated_credential(
        &self,
        sandbox_name: &str,
        sandbox_namespace: &str,
    ) -> Result<(), String> {
        let token = self.get_arm_token().await?;

        // Fedcred name: must be DNS-safe, max 120 chars
        let cred_name = format!("azureclaw-{sandbox_name}");

        let url = format!(
            "https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/{}/federatedIdentityCredentials/{}?api-version=2023-01-31",
            self.config.subscription_id,
            self.config.identity_resource_group,
            self.config.identity_name,
            cred_name,
        );

        let body = serde_json::json!({
            "properties": {
                "audiences": ["api://AzureADTokenExchange"],
                "issuer": self.config.oidc_issuer_url,
                "subject": format!("system:serviceaccount:{sandbox_namespace}:sandbox"),
            }
        });

        let resp = self.http
            .put(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("ARM PUT fedcred failed: {e}"))?;

        let status = resp.status();
        if status.is_success() {
            tracing::info!(
                sandbox = %sandbox_name,
                namespace = %sandbox_namespace,
                "Federated credential '{cred_name}' created/updated",
            );
            Ok(())
        } else if status.as_u16() == 409 {
            // Already exists with same config — fine
            tracing::debug!(sandbox = %sandbox_name, "Federated credential already exists");
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(format!(
                "Failed to create federated credential ({status}): {}",
                &body[..body.len().min(300)]
            ))
        }
    }

    /// Delete a federated identity credential (cleanup on sandbox deletion).
    pub async fn delete_federated_credential(&self, sandbox_name: &str) -> Result<(), String> {
        let token = self.get_arm_token().await?;
        let cred_name = format!("azureclaw-{sandbox_name}");

        let url = format!(
            "https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/{}/federatedIdentityCredentials/{}?api-version=2023-01-31",
            self.config.subscription_id,
            self.config.identity_resource_group,
            self.config.identity_name,
            cred_name,
        );

        let resp = self.http
            .delete(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| format!("ARM DELETE fedcred failed: {e}"))?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 404 {
            tracing::info!(sandbox = %sandbox_name, "Federated credential deleted");
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(format!("Failed to delete federated credential ({status}): {}", &body[..body.len().min(300)]))
        }
    }

}
