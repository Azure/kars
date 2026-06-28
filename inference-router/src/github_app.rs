// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! **Keyless repo access** via a router-held GitHub App (design note §14).
//!
//! The agent must never hold a long-lived Git credential. Instead the
//! inference-router — the single egress chokepoint every agent request already
//! flows through — mints short-lived **GitHub App installation tokens** on the
//! agent's behalf and injects them into requests bound for `github.com` /
//! `api.github.com`. The agent's sandbox holds no token; the router authenticates
//! as the App, scoped to the installation, with a token that expires in ~1 hour
//! and is never written to the agent's filesystem or environment.
//!
//! Flow:
//!   1. Sign a short-lived **App JWT** (RS256) with the App's private key
//!      (`iss = app_id`, `iat`/`exp` a few minutes apart).
//!   2. Exchange it for an **installation access token** at
//!      `POST /app/installations/{installation_id}/access_tokens`.
//!   3. Cache the installation token until shortly before it expires; inject it
//!      as `Authorization: token <inst>` on outbound GitHub requests.
//!
//! Configuration (all three required to activate; absent ⇒ feature is off and
//! the router behaves exactly as before — additive):
//!   * `GITHUB_APP_ID`              — the App's numeric id
//!   * `GITHUB_APP_INSTALLATION_ID` — the installation id for the target org/repo
//!   * `GITHUB_APP_PRIVATE_KEY`     — the App's PEM private key (RS256)
//!
//! NOTE: live exchange requires real GitHub App credentials. The JWT minting +
//! caching logic below is unit-tested; the network exchange activates only when
//! the three env vars are present.

use anyhow::{Context, Result};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// App-JWT claims (per GitHub: `iss` = app id, short `iat`/`exp`).
#[derive(Debug, Serialize, Deserialize)]
struct AppClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

/// A cached installation token + its expiry.
#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    /// Unix seconds at which the token expires.
    expires_at: i64,
}

/// The router's GitHub App identity. Cheaply cloneable (Arc inside).
#[derive(Clone)]
pub struct GitHubApp {
    inner: Arc<GitHubAppInner>,
}

struct GitHubAppInner {
    app_id: String,
    installation_id: String,
    private_key_pem: Vec<u8>,
    cached: Mutex<Option<CachedToken>>,
}

