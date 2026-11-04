// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — Connect GitHub (keyless git write, §14).
//
// Per-principal self-service GitHub connection. The Bridge holds the shared kars
// GitHub App; each authenticated principal gets an isolated ConfigMap containing
// only its installation id, account, and repos. Tokens are minted at run time.

pub(crate) use crate::kars::github_connection_name as connection_config_map_name;
use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::{GitWriteConfig, LocalObjectRef};
use crate::state::AppState;

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

/// GitHub App metadata the browser needs to render "Connect GitHub".
#[derive(Debug, Serialize)]
pub struct GithubAppDto {
    /// Whether the operator has configured the shared kars GitHub App.
    pub configured: bool,
    /// The exact App store is enrolled and readable under current authority.
    /// False requires operator enrollment before accepting credentials.
    pub storage_ready: bool,
    /// The App's slug (from the GitHub API), used to build the install URL.
    pub slug: Option<String>,
    /// `https://github.com/apps/<slug>/installations/new` — where the user picks
    /// repos + installs. `None` when the App isn't configured.
    pub install_url: Option<String>,
}

/// The authenticated principal's connection state.
#[derive(Debug, Serialize)]
pub struct GithubConnectionDto {
    pub connected: bool,
    pub account: Option<String>,
    /// `owner/repo` full names the installation can reach — the repo picker set.
    pub repos: Vec<String>,
}

// ── GitHub App auth helpers ──────────────────────────────────────────────────

pub(crate) fn mint_app_jwt(app_id: &str, private_key_pem: &str) -> AppResult<String> {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    #[derive(Serialize)]
    struct Claims {
        iat: i64,
        exp: i64,
        iss: String,
    }
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        iat: now - 60,
        exp: now + 540,
        iss: app_id.to_string(),
    };
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| AppError::Upstream(format!("invalid GitHub App key: {e}")))?;
    encode(&Header::new(Algorithm::RS256), &claims, &key)
        .map_err(|e| AppError::Upstream(format!("failed to sign App JWT: {e}")))
}

#[allow(dead_code)]
async fn gh_get_legacy(url: &str, bearer: &str) -> AppResult<serde_json::Value> {
    let resp = reqwest::Client::new()
        .get(url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "kars-bridge")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("GitHub request failed: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::Upstream(format!("GitHub {status}: {body}")));
    }
    serde_json::from_str(&body).map_err(|e| AppError::Upstream(format!("bad GitHub JSON: {e}")))
}

/// Mint an installation access token (to list the installation's repos).
#[allow(dead_code)]
async fn installation_token_legacy(app_jwt: &str, installation_id: &str) -> AppResult<String> {
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {app_jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "kars-bridge")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .bearer_auth(app_jwt)
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("installation token failed: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "installation token {status}: {body}"
        )));
    }

    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| AppError::Upstream(e.to_string()))?;
    v.get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::Upstream("no token in installation response".into()))
}

async fn gh_get(url: &str, bearer: &str) -> AppResult<serde_json::Value> {
    let resp = reqwest::Client::new()
        .get(url)
        .bearer_auth(bearer)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "kars-bridge")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("GitHub request failed: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "GitHub request returned {status}"
        )));
    }
    serde_json::from_str(&body).map_err(|e| AppError::Upstream(format!("bad GitHub JSON: {e}")))
}

/// Mint an installation access token (to list the installation's repos).
pub(crate) async fn installation_token(app_jwt: &str, installation_id: &str) -> AppResult<String> {
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(app_jwt)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "kars-bridge")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("installation token failed: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "installation token request returned {status}"
        )));
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| AppError::Upstream(e.to_string()))?;
    value
        .get("token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::Upstream("no token in installation response".into()))
}

