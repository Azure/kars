//! HTTP proxy client for the AGT governance sidecar.
//!
//! The sidecar runs at `localhost:8081` inside the same pod and provides
//! policy evaluation, trust management, and audit logging via REST.
//! When the AGT Rust SDK ships, this module is replaced with direct
//! crate calls — same `/agt/*` route contract, zero sidecar overhead.

use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

/// Default sidecar URL (same pod, loopback only).
const DEFAULT_SIDECAR_URL: &str = "http://127.0.0.1:8081";

/// Timeout for sidecar calls (local network — should be fast).
const SIDECAR_TIMEOUT: Duration = Duration::from_secs(3);

/// Proxy client for the AGT governance sidecar.
#[derive(Clone)]
pub struct SidecarProxy {
    client: Client,
    base_url: String,
    /// If true, sidecar is available and governance routes proxy to it.
    /// If false, governance routes return 503 (sidecar not configured).
    pub enabled: bool,
}

impl SidecarProxy {
    pub fn new(client: &Client) -> Self {
        let enabled = std::env::var("AGT_GOVERNANCE_ENABLED")
            .map(|v| v == "true")
            .unwrap_or(false);

        let base_url = std::env::var("AGT_SIDECAR_URL")
            .unwrap_or_else(|_| DEFAULT_SIDECAR_URL.to_string());

        tracing::info!(
            enabled,
            url = %base_url,
            "AGT sidecar proxy initialized"
        );

        Self {
            client: client.clone(),
            base_url,
            enabled,
        }
    }

    /// Forward a request to the sidecar and return its response verbatim.
    /// Returns (status_code, response_body).
    pub async fn forward(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), SidecarError> {
        let url = format!("{}{}", self.base_url, path);

        let mut req = match method {
            "POST" => self.client.post(&url),
            _ => self.client.get(&url),
        };

        req = req.timeout(SIDECAR_TIMEOUT);

        if let Some(json_body) = body {
            req = req.json(json_body);
        }

        let resp = req.send().await.map_err(|e| {
            tracing::warn!(url = %url, error = %e, "Sidecar request failed");
            SidecarError::Unreachable(e.to_string())
        })?;

        let status = resp.status().as_u16();
        let json = resp.json::<Value>().await.map_err(|e| {
            tracing::warn!(url = %url, error = %e, "Sidecar response parse failed");
            SidecarError::BadResponse(e.to_string())
        })?;

        Ok((status, json))
    }

    /// Health check — verify sidecar is reachable.
    pub async fn health_check(&self) -> bool {
        match self.forward("GET", "/healthz", None).await {
            Ok((200, _)) => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub enum SidecarError {
    /// Sidecar is not reachable (connection refused, timeout, etc.)
    Unreachable(String),
    /// Sidecar returned non-JSON or unparseable response.
    BadResponse(String),
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidecarError::Unreachable(e) => write!(f, "sidecar unreachable: {}", e),
            SidecarError::BadResponse(e) => write!(f, "sidecar bad response: {}", e),
        }
    }
}
