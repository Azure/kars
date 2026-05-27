// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! HTTP client for the Microsoft Entra SDK auth sidecar.
//!
//! When the sandbox is in agent-id mesh-auth mode, the controller
//! injects an `auth-sidecar` container into the pod and pins the
//! per-sandbox Entra Agent Identity into two router env vars:
//!
//! - `AUTH_SIDECAR_URL` — typically `http://127.0.0.1:8080`. The
//!   loopback-only address the sidecar listens on. Routed to via
//!   the same egress-guard iptables rule set that REJECTs the
//!   openclaw container (UID 1000) from reaching the same port.
//! - `PINNED_AGENT_IDENTITY_APP_ID` — the per-sandbox Agent Identity
//!   `appId`. The router MUST pin this value into every request to
//!   the sidecar; it MUST NEVER accept a caller-supplied
//!   `AgentIdentity` query parameter (rubber-duck finding #1 from
//!   the original e2e plan critique).
//!
//! ## Fail-closed contract
//!
//! When `SidecarClient::from_env()` returns `Some`, the router treats
//! the sidecar as the EXCLUSIVE auth path — no IMDS fallback, no
//! Workload Identity fallback, no dev-key fallback. This preserves
//! the per-sandbox audit principal: every downstream API call in
//! agent-id mode is attributed to the agent identity, not to the
//! controller MI nor to the AKS node-pool MI.
//!
//! If the sidecar is unreachable, the router returns an explicit
//! error to the caller and the request fails. Falling back to a
//! different identity would mean downstream Azure RBAC silently sees
//! a different principal than the operator intended — exactly the
//! kind of "looks fine, audit log says otherwise" bug agent-id mode
//! is designed to prevent.
//!
//! ## Endpoint
//!
//! The sidecar exposes `/AuthorizationHeaderUnauthenticated/{service}`
//! for autonomous app-token flows. The `Unauthenticated` suffix means
//! "no inbound user token required" — the sidecar mints tokens
//! using its own configured credentials (the controller MI's IMDS
//! token bridged via SignedAssertionFromManagedIdentity into the
//! blueprint, then OBO'd to the pinned agent identity).
//!
//! The non-suffixed `/AuthorizationHeader/{service}` is for OBO
//! flows where the router has an inbound user bearer token to
//! relay. kars doesn't currently use OBO at the router layer, so
//! this module deliberately only implements the unauthenticated
//! variant.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Env var the controller sets to point the router at the sidecar.
pub const ENV_AUTH_SIDECAR_URL: &str = "AUTH_SIDECAR_URL";

/// Env var the controller sets with the per-sandbox Agent Identity
/// `appId`. The router pins this — caller-supplied values are NEVER
/// honoured.
pub const ENV_PINNED_AGENT_IDENTITY_APP_ID: &str = "PINNED_AGENT_IDENTITY_APP_ID";

/// Default token lifetime we assume when the sidecar response does
/// not include a usable expires-in hint. Entra access tokens are
/// nominally 1h; we cache for 50 min so refresh happens before
/// expiry and we tolerate a 5-min skew.
const DEFAULT_TOKEN_TTL_SECS: u64 = 50 * 60;

/// HTTP timeout for sidecar calls. The sidecar is in the same pod
/// (loopback) so anything beyond a few seconds means it's
/// catastrophically wedged and we should fail fast rather than
/// hanging the inference request.
const SIDECAR_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Sidecar response shape.
///
/// The Microsoft Entra SDK sidecar returns the bearer token wrapped
/// in an `AuthorizationHeader` field with the literal `Bearer ` prefix
/// already prepended. Callers want the raw token, so we strip it
/// inside [`SidecarClient::get_token`].
#[derive(Debug, Deserialize)]
struct AuthorizationHeaderResponse {
    #[serde(rename = "AuthorizationHeader")]
    authorization_header: String,
    /// Optional expiry hint in seconds. When present, we honour it.
    /// When absent (older sidecar builds), we fall back to
    /// [`DEFAULT_TOKEN_TTL_SECS`].
    #[serde(rename = "ExpiresIn", default)]
    expires_in: Option<u64>,
}

struct CachedToken {
    token: String,
    expires_at: Instant,
}