async fn list_installation_repos(app_jwt: &str, installation_id: &str) -> AppResult<Vec<String>> {
    let token = installation_token(app_jwt, installation_id).await?;
    let v = gh_get(
        "https://api.github.com/installation/repositories?per_page=100",
        &token,
    )
    .await?;
    Ok(v.get("repositories")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    r.get("full_name")
                        .and_then(|f| f.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default())
}

// ── Endpoints ────────────────────────────────────────────────────────────────

/// `GET /api/github/app` — is the shared App configured, and where to install it.
pub async fn get_app(State(state): State<AppState>) -> AppResult<Json<GithubAppDto>> {
    let cluster = require_cluster(&state)?;
    let setup = cluster.github_app_setup().await.map_err(AppError::from)?;
    let Some((app_id, key)) = setup.credentials else {
        return Ok(Json(GithubAppDto {
            configured: false,
            storage_ready: setup.storage_ready,
            slug: None,
            install_url: None,
        }));
    };
    // Fetch the slug from the App itself so the install URL is always correct.
    let slug = async {
        let jwt = mint_app_jwt(&app_id, &key).ok()?;
        let app = gh_get("https://api.github.com/app", &jwt).await.ok()?;
        app.get("slug")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    }
    .await;
    let install_url = slug
        .as_ref()
        .map(|s| format!("https://github.com/apps/{s}/installations/new"));
    Ok(Json(GithubAppDto {
        configured: true,
        storage_ready: setup.storage_ready,
        slug,
        install_url,
    }))
}

/// Operator-submitted App credentials. Never derive Debug for credential inputs.
#[derive(Deserialize)]
pub struct GithubAppRequest {
    pub app_id: String,
    pub private_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubManifestRequest {
    pub code: String,
}

const GITHUB_SETUP_RESPONSE_LIMIT: usize = 64 * 1024;
const MANIFEST_RECOVERY: &str = "GitHub App registration could not be confirmed. GitHub may already have created the App and consumed the code; do not retry this code. Refresh GitHub App settings first. To recover, open GitHub Settings → Developer settings → GitHub Apps, select the created App, generate a new private key, and use existing-App upload with its App ID and PEM file. Repair or enroll Bridge storage first if needed. Disconnect the configured App explicitly before setting up a replacement.";

fn manifest_failure() -> AppError {
    AppError::Rejected(MANIFEST_RECOVERY.into())
}

fn validate_manifest_code(code: &str) -> AppResult<()> {
    if code.is_empty()
        || code.len() > 128
        || !code
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(AppError::BadRequest(
            "A valid GitHub App registration code is required".into(),
        ));
    }
    Ok(())
}

fn github_setup_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| AppError::Upstream("GitHub setup client unavailable".into()))
}

