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

        let base_url =
            std::env::var("AGT_SIDECAR_URL").unwrap_or_else(|_| DEFAULT_SIDECAR_URL.to_string());

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get, routing::post};
    use tokio::net::TcpListener;

    fn make_proxy(base_url: &str, enabled: bool) -> SidecarProxy {
        SidecarProxy {
            client: Client::new(),
            base_url: base_url.to_string(),
            enabled,
        }
    }

    async fn start_test_server(router: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[test]
    fn test_default_sidecar_url() {
        assert_eq!(DEFAULT_SIDECAR_URL, "http://127.0.0.1:8081");
    }

    #[test]
    fn test_sidecar_error_display_unreachable() {
        let err = SidecarError::Unreachable("connection refused".into());
        assert_eq!(err.to_string(), "sidecar unreachable: connection refused");
    }

    #[test]
    fn test_sidecar_error_display_bad_response() {
        let err = SidecarError::BadResponse("invalid json".into());
        assert_eq!(err.to_string(), "sidecar bad response: invalid json");
    }

    #[test]
    fn test_proxy_enabled_flag() {
        assert!(!make_proxy(DEFAULT_SIDECAR_URL, false).enabled);
        assert!(make_proxy(DEFAULT_SIDECAR_URL, true).enabled);
    }

    #[test]
    fn test_base_url_stored_correctly() {
        let proxy = make_proxy("http://10.0.0.5:9090", true);
        assert_eq!(proxy.base_url, "http://10.0.0.5:9090");
    }

    #[tokio::test]
    async fn test_forward_unreachable_returns_error() {
        let proxy = make_proxy("http://127.0.0.1:1", true);
        let result = proxy.forward("GET", "/evaluate", None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SidecarError::Unreachable(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_health_check_unreachable_returns_false() {
        let proxy = make_proxy("http://127.0.0.1:1", true);
        assert!(!proxy.health_check().await);
    }

    #[tokio::test]
    async fn test_forward_get_success() {
        let app = Router::new().route(
            "/healthz",
            get(|| async { Json(serde_json::json!({"status": "ok"})) }),
        );
        let base_url = start_test_server(app).await;
        let proxy = make_proxy(&base_url, true);

        let (status, body) = proxy.forward("GET", "/healthz", None).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn test_forward_post_with_body() {
        let app = Router::new().route(
            "/evaluate",
            post(|Json(body): Json<Value>| async move {
                Json(serde_json::json!({
                    "decision": "allow",
                    "input": body,
                }))
            }),
        );
        let base_url = start_test_server(app).await;
        let proxy = make_proxy(&base_url, true);

        let payload = serde_json::json!({"agent_id": "agent-1", "action": "read"});
        let (status, body) = proxy
            .forward("POST", "/evaluate", Some(&payload))
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["decision"], "allow");
        assert_eq!(body["input"]["agent_id"], "agent-1");
    }

    #[tokio::test]
    async fn test_health_check_success() {
        let app = Router::new().route(
            "/healthz",
            get(|| async { Json(serde_json::json!({"status": "healthy"})) }),
        );
        let base_url = start_test_server(app).await;
        let proxy = make_proxy(&base_url, true);
        assert!(proxy.health_check().await);
    }

    #[tokio::test]
    async fn test_forward_non_json_response_returns_bad_response() {
        let app = Router::new().route("/bad", get(|| async { "not json" }));
        let base_url = start_test_server(app).await;
        let proxy = make_proxy(&base_url, true);

        let result = proxy.forward("GET", "/bad", None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SidecarError::BadResponse(msg) => assert!(!msg.is_empty()),
            other => panic!("expected BadResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_forward_deny_response() {
        let app = Router::new().route(
            "/evaluate",
            post(|| async {
                (
                    axum::http::StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"decision": "deny", "reason": "policy violation"})),
                )
            }),
        );
        let base_url = start_test_server(app).await;
        let proxy = make_proxy(&base_url, true);

        let (status, body) = proxy
            .forward("POST", "/evaluate", Some(&serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(status, 403);
        assert_eq!(body["decision"], "deny");
        assert_eq!(body["reason"], "policy violation");
    }
}