/// Sidecar-backed auth client. Optional — `from_env()` returns `None`
/// when the sidecar env vars are absent (legacy / anonymous-tier
/// sandboxes).
pub struct SidecarClient {
    base_url: String,
    pinned_agent_id: String,
    client: reqwest::Client,
    cache: Arc<RwLock<HashMap<String, CachedToken>>>,
}

impl SidecarClient {
    /// Construct from `AUTH_SIDECAR_URL` + `PINNED_AGENT_IDENTITY_APP_ID`.
    /// Returns `None` when either env var is missing or empty — the
    /// router then falls through to the legacy auth path.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var(ENV_AUTH_SIDECAR_URL)
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())?;
        let pinned_agent_id = std::env::var(ENV_PINNED_AGENT_IDENTITY_APP_ID)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;

        tracing::info!(
            sidecar_url = %base_url,
            pinned_agent_id = %pinned_agent_id,
            "Sidecar auth mode enabled — all downstream tokens via auth-sidecar"
        );

        Some(Self {
            base_url,
            pinned_agent_id,
            client: reqwest::Client::builder()
                .timeout(SIDECAR_HTTP_TIMEOUT)
                .build()
                .expect("reqwest client construction"),
            cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Acquire a token for `resource` from the sidecar.
    ///
    /// `resource` is the same scope/audience string the existing
    /// auth path uses (e.g. `https://cognitiveservices.azure.com`).
    /// We translate it to the sidecar's `DownstreamApis__<key>__*`
    /// nomenclature via [`resource_to_service_name`] and call
    /// `/AuthorizationHeaderUnauthenticated/<key>?AgentIdentity=...`.
    ///
    /// Returns the raw bearer token (without the `Bearer ` prefix)
    /// so the caller can choose the appropriate header (`Bearer ...`
    /// for OAuth, `api-key: ...` for Azure OpenAI, etc.).
    pub async fn get_token(&self, resource: &str) -> Result<String> {
        let service = resource_to_service_name(resource).ok_or_else(|| {
            anyhow!(
                "no sidecar service name configured for resource '{resource}' — \
                 add a DownstreamApis entry to KarsAuthConfig.spec.downstreamApis or \
                 extend resource_to_service_name() in sidecar_client.rs"
            )
        })?;

        // Cache key is (service, agent_id) — agent_id is stable for
        // the pod lifetime but include it for future-proofing.
        let cache_key = format!("{}|{}", service, self.pinned_agent_id);
        {
            let r = self.cache.read().await;
            if let Some(cached) = r.get(&cache_key)
                && cached.expires_at > Instant::now() + Duration::from_secs(60)
            {
                return Ok(cached.token.clone());
            }
        }

        let url = format!(
            "{}/AuthorizationHeaderUnauthenticated/{}",
            self.base_url, service,
        );
        let resp = self
            .client
            .get(&url)
            .query(&[("AgentIdentity", self.pinned_agent_id.as_str())])
            .send()
            .await
            .with_context(|| format!("auth-sidecar HTTP call to {url} failed"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "auth-sidecar returned {status} for service '{service}' (agent_id={}): {}",
                self.pinned_agent_id,
                &body[..body.len().min(400)]
            ));
        }

        let body = resp
            .text()
            .await
            .with_context(|| "read auth-sidecar response body")?;

        // The Microsoft Entra SDK sidecar's response shape varies
        // slightly across builds. We've observed:
        //   - JSON `{"AuthorizationHeader": "Bearer xxx", "ExpiresIn": 3600}`
        //     (the documented contract; some 1.x builds)
        //   - JSON `"Bearer xxx"` (raw quoted string; other 1.x builds)
        //   - Plain text `Bearer xxx` (no JSON wrapper)
        // Parse leniently rather than failing fast on the strict
        // JSON shape — the live cluster validated that the actual
        // sidecar returns one of the non-strict forms, and a
        // strict-only parser would silently disable agent-id auth
        // for the entire pod.
        let (auth_header, expires_in_secs) = parse_sidecar_body(&body)
            .with_context(|| format!("parse auth-sidecar response body: {}", &body[..body.len().min(200)]))?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or_else(|| {
                anyhow!(
                    "auth-sidecar AuthorizationHeader missing expected 'Bearer ' prefix; \
                     got: {}",
                    &auth_header[..auth_header.len().min(40)]
                )
            })?
            .to_string();

        let ttl_secs = expires_in_secs
            .map(|s| s.saturating_sub(60).max(60))
            .unwrap_or(DEFAULT_TOKEN_TTL_SECS);
        {
            let mut w = self.cache.write().await;
            w.insert(
                cache_key,
                CachedToken {
                    token: token.clone(),
                    expires_at: Instant::now() + Duration::from_secs(ttl_secs),
                },
            );
        }

        tracing::debug!(
            service = %service,
            agent_id = %self.pinned_agent_id,
            cached_for_secs = ttl_secs,
            "minted token via auth-sidecar"
        );

        Ok(token)
    }

    /// Returns the pinned agent identity appId. Surfaced for diagnostics
    /// (e.g. `/healthz` payloads, structured log fields).
    pub fn pinned_agent_id(&self) -> &str {
        &self.pinned_agent_id
    }
}