/// Never format request errors: conversion URLs contain a one-use secret.
async fn github_setup_response(request: reqwest::RequestBuilder) -> AppResult<Vec<u8>> {
    let mut response = request
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "kars-bridge")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|_| AppError::Upstream("GitHub setup request failed".into()))?;
    if !response.status().is_success() {
        return Err(AppError::Upstream(
            "GitHub setup request was rejected".into(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > GITHUB_SETUP_RESPONSE_LIMIT as u64)
    {
        return Err(AppError::Upstream(
            "GitHub setup response exceeds its limit".into(),
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::Upstream("GitHub setup response could not be read".into()))?
    {
        if chunk.len() > GITHUB_SETUP_RESPONSE_LIMIT - body.len() {
            return Err(AppError::Upstream(
                "GitHub setup response exceeds its limit".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_manifest_credentials(body: &[u8]) -> AppResult<GithubAppRequest> {
    #[derive(Deserialize)]
    struct ConvertedApp {
        id: u64,
        pem: String,
    }
    if body.len() > GITHUB_SETUP_RESPONSE_LIMIT {
        return Err(manifest_failure());
    }
    let app: ConvertedApp = serde_json::from_slice(body).map_err(|_| manifest_failure())?;
    let pem = app.pem.trim();
    if app.id == 0
        || pem.len() > 16 * 1024
        || !pem.is_ascii()
        || pem.contains('\0')
        || !(pem.starts_with("-----BEGIN RSA PRIVATE KEY-----")
            && pem.ends_with("-----END RSA PRIVATE KEY-----")
            || pem.starts_with("-----BEGIN PRIVATE KEY-----")
                && pem.ends_with("-----END PRIVATE KEY-----"))
    {
        return Err(manifest_failure());
    }
    Ok(GithubAppRequest {
        app_id: app.id.to_string(),
        private_key: pem.to_string(),
    })
}

/// `POST /api/operator/github-app` — finish guided manifest registration. The
/// web callback validates signed session-bound state before submitting a code;
/// this endpoint independently requires an authenticated admin and ready store.
pub async fn register_app(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<GithubManifestRequest>,
) -> AppResult<Json<serde_json::Value>> {
    crate::auth::require_admin(&principal)?;
    validate_manifest_code(&req.code)?;
    let cluster = require_cluster(&state)?;
    let setup = cluster.github_app_setup().await.map_err(AppError::from)?;
    if !setup.storage_ready {
        return Err(AppError::CredentialAuthorityUnavailable);
    }
    if setup.credentials.is_some() {
        return Err(AppError::Conflict(
            "A GitHub App is already configured. Disconnect the configured App explicitly before setting up a replacement. No registration code was exchanged.".into(),
        ));
    }
    let (uid, version) = setup
        .store_identity
        .ok_or(AppError::CredentialAuthorityUnavailable)?;
    let client = github_setup_client()?;
    let body = github_setup_response(client.post(format!(
        "https://api.github.com/app-manifests/{}/conversions",
        req.code
    )))
    .await
    .map_err(|_| manifest_failure())?;
    let credentials = parse_manifest_credentials(&body)?;
    verify_and_store_app(
        cluster,
        &credentials.app_id,
        &credentials.private_key,
        Some((&uid, &version)),
    )
    .await
    .map_err(|_| manifest_failure())
}

/// `PUT /api/operator/github-app` — the operator's self-service setup for the
/// ONE shared kars GitHub App, after explicit operator store enrollment.
/// Checks storage authority, then verifies the submitted App id + key against
/// GitHub's `/app` endpoint BEFORE saving. The private key is stored in the
/// enrolled `kars-github-app` Secret and never read back into a response.
pub async fn put_app(
    State(state): State<AppState>,
    Json(req): Json<GithubAppRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let app_id = req.app_id.trim();
    let key = req.private_key.trim();
    if app_id.is_empty() || key.is_empty() {
        return Err(AppError::BadRequest(
            "app_id and private_key are required".into(),
        ));
    }
    if !app_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::BadRequest(
            "app_id must be the numeric App ID shown on the App's settings page".into(),
        ));
    }
    if !key.contains("PRIVATE KEY") {
        return Err(AppError::BadRequest(
            "private_key doesn't look like a PEM private key (expected a '-----BEGIN ... PRIVATE KEY-----' block) — paste the .pem file GitHub generated, not the App ID or webhook secret".into(),
        ));
    }

    // Do not verify a private key externally when storage cannot accept it.
    // The write adapter rechecks authority after GitHub verification as well.
    if !cluster
        .github_app_setup()
        .await
        .map_err(AppError::from)?
        .storage_ready
    {
        return Err(AppError::CredentialAuthorityUnavailable);
    }

    verify_and_store_app(cluster, app_id, key, None).await
}

async fn verify_and_store_app(
    cluster: &crate::kars::cluster::Cluster,
    app_id: &str,
    key: &str,
    expected: Option<(&str, &str)>,
) -> AppResult<Json<serde_json::Value>> {
    let jwt = mint_app_jwt(app_id, key).map_err(|_| {
        AppError::Rejected(
            "That doesn't parse as a valid RSA private key. Paste the exact contents of the .pem file GitHub generated when you created the App (Settings → Developer settings → GitHub Apps → your App → Generate a private key).".into(),
        )
    })?;
    let client = github_setup_client()?;
    let body = github_setup_response(client.get("https://api.github.com/app").bearer_auth(jwt))
        .await
        .map_err(|_| {
            AppError::Rejected(
                "GitHub could not verify these credentials — double check the App ID and that the private key belongs to this App and has not been deleted. GitHub supports multiple active private keys; generating another does not invalidate existing keys.".into(),
            )
        })?;
    #[derive(Deserialize)]
    struct VerifiedApp {
        id: u64,
        slug: Option<String>,
        name: Option<String>,
    }
    let app: VerifiedApp = serde_json::from_slice(&body)
        .map_err(|_| AppError::Rejected("GitHub returned invalid App metadata".into()))?;
    if app.id == 0 || Some(app.id) != app_id.parse::<u64>().ok() {
        return Err(AppError::Rejected(
            "GitHub returned a different App ID".into(),
        ));
    }
    cluster
        .save_github_app(app_id, key, expected)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "configured": true,
        "slug": app.slug,
        "name": app.name,
        "note": "Verified against GitHub and stored write-only — users can now connect their own installation from Workspace → Connections.",
    })))
}

/// `DELETE /api/operator/github-app` — clear the shared App credentials while
/// retaining the enrolled Secret's UID. Existing per-principal connections
/// become unusable until a new App is configured.
pub async fn delete_app(State(state): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    cluster
        .delete_secret("kars-system", "kars-github-app")
        .await
        .map_err(AppError::from)?;
    Ok(Json(serde_json::json!({ "configured": false })))
}

pub(crate) fn authorize_repo_set(
    requested: &[String],
    granted: &[String],
) -> AppResult<Vec<String>> {
    let granted = granted
        .iter()
        .map(|repo| (repo.trim().to_ascii_lowercase(), repo.trim().to_string()))
        .filter(|(key, _)| !key.is_empty())
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut authorized = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut denied = Vec::new();
    for repo in requested
        .iter()
        .map(|repo| repo.trim())
        .filter(|repo| !repo.is_empty())
    {
        let key = repo.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            continue;
        }
        match granted.get(&key) {
            Some(repo) => authorized.push(repo.clone()),
            None => denied.push(repo.to_string()),
        }
    }
    if !denied.is_empty() {
        return Err(AppError::Rejected(format!(
            "requested repositories are not granted by your GitHub connection: {}",
            denied.join(", ")
        )));
    }
    Ok(authorized)
}

fn git_write_config(principal_sub: &str, repos: Vec<String>) -> GitWriteConfig {
    GitWriteConfig {
        connection_config_map_ref: LocalObjectRef {
            name: connection_config_map_name(principal_sub),
        },
        repos,
    }
}

pub(crate) fn validate_bound_github_egress<'a>(
    hosts: impl Iterator<Item = &'a str>,
) -> AppResult<()> {
    let conflicts = hosts
        .filter(|host| crate::kars::github_egress::opaque_github_egress(host))
        .collect::<std::collections::BTreeSet<_>>();
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(AppError::Rejected(format!(
        "The selected repositories use protected GitHub access, but the Network settings also allow a direct connection to {}. Remove those entries from the package's Network settings (including any Team role settings), keep the repositories selected, then check readiness again. Kars will still access those repositories through its protected connection. Nothing was removed automatically.",
        conflicts.into_iter().collect::<Vec<_>>().join(", ")
    )))
}

pub(crate) async fn authorize_github_access(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    principal: &Principal,
    requested: Option<&[String]>,
    write: bool,
) -> AppResult<
    Option<(
        GitWriteConfig,
        crate::kars::credential_contract::GitHubBinding,
    )>,
> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    if requested.iter().all(|repo| repo.trim().is_empty()) {
        return Ok(None);
    }
    let connection_name = connection_config_map_name(&principal.sub);
    let (_, _, granted) = cluster
        .read_github_connection_result(ns, &connection_name)
        .await
        .map_err(|error| {
            AppError::Upstream(format!("GitHub connection authority unavailable: {error}"))
        })?
        .ok_or_else(|| {
            AppError::Rejected(
                "connect GitHub for your user before granting repository access".into(),
            )
        })?;
    let repos = authorize_repo_set(requested, &granted)?;
    if repos.is_empty() {
        return Ok(None);
    }
    let binding = cluster
        .github_connection_grant(ns, &principal.sub, repos.clone(), write)
        .await
        .map_err(|error| {
            AppError::Rejected(format!("Keyless GitHub authority unavailable: {error}"))
        })?;
    Ok(Some((git_write_config(&principal.sub, repos), binding)))
}

