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
    pub foundry_resource_group: String,
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
        let foundry_resource_group = std::env::var("FOUNDRY_RESOURCE_GROUP")
            .unwrap_or_else(|_| identity_resource_group.clone());
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
            foundry_resource_group,
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

        // Exchange projected SA token for ARM token
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

        // Cache the token
        {
            let mut cached = self.cached_token.write().await;
            *cached = Some(CachedToken {
                token: token_resp.access_token.clone(),
                acquired_at: std::time::Instant::now(),
            });
        }

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

    /// Ensure the managed identity has the required RBAC roles on the Foundry resource.
    /// Called once at controller startup. Idempotent — uses deterministic GUIDs.
    ///
    /// Assigns:
    /// - Azure AI User (53ca6127...) — wildcard CognitiveServices data actions
    /// - Cognitive Services OpenAI User (5e0bd9bd...) — explicit chat completions
    pub async fn ensure_role_assignments(&self, foundry_endpoint: &str) -> Result<(), String> {
        // Derive Foundry account name from endpoint URL
        // e.g. https://azureclaw-foundry-services.services.ai.azure.com/api/projects/azureclaw
        //   → account name: azureclaw-foundry-services
        let account_name = foundry_endpoint
            .trim_start_matches("https://")
            .split('.')
            .next()
            .ok_or_else(|| format!("Cannot parse Foundry account name from: {foundry_endpoint}"))?;

        // Get the managed identity's principal (object) ID
        let token = self.get_arm_token().await?;
        let identity_url = format!(
            "https://management.azure.com/subscriptions/{}/resourceGroups/{}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/{}?api-version=2023-01-31",
            self.config.subscription_id,
            self.config.identity_resource_group,
            self.config.identity_name,
        );

        let resp = self.http
            .get(&identity_url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| format!("ARM GET identity failed: {e}"))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Failed to get managed identity: {}", &body[..body.len().min(300)]));
        }

        let identity: serde_json::Value = resp.json().await
            .map_err(|e| format!("Failed to parse identity response: {e}"))?;
        let principal_id = identity["properties"]["principalId"]
            .as_str()
            .ok_or_else(|| "Managed identity has no principalId".to_string())?;

        // Build the scope: /subscriptions/.../resourceGroups/.../providers/Microsoft.CognitiveServices/accounts/{name}
        let scope = format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.CognitiveServices/accounts/{}",
            self.config.subscription_id,
            self.config.foundry_resource_group,
            account_name,
        );

        // Role definitions to assign
        let roles = [
            ("53ca6127-db72-4b80-b1b0-d745d6d5456d", "Azure AI User"),
            ("5e0bd9bd-7b93-4f28-af87-19fc36ad61bd", "Cognitive Services OpenAI User"),
        ];

        for (role_id, role_name) in &roles {
            // Deterministic GUID based on scope + principal + role
            let assignment_name = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("{scope}/{principal_id}/{role_id}").as_bytes(),
            );

            let url = format!(
                "https://management.azure.com{}/providers/Microsoft.Authorization/roleAssignments/{}?api-version=2022-04-01",
                scope, assignment_name,
            );

            let body = serde_json::json!({
                "properties": {
                    "roleDefinitionId": format!("/subscriptions/{}/providers/Microsoft.Authorization/roleDefinitions/{}", self.config.subscription_id, role_id),
                    "principalId": principal_id,
                    "principalType": "ServicePrincipal",
                }
            });

            let token = self.get_arm_token().await?;
            let resp = self.http
                .put(&url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("ARM PUT role assignment failed: {e}"))?;

            let status = resp.status();
            if status.is_success() {
                tracing::info!(
                    role = %role_name,
                    principal = %principal_id,
                    "Role assignment created: {role_name} on {account_name}",
                );
            } else if status.as_u16() == 409 {
                tracing::debug!(role = %role_name, "Role assignment already exists");
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(
                    role = %role_name,
                    status = %status,
                    "Failed to create role assignment: {}",
                    &body[..body.len().min(300)]
                );
            }
        }

        Ok(())
    }
}
