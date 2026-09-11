// kars Bridge BFF — config helpers for engineering intake.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::ResourceExt;

use crate::error::{AppError, AppResult};
use crate::providers::signing::sha256;
use crate::routes::github::authorize_repo_set;

use super::{
    CONFIG_KEY, CONNECTION_ANNOTATION, CURSOR_KEY, DEFAULT_POLL_INTERVAL_SECONDS,
    EngineeringCursor, EngineeringSignal, EngineeringSourceConfig, EngineeringSourceDto,
    EngineeringSourceStatus, EngineeringSyncState, MAX_POLL_INTERVAL_SECONDS, MAX_REPOS,
    MIN_POLL_INTERVAL_SECONDS, OWNER_ANNOTATION, PutEngineeringSourceRequest, STATUS_KEY,
    TEAM_NAME_ANNOTATION, TEAM_NAMESPACE_ANNOTATION,
};

pub(super) fn default_poll_interval() -> u64 {
    DEFAULT_POLL_INTERVAL_SECONDS
}

pub(super) fn default_true() -> bool {
    true
}

pub(crate) fn source_config_map_name(namespace: &str, team: &str) -> String {
    let digest = sha256(format!("{namespace}/{team}").as_bytes());
    let stem = team.chars().take(40).collect::<String>();
    format!("kars-eng-{stem}-{}", hex::encode(&digest[..6]))
}

pub(super) fn source_annotations(config: &EngineeringSourceConfig) -> BTreeMap<String, String> {
    BTreeMap::from([
        (OWNER_ANNOTATION.to_string(), config.owner_sub.clone()),
        (
            TEAM_NAMESPACE_ANNOTATION.to_string(),
            config.team_namespace.clone(),
        ),
        (TEAM_NAME_ANNOTATION.to_string(), config.team_name.clone()),
        (
            CONNECTION_ANNOTATION.to_string(),
            config.connection_config_map_ref.clone(),
        ),
    ])
}

pub(super) fn source_data(
    config: &EngineeringSourceConfig,
    cursor: &EngineeringCursor,
    status: &EngineeringSourceStatus,
) -> AppResult<BTreeMap<String, String>> {
    Ok(BTreeMap::from([
        (
            CONFIG_KEY.to_string(),
            serde_json::to_string(config).map_err(|e| AppError::Internal(e.into()))?,
        ),
        (
            CURSOR_KEY.to_string(),
            serde_json::to_string(cursor).map_err(|e| AppError::Internal(e.into()))?,
        ),
        (
            STATUS_KEY.to_string(),
            serde_json::to_string(status).map_err(|e| AppError::Internal(e.into()))?,
        ),
    ]))
}

pub(super) fn parse_source(
    cm: &ConfigMap,
) -> Result<
    (
        EngineeringSourceConfig,
        EngineeringCursor,
        EngineeringSourceStatus,
    ),
    String,
> {
    let data = cm
        .data
        .as_ref()
        .ok_or_else(|| "engineering source has no data".to_string())?;
    let config = serde_json::from_str::<EngineeringSourceConfig>(
        data.get(CONFIG_KEY)
            .ok_or_else(|| "engineering source is missing config.json".to_string())?,
    )
    .map_err(|e| format!("invalid engineering source config: {e}"))?;
    if config.version != 1 {
        return Err(format!(
            "unsupported engineering source config version {}",
            config.version
        ));
    }
    let cursor = data
        .get(CURSOR_KEY)
        .map(|raw| serde_json::from_str(raw))
        .transpose()
        .map_err(|e| format!("invalid engineering source cursor: {e}"))?
        .unwrap_or_default();
    let status = data
        .get(STATUS_KEY)
        .map(|raw| serde_json::from_str(raw))
        .transpose()
        .map_err(|e| format!("invalid engineering source status: {e}"))?
        .unwrap_or_default();
    Ok((config, cursor, status))
}

pub(super) fn verify_source_owner(
    cm: &ConfigMap,
    config: &EngineeringSourceConfig,
    owner_sub: &str,
) -> bool {
    config.owner_sub == owner_sub
        && cm
            .annotations()
            .get(OWNER_ANNOTATION)
            .is_some_and(|stored| stored == owner_sub)
        && cm
            .annotations()
            .get(CONNECTION_ANNOTATION)
            .is_some_and(|stored| stored == &config.connection_config_map_ref)
        && cm
            .annotations()
            .get(TEAM_NAMESPACE_ANNOTATION)
            .is_some_and(|stored| stored == &config.team_namespace)
        && cm
            .annotations()
            .get(TEAM_NAME_ANNOTATION)
            .is_some_and(|stored| stored == &config.team_name)
}

pub(super) fn to_dto(
    configured: bool,
    config: Option<&EngineeringSourceConfig>,
    status: EngineeringSourceStatus,
) -> EngineeringSourceDto {
    EngineeringSourceDto {
        configured,
        enabled: config.is_some_and(|c| c.enabled),
        auto_run: config.is_none_or(|c| c.auto_run),
        repos: config.map(|c| c.repos.clone()).unwrap_or_default(),
        signals: config.map(|c| c.signals.clone()).unwrap_or_default(),
        poll_interval_seconds: config
            .map(|c| c.poll_interval_seconds)
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
        status,
    }
}

fn validate_repo_name(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !owner.is_empty()
        && !name.is_empty()
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub(super) fn validate_request(
    request: &PutEngineeringSourceRequest,
    granted: &[String],
) -> AppResult<(Vec<String>, Vec<EngineeringSignal>)> {
    if !(MIN_POLL_INTERVAL_SECONDS..=MAX_POLL_INTERVAL_SECONDS)
        .contains(&request.poll_interval_seconds)
    {
        return Err(AppError::BadRequest(format!(
            "poll_interval_seconds must be between {MIN_POLL_INTERVAL_SECONDS} and {MAX_POLL_INTERVAL_SECONDS}"
        )));
    }

    let repos = authorize_repo_set(&request.repos, granted)?;
    if repos.len() > MAX_REPOS {
        return Err(AppError::BadRequest(format!(
            "at most {MAX_REPOS} repositories can be configured per team"
        )));
    }
    if let Some(invalid) = repos.iter().find(|repo| !validate_repo_name(repo)) {
        return Err(AppError::BadRequest(format!(
            "invalid repository name `{invalid}`; expected owner/repo"
        )));
    }

    let signals = request
        .signals
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if request.enabled && repos.is_empty() {
        return Err(AppError::BadRequest(
            "select at least one authorized repository before enabling engineering intake".into(),
        ));
    }
    if request.enabled && signals.is_empty() {
        return Err(AppError::BadRequest(
            "select at least one engineering signal before enabling intake".into(),
        ));
    }
    Ok((repos, signals))
}

pub(super) fn initial_jitter_seconds(source_name: &str) -> i64 {
    let digest = sha256(source_name.as_bytes());
    i64::from(digest[0] % 60)
}

pub(super) fn next_poll_at(config: &EngineeringSourceConfig, now: DateTime<Utc>) -> String {
    (now + chrono::Duration::seconds(config.poll_interval_seconds as i64)).to_rfc3339()
}

pub(super) fn is_due(status: &EngineeringSourceStatus, now: DateTime<Utc>) -> bool {
    status
        .next_poll_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .is_none_or(|value| value.with_timezone(&Utc) <= now)
}

pub(super) fn sync_claim_active(status: &EngineeringSourceStatus, now: DateTime<Utc>) -> bool {
    status.state == EngineeringSyncState::Syncing
        && status
            .sync_claim_expires_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|expires| expires.with_timezone(&Utc) > now)
}