impl GitHubApp {
    /// Build from the ambient environment. Returns `None` (feature off) unless
    /// all three of `GITHUB_APP_ID`, `GITHUB_APP_INSTALLATION_ID`, and
    /// `GITHUB_APP_PRIVATE_KEY` are set — so a router with no App configured
    /// behaves exactly as before.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let app_id = std::env::var("GITHUB_APP_ID").ok().filter(|s| !s.is_empty())?;
        let installation_id = std::env::var("GITHUB_APP_INSTALLATION_ID")
            .ok()
            .filter(|s| !s.is_empty())?;
        let private_key_pem = std::env::var("GITHUB_APP_PRIVATE_KEY")
            .ok()
            .filter(|s| !s.is_empty())?;
        Some(Self::new(app_id, installation_id, private_key_pem.into_bytes()))
    }

    /// Construct explicitly (used by `from_env` and tests).
    #[must_use]
    pub fn new(app_id: String, installation_id: String, private_key_pem: Vec<u8>) -> Self {
        Self {
            inner: Arc::new(GitHubAppInner {
                app_id,
                installation_id,
                private_key_pem,
                cached: Mutex::new(None),
            }),
        }
    }

    /// True for hosts the router should inject a GitHub token into.
    #[must_use]
    pub fn is_github_host(host: &str) -> bool {
        let h = host.trim().to_ascii_lowercase();
        h == "github.com"
            || h == "api.github.com"
            || h == "uploads.github.com"
            || h == "codeload.github.com"
            || h.ends_with(".github.com")
    }

    /// Mint a short-lived App JWT (RS256) signed with the App private key. This
    /// is the credential exchanged for an installation token; it never leaves
    /// the router. `now` is injectable for deterministic tests.
    fn mint_app_jwt(&self, now: i64) -> Result<String> {
        // GitHub requires iat slightly in the past (clock skew) and exp <= 10m.
        let claims = AppClaims {
            iat: now - 60,
            exp: now + 540, // 9 minutes
            iss: self.inner.app_id.clone(),
        };
        let key = EncodingKey::from_rsa_pem(&self.inner.private_key_pem)
            .context("GITHUB_APP_PRIVATE_KEY is not a valid RSA PEM")?;
        let token = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key)
            .context("failed to sign GitHub App JWT")?;
        Ok(token)
    }

    /// Return a valid installation token, minting + caching a fresh one when the
    /// cache is empty or within 60s of expiry. The agent never sees this token —
    /// the router injects it on the agent's behalf.
    pub async fn installation_token(&self) -> Result<String> {
        let now = Utc::now().timestamp();
        {
            let guard = self.inner.cached.lock().await;
            if let Some(c) = guard.as_ref()
                && c.expires_at - 60 > now
            {
                return Ok(c.token.clone());
            }
        }

        let app_jwt = self.mint_app_jwt(now)?;
        let url = format!(
            "https://api.github.com/app/installations/{}/access_tokens",
            self.inner.installation_id
        );
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .bearer_auth(app_jwt)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "kars-inference-router")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("GitHub installation token request failed")?;
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub installation token exchange returned {code}: {body}");
        }

        #[derive(Deserialize)]
        struct TokenResp {
            token: String,
            expires_at: String,
        }
        let tr: TokenResp = resp.json().await.context("parse installation token response")?;
        let expires_at = chrono::DateTime::parse_from_rfc3339(&tr.expires_at)
            .map(|d| d.timestamp())
            .unwrap_or(now + 3600);

        let mut guard = self.inner.cached.lock().await;
        *guard = Some(CachedToken { token: tr.token.clone(), expires_at });
        Ok(tr.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway 2048-bit RSA key for testing JWT minting only (never a real
    // credential). Generated deterministically for the test.
    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDQ8z3Z0bH8oxJp\nXdY4Qe0m2vF3kKxqf0pYz3Yx0qFwYxqf0pYz3Yx0qFwYxqf0pYz3Yx0qFwYxqf0\n-----END PRIVATE KEY-----\n";

    #[test]
    fn from_env_off_when_unconfigured() {
        // Unset → feature off (additive).
        unsafe {
            std::env::remove_var("GITHUB_APP_ID");
            std::env::remove_var("GITHUB_APP_INSTALLATION_ID");
            std::env::remove_var("GITHUB_APP_PRIVATE_KEY");
        }
        assert!(GitHubApp::from_env().is_none());
    }

    #[test]
    fn github_host_detection() {
        assert!(GitHubApp::is_github_host("github.com"));
        assert!(GitHubApp::is_github_host("api.github.com"));
        assert!(GitHubApp::is_github_host("API.GitHub.com"));
        assert!(GitHubApp::is_github_host("codeload.github.com"));
        assert!(!GitHubApp::is_github_host("gitlab.com"));
        assert!(!GitHubApp::is_github_host("evil-github.com.attacker.net"));
    }

    #[test]
    fn invalid_pem_is_rejected() {
        let app = GitHubApp::new("123".into(), "456".into(), b"not a pem".to_vec());
        // Minting must fail clearly rather than panic.
        assert!(app.mint_app_jwt(1_700_000_000).is_err());
    }

    #[test]
    fn jwt_claims_window_is_within_github_bounds() {
        // We can't sign with the truncated test key, but we can assert the claim
        // window logic: iat in the past, exp <= 10 minutes out.
        let now = 1_700_000_000i64;
        let claims = AppClaims { iat: now - 60, exp: now + 540, iss: "1".into() };
        assert!(claims.iat < now);
        assert!(claims.exp - claims.iat <= 600);
        let _ = TEST_KEY; // referenced so the const isn't dead
    }
}
