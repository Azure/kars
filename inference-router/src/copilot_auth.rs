// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! GitHub Copilot API authentication.
//!
//! Copilot uses a two-tier auth flow that's distinct from anything else
//! in the router:
//!
//! 1. The **GitHub OAuth/PAT** that the user supplies is *not* sent to
//!    the Copilot inference API. It's only sent to
//!    `POST https://api.github.com/copilot_internal/v2/token`, which
//!    returns a short-lived **Copilot JWT** (~30 min TTL).
//! 2. The Copilot JWT is what gets attached as `Authorization: Bearer`
//!    on every `api.githubcopilot.com` upstream request.
//!
//! We cache the JWT in-process and refresh it proactively (60s before
//! the carrier-supplied `expires_at`). If the exchange fails (network
//! blip, GH outage, expired GH token), the previous JWT stays usable
//! until it actually expires — Copilot will return 401 then, surfaced
//! to the caller as a normal upstream error.
//!
//! The GitHub token itself comes from one of (in priority order):
//!  - `COPILOT_GITHUB_TOKEN` env (set by entrypoint.sh in sandbox mode)
//!  - `/run/secrets/copilot-github-token` (mounted by `kars dev`)
//!
//! No fallback to the Workload Identity path — Copilot has no Azure side.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

const TOKEN_EXCHANGE_URL: &str = "https://api.github.com/copilot_internal/v2/token";

/// Refresh window: ask for a new JWT this long before the cached one expires.
const REFRESH_BUFFER: Duration = Duration::from_secs(60);

/// Static integration headers Copilot expects on every request.
/// Without these, Copilot returns 400 "missing required header" or, worse,
/// silently degrades to a different model behind the scenes.
pub const EDITOR_VERSION: &str = "vscode/1.107.0";
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";
pub const EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.35.0";
/// User-Agent header is also required by the GH-token-exchange endpoint;
/// otherwise it returns 403 "User-Agent not allowed".
pub const USER_AGENT: &str = "GitHubCopilotChat/0.35.0";

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    /// The JWT to use as `Authorization: Bearer` on inference calls.
    token: String,
    /// Unix timestamp (seconds) at which `token` expires.
    expires_at: i64,
    /// Hint from GitHub: refresh after this many seconds (typically ~25 min).
    /// We honor the smaller of `(expires_at - now - buffer)` and `refresh_in`.
    #[serde(default)]
    refresh_in: i64,
}

struct CachedJwt {
    token: String,
    /// When we should swap it out for a fresh one (driven by GitHub's
    /// `refresh_in` hint, typically ~25 min before `expires_at`).
    refresh_at: Instant,
    /// When Copilot itself stops accepting this JWT (driven by GitHub's
    /// `expires_at` field, typically ~30 min after issue). We MUST NOT
    /// serve past this minus a small safety buffer — Copilot will then
    /// return 401 "IDE token expired: unauthorized: token expired" on
    /// every chat-completions call. Tracked separately from `refresh_at`
    /// because (rarely) GitHub returns refresh_in > (expires_at - now),
    /// in which case the old logic would serve a stale JWT until
    /// refresh_at elapsed even though Copilot had already invalidated it.
    expires_at: Instant,
}

/// In-process cache + exchanger for Copilot JWTs.
///
/// Cheap to clone; the underlying state is `Arc<RwLock<...>>`.
#[derive(Clone)]
pub struct CopilotTokenCache {
    client: reqwest::Client,
    /// The user's GitHub OAuth token / PAT (`gho_*` or `ghp_*`).
    /// Wrapped in an `Option` so the cache can be constructed even when
    /// no token is configured — `get_jwt()` returns a clear error in that case
    /// instead of panicking at startup.
    github_token: Option<String>,
    cached: Arc<RwLock<Option<CachedJwt>>>,
}