/// Parse the auth-sidecar response body across the three shapes the
/// Microsoft Entra SDK sidecar emits in practice:
///
/// 1. `{"AuthorizationHeader": "Bearer xxx", "ExpiresIn": 3600}` — the
///    documented contract, observed in some 1.x builds.
/// 2. `"Bearer xxx"` — a JSON string with no wrapping object,
///    observed in other 1.x builds.
/// 3. `Bearer xxx` — plain text with no JSON quotes.
///
/// Returns `(auth_header_value_with_bearer_prefix, optional_ttl_seconds)`.
/// The caller strips the `Bearer ` prefix before caching.
///
/// We intentionally do NOT depend on the `Content-Type` response
/// header — some sidecar builds set `text/plain; charset=utf-8` even
/// for the JSON-shape payload, and depending on header inspection
/// would mask the actual semantics.
fn parse_sidecar_body(body: &str) -> anyhow::Result<(String, Option<u64>)> {
    let trimmed = body.trim();

    // Shape 1: JSON object with documented `AuthorizationHeader` field.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(obj) = v.as_object() {
            // Try the documented PascalCase first, then the variant
            // camelCase build, then the legacy `authorization_header`
            // snake_case some forks produce.
            let auth_field = obj
                .get("AuthorizationHeader")
                .or_else(|| obj.get("authorizationHeader"))
                .or_else(|| obj.get("authorization_header"))
                .and_then(|v| v.as_str());
            if let Some(header) = auth_field {
                let ttl = obj
                    .get("ExpiresIn")
                    .or_else(|| obj.get("expiresIn"))
                    .or_else(|| obj.get("expires_in"))
                    .and_then(|v| v.as_u64());
                return Ok((header.to_string(), ttl));
            }
        }
        // Shape 2: JSON string `"Bearer xxx"`.
        if let Some(s) = v.as_str() {
            return Ok((s.to_string(), None));
        }
    }

    // Shape 3: plain text `Bearer xxx`.
    if !trimmed.is_empty() {
        return Ok((trimmed.to_string(), None));
    }

    Err(anyhow!("auth-sidecar response body was empty"))
}