/// `GET /api/namespaces/{ns}/github/connection` — this principal's connection.
pub async fn get_connection(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
) -> AppResult<Json<GithubConnectionDto>> {
    let cluster = require_cluster(&state)?;
    let connection_name = connection_config_map_name(&principal.sub);
    match cluster
        .read_github_connection_result(&ns, &connection_name)
        .await
        .map_err(|error| {
            AppError::Upstream(format!("GitHub connection authority unavailable: {error}"))
        })? {
        Some((_, account, repos)) => Ok(Json(GithubConnectionDto {
            connected: true,
            account: Some(account),
            repos,
        })),
        None => Ok(Json(GithubConnectionDto {
            connected: false,
            account: None,
            repos: vec![],
        })),
    }
}

#[derive(Debug, Deserialize)]
pub struct ConnectRequest {
    /// Optional: the GitHub account/org login to bind (when the App has multiple
    /// installations). When omitted and there is exactly one installation, that
    /// one is used.
    #[serde(default)]
    pub account: Option<String>,
}

/// `POST /api/namespaces/{ns}/github/connect` — discover the installation the
/// user just created and store it for this authenticated principal. Localhost-friendly: no
/// webhook/redirect needed — after installing the App, the user clicks Connect
/// and the Bridge finds the installation via the App API.
pub async fn connect(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(req): Json<ConnectRequest>,
) -> AppResult<Json<GithubConnectionDto>> {
    let cluster = require_cluster(&state)?;
    let (app_id, key) = cluster
        .github_app_creds()
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or_else(|| {
            AppError::Rejected("the kars GitHub App isn't configured on this cluster".into())
        })?;
    let jwt = mint_app_jwt(&app_id, &key)?;
    let installs = gh_get(
        "https://api.github.com/app/installations?per_page=100",
        &jwt,
    )
    .await?;
    let arr = installs.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        return Err(AppError::Rejected(
            "no installations found — install the kars app on your repos first, then Connect"
                .into(),
        ));
    }
    // Pick the installation: by account when given, else the sole one.
    let chosen = if let Some(acct) = req.account.as_deref() {
        arr.iter().find(|i| {
            i.get("account")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .map(|l| l.eq_ignore_ascii_case(acct))
                .unwrap_or(false)
        })
    } else if arr.len() == 1 {
        arr.first()
    } else {
        return Err(AppError::Rejected(
            "multiple GitHub installations exist — specify which account to connect".into(),
        ));
    };
    let chosen = chosen.ok_or_else(|| AppError::Rejected("no matching installation".into()))?;
    let installation_id = chosen
        .get("id")
        .and_then(|i| i.as_i64())
        .map(|i| i.to_string())
        .ok_or_else(|| AppError::Upstream("installation has no id".into()))?;
    let account = chosen
        .get("account")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or_default()
        .to_string();
    let repos = list_installation_repos(&jwt, &installation_id)
        .await
        .unwrap_or_default();
    let connection_name = connection_config_map_name(&principal.sub);
    cluster
        .write_github_connection(&ns, &connection_name, &installation_id, &account, &repos)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(GithubConnectionDto {
        connected: true,
        account: Some(account),
        repos,
    }))
}

