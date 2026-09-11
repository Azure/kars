// kars Bridge BFF — github helpers for engineering intake.

use super::{
    EngineeringSignal, EngineeringSignalResult, EngineeringSignalSyncState,
    GithubCodeScanningAlert, GithubDependabotAlert, GithubListError, GithubListResult, GithubPull,
    GithubRepositoryFeatures, GithubSecretScanningAlert, MAX_ALERTS_PER_SIGNAL, MAX_GITHUB_PAGES,
    MAX_OPEN_PRS_PER_REPO,
};

pub(super) async fn repository_features(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
) -> Option<GithubRepositoryFeatures> {
    client
        .get(format!("https://api.github.com/repos/{repo}"))
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(reqwest::header::USER_AGENT, "kars-bridge")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()
}

pub(super) fn unavailable_security_product(
    features: Option<&GithubRepositoryFeatures>,
    signal: EngineeringSignal,
    error: &GithubListError,
) -> Option<GithubListError> {
    let unsupported_signal = matches!(
        signal,
        EngineeringSignal::CodeScanningAlert | EngineeringSignal::SecretScanningAlert
    );
    let private_without_security_product =
        features.is_some_and(|repo| repo.private && repo.security_and_analysis.is_none());
    let unsupported_response = matches!(
        error.state,
        EngineeringSignalSyncState::Forbidden | EngineeringSignalSyncState::Unavailable
    );
    (unsupported_signal && private_without_security_product && unsupported_response).then(|| {
        GithubListError {
            state: EngineeringSignalSyncState::Unavailable,
            detail: format!(
                "{} is unavailable because GitHub Code Security / Secret Protection is not enabled or licensed for this private repository",
                match signal {
                    EngineeringSignal::CodeScanningAlert => "Code scanning",
                    EngineeringSignal::SecretScanningAlert => "Secret scanning",
                    _ => "Security scanning",
                }
            ),
        }
    })
}

pub(super) fn truncate_error(value: impl Into<String>) -> String {
    let value = value.into();
    value.chars().take(1000).collect()
}

pub(super) fn next_link(value: &str) -> Option<String> {
    value.split(',').find_map(|entry| {
        let mut sections = entry.trim().split(';');
        let url = sections.next()?.trim();
        if !sections.any(|section| section.trim() == r#"rel="next""#) {
            return None;
        }
        url.strip_prefix('<')?.strip_suffix('>').map(str::to_string)
    })
}

async fn github_get_paginated<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    token: &str,
    initial_url: String,
    label: &str,
    max_items: usize,
) -> Result<GithubListResult<T>, GithubListError> {
    let mut url = Some(initial_url);
    let mut items = Vec::new();
    let mut pages = 0;
    while let Some(current) = url.take() {
        pages += 1;
        let response = client
            .get(&current)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "kars-bridge")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| GithubListError {
                state: EngineeringSignalSyncState::Error,
                detail: format!("{label} request failed: {error}"),
            })?;
        let status = response.status();
        let next = response
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|value| value.to_str().ok())
            .and_then(next_link);
        let body = response.text().await.map_err(|error| GithubListError {
            state: EngineeringSignalSyncState::Error,
            detail: format!("{label} response could not be read: {error}"),
        })?;
        if !status.is_success() {
            return Err(GithubListError {
                state: match status.as_u16() {
                    403 => EngineeringSignalSyncState::Forbidden,
                    404 => EngineeringSignalSyncState::Unavailable,
                    _ => EngineeringSignalSyncState::Error,
                },
                detail: format!("{label} returned HTTP {status}"),
            });
        }
        let mut page = serde_json::from_str::<Vec<T>>(&body).map_err(|error| GithubListError {
            state: EngineeringSignalSyncState::Error,
            detail: format!("{label} returned invalid JSON: {error}"),
        })?;
        let remaining = max_items.saturating_sub(items.len());
        if page.len() > remaining {
            page.truncate(remaining);
        }
        items.extend(page);
        if next.is_some() && (items.len() >= max_items || pages >= MAX_GITHUB_PAGES) {
            return Ok(GithubListResult {
                items,
                truncated: true,
            });
        }
        url = next;
    }
    Ok(GithubListResult {
        items,
        truncated: false,
    })
}