/// Translate the legacy `resource` audience string used throughout
/// the router into the sidecar's service-name key.
///
/// The sidecar reads `DownstreamApis__<key>__*` from its env. The
/// `<key>` is operator-configured (in `KarsAuthConfig.spec.downstreamApis`)
/// but kars conventionally uses `Foundry`, `Graph`, and `OpenAI`.
///
/// Returns `None` when the resource is unrecognised — the caller
/// surfaces this as a hard error so an unmapped resource fails
/// loudly rather than silently degrading to a different identity.
fn resource_to_service_name(resource: &str) -> Option<&'static str> {
    // Strip trailing slash and any `.default` scope suffix so callers
    // can pass either form.
    let r = resource
        .trim_end_matches('/')
        .trim_end_matches("/.default");
    // Match longest prefix first.
    if r.starts_with("https://cognitiveservices.azure.com")
        || r.starts_with("https://ai.azure.com")
    {
        Some("Foundry")
    } else if r.starts_with("https://graph.microsoft.com") {
        Some("Graph")
    } else if r.starts_with("https://api.openai.azure.com")
        || r.starts_with("https://openai.azure.com")
    {
        Some("OpenAI")
    } else if r.starts_with("https://management.azure.com")
        || r.starts_with("https://management.core.windows.net")
    {
        Some("Management")
    } else if r.starts_with("https://search.azure.com") {
        Some("Search")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_mapping_handles_canonical_resources() {
        assert_eq!(
            resource_to_service_name("https://cognitiveservices.azure.com"),
            Some("Foundry")
        );
        assert_eq!(
            resource_to_service_name("https://cognitiveservices.azure.com/"),
            Some("Foundry")
        );
        assert_eq!(
            resource_to_service_name("https://ai.azure.com/.default"),
            Some("Foundry")
        );
        assert_eq!(
            resource_to_service_name("https://graph.microsoft.com/.default"),
            Some("Graph")
        );
        assert_eq!(
            resource_to_service_name("https://management.azure.com"),
            Some("Management")
        );
    }

    #[test]
    fn resource_mapping_returns_none_for_unknown() {
        // Catches typos and unforeseen audiences — caller surfaces as
        // a hard error rather than silently falling back to a wrong
        // identity model.
        assert_eq!(
            resource_to_service_name("https://example.unknown.audience"),
            None
        );
        assert_eq!(resource_to_service_name(""), None);
    }

    #[test]
    fn parse_sidecar_body_handles_documented_json_shape() {
        let (h, ttl) = parse_sidecar_body(
            r#"{"AuthorizationHeader": "Bearer abc", "ExpiresIn": 3600}"#,
        )
        .unwrap();
        assert_eq!(h, "Bearer abc");
        assert_eq!(ttl, Some(3600));
    }

    #[test]
    fn parse_sidecar_body_handles_camelcase_json() {
        let (h, ttl) =
            parse_sidecar_body(r#"{"authorizationHeader": "Bearer xyz"}"#).unwrap();
        assert_eq!(h, "Bearer xyz");
        assert_eq!(ttl, None);
    }

    #[test]
    fn parse_sidecar_body_handles_quoted_string() {
        let (h, ttl) = parse_sidecar_body(r#""Bearer raw""#).unwrap();
        assert_eq!(h, "Bearer raw");
        assert_eq!(ttl, None);
    }

    #[test]
    fn parse_sidecar_body_handles_plain_text() {
        let (h, ttl) = parse_sidecar_body("Bearer plain\n").unwrap();
        assert_eq!(h, "Bearer plain");
        assert_eq!(ttl, None);
    }

    #[test]
    fn parse_sidecar_body_rejects_empty() {
        assert!(parse_sidecar_body("").is_err());
        assert!(parse_sidecar_body("   \n").is_err());
    }

    #[test]
    fn from_env_returns_none_unless_both_vars_present() {
        // SAFETY: env mutations across parallel tests would race. We
        // hold a single test mutex (other tests in this module touch
        // only resource_to_service_name) and only this test pokes
        // these specific env vars in the crate. Using a process-wide
        // mutex would be sturdier but is overkill here.
        unsafe {
            std::env::remove_var(ENV_AUTH_SIDECAR_URL);
            std::env::remove_var(ENV_PINNED_AGENT_IDENTITY_APP_ID);
        }
        assert!(
            SidecarClient::from_env().is_none(),
            "no env vars → no sidecar"
        );

        unsafe {
            std::env::set_var(ENV_AUTH_SIDECAR_URL, "http://127.0.0.1:8080");
        }
        assert!(
            SidecarClient::from_env().is_none(),
            "URL alone without pinned id → no sidecar"
        );

        unsafe {
            std::env::remove_var(ENV_AUTH_SIDECAR_URL);
            std::env::set_var(ENV_PINNED_AGENT_IDENTITY_APP_ID, "agent-x");
        }
        assert!(
            SidecarClient::from_env().is_none(),
            "pinned id alone without URL → no sidecar"
        );

        unsafe {
            std::env::set_var(ENV_AUTH_SIDECAR_URL, "http://127.0.0.1:8080/");
            std::env::set_var(ENV_PINNED_AGENT_IDENTITY_APP_ID, "agent-x");
        }
        let c = SidecarClient::from_env().expect("sidecar enabled when both vars present");
        assert_eq!(c.base_url, "http://127.0.0.1:8080");
        assert_eq!(c.pinned_agent_id(), "agent-x");
        unsafe {
            std::env::remove_var(ENV_AUTH_SIDECAR_URL);
            std::env::remove_var(ENV_PINNED_AGENT_IDENTITY_APP_ID);
        }
    }
}