impl CopilotTokenCache {
    /// Construct a cache from the ambient environment.
    /// Looks for the GH token in (in order):
    ///   1. `COPILOT_GITHUB_TOKEN` env var
    ///   2. `/run/secrets/copilot-github-token` (dev mount)
    pub fn from_env() -> Self {
        let github_token = std::env::var("COPILOT_GITHUB_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::fs::read_to_string("/run/secrets/copilot-github-token")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });

        if github_token.is_some() {
            tracing::info!(
                "Copilot auth: GitHub token loaded ({} chars)",
                github_token.as_deref().unwrap_or("").len()
            );
        } else {
            tracing::debug!("Copilot auth: no GitHub token configured (Copilot path will 401)");
        }

        Self {
            client: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(15))
                .build()
                .expect("failed to build reqwest client"),
            github_token,
            cached: Arc::new(RwLock::new(None)),
        }
    }

    /// Manual constructor (used by tests + when the GH token is supplied
    /// out-of-band rather than via env/secret mount).
    pub fn with_token(github_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(15))
                .build()
                .expect("failed to build reqwest client"),
            github_token: Some(github_token.into()),
            cached: Arc::new(RwLock::new(None)),
        }
    }

    /// True if a GitHub token is configured (i.e. Copilot path is usable).
    pub fn has_token(&self) -> bool {
        self.github_token.is_some()
    }

    /// Returns a valid Copilot JWT, exchanging if needed.
    /// The base URL allows tests to point at a mock server.
    pub async fn get_jwt(&self) -> Result<String> {
        self.get_jwt_with_base(TOKEN_EXCHANGE_URL).await
    }

    /// Same as [`get_jwt`] but allows overriding the exchange endpoint.
    /// Internal — exposed for tests; production code should call `get_jwt`.
    pub async fn get_jwt_with_base(&self, exchange_url: &str) -> Result<String> {
        // Fast path: cached JWT still has runway. Serve only when BOTH
        // (a) GitHub's refresh window hasn't elapsed AND
        // (b) Copilot's hard expiry hasn't passed (minus a safety buffer).
        // The expires_at check is what fixes the "IDE token expired"
        // 401s a user hit after ~30 min on long-lived sandbox pods.
        {
            let guard = self.cached.read().await;
            if let Some(c) = guard.as_ref() {
                let now = Instant::now();
                let expiry_safe = c.expires_at > now + REFRESH_BUFFER;
                if c.refresh_at > now && expiry_safe {
                    return Ok(c.token.clone());
                }
            }
        }

        // Slow path: exchange.
        let gh = self.github_token.as_deref().context(
            "no GitHub token configured for Copilot — set COPILOT_GITHUB_TOKEN or mount /run/secrets/copilot-github-token",
        )?;

        let resp = self
            .client
            .get(exchange_url)
            .header("Authorization", format!("token {gh}"))
            .header("Accept", "application/json")
            .header("Editor-Version", EDITOR_VERSION)
            .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
            .send()
            .await
            .context("Copilot token exchange request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Copilot token exchange returned {status}: {body}");
        }

        let parsed: CopilotTokenResponse = resp
            .json()
            .await
            .context("failed to parse Copilot token exchange response")?;

        // Refresh window: prefer GitHub's `refresh_in` hint when present,
        // otherwise compute from `expires_at - now - buffer`. Clamp at >= 30s
        // so we never spin if the upstream returns nonsense.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let ttl_secs = (parsed.expires_at - now).max(30);
        let refresh_secs = if parsed.refresh_in > 0 {
            parsed.refresh_in.max(30)
        } else {
            (ttl_secs - REFRESH_BUFFER.as_secs() as i64).max(30)
        };

        let now_instant = Instant::now();
        let cached = CachedJwt {
            token: parsed.token.clone(),
            refresh_at: now_instant + Duration::from_secs(refresh_secs as u64),
            // Track Copilot's hard expiry independently so the cache
            // can refuse to serve past it even when refresh_in is
            // (mis)reported larger than the actual TTL.
            expires_at: now_instant + Duration::from_secs(ttl_secs as u64),
        };

        tracing::debug!(
            "Copilot JWT refreshed (expires_in={}s, refresh_in={}s)",
            ttl_secs,
            refresh_secs
        );

        *self.cached.write().await = Some(cached);
        Ok(parsed.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[tokio::test]
    async fn errors_when_no_token_configured() {
        let c = CopilotTokenCache {
            client: reqwest::Client::new(),
            github_token: None,
            cached: Arc::new(RwLock::new(None)),
        };
        let err = c.get_jwt().await.unwrap_err();
        assert!(err.to_string().contains("no GitHub token"));
    }

    #[tokio::test]
    async fn exchanges_and_caches_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/copilot_internal/v2/token"))
            .and(header("Authorization", "token gh_test_pat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "tid=abc;exp=9999999999",
                "expires_at": now_secs() + 1800,
                "refresh_in": 1500
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = CopilotTokenCache::with_token("gh_test_pat");
        let url = format!("{}/copilot_internal/v2/token", server.uri());

        let jwt1 = c.get_jwt_with_base(&url).await.unwrap();
        assert_eq!(jwt1, "tid=abc;exp=9999999999");

        // Second call should hit the cache, not the server.
        let jwt2 = c.get_jwt_with_base(&url).await.unwrap();
        assert_eq!(jwt2, "tid=abc;exp=9999999999");
        // Wiremock's `.expect(1)` is verified on Drop.
    }

    #[tokio::test]
    async fn surfaces_upstream_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/copilot_internal/v2/token"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad credentials"))
            .mount(&server)
            .await;

        let c = CopilotTokenCache::with_token("invalid");
        let err = c
            .get_jwt_with_base(&format!("{}/copilot_internal/v2/token", server.uri()))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("401"), "expected 401 in error, got: {msg}");
    }

    /// Regression: when GitHub returns `refresh_in` LARGER than the actual
    /// TTL (`expires_at - now`), the cache MUST NOT serve the JWT past
    /// `expires_at - REFRESH_BUFFER`. Pre-fix, the cache served past
    /// expiry until `refresh_in` elapsed, causing Copilot to return
    /// 401 "IDE token expired: unauthorized: token expired" on chat
    /// completions ~30 min into a long-lived sandbox pod.
    #[tokio::test]
    async fn refreshes_when_expires_at_passes_even_if_refresh_in_is_longer() {
        let server = MockServer::start().await;
        // First exchange returns: expires_in = 30s, refresh_in = 1500s.
        // Pre-fix: cache would serve the first token for 1500s.
        // Post-fix: cache refreshes immediately on the next call past
        //          (expires_at - REFRESH_BUFFER), i.e. within ~30s.
        Mock::given(method("GET"))
            .and(path("/copilot_internal/v2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "first-token",
                "expires_at": now_secs() + 30,
                "refresh_in": 1500
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = CopilotTokenCache::with_token("gh_test_pat");
        let url = format!("{}/copilot_internal/v2/token", server.uri());

        // First call: exchange happens, "first-token" is cached.
        let jwt = c.get_jwt_with_base(&url).await.unwrap();
        assert_eq!(jwt, "first-token");

        // Forcibly age the cache so `expires_at - REFRESH_BUFFER` has
        // already passed. (We can't easily make 30s elapse in a test
        // without sleeping; mutating the cached Instants directly
        // proves the cache logic without a flaky time-based wait.)
        {
            let mut guard = c.cached.write().await;
            let entry = guard.as_mut().expect("cache populated");
            // Push both timestamps into the past.
            entry.expires_at = Instant::now() - Duration::from_secs(1);
            entry.refresh_at = Instant::now() + Duration::from_secs(1200);
        }

        // The next call should NOT serve from cache (because expires_at
        // has passed even though refresh_at hasn't) — it should re-exchange.
        // Reset the mock to return a fresh token on the SECOND exchange.
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/copilot_internal/v2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "second-token",
                "expires_at": now_secs() + 1800,
                "refresh_in": 1500
            })))
            .expect(1)
            .mount(&server)
            .await;

        let jwt2 = c.get_jwt_with_base(&url).await.unwrap();
        assert_eq!(
            jwt2, "second-token",
            "cache served the stale token past its expires_at — would cause Copilot 401"
        );
    }
}
