//! Registry verification — validates connecting agents are registered.
//!
//! When `REQUIRE_REGISTRATION=true` and `REGISTRY_URL` is set, the relay
//! calls the registry's lookup endpoint after Ed25519 signature verification
//! to confirm the AMID is actually registered. Unregistered or revoked agents
//! are rejected.

use serde::Deserialize;
use tracing::{info, warn, error, debug};
use std::sync::Arc;
use std::time::Duration;

use crate::types::Amid;

/// Registry verification client
#[derive(Debug, Clone)]
pub struct RegistryVerifier {
    /// HTTP client with connection pooling
    client: reqwest::Client,
    /// Base URL of the registry (e.g. "http://agentmesh-registry:8080")
    registry_url: String,
    /// Whether verification is enabled
    enabled: bool,
}

/// Lookup response from the registry
#[derive(Debug, Deserialize)]
struct LookupResponse {
    #[serde(default)]
    #[allow(dead_code)]
    amid: String,
    #[serde(default)]
    status: String,
}

impl RegistryVerifier {
    /// Create from environment variables
    pub fn from_env() -> Self {
        let registry_url = std::env::var("REGISTRY_URL")
            .unwrap_or_default();
        let require_registration = std::env::var("REQUIRE_REGISTRATION")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let enabled = require_registration && !registry_url.is_empty();

        if enabled {
            info!(
                "Registry verification enabled (registry: {})",
                registry_url
            );
        } else {
            info!("Registry verification disabled (local mode)");
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(4)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            registry_url,
            enabled,
        }
    }

    /// Check whether an AMID is registered and active.
    /// Returns Ok(()) if verification passes or is disabled.
    /// Returns Err(reason) if the agent is not registered or revoked.
    pub async fn verify_registered(&self, amid: &Amid) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        let url = format!(
            "{}/v1/registry/lookup?amid={}",
            self.registry_url,
            urlencoding::encode(amid)
        );

        debug!("Verifying registration for AMID {} at {}", amid, url);

        let response = self.client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                error!("Registry lookup request failed for {}: {}", amid, e);
                format!("Registry unavailable: {}", e)
            })?;

        let status = response.status();

        if status.is_success() {
            // Parse the response to check agent status
            match response.json::<LookupResponse>().await {
                Ok(lookup) => {
                    let agent_status = lookup.status.to_lowercase();
                    match agent_status.as_str() {
                        "active" | "online" | "away" | "offline" | "dnd" => {
                            debug!("AMID {} verified as registered (status: {})", amid, agent_status);
                            Ok(())
                        }
                        "revoked" => {
                            warn!("AMID {} is revoked — rejecting connection", amid);
                            Err("Agent registration has been revoked".to_string())
                        }
                        "dormant" => {
                            // Dormant agents are in handoff — allow connection
                            // (the successor might be reconnecting)
                            debug!("AMID {} is dormant (handoff in progress) — allowing", amid);
                            Ok(())
                        }
                        _ => {
                            debug!("AMID {} has status '{}' — allowing", amid, agent_status);
                            Ok(())
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to parse registry response for {}: {}", amid, e);
                    // Fail open on parse errors to avoid blocking legitimate agents
                    Ok(())
                }
            }
        } else if status.as_u16() == 404 {
            warn!("AMID {} not found in registry — rejecting connection", amid);
            Err("Agent not registered".to_string())
        } else {
            // For server errors (5xx), fail open to avoid cascading failures
            let body = response.text().await.unwrap_or_default();
            warn!(
                "Registry returned {} for AMID {}: {}",
                status, amid, body
            );
            if status.is_server_error() {
                debug!("Registry server error — failing open for {}", amid);
                Ok(())
            } else {
                Err(format!("Registry rejected agent: {}", status))
            }
        }
    }

    /// Whether verification is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[cfg(test)]
    fn new_disabled() -> Self {
        Self {
            client: reqwest::Client::new(),
            registry_url: String::new(),
            enabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_disabled_verifier_allows_all() {
        let verifier = RegistryVerifier::new_disabled();
        assert!(!verifier.is_enabled());

        // Any AMID should pass when verification is disabled
        let amid: Amid = "agent:alice@example.com".into();
        assert!(verifier.verify_registered(&amid).await.is_ok());

        let amid2: Amid = "agent:unknown@nowhere".into();
        assert!(verifier.verify_registered(&amid2).await.is_ok());
    }

    #[test]
    fn test_from_env_disabled_by_default() {
        // Clear relevant env vars to ensure default behavior
        std::env::remove_var("REGISTRY_URL");
        std::env::remove_var("REQUIRE_REGISTRATION");

        let verifier = RegistryVerifier::from_env();
        assert!(!verifier.is_enabled());
    }

    #[test]
    fn test_from_env_enabled() {
        std::env::set_var("REGISTRY_URL", "http://localhost:9999");
        std::env::set_var("REQUIRE_REGISTRATION", "true");

        let verifier = RegistryVerifier::from_env();
        assert!(verifier.is_enabled());

        // Clean up
        std::env::remove_var("REGISTRY_URL");
        std::env::remove_var("REQUIRE_REGISTRATION");
    }
}