/// `DELETE /api/namespaces/{ns}/github/connection` — disconnect this principal.
pub async fn disconnect(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
) -> AppResult<Json<GithubConnectionDto>> {
    let cluster = require_cluster(&state)?;
    let connection_name = connection_config_map_name(&principal.sub);
    cluster
        .delete_github_connection(&ns, &connection_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(GithubConnectionDto {
        connected: false,
        account: None,
        repos: vec![],
    }))
}

#[cfg(test)]
#[path = "github_setup_tests.rs"]
mod github_setup_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_names_keep_the_original_raw_subject_and_eight_byte_digest() {
        for (subject, suffix) in [
            ("Alice", "3bc51062973c458d"),
            ("alice", "2bd806c97f0e00af"),
            (" Alice ", "f3a5bb89166b4962"),
        ] {
            let expected = format!("kars-github-connection-{suffix}");
            assert_eq!(connection_config_map_name(subject), expected);
            assert_eq!(crate::kars::github_connection_name(subject), expected);
        }
    }

    #[test]
    fn different_principals_have_different_non_identifying_names() {
        let alice = connection_config_map_name("immutable-alice-subject");
        let bob = connection_config_map_name("immutable-bob-subject");
        assert_ne!(alice, bob);
        assert_eq!(alice, connection_config_map_name("immutable-alice-subject"));
        assert_eq!(alice.len(), "kars-github-connection-".len() + 16);
        assert!(!alice.contains("alice"));
    }

    #[test]
    fn repo_authorization_isolated_to_the_selected_principal_grant() {
        let alice = vec!["org/alice-repo".to_string()];
        let bob = vec!["org/bob-repo".to_string()];
        assert!(authorize_repo_set(&["org/alice-repo".into()], &alice).is_ok());
        assert!(authorize_repo_set(&["org/alice-repo".into()], &bob).is_err());
    }

    #[test]
    fn repo_authorization_fails_if_any_requested_repo_is_outside_grant() {
        let granted = vec!["org/allowed".to_string()];
        let result =
            authorize_repo_set(&["org/allowed".into(), "org/not-allowed".into()], &granted);
        assert!(result.is_err());
    }

    #[test]
    fn git_write_reference_is_server_derived_from_principal() {
        let config = git_write_config("immutable-subject", vec!["org/repo".into()]);
        assert_eq!(
            config.connection_config_map_ref.name,
            connection_config_map_name("immutable-subject")
        );
    }
}
