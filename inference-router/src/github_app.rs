// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Router-private, single-installation GitHub App authentication. No PAT fallback.

use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub(crate) const API: &str = "https://api.github.com";
pub(crate) const GIT: &str = "https://github.com";

/// Errors deliberately contain neither upstream bodies nor credential-bearing URLs.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("GitHub service configuration is invalid")]
    Configuration,
    #[error("GitHub repository is outside the installation scope")]
    Scope,
    #[error("GitHub authentication upstream is unavailable")]
    Upstream,
    #[error("GitHub response exceeds the service limit")]
    Limit,
}

pub(crate) fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|_| Error::Configuration)
}

pub(crate) async fn bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(Error::Limit);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Upstream)? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(Error::Limit);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn repository(value: &str) -> Option<String> {
    let mut parts = value.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let safe = |value: &str, max| {
        !value.is_empty()
            && value.len() <= max
            && !matches!(value, "." | "..")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    (parts.next().is_none() && safe(owner, 39) && safe(repo, 100))
        .then(|| value.to_ascii_lowercase())
}

#[derive(Serialize)]
struct Claims<'a> {
    iat: i64,
    exp: i64,
    iss: &'a str,
}

struct Cached {
    token: String,
    expires: i64,
}

pub(crate) struct GitHubApp {
    app_id: String,
    installation: u64,
    key: EncodingKey,
    repositories: Vec<String>,
    write: bool,
    client: reqwest::Client,
    api: String,
    // Cache belongs to this immutable credential incarnation. The mutex includes
    // minting to prevent concurrent misses from producing a token-exchange storm.
    cache: Mutex<BTreeMap<String, Cached>>,
}

impl GitHubApp {
    pub(crate) fn new(
        app_id: String,
        installation: u64,
        pem: &[u8],
        repositories: Vec<String>,
        write: bool,
        client: reqwest::Client,
    ) -> Result<Arc<Self>, Error> {
        if app_id.is_empty()
            || app_id.len() > 20
            || !app_id.bytes().all(|byte| byte.is_ascii_digit())
            || app_id.parse::<u64>().ok().is_none_or(|id| id == 0)
            || installation == 0
            || repositories.is_empty()
            || repositories.len() > 32
            || repositories
                .iter()
                .any(|repo| repository(repo).as_ref() != Some(repo))
        {
            return Err(Error::Configuration);
        }
        Ok(Arc::new(Self {
            app_id,
            installation,
            key: EncodingKey::from_rsa_pem(pem).map_err(|_| Error::Configuration)?,
            repositories,
            write,
            client,
            api: API.into(),
            cache: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(crate) fn allows(&self, repo: &str) -> bool {
        self.repositories.iter().any(|entry| entry == repo)
    }

    pub(crate) fn write_enabled(&self) -> bool {
        self.write
    }

    fn jwt(&self, now: i64) -> Result<String, Error> {
        jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iat: now - 60,
                exp: now + 540,
                iss: &self.app_id,
            },
            &self.key,
        )
        .map_err(|_| Error::Configuration)
    }

    fn request(&self, method: reqwest::Method, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.api))
            .bearer_auth(token)
            .header("User-Agent", "kars-inference-router")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, Error> {
        let response = request.send().await.map_err(|_| Error::Upstream)?;
        if !response.status().is_success() {
            return Err(Error::Upstream);
        }
        serde_json::from_slice(&bounded(response, 256 * 1024).await?).map_err(|_| Error::Upstream)
    }

    pub(crate) async fn token(&self, repo: &str) -> Result<String, Error> {
        if !self.allows(repo) {
            return Err(Error::Scope);
        }
        let mut cache = self.cache.lock().await;
        let now = Utc::now().timestamp();
        if let Some(cached) = cache.get(repo)
            && cached.expires > now + 60
        {
            return Ok(cached.token.clone());
        }
        cache.remove(repo);
        let jwt = self.jwt(now)?;
        #[derive(Deserialize)]
        struct Installation {
            id: u64,
            app_id: u64,
            suspended_at: Option<String>,
        }
        let installation: Installation = self
            .json(self.request(
                reqwest::Method::GET,
                &format!("/repos/{repo}/installation"),
                &jwt,
            ))
            .await?;
        if installation.id != self.installation
            || installation.app_id.to_string() != self.app_id
            || installation.suspended_at.is_some()
        {
            return Err(Error::Scope);
        }
        let permission = if self.write { "write" } else { "read" };
        let permissions = BTreeMap::from([
            ("actions".to_string(), "read".to_string()),
            ("checks".to_string(), "read".to_string()),
            ("contents".to_string(), permission.to_string()),
            ("issues".to_string(), permission.to_string()),
            ("metadata".to_string(), "read".to_string()),
            ("pull_requests".to_string(), permission.to_string()),
            ("statuses".to_string(), "read".to_string()),
        ]);
        #[derive(Deserialize)]
        struct Repo {
            full_name: String,
        }
        #[derive(Deserialize)]
        struct Token {
            token: String,
            expires_at: chrono::DateTime<Utc>,
            permissions: BTreeMap<String, String>,
            repositories: Vec<Repo>,
        }
        let minted: Token = self
            .json(
                self.request(
                    reqwest::Method::POST,
                    &format!("/app/installations/{}/access_tokens", self.installation),
                    &jwt,
                )
                .json(&serde_json::json!({
                    "repositories": [repo.split_once('/').ok_or(Error::Scope)?.1],
                    "permissions": permissions,
                })),
            )
            .await?;
        // GitHub must attest the FULL owner/repo, not just a same-named repo in
        // another installation. Reject omitted/broader permission provenance.
        if minted.repositories.len() != 1
            || repository(&minted.repositories[0].full_name).as_deref() != Some(repo)
            || minted.permissions != permissions
            || minted.expires_at.timestamp() <= now + 60
            || minted.expires_at.timestamp() > now + 3660
            || !(16..=4096).contains(&minted.token.len())
            || !minted
                .token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(Error::Scope);
        }
        let token = minted.token;
        cache.insert(
            repo.into(),
            Cached {
                token: token.clone(),
                expires: minted.expires_at.timestamp(),
            },
        );
        Ok(token)
    }

    pub(crate) async fn invalidate(&self, repo: &str, rejected_token: &str) {
        let mut cache = self.cache.lock().await;
        if cache
            .get(repo)
            .is_some_and(|cached| cached.token == rejected_token)
        {
            cache.remove(repo);
        }
    }
}

#[cfg(test)]
#[path = "github_app_tests.rs"]
pub(crate) mod tests;