#[allow(dead_code)]
async fn list_open_pulls_legacy(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
) -> Result<Vec<GithubPull>, String> {
    let url = format!(
        "https://api.github.com/repos/{repo}/pulls?state=open&per_page={MAX_OPEN_PRS_PER_REPO}"
    );
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "kars-bridge")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| format!("GitHub request for {repo} failed: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("GitHub response for {repo} could not be read: {e}"))?;
    if !status.is_success() {
        return Err(truncate_error(format!(
            "GitHub returned {status} while listing open pull requests for {repo}: {body}"
        )));
    }
    serde_json::from_str(&body)
        .map_err(|e| format!("GitHub returned invalid pull request data for {repo}: {e}"))
}

pub(super) async fn list_open_pulls(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
) -> Result<GithubListResult<GithubPull>, GithubListError> {
    github_get_paginated(
        client,
        token,
        format!("https://api.github.com/repos/{repo}/pulls?state=open&per_page=100"),
        &format!("listing open pull requests for {repo}"),
        MAX_OPEN_PRS_PER_REPO,
    )
    .await
}

pub(super) async fn list_dependabot_alerts(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
) -> Result<GithubListResult<GithubDependabotAlert>, GithubListError> {
    github_get_paginated(
        client,
        token,
        format!("https://api.github.com/repos/{repo}/dependabot/alerts?state=open&per_page=100"),
        &format!("Dependabot alerts for {repo}"),
        MAX_ALERTS_PER_SIGNAL,
    )
    .await
}

pub(super) async fn list_code_scanning_alerts(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
) -> Result<GithubListResult<GithubCodeScanningAlert>, GithubListError> {
    github_get_paginated(
        client,
        token,
        format!("https://api.github.com/repos/{repo}/code-scanning/alerts?state=open&per_page=100"),
        &format!("code scanning alerts for {repo}"),
        MAX_ALERTS_PER_SIGNAL,
    )
    .await
}

pub(super) async fn list_secret_scanning_alerts(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
) -> Result<GithubListResult<GithubSecretScanningAlert>, GithubListError> {
    github_get_paginated(
        client,
        token,
        format!(
            "https://api.github.com/repos/{repo}/secret-scanning/alerts?state=open&per_page=100"
        ),
        &format!("secret scanning alerts for {repo}"),
        MAX_ALERTS_PER_SIGNAL,
    )
    .await
}

pub(super) fn signal_result(
    repo: &str,
    signal: EngineeringSignal,
    result: Result<usize, GithubListError>,
    truncation_detail: Option<String>,
) -> EngineeringSignalResult {
    match result {
        Ok(discovered) if truncation_detail.is_some() => EngineeringSignalResult {
            repo: repo.to_string(),
            signal,
            state: EngineeringSignalSyncState::Truncated,
            discovered,
            detail: truncation_detail.unwrap_or_default(),
        },
        Ok(discovered) => EngineeringSignalResult {
            repo: repo.to_string(),
            signal,
            state: EngineeringSignalSyncState::Ok,
            discovered,
            detail: if discovered == 0 {
                "Scanned successfully; no open items.".into()
            } else {
                format!("Scanned successfully; found {discovered} open item(s).")
            },
        },
        Err(error) => EngineeringSignalResult {
            repo: repo.to_string(),
            signal,
            state: error.state,
            discovered: 0,
            detail: error.detail,
        },
    }
}

pub(super) async fn github_get_json(
    client: &reqwest::Client,
    token: &str,
    url: &str,
) -> Result<serde_json::Value, String> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "kars-bridge")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| format!("GitHub request failed: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("GitHub response could not be read: {e}"))?;
    if !status.is_success() {
        return Err(truncate_error(format!("GitHub returned {status}: {body}")));
    }
    serde_json::from_str(&body).map_err(|e| format!("GitHub returned invalid JSON: {e}"))
}
