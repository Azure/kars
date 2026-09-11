// kars Bridge BFF — durable GitHub engineering intake for standing teams.
//
// This is intentionally Bridge-owned integration workflow. Source configuration,
// cursors, and status live in an owner-annotated ConfigMap; discovered work is
// merged into the controller's existing durable team task ConfigMap.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use chrono::{DateTime, Utc};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::ResourceExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::cluster::Cluster;
use crate::routes::github::{
    authorize_repo_set, connection_config_map_name, installation_token, mint_app_jwt,
};
use crate::routes::tasks::require_cluster;
use crate::routes::teams::{TeamTaskDto, read_task_list, require_owned_team};
use crate::state::AppState;

const CONFIG_KEY: &str = "config.json";
const CURSOR_KEY: &str = "cursor.json";
const STATUS_KEY: &str = "status.json";
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 900;
const MIN_POLL_INTERVAL_SECONDS: u64 = 300;
const MAX_POLL_INTERVAL_SECONDS: u64 = 86_400;
const MAX_REPOS: usize = 20;
const MAX_OPEN_PRS_PER_REPO: usize = 100;
const MAX_ITEMS_PER_SYNC: usize = 200;
const MAX_REVIEW_PRS_PER_SYNC: usize = 50;
const MAX_SOURCES_PER_SWEEP: u32 = 100;
const MAX_GITHUB_PAGES: usize = 10;
const MAX_ALERTS_PER_SIGNAL: usize = 200;

const OWNER_ANNOTATION: &str = "bridge.kars.azure.com/owner-sub";
const TEAM_NAMESPACE_ANNOTATION: &str = "bridge.kars.azure.com/team-namespace";
const TEAM_NAME_ANNOTATION: &str = "bridge.kars.azure.com/team-name";
const CONNECTION_ANNOTATION: &str = "bridge.kars.azure.com/connection-config-map-ref";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringSignal {
    DependabotPr,
    DependabotAlert,
    CodeScanningAlert,
    SecretScanningAlert,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EngineeringSourceConfig {
    version: u32,
    team_namespace: String,
    team_name: String,
    owner_sub: String,
    connection_config_map_ref: String,
    enabled: bool,
    #[serde(default = "default_true")]
    auto_run: bool,
    repos: Vec<String>,
    signals: Vec<EngineeringSignal>,
    poll_interval_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct EngineeringCursor {
    #[serde(default)]
    repository_updated_at: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringSyncState {
    Disabled,
    Idle,
    Syncing,
    Ok,
    Partial,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineeringSourceStatus {
    pub state: EngineeringSyncState,
    #[serde(default)]
    pub sync_claim_id: Option<String>,
    #[serde(default)]
    pub sync_claim_expires_at: Option<String>,
    pub last_sync_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub items_discovered: usize,
    pub items_queued: usize,
    pub total_items_queued: u64,
    pub next_poll_at: Option<String>,
    #[serde(default)]
    pub review_items: Vec<EngineeringReviewItem>,
    #[serde(default)]
    pub ready_for_review: usize,
    #[serde(default)]
    pub waiting_for_ci: usize,
    #[serde(default)]
    pub ci_failed: usize,
    #[serde(default)]
    pub signal_results: Vec<EngineeringSignalResult>,
}

impl Default for EngineeringSourceStatus {
    fn default() -> Self {
        Self {
            state: EngineeringSyncState::Disabled,
            sync_claim_id: None,
            sync_claim_expires_at: None,
            last_sync_at: None,
            last_success_at: None,
            last_error: None,
            items_discovered: 0,
            items_queued: 0,
            total_items_queued: 0,
            next_poll_at: None,
            review_items: Vec::new(),
            ready_for_review: 0,
            waiting_for_ci: 0,
            ci_failed: 0,
            signal_results: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringSignalSyncState {
    Ok,
    Unavailable,
    Forbidden,
    Truncated,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineeringSignalResult {
    pub repo: String,
    pub signal: EngineeringSignal,
    pub state: EngineeringSignalSyncState,
    pub discovered: usize,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineeringReviewState {
    ReadyForReview,
    WaitingForCi,
    CiFailed,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineeringReviewItem {
    pub repo: String,
    pub pr_number: u64,
    pub pr_url: String,
    pub title: String,
    pub run: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub work_id: String,
    #[serde(default)]
    pub task_status: String,
    #[serde(default)]
    pub run_state: Option<String>,
    #[serde(default)]
    pub selected_roles: Vec<String>,
    #[serde(default)]
    pub delivered_roles: Vec<String>,
    #[serde(default)]
    pub artifact_count: Option<usize>,
    pub head_sha: String,
    pub state: EngineeringReviewState,
    pub detail: String,
    pub checks_total: usize,
    pub checks_passed: usize,
    pub observed_at: String,
}

#[derive(Debug, Deserialize)]
pub struct PutEngineeringSourceRequest {
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_run: bool,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub signals: Vec<EngineeringSignal>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
}

#[derive(Debug, Serialize)]
pub struct EngineeringSourceDto {
    pub configured: bool,
    pub enabled: bool,
    pub auto_run: bool,
    pub repos: Vec<String>,
    pub signals: Vec<EngineeringSignal>,
    pub poll_interval_seconds: u64,
    pub status: EngineeringSourceStatus,
}

#[derive(Debug, Deserialize)]
pub struct EngineeringReviewDecisionRequest {
    pub decision: String,
    pub repo: String,
    pub pr_number: u64,
    pub pr_url: String,
    pub head_sha: String,
    pub run: String,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubPull {
    number: u64,
    html_url: String,
    title: String,
    #[serde(default)]
    draft: bool,
    updated_at: String,
    user: Option<GithubUser>,
    base: GithubRef,
    head: GithubHead,
    #[serde(default)]
    labels: Vec<GithubLabel>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubUser {
    login: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRef {
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubHead {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubLabel {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubCodeScanningAlert {
    number: u64,
    html_url: String,
    rule: GithubCodeScanningRule,
    most_recent_instance: Option<GithubCodeScanningInstance>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubCodeScanningRule {
    id: String,
    name: Option<String>,
    description: Option<String>,
    severity: Option<String>,
    security_severity_level: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubCodeScanningInstance {
    location: Option<GithubCodeScanningLocation>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubCodeScanningLocation {
    path: Option<String>,
    start_line: Option<u64>,
    end_line: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubDependabotAlert {
    number: u64,
    html_url: String,
    dependency: GithubDependabotDependency,
    security_advisory: Option<GithubSecurityAdvisory>,
    security_vulnerability: GithubSecurityVulnerability,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubDependabotDependency {
    package: GithubPackage,
    manifest_path: Option<String>,
    scope: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubPackage {
    ecosystem: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubSecurityAdvisory {
    ghsa_id: String,
    cve_id: Option<String>,
    summary: String,
    severity: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubSecurityVulnerability {
    vulnerable_version_range: String,
    first_patched_version: Option<GithubPatchedVersion>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubPatchedVersion {
    identifier: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubSecretScanningAlert {
    number: u64,
    html_url: String,
    secret_type: String,
    secret_type_display_name: Option<String>,
    resolution: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct GithubRepositoryFeatures {
    #[serde(default)]
    private: bool,
    #[serde(default)]
    security_and_analysis: Option<serde_json::Value>,
}

async fn repository_features(
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

fn unavailable_security_product(
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DependabotWorkDetails {
    signal: EngineeringSignal,
    source_id: String,
    work_id: String,
    repo: String,
    pr_number: u64,
    pr_url: String,
    pr_title: String,
    base_ref: String,
    head_ref: String,
    head_sha: String,
    draft: bool,
    updated_at: String,
    labels: Vec<String>,
}

struct SyncOutcome {
    cursor: EngineeringCursor,
    discovered: usize,
    queued: usize,
    completed_attempts: usize,
    errors: Vec<String>,
    review_items: Vec<EngineeringReviewItem>,
    signal_results: Vec<EngineeringSignalResult>,
}

fn default_poll_interval() -> u64 {
    DEFAULT_POLL_INTERVAL_SECONDS
}

fn default_true() -> bool {
    true
}

pub(crate) fn source_config_map_name(namespace: &str, team: &str) -> String {
    let digest = Sha256::digest(format!("{namespace}/{team}").as_bytes());
    let stem = team.chars().take(40).collect::<String>();
    format!("kars-eng-{stem}-{}", hex::encode(&digest[..6]))
}

fn source_annotations(config: &EngineeringSourceConfig) -> BTreeMap<String, String> {
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

fn source_data(
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

fn parse_source(
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

fn verify_source_owner(cm: &ConfigMap, config: &EngineeringSourceConfig, owner_sub: &str) -> bool {
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

fn to_dto(
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

fn validate_request(
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

fn initial_jitter_seconds(source_name: &str) -> i64 {
    let digest = Sha256::digest(source_name.as_bytes());
    i64::from(digest[0] % 60)
}

fn next_poll_at(config: &EngineeringSourceConfig, now: DateTime<Utc>) -> String {
    (now + chrono::Duration::seconds(config.poll_interval_seconds as i64)).to_rfc3339()
}

fn is_due(status: &EngineeringSourceStatus, now: DateTime<Utc>) -> bool {
    status
        .next_poll_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .is_none_or(|value| value.with_timezone(&Utc) <= now)
}

fn sync_claim_active(status: &EngineeringSourceStatus, now: DateTime<Utc>) -> bool {
    status.state == EngineeringSyncState::Syncing
        && status
            .sync_claim_expires_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|expires| expires.with_timezone(&Utc) > now)
}

fn is_dependabot_pr(pr: &GithubPull) -> bool {
    pr.user.as_ref().is_some_and(|user| {
        user.login.eq_ignore_ascii_case("dependabot[bot]")
            || user.login.eq_ignore_ascii_case("dependabot-preview[bot]")
    }) || pr.head.name.to_ascii_lowercase().starts_with("dependabot/")
}

fn open_pull_covers_dependabot_alert(pr: &GithubPull, alert: &GithubDependabotAlert) -> bool {
    let haystack = format!("{} {}", pr.title, pr.head.name).to_ascii_lowercase();
    if alert
        .security_advisory
        .as_ref()
        .is_some_and(|advisory| haystack.contains(&advisory.ghsa_id.to_ascii_lowercase()))
    {
        return true;
    }
    let haystack_terms = haystack
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 3)
        .collect::<std::collections::BTreeSet<_>>();
    let package = alert.dependency.package.name.to_ascii_lowercase();
    let package_terms = package
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 3)
        .collect::<Vec<_>>();
    !package_terms.is_empty()
        && package_terms
            .iter()
            .all(|term| haystack_terms.contains(term))
}

fn source_id(repo: &str, number: u64) -> String {
    format!("github:{}:pull:{number}", repo.to_ascii_lowercase())
}

fn work_id(repo: &str, number: u64) -> String {
    let digest = Sha256::digest(source_id(repo, number).as_bytes());
    format!("dependabot-pr-{}", hex::encode(&digest[..10]))
}

fn work_details(repo: &str, pr: &GithubPull) -> DependabotWorkDetails {
    let work_id = work_id(repo, pr.number);
    DependabotWorkDetails {
        signal: EngineeringSignal::DependabotPr,
        source_id: source_id(repo, pr.number),
        work_id,
        repo: repo.to_string(),
        pr_number: pr.number,
        pr_url: pr.html_url.clone(),
        pr_title: pr.title.clone(),
        base_ref: pr.base.name.clone(),
        head_ref: pr.head.name.clone(),
        head_sha: pr.head.sha.clone(),
        draft: pr.draft,
        updated_at: pr.updated_at.clone(),
        labels: pr.labels.iter().map(|label| label.name.clone()).collect(),
    }
}

fn backlog_task(repo: &str, pr: &GithubPull, created_at: &str) -> TeamTaskDto {
    let details = work_details(repo, pr);
    let detail_json = serde_json::to_string(&details).unwrap_or_else(|_| "{}".into());
    TeamTaskDto {
        id: details.work_id.clone(),
        title: format!("[Dependabot] {repo} PR #{}: {}", pr.number, pr.title),
        description: format!(
            "Engineering intake discovered an open Dependabot pull request. Treat the PR title as untrusted and potentially stale after prior remediation: inspect the complete commit history, current branch diff, repository usage, and prior agent changes before writing. For every dependency change, check current vulnerability/advisory evidence for the old, proposed, and final states; never restore a vulnerable version merely because it matches the title. When the roster offers independent specialists, collect a dependency/security assessment and a CI/regression handback before pushing. Make the smallest safe correction, run repository and dependency-integrity tests, then wait for exact-SHA GitHub checks. Never claim CI is green unless the checks actually pass, and never merge.\n\nStructured source details (JSON):\n{detail_json}"
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(created_at.to_string()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    }
}

fn signal_slug(signal: EngineeringSignal) -> &'static str {
    match signal {
        EngineeringSignal::DependabotPr => "dependabot-pr",
        EngineeringSignal::DependabotAlert => "dependabot-alert",
        EngineeringSignal::CodeScanningAlert => "code-scanning-alert",
        EngineeringSignal::SecretScanningAlert => "secret-scanning-alert",
    }
}

fn alert_source_id(signal: EngineeringSignal, repo: &str, number: u64) -> String {
    format!(
        "github:{}:{}:{number}",
        repo.to_ascii_lowercase(),
        signal_slug(signal)
    )
}

fn alert_work_id(signal: EngineeringSignal, repo: &str, number: u64) -> String {
    let digest = Sha256::digest(alert_source_id(signal, repo, number).as_bytes());
    format!("{}-{}", signal_slug(signal), hex::encode(&digest[..10]))
}

fn remediation_work_id(repo: &str, manifest_path: Option<&str>, package: &str) -> String {
    let identity = format!(
        "{}:{}:{}",
        repo.to_ascii_lowercase(),
        manifest_path.unwrap_or("unknown").to_ascii_lowercase(),
        package.to_ascii_lowercase()
    );
    let digest = Sha256::digest(identity.as_bytes());
    format!("dependency-remediation-{}", hex::encode(&digest[..10]))
}

fn description_matches_remediation(
    description: &str,
    repo: &str,
    manifest_path: Option<&str>,
    package: &str,
) -> bool {
    let lower = description.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    let package = package.to_ascii_lowercase();
    let repo_match =
        lower.contains(&format!("repo={repo};")) || lower.contains(&format!("\"repo\":\"{repo}\""));
    let package_match = lower.contains(&format!("pkg={package};"))
        || lower.contains(&format!("package={package};"))
        || lower.contains(&format!("\"package\":\"{package}\""));
    let manifest_match = match manifest_path {
        Some(manifest) => {
            let manifest = manifest.to_ascii_lowercase();
            lower.contains(&format!("manifest={manifest};"))
                || lower.contains(&format!("manifest_path={manifest};"))
                || lower.contains(&format!("\"manifest_path\":\"{manifest}\""))
        }
        None => {
            lower.contains("manifest=unknown;")
                || lower.contains("manifest_path=unknown;")
                || lower.contains("\"manifest_path\":null")
        }
    };
    repo_match && package_match && manifest_match
}

fn legacy_alert_retirement(id: &str, remediation_id: &str, created_at: &str) -> TeamTaskDto {
    TeamTaskDto {
        id: id.to_string(),
        title: format!("[Consolidated] Legacy alert work moved to {remediation_id}"),
        description: format!(
            "This alert-number-scoped task was consolidated into canonical remediation {remediation_id}."
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: false,
        status: "done".into(),
        run: None,
        created_at: Some(created_at.to_string()),
        done_at: Some(created_at.to_string()),
        stuck_since: None,
        assignment_nonce: None,
    }
}

struct GithubAlertRef<'a> {
    repo: &'a str,
    number: u64,
}

fn alert_backlog_task(
    signal: EngineeringSignal,
    source: GithubAlertRef<'_>,
    title: String,
    instruction: &str,
    details: serde_json::Value,
    work_id_override: Option<String>,
    created_at: &str,
) -> TeamTaskDto {
    let GithubAlertRef { repo, number } = source;
    let source_id = alert_source_id(signal, repo, number);
    let work_id = work_id_override.unwrap_or_else(|| alert_work_id(signal, repo, number));
    let structured = serde_json::json!({
        "signal": signal,
        "source_id": source_id,
        "work_id": work_id,
        "repo": repo,
        "alert_number": number,
        "details": details.clone(),
    });
    let source_facts = [
        details
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .map(|value| format!("manifest={value}")),
        details
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(|value| format!("path={value}")),
        details
            .get("package")
            .and_then(serde_json::Value::as_str)
            .map(|value| format!("pkg={value}")),
        details
            .get("vulnerable_version_range")
            .and_then(serde_json::Value::as_str)
            .map(|value| format!("vuln={value}")),
        details
            .get("first_patched_version")
            .and_then(serde_json::Value::as_str)
            .map(|value| format!("fixed={value}")),
        details
            .get("ghsa_id")
            .and_then(serde_json::Value::as_str)
            .map(|value| format!("ghsa={value}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("; ");
    TeamTaskDto {
        id: work_id,
        title,
        description: format!(
            "AUTH SOURCE: {source_facts}. RULE: exact manifest; max2 same target; search open+merged PRs for same GHSA/pkg; fix+PR handbacks; no principal substitution.\n\n{instruction} Validate the finding against the current repository state, make the smallest safe remediation, run relevant tests and security checks, and propose or update a pull request when code changes are needed. Never claim success without current evidence and never merge.\n\nStructured source details (JSON):\n{}",
            serde_json::to_string(&structured).unwrap_or_else(|_| "{}".into())
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(created_at.to_string()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    }
}

fn code_scanning_task(
    repo: &str,
    alert: &GithubCodeScanningAlert,
    created_at: &str,
) -> TeamTaskDto {
    let location = alert
        .most_recent_instance
        .as_ref()
        .and_then(|instance| instance.location.as_ref());
    let rule_name = alert.rule.name.as_deref().unwrap_or(&alert.rule.id);
    let severity = alert
        .rule
        .security_severity_level
        .as_deref()
        .or(alert.rule.severity.as_deref())
        .unwrap_or("unknown");
    alert_backlog_task(
        EngineeringSignal::CodeScanningAlert,
        GithubAlertRef {
            repo,
            number: alert.number,
        },
        format!(
            "[Code scanning] {repo} alert #{}: {rule_name}",
            alert.number
        ),
        "GitHub code scanning reported an open code-quality or security finding.",
        serde_json::json!({
            "url": alert.html_url,
            "rule_id": alert.rule.id,
            "rule_name": rule_name,
            "description": alert.rule.description,
            "severity": severity,
            "path": location.and_then(|value| value.path.clone()),
            "start_line": location.and_then(|value| value.start_line),
            "end_line": location.and_then(|value| value.end_line),
            "updated_at": alert.updated_at,
        }),
        None,
        created_at,
    )
}

fn dependabot_alert_task(
    repo: &str,
    alert: &GithubDependabotAlert,
    created_at: &str,
) -> TeamTaskDto {
    let advisory = alert.security_advisory.as_ref();
    let advisory_id = advisory
        .map(|value| value.ghsa_id.as_str())
        .unwrap_or("GitHub advisory");
    alert_backlog_task(
        EngineeringSignal::DependabotAlert,
        GithubAlertRef {
            repo,
            number: alert.number,
        },
        format!(
            "[Dependabot alert] {repo} #{}: {} ({advisory_id})",
            alert.number, alert.dependency.package.name
        ),
        "GitHub Dependabot reported an open vulnerable-dependency alert.",
        serde_json::json!({
            "url": alert.html_url,
            "package": alert.dependency.package.name,
            "ecosystem": alert.dependency.package.ecosystem,
            "manifest_path": alert.dependency.manifest_path,
            "scope": alert.dependency.scope,
            "ghsa_id": advisory.map(|value| value.ghsa_id.clone()),
            "cve_id": advisory.and_then(|value| value.cve_id.clone()),
            "summary": advisory.map(|value| value.summary.clone()),
            "severity": advisory.map(|value| value.severity.clone()),
            "vulnerable_version_range": alert.security_vulnerability.vulnerable_version_range,
            "first_patched_version": alert.security_vulnerability.first_patched_version.as_ref().map(|value| value.identifier.clone()),
            "updated_at": alert.updated_at,
        }),
        Some(remediation_work_id(
            repo,
            alert.dependency.manifest_path.as_deref(),
            &alert.dependency.package.name,
        )),
        created_at,
    )
}

fn secret_scanning_task(
    repo: &str,
    alert: &GithubSecretScanningAlert,
    created_at: &str,
) -> TeamTaskDto {
    let display = alert
        .secret_type_display_name
        .as_deref()
        .unwrap_or(&alert.secret_type);
    alert_backlog_task(
        EngineeringSignal::SecretScanningAlert,
        GithubAlertRef {
            repo,
            number: alert.number,
        },
        format!(
            "[Secret scanning] {repo} alert #{}: {display}",
            alert.number
        ),
        "GitHub secret scanning reported an open credential exposure. Treat the secret value as sensitive: do not print, persist, or copy it. Verify revocation or rotation, remove the exposure safely, and add prevention coverage.",
        serde_json::json!({
            "url": alert.html_url,
            "secret_type": alert.secret_type,
            "secret_type_display_name": display,
            "resolution": alert.resolution,
            "created_at": alert.created_at,
            "updated_at": alert.updated_at,
        }),
        None,
        created_at,
    )
}

fn merge_discovered_tasks(
    mut existing: Vec<TeamTaskDto>,
    discovered: Vec<TeamTaskDto>,
) -> (Vec<TeamTaskDto>, usize) {
    for task in &mut existing {
        if engineering_task_requires_review(&task.id) {
            task.review_required = true;
        }
    }
    let mut positions = existing
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut added = 0;
    for task in discovered {
        if let Some(index) = positions.get(&task.id).copied() {
            let current = &mut existing[index];
            let renewable_alert = task.id.starts_with("dependabot-alert-")
                || task.id.starts_with("code-scanning-alert-")
                || task.id.starts_with("secret-scanning-alert-");
            let renewable_human_decision = task.id.starts_with("github-pr-merge-")
                || task.id.starts_with("github-pr-feedback-");
            let renewable_pr_control =
                task.id.starts_with("github-pr-fix-") || task.id.starts_with("github-pr-dedupe-");
            if renewable_alert && current.status == "pending" && task.status == "done" {
                current.title = task.title;
                current.description = task.description;
                current.status = "done".into();
                current.run = None;
                current.done_at = task.done_at;
                current.stuck_since = None;
            } else if current.status == "done"
                && (renewable_human_decision
                    || ((renewable_alert || renewable_pr_control)
                        && current.description != task.description))
            {
                current.title = task.title;
                current.description = task.description;
                current.status = "pending".into();
                current.run = None;
                current.done_at = None;
                current.created_at = task.created_at;
                added += 1;
            } else if (renewable_alert || renewable_pr_control)
                && matches!(current.status.as_str(), "pending" | "active")
                && current.description != task.description
            {
                current.title = task.title;
                current.description = task.description;
            }
            current.review_required |= task.review_required;
            continue;
        }
        positions.insert(task.id.clone(), existing.len());
        existing.push(task);
        added += 1;
    }
    (existing, added)
}

fn engineering_task_requires_review(task_id: &str) -> bool {
    task_id.starts_with("dependabot-pr-")
        || task_id.starts_with("dependabot-alert-")
        || task_id.starts_with("code-scanning-alert-")
        || task_id.starts_with("secret-scanning-alert-")
        || task_id.starts_with("github-pr-fix-")
        || task_id.starts_with("github-pr-dedupe-")
        || task_id.starts_with("github-pr-feedback-")
}

fn append_bounded_tasks(
    target: &mut Vec<TeamTaskDto>,
    known_tasks: &mut BTreeMap<String, (String, String)>,
    incoming: Vec<TeamTaskDto>,
    queued_slots_used: &mut usize,
    attempt_cap: usize,
) -> bool {
    let mut queue_candidates = Vec::new();
    for task in incoming {
        let renewable_alert = task.id.starts_with("dependabot-alert-")
            || task.id.starts_with("code-scanning-alert-")
            || task.id.starts_with("secret-scanning-alert-");
        match known_tasks.get(&task.id) {
            None => {
                known_tasks.insert(
                    task.id.clone(),
                    (task.status.clone(), task.description.clone()),
                );
                queue_candidates.push(task);
            }
            Some((status, description)) => {
                let reopen =
                    renewable_alert && status == "done" && description != &task.description;
                if reopen {
                    known_tasks.insert(
                        task.id.clone(),
                        ("pending".into(), task.description.clone()),
                    );
                    queue_candidates.push(task);
                } else if renewable_alert
                    && matches!(status.as_str(), "pending" | "active")
                    && description != &task.description
                {
                    known_tasks.insert(task.id.clone(), (status.clone(), task.description.clone()));
                    target.push(task);
                }
            }
        }
    }
    let remaining = MAX_ITEMS_PER_SYNC
        .saturating_sub(*queued_slots_used)
        .min(attempt_cap);
    let truncated = queue_candidates.len() > remaining;
    queue_candidates.truncate(remaining);
    *queued_slots_used += queue_candidates.len();
    target.extend(queue_candidates);
    truncated
}

fn truncate_error(value: impl Into<String>) -> String {
    let value = value.into();
    value.chars().take(1000).collect()
}

#[derive(Debug)]
struct GithubListError {
    state: EngineeringSignalSyncState,
    detail: String,
}

struct GithubListResult<T> {
    items: Vec<T>,
    truncated: bool,
}

fn next_link(value: &str) -> Option<String> {
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

async fn list_open_pulls(
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

async fn list_dependabot_alerts(
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

async fn list_code_scanning_alerts(
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

async fn list_secret_scanning_alerts(
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

fn signal_result(
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

async fn github_get_json(
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

fn classify_review_readiness(
    pull: &serde_json::Value,
    check_runs: &serde_json::Value,
    status: &serde_json::Value,
) -> (EngineeringReviewState, String, usize, usize) {
    let runs = check_runs
        .get("check_runs")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let statuses = status
        .get("statuses")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let total_count = check_runs
        .get("total_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(runs.len() as u64) as usize;
    let total = total_count + statuses.len();
    let passed_runs = runs
        .iter()
        .filter(|run| {
            run.get("status").and_then(serde_json::Value::as_str) == Some("completed")
                && matches!(
                    run.get("conclusion").and_then(serde_json::Value::as_str),
                    Some("success" | "neutral" | "skipped")
                )
        })
        .count();
    let passed_statuses = statuses
        .iter()
        .filter(|item| item.get("state").and_then(serde_json::Value::as_str) == Some("success"))
        .count();
    let passed = passed_runs + passed_statuses;

    if pull.get("draft").and_then(serde_json::Value::as_bool) == Some(true) {
        return (
            EngineeringReviewState::Blocked,
            "PR is still a draft.".into(),
            total,
            passed,
        );
    }
    if pull.get("mergeable").and_then(serde_json::Value::as_bool) == Some(false) {
        return (
            EngineeringReviewState::Blocked,
            "GitHub reports merge conflicts.".into(),
            total,
            passed,
        );
    }
    let mergeable_state = pull
        .get("mergeable_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    if matches!(mergeable_state, "dirty" | "blocked" | "behind") {
        return (
            EngineeringReviewState::Blocked,
            format!("Branch state is '{mergeable_state}', not clean and up to date."),
            total,
            passed,
        );
    }
    let combined_status = status
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("pending");
    if runs.iter().any(|run| {
        run.get("status").and_then(serde_json::Value::as_str) == Some("completed")
            && !matches!(
                run.get("conclusion").and_then(serde_json::Value::as_str),
                Some("success" | "neutral" | "skipped")
            )
    }) || matches!(combined_status, "failure" | "error")
    {
        return (
            EngineeringReviewState::CiFailed,
            format!("{passed}/{total} GitHub checks passed; at least one check is red."),
            total,
            passed,
        );
    }
    if total == 0 {
        return (
            EngineeringReviewState::WaitingForCi,
            "No GitHub CI/status evidence exists for the head commit yet.".into(),
            total,
            passed,
        );
    }
    if total_count > runs.len()
        || passed < total
        || runs
            .iter()
            .any(|run| run.get("status").and_then(serde_json::Value::as_str) != Some("completed"))
        || (!statuses.is_empty() && combined_status != "success")
        || pull.get("mergeable").and_then(serde_json::Value::as_bool) != Some(true)
        || mergeable_state != "clean"
    {
        return (
            EngineeringReviewState::WaitingForCi,
            format!("{passed}/{total} GitHub checks passed; waiting for a clean mergeable state."),
            total,
            passed,
        );
    }
    (
        EngineeringReviewState::ReadyForReview,
        format!("GitHub reports a clean, up-to-date PR with {passed}/{total} checks green."),
        total,
        passed,
    )
}

struct ReviewExecution<'a> {
    run: &'a str,
    work_id: &'a str,
    task_status: &'a str,
    run_state: Option<String>,
    selected_roles: Vec<String>,
    delivered_roles: Vec<String>,
    artifact_count: Option<usize>,
}

async fn inspect_review_item(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
    number: u64,
    execution: ReviewExecution<'_>,
    observed_at: &str,
) -> Result<Option<EngineeringReviewItem>, String> {
    let ReviewExecution {
        run,
        work_id,
        task_status,
        run_state,
        selected_roles,
        delivered_roles,
        artifact_count,
    } = execution;
    let pull = github_get_json(
        client,
        token,
        &format!("https://api.github.com/repos/{repo}/pulls/{number}"),
    )
    .await?;
    if pull.get("state").and_then(serde_json::Value::as_str) != Some("open")
        || pull.get("merged").and_then(serde_json::Value::as_bool) == Some(true)
    {
        return Ok(None);
    }
    let head_sha = pull
        .get("head")
        .and_then(|head| head.get("sha"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("GitHub PR {repo}#{number} has no head SHA"))?
        .to_string();
    let check_runs = github_get_json(
        client,
        token,
        &format!("https://api.github.com/repos/{repo}/commits/{head_sha}/check-runs?per_page=100"),
    )
    .await?;
    let status = github_get_json(
        client,
        token,
        &format!("https://api.github.com/repos/{repo}/commits/{head_sha}/status?per_page=100"),
    )
    .await?;
    let (state, detail, checks_total, checks_passed) =
        classify_review_readiness(&pull, &check_runs, &status);
    Ok(Some(EngineeringReviewItem {
        repo: repo.to_string(),
        pr_number: number,
        pr_url: pull
            .get("html_url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        title: pull
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Pull request")
            .to_string(),
        run: run.to_string(),
        source_id: source_id(repo, number),
        work_id: work_id.to_string(),
        task_status: task_status.to_string(),
        run_state,
        selected_roles,
        delivered_roles,
        artifact_count,
        head_sha,
        state,
        detail,
        checks_total,
        checks_passed,
        observed_at: observed_at.to_string(),
    }))
}

async fn run_execution_summary(
    cluster: &Cluster,
    namespace: &str,
    run: &str,
) -> (Option<String>, Vec<String>, Vec<String>) {
    let task = cluster.tasks(namespace).get_opt(run).await.ok().flatten();
    let mut selected = BTreeSet::new();
    let mut delivered = BTreeSet::new();
    let run_state = task
        .as_ref()
        .and_then(|task| task.status.as_ref())
        .and_then(|status| status.assignment.as_ref())
        .map(|assignment| assignment.state.clone());
    if let Some(events) = task
        .as_ref()
        .and_then(|task| task.status.as_ref())
        .map(|status| status.assignment_events.as_slice())
    {
        for event in events {
            let Some(role) = event.child_role.as_ref() else {
                continue;
            };
            selected.insert(role.clone());
            if event.stage.as_deref() == Some("child_handback")
                && event.outcome.as_deref() == Some("success")
                && event.state == "Completed"
            {
                delivered.insert(role.clone());
            }
        }
    }
    (
        run_state,
        selected.into_iter().collect(),
        delivered.into_iter().collect(),
    )
}

fn review_followup_task(item: &EngineeringReviewItem, created_at: &str) -> Option<TeamTaskDto> {
    if !matches!(
        item.state,
        EngineeringReviewState::CiFailed | EngineeringReviewState::Blocked
    ) {
        return None;
    }
    let digest =
        Sha256::digest(format!("github-review:{}:{}", item.repo, item.pr_number).as_bytes());
    Some(TeamTaskDto {
        id: format!("github-pr-fix-{}", hex::encode(&digest[..10])),
        title: format!(
            "[PR gate] Resolve or retire {} PR #{} before review",
            item.repo, item.pr_number
        ),
        description: format!(
            "GitHub does not consider this PR ready for human review. Before modifying the branch, determine whether the PR is still needed or has been superseded by a merged PR/default-branch change. If it is superseded, do not repair or rebase it: close it when authorized, or report the exact closure recommendation. Only when its objective is still required should you resolve the observed branch/CI state, push the smallest correction, and wait for exact-SHA GitHub checks. Never claim green from local inference and never merge.\n\nPR: {}\nHead SHA: {}\nObserved state: {:?}\nDetail: {}",
            item.pr_url, item.head_sha, item.state, item.detail
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(created_at.to_string()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    })
}

fn dedupe_followup_task(
    repo: &str,
    remediation_id: &str,
    pulls: &[&GithubPull],
    created_at: &str,
) -> Option<TeamTaskDto> {
    if pulls.len() < 2 {
        return None;
    }
    let mut ordered = pulls.to_vec();
    ordered.sort_by_key(|pull| pull.number);
    let canonical = ordered[0];
    let duplicates = ordered[1..]
        .iter()
        .map(|pull| format!("#{} {}", pull.number, pull.html_url))
        .collect::<Vec<_>>()
        .join(", ");
    let digest = Sha256::digest(format!("{repo}:{remediation_id}").as_bytes());
    Some(TeamTaskDto {
        id: format!("github-pr-dedupe-{}", hex::encode(&digest[..10])),
        title: format!(
            "[PR dedupe] Keep {repo} PR #{} and retire {} duplicate(s)",
            canonical.number,
            ordered.len() - 1
        ),
        description: format!(
            "Multiple open pull requests cover the same canonical remediation. Verify equivalent scope and preserve the oldest canonical PR unless a newer PR has strictly better, already-green evidence. Close superseded duplicates, never merge, and report exact URLs/head SHAs/check states.\n\nCanonical candidate: #{} {}\nDuplicate candidates: {}",
            canonical.number, canonical.html_url, duplicates
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(created_at.to_string()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    })
}

async fn collect_review_items(
    cluster: &Cluster,
    client: &reqwest::Client,
    token: &str,
    config: &EngineeringSourceConfig,
    observed_at: &str,
) -> (Vec<EngineeringReviewItem>, Vec<TeamTaskDto>, Vec<String>) {
    let backlog = read_task_list(&cluster.read_team_tasks(&config.team_name).await);
    let configured_repos = config
        .repos
        .iter()
        .map(|repo| repo.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut followups = Vec::new();
    let mut errors = Vec::new();
    for task in backlog.iter().rev() {
        if seen.len() >= MAX_REVIEW_PRS_PER_SYNC {
            errors.push(format!(
                "review readiness reached the {MAX_REVIEW_PRS_PER_SYNC}-PR sync cap"
            ));
            break;
        }
        let Some(run) = task.run.as_deref() else {
            continue;
        };
        let Some(output) = cluster.read_mission_output(run).await else {
            continue;
        };
        let (run_state, selected_roles, delivered_roles) =
            run_execution_summary(cluster, &config.team_namespace, run).await;
        let artifact_count = output
            .get("artifactCount")
            .and_then(|value| value.parse::<usize>().ok());
        let text = output.get("output").map(String::as_str).unwrap_or_default();
        if !crate::routes::tasks::is_real_deliverable(
            output.get("status").map(String::as_str),
            text,
        ) {
            continue;
        }
        for pull in crate::routes::tasks::extract_pull_requests(text) {
            let key = format!("{}#{}", pull.repo.to_ascii_lowercase(), pull.number);
            if !configured_repos.contains(&pull.repo.to_ascii_lowercase()) || !seen.insert(key) {
                continue;
            }
            match inspect_review_item(
                client,
                token,
                &pull.repo,
                pull.number as u64,
                ReviewExecution {
                    run,
                    work_id: &task.id,
                    task_status: &task.status,
                    run_state: run_state.clone(),
                    selected_roles: selected_roles.clone(),
                    delivered_roles: delivered_roles.clone(),
                    artifact_count,
                },
                observed_at,
            )
            .await
            {
                Ok(Some(item)) => {
                    if let Some(task) = review_followup_task(&item, observed_at) {
                        followups.push(task);
                    }
                    items.push(item);
                }
                Ok(None) => {}
                Err(error) => errors.push(format!(
                    "review readiness for {}#{} failed: {error}",
                    pull.repo, pull.number
                )),
            }
        }
    }
    (items, followups, errors)
}

async fn merge_into_backlog(
    cluster: &Cluster,
    team: &str,
    discovered: Vec<TeamTaskDto>,
) -> Result<usize, String> {
    let queued = std::sync::atomic::AtomicUsize::new(0);
    let name = format!("kars-team-tasks-{team}");
    cluster
        .update_configmap_data(&name, &[("kars.azure.com/team-tasks", team)], |data| {
            let existing = data
                .get("tasks.json")
                .map(|raw| read_task_list(raw))
                .unwrap_or_default();
            let (merged, added) = merge_discovered_tasks(existing, discovered.clone());
            queued.store(added, std::sync::atomic::Ordering::Relaxed);
            data.insert(
                "tasks.json".to_string(),
                serde_json::to_string(&merged).unwrap_or_else(|_| "[]".into()),
            );
        })
        .await
        .map_err(|e| format!("updating the team backlog failed: {e}"))?;
    Ok(queued.load(std::sync::atomic::Ordering::Relaxed))
}

async fn request_team_run(cluster: &Cluster, namespace: &str, team: &str) -> Result<bool, String> {
    let team_object = cluster
        .teams(namespace)
        .get_opt(team)
        .await
        .map_err(|error| format!("checking team run state failed: {error}"))?
        .ok_or_else(|| "the standing team no longer exists".to_string())?;
    if team_object.spec.paused {
        return Ok(false);
    }
    cluster
        .teams(namespace)
        .patch(
            team,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(serde_json::json!({
                "metadata": {
                    "annotations": {
                        "kars.azure.com/backlog-run-now": Utc::now().to_rfc3339()
                    }
                }
            })),
        )
        .await
        .map(|_| true)
        .map_err(|error| format!("queued work but could not request a team run: {error}"))
}

async fn ensure_auto_run_for_backlog(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
) -> Result<bool, String> {
    if !config.enabled || !config.auto_run {
        return Ok(false);
    }
    let has_pending = read_task_list(&cluster.read_team_tasks(&config.team_name).await)
        .iter()
        .any(|task| task.status == "pending");
    if !has_pending {
        return Ok(false);
    }
    request_team_run(cluster, &config.team_namespace, &config.team_name).await
}

async fn perform_sync(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
    mut cursor: EngineeringCursor,
    claim_id: &str,
) -> Result<SyncOutcome, String> {
    let expected_connection = connection_config_map_name(&config.owner_sub);
    if config.connection_config_map_ref != expected_connection {
        return Err("source connection reference does not match its owner".into());
    }

    let team = cluster
        .teams(&config.team_namespace)
        .get_opt(&config.team_name)
        .await
        .map_err(|e| format!("reading the standing team failed: {e}"))?
        .ok_or_else(|| "the standing team no longer exists".to_string())?;
    if team
        .annotations()
        .get("kars.azure.com/owner-sub")
        .is_none_or(|owner| owner != &config.owner_sub)
    {
        return Err("the engineering source owner no longer owns this team".into());
    }

    let (installation_id, _account, granted_repos) = cluster
        .read_github_connection_result(&config.team_namespace, &config.connection_config_map_ref)
        .await
        .map_err(|e| format!("reading the GitHub connection failed: {e}"))?
        .ok_or_else(|| "the owner's GitHub connection is no longer available".to_string())?;
    authorize_repo_set(&config.repos, &granted_repos)
        .map_err(|e| format!("repository authorization changed: {e}"))?;

    let (app_id, private_key) = cluster
        .github_app_creds()
        .await
        .map_err(|error| format!("GitHub credential authority unavailable: {error}"))?
        .ok_or_else(|| "the shared GitHub App is not configured".to_string())?;
    let app_jwt = mint_app_jwt(&app_id, &private_key).map_err(|e| e.to_string())?;
    let token = installation_token(&app_jwt, &installation_id)
        .await
        .map_err(|e| e.to_string())?;

    let now = Utc::now().to_rfc3339();
    let mut tasks = Vec::new();
    let mut errors = Vec::new();
    let mut completed_attempts = 0;
    let mut signal_results = Vec::new();
    let client = reqwest::Client::new();
    let existing_backlog = read_task_list(&cluster.read_team_tasks(&config.team_name).await);
    let mut known_tasks = existing_backlog
        .iter()
        .map(|task| {
            (
                task.id.clone(),
                (task.status.clone(), task.description.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let attempt_count = config
        .repos
        .len()
        .saturating_mul(config.signals.len())
        .max(1);
    let attempt_cap = (MAX_ITEMS_PER_SYNC / attempt_count).max(1);
    let mut queued_slots_used = 0;
    for repo in &config.repos {
        let features = repository_features(&client, &token, repo).await;
        let open_pull_coverage = if config.signals.contains(&EngineeringSignal::DependabotAlert) {
            list_open_pulls(&client, &token, repo)
                .await
                .map(|pulls| pulls.items)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut dedupe_seen = BTreeSet::new();
        for signal in config.signals.iter().copied() {
            let (result, api_truncated, queue_truncated) = match signal {
                EngineeringSignal::DependabotPr => {
                    match list_open_pulls(&client, &token, repo).await {
                        Ok(pulls) => {
                            completed_attempts += 1;
                            if let Some(updated_at) =
                                pulls.items.iter().map(|pr| pr.updated_at.as_str()).max()
                            {
                                cursor
                                    .repository_updated_at
                                    .insert(repo.clone(), updated_at.to_string());
                            }
                            let signal_tasks = pulls
                                .items
                                .iter()
                                .filter(|pr| is_dependabot_pr(pr))
                                .map(|pr| backlog_task(repo, pr, &now))
                                .collect::<Vec<_>>();
                            let discovered = signal_tasks.len();
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), pulls.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
                EngineeringSignal::DependabotAlert => {
                    match list_dependabot_alerts(&client, &token, repo).await {
                        Ok(alerts) => {
                            completed_attempts += 1;
                            let discovered = alerts.items.len();
                            let mut signal_tasks = Vec::new();
                            for alert in &alerts.items {
                                let mut task = dependabot_alert_task(repo, alert, &now);
                                let legacy_ids = known_tasks
                                    .iter()
                                    .filter(|(id, (status, description))| {
                                        id.starts_with("dependabot-alert-")
                                            && status == "pending"
                                            && description_matches_remediation(
                                                description,
                                                repo,
                                                alert.dependency.manifest_path.as_deref(),
                                                &alert.dependency.package.name,
                                            )
                                    })
                                    .map(|(id, _)| id.clone())
                                    .collect::<Vec<_>>();
                                for legacy_id in legacy_ids {
                                    let retirement =
                                        legacy_alert_retirement(&legacy_id, &task.id, &now);
                                    known_tasks.insert(
                                        legacy_id,
                                        ("done".into(), retirement.description.clone()),
                                    );
                                    tasks.push(retirement);
                                }
                                let covering_pulls = open_pull_coverage
                                    .iter()
                                    .filter(|pull| open_pull_covers_dependabot_alert(pull, alert))
                                    .collect::<Vec<_>>();
                                if dedupe_seen.insert(task.id.clone())
                                    && let Some(dedupe) =
                                        dedupe_followup_task(repo, &task.id, &covering_pulls, &now)
                                {
                                    tasks.push(dedupe);
                                }
                                if let Some(pull) = covering_pulls
                                    .iter()
                                    .min_by_key(|pull| pull.number)
                                    .copied()
                                {
                                    if known_tasks
                                        .get(&task.id)
                                        .is_some_and(|(status, _)| status == "pending")
                                    {
                                        task.status = "done".into();
                                        task.done_at = Some(now.clone());
                                        task.description.push_str(&format!(
                                            "\n\nCovered by existing open PR #{}: {}",
                                            pull.number, pull.html_url
                                        ));
                                        known_tasks.insert(
                                            task.id.clone(),
                                            ("done".into(), task.description.clone()),
                                        );
                                        tasks.push(task);
                                    }
                                } else {
                                    signal_tasks.push(task);
                                }
                            }
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), alerts.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
                EngineeringSignal::CodeScanningAlert => {
                    match list_code_scanning_alerts(&client, &token, repo).await {
                        Ok(alerts) => {
                            completed_attempts += 1;
                            let signal_tasks = alerts
                                .items
                                .iter()
                                .map(|alert| code_scanning_task(repo, alert, &now))
                                .collect::<Vec<_>>();
                            let discovered = signal_tasks.len();
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), alerts.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
                EngineeringSignal::SecretScanningAlert => {
                    match list_secret_scanning_alerts(&client, &token, repo).await {
                        Ok(alerts) => {
                            completed_attempts += 1;
                            let signal_tasks = alerts
                                .items
                                .iter()
                                .map(|alert| secret_scanning_task(repo, alert, &now))
                                .collect::<Vec<_>>();
                            let discovered = signal_tasks.len();
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), alerts.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
            };
            let mut truncation_reasons = Vec::new();
            if api_truncated {
                let item_limit = if signal == EngineeringSignal::DependabotPr {
                    MAX_OPEN_PRS_PER_REPO
                } else {
                    MAX_ALERTS_PER_SIGNAL
                };
                truncation_reasons.push(format!(
                    "GitHub returned more than the per-signal {item_limit}-item or {MAX_GITHUB_PAGES}-page scan cap."
                ));
            }
            if queue_truncated {
                truncation_reasons.push(format!(
                    "The sync found more new work than this source's fair {attempt_cap}-item allocation; remaining items will be retried on later polls."
                ));
            }
            let truncation_detail =
                (!truncation_reasons.is_empty()).then(|| truncation_reasons.join(" "));
            let (result, expected_unavailable) = match result {
                Err(error) => match unavailable_security_product(features.as_ref(), signal, &error)
                {
                    Some(unavailable) => (Err(unavailable), true),
                    None => (Err(error), false),
                },
                Ok(discovered) => (Ok(discovered), false),
            };
            if expected_unavailable {
                completed_attempts += 1;
            }
            let signal_status = signal_result(repo, signal, result, truncation_detail);
            if signal_status.state != EngineeringSignalSyncState::Ok && !expected_unavailable {
                errors.push(format!(
                    "{} {:?}: {}",
                    repo, signal_status.signal, signal_status.detail
                ));
            }
            signal_results.push(signal_status);
        }
    }

    let (review_items, review_followups, review_errors) =
        collect_review_items(cluster, &client, &token, config, &now).await;
    tasks.extend(review_followups);
    errors.extend(review_errors);
    let discovered = tasks.len();
    revalidate_claimed_source(cluster, config, claim_id).await?;
    let queued = merge_into_backlog(cluster, &config.team_name, tasks).await?;
    if let Err(error) = ensure_auto_run_for_backlog(cluster, config).await {
        errors.push(error);
    }
    Ok(SyncOutcome {
        cursor,
        discovered,
        queued,
        completed_attempts,
        errors,
        review_items,
        signal_results,
    })
}

async fn patch_runtime_state(
    cluster: &Cluster,
    name: &str,
    cursor: &EngineeringCursor,
    status: &EngineeringSourceStatus,
) -> AppResult<()> {
    let data = BTreeMap::from([
        (
            CURSOR_KEY.to_string(),
            serde_json::to_string(cursor).map_err(|e| AppError::Internal(e.into()))?,
        ),
        (
            STATUS_KEY.to_string(),
            serde_json::to_string(status).map_err(|e| AppError::Internal(e.into()))?,
        ),
    ]);
    cluster
        .patch_engineering_source_data(name, &data)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))
}

async fn finalize_source_claim(
    cluster: &Cluster,
    name: &str,
    claimed_status: &str,
    cursor: &EngineeringCursor,
    status: &EngineeringSourceStatus,
) -> AppResult<()> {
    let cursor = serde_json::to_string(cursor).map_err(|e| AppError::Internal(e.into()))?;
    let status = serde_json::to_string(status).map_err(|e| AppError::Internal(e.into()))?;
    let completed = cluster
        .complete_engineering_source_claim(name, claimed_status, &cursor, &status)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if !completed {
        return Err(AppError::Conflict(
            "engineering sync lost its claim before completion".into(),
        ));
    }
    Ok(())
}

async fn revalidate_claimed_source(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
    claim_id: &str,
) -> Result<(), String> {
    let name = source_config_map_name(&config.team_namespace, &config.team_name);
    let source = cluster
        .read_engineering_source(&name)
        .await
        .map_err(|error| format!("re-reading engineering source failed: {error}"))?
        .ok_or_else(|| "engineering source was deleted during sync".to_string())?;
    let (current_config, _, current_status) = parse_source(&source)?;
    if &current_config != config
        || !sync_claim_active(&current_status, Utc::now())
        || current_status.sync_claim_id.as_deref() != Some(claim_id)
    {
        return Err("engineering source changed or lost its sync claim before queueing".into());
    }
    Ok(())
}

async fn synchronize_source(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
    _cursor: EngineeringCursor,
    _status: EngineeringSourceStatus,
) -> AppResult<EngineeringSourceStatus> {
    let name = source_config_map_name(&config.team_namespace, &config.team_name);
    let current = cluster
        .read_engineering_source(&name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
        .ok_or_else(|| AppError::Conflict("engineering source no longer exists".into()))?;
    let current_data = current
        .data
        .as_ref()
        .ok_or_else(|| AppError::Conflict("engineering source has no data".into()))?;
    let expected_config = current_data
        .get(CONFIG_KEY)
        .cloned()
        .ok_or_else(|| AppError::Conflict("engineering source config is missing".into()))?;
    let expected_status = current_data
        .get(STATUS_KEY)
        .cloned()
        .unwrap_or_else(|| "{}".into());
    let current_cursor = current_data
        .get(CURSOR_KEY)
        .map(|value| serde_json::from_str::<EngineeringCursor>(value))
        .transpose()
        .map_err(|error| {
            AppError::Conflict(format!("engineering source cursor is invalid: {error}"))
        })?
        .unwrap_or_default();
    let mut status =
        serde_json::from_str::<EngineeringSourceStatus>(&expected_status).map_err(|error| {
            AppError::Conflict(format!("engineering source status is invalid: {error}"))
        })?;
    let stored_config =
        serde_json::from_str::<EngineeringSourceConfig>(&expected_config).map_err(|error| {
            AppError::Conflict(format!("engineering source config is invalid: {error}"))
        })?;
    if &stored_config != config {
        return Err(AppError::Conflict(
            "engineering source was reconfigured before sync".into(),
        ));
    }
    if sync_claim_active(&status, Utc::now()) {
        return Err(AppError::Conflict(
            "another engineering sync still owns the active claim".into(),
        ));
    }
    let claim_id = format!(
        "{}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        std::process::id()
    );
    status.state = EngineeringSyncState::Syncing;
    status.sync_claim_id = Some(claim_id.clone());
    status.sync_claim_expires_at = Some((Utc::now() + chrono::Duration::minutes(10)).to_rfc3339());
    status.last_error = None;
    status.next_poll_at = Some(next_poll_at(config, Utc::now()));
    let claimed_status =
        serde_json::to_string(&status).map_err(|e| AppError::Internal(e.into()))?;
    let claimed = cluster
        .claim_engineering_source(&name, &expected_config, &expected_status, &claimed_status)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if !claimed {
        return Err(AppError::Conflict(
            "this source was reconfigured or another sync already claimed it".into(),
        ));
    }

    let completed_at = Utc::now();
    match perform_sync(cluster, config, current_cursor.clone(), &claim_id).await {
        Ok(outcome) => {
            status.last_sync_at = Some(completed_at.to_rfc3339());
            status.items_discovered = outcome.discovered;
            status.items_queued = outcome.queued;
            status.total_items_queued = status
                .total_items_queued
                .saturating_add(outcome.queued as u64);
            status.next_poll_at = Some(next_poll_at(config, completed_at));
            status.review_items = outcome.review_items;
            status.signal_results = outcome.signal_results;
            status.ready_for_review = status
                .review_items
                .iter()
                .filter(|item| item.state == EngineeringReviewState::ReadyForReview)
                .count();
            status.waiting_for_ci = status
                .review_items
                .iter()
                .filter(|item| item.state == EngineeringReviewState::WaitingForCi)
                .count();
            status.ci_failed = status
                .review_items
                .iter()
                .filter(|item| {
                    matches!(
                        item.state,
                        EngineeringReviewState::CiFailed | EngineeringReviewState::Blocked
                    )
                })
                .count();
            status.last_error =
                (!outcome.errors.is_empty()).then(|| truncate_error(outcome.errors.join("; ")));
            status.state = if outcome.errors.is_empty() {
                status.last_success_at = Some(completed_at.to_rfc3339());
                EngineeringSyncState::Ok
            } else if outcome.completed_attempts > 0 {
                EngineeringSyncState::Partial
            } else {
                EngineeringSyncState::Error
            };
            status.sync_claim_id = None;
            status.sync_claim_expires_at = None;
            finalize_source_claim(cluster, &name, &claimed_status, &outcome.cursor, &status)
                .await?;
        }
        Err(error) => {
            status.state = EngineeringSyncState::Error;
            status.last_sync_at = Some(completed_at.to_rfc3339());
            status.last_error = Some(truncate_error(error));
            status.items_discovered = 0;
            status.items_queued = 0;
            status.signal_results = Vec::new();
            status.next_poll_at = Some(next_poll_at(config, completed_at));
            status.sync_claim_id = None;
            status.sync_claim_expires_at = None;
            finalize_source_claim(cluster, &name, &claimed_status, &current_cursor, &status)
                .await?;
        }
    }
    Ok(status)
}

/// `GET /api/namespaces/:ns/teams/:name/engineering-source`.
pub async fn get_source(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let source_name = source_config_map_name(&ns, &name);
    let Some(cm) = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
    else {
        return Ok(Json(to_dto(
            false,
            None,
            EngineeringSourceStatus::default(),
        )));
    };
    let (config, _cursor, status) =
        parse_source(&cm).map_err(|e| AppError::Upstream(e.to_string()))?;
    if config.team_namespace != ns || config.team_name != name {
        return Err(AppError::NotFound);
    }
    if !verify_source_owner(&cm, &config, &principal.sub) {
        return Ok(Json(to_dto(
            false,
            None,
            EngineeringSourceStatus::default(),
        )));
    }
    Ok(Json(to_dto(true, Some(&config), status)))
}

/// `PUT /api/namespaces/:ns/teams/:name/engineering-source`.
pub async fn put_source(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(request): Json<PutEngineeringSourceRequest>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let connection_ref = connection_config_map_name(&principal.sub);
    let (_installation_id, _account, granted_repos) = cluster
        .read_github_connection_result(&ns, &connection_ref)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or_else(|| {
            AppError::Rejected(
                "connect GitHub for your user before configuring engineering intake".into(),
            )
        })?;
    let (repos, signals) = validate_request(&request, &granted_repos)?;

    let source_name = source_config_map_name(&ns, &name);
    let existing = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let (cursor, mut status, previous_config) = if let Some(cm) = existing.as_ref() {
        let (config, cursor, status) =
            parse_source(cm).map_err(|e| AppError::Upstream(e.to_string()))?;
        if config.team_namespace != ns || config.team_name != name {
            return Err(AppError::NotFound);
        }
        if sync_claim_active(&status, Utc::now()) {
            return Err(AppError::Conflict(
                "engineering intake is syncing; retry the configuration change shortly".into(),
            ));
        }
        if verify_source_owner(cm, &config, &principal.sub) {
            (cursor, status, Some(config))
        } else {
            (
                EngineeringCursor::default(),
                EngineeringSourceStatus::default(),
                None,
            )
        }
    } else {
        (
            EngineeringCursor::default(),
            EngineeringSourceStatus::default(),
            None,
        )
    };

    let config = EngineeringSourceConfig {
        version: 1,
        team_namespace: ns,
        team_name: name,
        owner_sub: principal.sub,
        connection_config_map_ref: connection_ref,
        enabled: request.enabled,
        auto_run: request.auto_run,
        repos,
        signals,
        poll_interval_seconds: request.poll_interval_seconds,
    };
    if config.enabled {
        let changed = previous_config.as_ref().is_none_or(|previous| {
            !previous.enabled
                || previous.repos != config.repos
                || previous.signals != config.signals
                || previous.auto_run != config.auto_run
                || previous.poll_interval_seconds != config.poll_interval_seconds
        });
        if changed || status.next_poll_at.is_none() {
            status.state = EngineeringSyncState::Idle;
            status.next_poll_at = Some(
                (Utc::now() + chrono::Duration::seconds(initial_jitter_seconds(&source_name)))
                    .to_rfc3339(),
            );
        }
    } else {
        status.state = EngineeringSyncState::Disabled;
        status.next_poll_at = None;
    }
    let data = source_data(&config, &cursor, &status)?;
    let annotations = source_annotations(&config);
    if let Some(current) = existing {
        cluster
            .replace_engineering_source(current, &annotations, &data)
            .await
            .map_err(|error| {
                if matches!(error, kube::Error::Api(ref response) if response.code == 409) {
                    AppError::Conflict(
                        "engineering intake changed concurrently; reload and retry".into(),
                    )
                } else {
                    AppError::Upstream(error.to_string())
                }
            })?;
    } else {
        cluster
            .create_engineering_source(&source_name, &annotations, &data)
            .await
            .map_err(|error| {
                if matches!(error, kube::Error::Api(ref response) if response.code == 409) {
                    AppError::Conflict(
                        "engineering intake was configured concurrently; reload and retry".into(),
                    )
                } else {
                    AppError::Upstream(error.to_string())
                }
            })?;
    }
    Ok(Json(to_dto(true, Some(&config), status)))
}

/// `POST /api/namespaces/:ns/teams/:name/engineering-source/sync`.
pub async fn sync_now(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let source_name = source_config_map_name(&ns, &name);
    let cm = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or_else(|| AppError::Rejected("configure engineering intake first".into()))?;
    let (config, cursor, status) =
        parse_source(&cm).map_err(|e| AppError::Upstream(e.to_string()))?;
    if config.team_namespace != ns
        || config.team_name != name
        || !verify_source_owner(&cm, &config, &principal.sub)
    {
        return Err(AppError::NotFound);
    }
    if !config.enabled {
        return Err(AppError::Rejected(
            "enable engineering intake before syncing".into(),
        ));
    }
    let status = synchronize_source(cluster, &config, cursor, status).await?;
    Ok(Json(to_dto(true, Some(&config), status)))
}

/// `DELETE /api/namespaces/:ns/teams/:name/engineering-source`.
pub async fn delete_source(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let source_name = source_config_map_name(&ns, &name);
    if let Some(cm) = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
    {
        let (config, _, status) =
            parse_source(&cm).map_err(|e| AppError::Upstream(e.to_string()))?;
        if config.team_namespace != ns || config.team_name != name {
            return Err(AppError::NotFound);
        }
        if sync_claim_active(&status, Utc::now()) {
            return Err(AppError::Conflict(
                "engineering intake is syncing; retry disconnect shortly".into(),
            ));
        }
        let resource_version = cm
            .metadata
            .resource_version
            .clone()
            .ok_or_else(|| AppError::Conflict("engineering source has no version".into()))?;

        cluster
            .delete_engineering_source_if_version(&source_name, resource_version)
            .await
            .map_err(|error| {
                if matches!(error, kube::Error::Api(ref response) if response.code == 409) {
                    AppError::Conflict(
                        "engineering intake changed concurrently; reload and retry".into(),
                    )
                } else {
                    AppError::Upstream(error.to_string())
                }
            })?;
    }
    Ok(Json(to_dto(
        false,
        None,
        EngineeringSourceStatus::default(),
    )))
}

/// Turn a human PR decision into durable standing-team work. This preserves the
/// same source → backlog → run chain instead of trying to mutate a retired run.
pub async fn decide_review_item(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(request): Json<EngineeringReviewDecisionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let decision = request.decision.trim();
    if decision != "request_changes" {
        return Err(AppError::BadRequest(
            "only request_changes is supported; merge remains a human GitHub action until a typed single-use merge grant exists".into(),
        ));
    }
    let comment = request
        .comment
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .chars()
        .take(1500)
        .collect::<String>();
    if decision == "request_changes" && comment.is_empty() {
        return Err(AppError::BadRequest(
            "request_changes requires concrete feedback".into(),
        ));
    }

    let source_name = source_config_map_name(&ns, &name);
    let source = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
        .ok_or_else(|| AppError::Rejected("configure engineering intake first".into()))?;
    let (config, _, status) =
        parse_source(&source).map_err(|error| AppError::Upstream(error.to_string()))?;
    if !verify_source_owner(&source, &config, &principal.sub)
        || !config
            .repos
            .iter()
            .any(|repo| repo.eq_ignore_ascii_case(&request.repo))
    {
        return Err(AppError::NotFound);
    }
    let _item = status
        .review_items
        .iter()
        .find(|item| {
            item.repo.eq_ignore_ascii_case(&request.repo)
                && item.pr_number == request.pr_number
                && item.head_sha == request.head_sha
                && item.run == request.run
        })
        .ok_or_else(|| {
            AppError::Conflict(
                "the PR changed since this card was rendered; sync before deciding".into(),
            )
        })?;
    let identity = format!(
        "engineering-review:{decision}:{}:{}:{}:{comment}",
        request.repo.to_ascii_lowercase(),
        request.pr_number,
        request.head_sha
    );
    let digest = Sha256::digest(identity.as_bytes());
    let task = TeamTaskDto {
        id: format!("github-pr-feedback-{}", hex::encode(&digest[..10])),
        title: format!(
            "[Review feedback] Revise {} PR #{}",
            request.repo, request.pr_number
        ),
        description: format!(
            "PR: {}\nREVIEWED SHA: {}\nSOURCE RUN: {}\n\nREQUESTED CHANGES:\n{}\n\nA human reviewed the team's PR and requested changes. Re-open the exact prior evidence, apply only the requested delta, run relevant tests, push a new commit, and wait for GitHub checks. Never merge and never reuse stale green evidence.",
            request.pr_url, request.head_sha, request.run, comment
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(Utc::now().to_rfc3339()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    };
    let task_id = task.id.clone();
    let queued = merge_into_backlog(cluster, &name, vec![task])
        .await
        .map_err(AppError::Upstream)?;
    let pending = read_task_list(&cluster.read_team_tasks(&name).await)
        .iter()
        .any(|task| task.id == task_id && task.status == "pending");
    let run_requested = if pending {
        request_team_run(cluster, &ns, &name)
            .await
            .map_err(AppError::Upstream)?
    } else {
        false
    };
    Ok(Json(serde_json::json!({
        "queued": queued > 0,
        "run_requested": run_requested,
        "decision": decision,
        "team": name,
    })))
}

/// Start the bounded best-effort source poller. Durable `next_poll_at` values
/// and a stable initial jitter spread GitHub traffic across teams.
pub fn spawn_poller(state: AppState, sweep_interval: Duration) {
    if state.cluster().is_none() {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut interval = tokio::time::interval(sweep_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let Some(cluster) = state.cluster() else {
                continue;
            };
            let sources = match cluster
                .list_engineering_sources(MAX_SOURCES_PER_SWEEP)
                .await
            {
                Ok(sources) => sources,
                Err(error) => {
                    tracing::error!(error = %error, "engineering intake source listing failed");
                    continue;
                }
            };
            for source in sources {
                let source_name = source.name_any();
                let (config, cursor, status) = match parse_source(&source) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        tracing::error!(source = %source_name, error = %error, "invalid engineering intake source");
                        let failed = EngineeringSourceStatus {
                            state: EngineeringSyncState::Error,
                            last_sync_at: Some(Utc::now().to_rfc3339()),
                            last_error: Some(truncate_error(error)),
                            ..EngineeringSourceStatus::default()
                        };
                        if let Err(patch_error) = patch_runtime_state(
                            cluster,
                            &source_name,
                            &EngineeringCursor::default(),
                            &failed,
                        )
                        .await
                        {
                            tracing::error!(source = %source_name, error = %patch_error, "failed to record engineering intake source error");
                        }
                        continue;
                    }
                };
                if !verify_source_owner(&source, &config, &config.owner_sub) {
                    let error = "engineering source owner annotations do not match its config";
                    tracing::error!(source = %source_name, error, "invalid engineering intake source");
                    let failed = EngineeringSourceStatus {
                        state: EngineeringSyncState::Error,
                        last_sync_at: Some(Utc::now().to_rfc3339()),
                        last_error: Some(error.into()),
                        ..status
                    };
                    if let Err(patch_error) =
                        patch_runtime_state(cluster, &source_name, &cursor, &failed).await
                    {
                        tracing::error!(source = %source_name, error = %patch_error, "failed to record engineering intake ownership error");
                    }
                    continue;
                }
                if let Err(error) = ensure_auto_run_for_backlog(cluster, &config).await {
                    tracing::warn!(source = %source_name, team = %config.team_name, %error, "engineering intake could not rearm queued work");
                }
                if !config.enabled || !is_due(&status, Utc::now()) {
                    continue;
                }
                match synchronize_source(cluster, &config, cursor, status).await {
                    Ok(updated) => {
                        if let Some(error) = updated.last_error.as_deref() {
                            tracing::warn!(source = %source_name, team = %config.team_name, error, "engineering intake sync completed with errors");
                        } else {
                            tracing::info!(source = %source_name, team = %config.team_name, discovered = updated.items_discovered, queued = updated.items_queued, "engineering intake sync complete");
                        }
                    }
                    Err(error) => {
                        tracing::error!(source = %source_name, team = %config.team_name, error = %error, "engineering intake sync failed")
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(login: &str, head: &str, number: u64) -> GithubPull {
        GithubPull {
            number,
            html_url: format!("https://github.com/acme/api/pull/{number}"),
            title: "Bump serde from 1.0.1 to 1.0.2".into(),
            draft: false,
            updated_at: "2026-07-20T12:00:00Z".into(),
            user: Some(GithubUser {
                login: login.into(),
            }),
            base: GithubRef {
                name: "main".into(),
            },
            head: GithubHead {
                name: head.into(),
                sha: "abc123".into(),
            },
            labels: vec![GithubLabel {
                name: "dependencies".into(),
            }],
        }
    }

    fn task(id: &str, status: &str) -> TeamTaskDto {
        TeamTaskDto {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            depends_on: Vec::new(),
            acceptance_criteria: Vec::new(),
            review_required: false,
            status: status.into(),
            run: (status == "active").then(|| "run-1".into()),
            created_at: Some("2026-07-20T00:00:00Z".into()),
            done_at: (status == "done").then(|| "2026-07-20T01:00:00Z".into()),
            stuck_since: (status == "active").then(|| "2026-07-20T00:30:00Z".into()),
            assignment_nonce: None,
        }
    }

    #[test]
    fn deterministic_ids_are_repo_and_pr_scoped() {
        assert_eq!(work_id("Acme/API", 42), work_id("acme/api", 42));
        assert_ne!(work_id("acme/api", 42), work_id("acme/api", 43));
        assert_ne!(work_id("acme/api", 42), work_id("acme/web", 42));
        assert_eq!(source_id("Acme/API", 42), "github:acme/api:pull:42");
        assert_eq!(
            source_config_map_name("kars-system", "platform"),
            source_config_map_name("kars-system", "platform")
        );
        assert_ne!(
            source_config_map_name("tenant-a", "platform"),
            source_config_map_name("tenant-b", "platform")
        );
        assert_eq!(
            remediation_work_id("Acme/API", Some("package-lock.json"), "@babel/core"),
            remediation_work_id("acme/api", Some("package-lock.json"), "@babel/core")
        );
        assert_ne!(
            remediation_work_id("acme/api", Some("package-lock.json"), "@babel/core"),
            remediation_work_id("acme/api", Some("other/package-lock.json"), "@babel/core")
        );
    }

    #[test]
    fn alert_tasks_lead_with_authoritative_source_facts() {
        let task = alert_backlog_task(
            EngineeringSignal::DependabotAlert,
            GithubAlertRef {
                repo: "pallakatos/kars",
                number: 9,
            },
            "vite alert".into(),
            "Dependabot reported a finding.",
            serde_json::json!({
                "manifest_path": "tests/compat/package-lock.json",
                "package": "vite",
                "vulnerable_version_range": ">= 8.0.0, <= 8.0.15",
                "first_patched_version": "8.0.16",
                "ghsa_id": "GHSA-v6wh-96g9-6wx3",
            }),
            None,
            "2026-07-23T00:00:00Z",
        );
        let prefix = task.description.lines().next().unwrap_or_default();
        assert!(prefix.contains("manifest=tests/compat/package-lock.json"));
        assert!(prefix.contains("fixed=8.0.16"));
        assert!(prefix.contains("ghsa=GHSA-v6wh-96g9-6wx3"));
        assert!(prefix.contains("max2 same target"));
        assert!(prefix.contains("search open+merged PRs"));
        assert!(prefix.contains("no principal substitution"));
    }

    #[test]
    fn legacy_remediation_matching_is_exact_and_repo_scoped() {
        let description = concat!(
            "AUTH SOURCE: manifest=package-lock.json; pkg=react-dom; ghsa=GHSA-a. ",
            "Structured: {\"repo\":\"acme/web\",\"details\":{\"manifest_path\":\"package-lock.json\",",
            "\"package\":\"react-dom\"}}"
        );
        assert!(description_matches_remediation(
            description,
            "acme/web",
            Some("package-lock.json"),
            "react-dom",
        ));
        assert!(!description_matches_remediation(
            description,
            "acme/api",
            Some("package-lock.json"),
            "react-dom",
        ));
        assert!(!description_matches_remediation(
            description,
            "acme/web",
            Some("package-lock.json"),
            "react",
        ));
        assert!(!description_matches_remediation(
            description,
            "acme/web",
            None,
            "react-dom",
        ));
    }

    #[test]
    fn dependabot_detection_accepts_bot_login_or_head_prefix() {
        assert!(is_dependabot_pr(&pull(
            "dependabot[bot]",
            "renovate/foo",
            1
        )));
        assert!(is_dependabot_pr(&pull(
            "someone",
            "dependabot/npm/foo-1.2.3",
            2
        )));
        assert!(!is_dependabot_pr(&pull("renovate[bot]", "renovate/foo", 3)));
    }

    #[test]
    fn open_pull_covers_same_package_or_advisory() {
        let alert = GithubDependabotAlert {
            number: 17,
            html_url: "https://github.com/acme/api/security/dependabot/17".into(),
            dependency: GithubDependabotDependency {
                package: GithubPackage {
                    ecosystem: "npm".into(),
                    name: "@babel/core".into(),
                },
                manifest_path: Some("package-lock.json".into()),
                scope: Some("development".into()),
            },
            security_advisory: Some(GithubSecurityAdvisory {
                ghsa_id: "GHSA-aaaa-bbbb-cccc".into(),
                cve_id: None,
                summary: "test".into(),
                severity: "high".into(),
            }),
            security_vulnerability: GithubSecurityVulnerability {
                vulnerable_version_range: "< 8".into(),
                first_patched_version: Some(GithubPatchedVersion {
                    identifier: "8.0.0".into(),
                }),
            },
            updated_at: None,
        };
        let mut package_pr = pull("agent", "fix-babel-core", 42);
        package_pr.title = "chore: bump @babel/core to 8.0.0".into();
        assert!(open_pull_covers_dependabot_alert(&package_pr, &alert));
        let mut advisory_pr = pull("agent", "security-fix", 43);
        advisory_pr.title = "fix GHSA-aaaa-bbbb-cccc".into();
        assert!(open_pull_covers_dependabot_alert(&advisory_pr, &alert));
        let unrelated = pull("agent", "fix-vite", 44);
        assert!(!open_pull_covers_dependabot_alert(&unrelated, &alert));
    }

    #[test]
    fn parses_github_pull_and_builds_structured_task() {
        let raw = serde_json::json!({
            "number": 7,
            "html_url": "https://github.com/acme/api/pull/7",
            "title": "Bump axum",
            "draft": true,
            "updated_at": "2026-07-20T12:00:00Z",
            "user": {"login": "dependabot[bot]"},
            "base": {"ref": "main"},
            "head": {"ref": "dependabot/cargo/axum-1", "sha": "deadbeef"},
            "labels": [{"name": "dependencies"}, {"name": "rust"}]
        });
        let parsed: GithubPull = serde_json::from_value(raw).unwrap();
        let task = backlog_task("acme/api", &parsed, "2026-07-20T13:00:00Z");
        assert_eq!(task.status, "pending");
        assert!(task.description.contains("\"head_sha\":\"deadbeef\""));
        assert!(task.description.contains("\"draft\":true"));
        assert!(task.description.contains("Never claim CI is green"));
    }

    #[test]
    fn security_alert_ids_are_stable_and_signal_scoped() {
        assert_eq!(
            alert_work_id(EngineeringSignal::CodeScanningAlert, "Acme/API", 42),
            alert_work_id(EngineeringSignal::CodeScanningAlert, "acme/api", 42)
        );
        assert_ne!(
            alert_work_id(EngineeringSignal::CodeScanningAlert, "acme/api", 42),
            alert_work_id(EngineeringSignal::DependabotAlert, "acme/api", 42)
        );
        assert_eq!(
            alert_source_id(EngineeringSignal::SecretScanningAlert, "Acme/API", 7),
            "github:acme/api:secret-scanning-alert:7"
        );
    }

    #[test]
    fn code_scanning_alert_builds_actionable_task() {
        let alert: GithubCodeScanningAlert = serde_json::from_value(serde_json::json!({
            "number": 12,
            "html_url": "https://github.com/acme/api/security/code-scanning/12",
            "rule": {
                "id": "rust/path-injection",
                "name": "Path injection",
                "description": "User-controlled path reaches filesystem access",
                "severity": "error",
                "security_severity_level": "high"
            },
            "most_recent_instance": {
                "location": {"path": "src/files.rs", "start_line": 44, "end_line": 47}
            },
            "updated_at": "2026-07-21T00:00:00Z"
        }))
        .unwrap();
        let task = code_scanning_task("acme/api", &alert, "2026-07-21T01:00:00Z");
        assert!(task.title.contains("Path injection"));
        assert!(task.description.contains("\"severity\":\"high\""));
        assert!(task.description.contains("\"path\":\"src/files.rs\""));
        assert!(task.description.contains("Never claim success"));
    }

    #[test]
    fn secret_scanning_task_never_persists_secret_value() {
        let secret = "ghp_live_secret_value";
        let alert: GithubSecretScanningAlert = serde_json::from_value(serde_json::json!({
            "number": 9,
            "html_url": "https://github.com/acme/api/security/secret-scanning/9",
            "secret_type": "github_personal_access_token",
            "secret_type_display_name": "GitHub Personal Access Token",
            "secret": secret,
            "resolution": null,
            "created_at": "2026-07-21T00:00:00Z",
            "updated_at": "2026-07-21T00:00:00Z"
        }))
        .unwrap();
        let task = secret_scanning_task("acme/api", &alert, "2026-07-21T01:00:00Z");
        assert!(task.description.contains("do not print, persist, or copy"));
        assert!(!task.description.contains(secret));
    }

    #[test]
    fn private_repo_without_security_products_is_unavailable_not_error() {
        let features = GithubRepositoryFeatures {
            private: true,
            security_and_analysis: None,
        };
        let code_error = GithubListError {
            state: EngineeringSignalSyncState::Forbidden,
            detail: "HTTP 403".into(),
        };
        let secret_error = GithubListError {
            state: EngineeringSignalSyncState::Unavailable,
            detail: "HTTP 404".into(),
        };
        for (signal, error) in [
            (EngineeringSignal::CodeScanningAlert, code_error),
            (EngineeringSignal::SecretScanningAlert, secret_error),
        ] {
            let mapped = unavailable_security_product(Some(&features), signal, &error).unwrap();
            assert_eq!(mapped.state, EngineeringSignalSyncState::Unavailable);
            assert!(mapped.detail.contains("not enabled or licensed"));
        }
    }

    #[test]
    fn github_link_parser_finds_next_page() {
        assert_eq!(
            next_link(
                r#"<https://api.github.com/repositories/1/alerts?page=2>; rel="next", <https://api.github.com/repositories/1/alerts?page=4>; rel="last""#
            )
            .as_deref(),
            Some("https://api.github.com/repositories/1/alerts?page=2")
        );
        assert_eq!(next_link(""), None);
    }

    #[test]
    fn dedupe_preserves_existing_active_and_done_tasks() {
        let mut active = task("dependabot-pr-active", "active");
        active.assignment_nonce = Some("run-1-assign-7".into());
        let done = task("dependabot-pr-done", "done");
        let (merged, added) = merge_discovered_tasks(
            vec![active.clone(), done.clone()],
            vec![
                task("dependabot-pr-active", "pending"),
                task("dependabot-pr-done", "pending"),
                task("dependabot-pr-new", "pending"),
            ],
        );
        assert_eq!(added, 1);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].status, "active");
        assert_eq!(merged[0].run, active.run);
        assert_eq!(merged[0].assignment_nonce, active.assignment_nonce);
        assert!(merged[0].review_required);
        assert_eq!(merged[1].status, "done");
        assert_eq!(merged[1].done_at, done.done_at);
        assert!(merged[1].review_required);
    }

    #[test]
    fn changed_open_security_alert_requeues_completed_work() {
        let mut completed = task("code-scanning-alert-abc", "done");
        completed.description = "updated_at=old".into();
        let mut rediscovered = task("code-scanning-alert-abc", "pending");
        rediscovered.description = "updated_at=new".into();
        let (merged, queued) = merge_discovered_tasks(vec![completed], vec![rediscovered.clone()]);
        assert_eq!(queued, 1);
        assert_eq!(merged[0].status, "pending");
        assert_eq!(merged[0].description, rediscovered.description);
        assert!(merged[0].run.is_none());
        assert!(merged[0].done_at.is_none());

        let (unchanged, queued) = merge_discovered_tasks(merged, vec![rediscovered]);
        assert_eq!(queued, 0);
        assert_eq!(unchanged[0].status, "pending");

        let mut refreshed = task("code-scanning-alert-abc", "pending");
        refreshed.description = "updated_at=newer".into();
        let (refreshed_tasks, queued) = merge_discovered_tasks(unchanged, vec![refreshed.clone()]);
        assert_eq!(queued, 0);
        assert_eq!(refreshed_tasks[0].description, refreshed.description);
    }

    #[test]
    fn changed_pending_alert_flows_through_without_using_queue_capacity() {
        let mut existing = task("secret-scanning-alert-abc", "pending");
        existing.description = "updated_at=old".into();
        let mut refreshed = task("secret-scanning-alert-abc", "pending");
        refreshed.description = "updated_at=new".into();
        let mut candidates = Vec::new();
        let mut known = BTreeMap::from([(
            existing.id.clone(),
            (existing.status.clone(), existing.description.clone()),
        )]);
        let mut queued_slots = 0;
        assert!(!append_bounded_tasks(
            &mut candidates,
            &mut known,
            vec![refreshed.clone()],
            &mut queued_slots,
            1,
        ));
        assert_eq!(queued_slots, 0);
        assert_eq!(candidates.len(), 1);
        let (merged, queued) = merge_discovered_tasks(vec![existing], candidates);
        assert_eq!(queued, 0);
        assert_eq!(merged[0].description, refreshed.description);
    }

    #[test]
    fn changed_active_alert_refreshes_source_facts_without_restarting_run() {
        let mut existing = task("dependabot-alert-abc", "active");
        existing.description = "old source facts".into();
        existing.run = Some("run-in-progress".into());
        let mut refreshed = task("dependabot-alert-abc", "pending");
        refreshed.description =
            "AUTHORITATIVE SOURCE FACTS: manifest_path=tests/compat/package-lock.json".into();
        let (merged, queued) = merge_discovered_tasks(vec![existing], vec![refreshed.clone()]);
        assert_eq!(queued, 0);
        assert_eq!(merged[0].status, "active");
        assert_eq!(merged[0].run.as_deref(), Some("run-in-progress"));
        assert_eq!(merged[0].description, refreshed.description);
    }

    #[test]
    fn covered_pending_alert_is_retired_without_touching_active_run() {
        let mut pending = task("dependabot-alert-pending", "pending");
        let mut retirement = task("dependabot-alert-pending", "done");
        retirement.description = "Covered by existing open PR #42".into();
        retirement.done_at = Some("2026-07-23T00:00:00Z".into());
        let (merged, queued) = merge_discovered_tasks(vec![pending.clone()], vec![retirement]);
        assert_eq!(queued, 0);
        assert_eq!(merged[0].status, "done");
        assert!(merged[0].run.is_none());

        pending.status = "active".into();
        pending.run = Some("run-in-progress".into());
        let mut covered = task("dependabot-alert-pending", "done");
        covered.description = "Covered by existing open PR #42".into();
        let (active, queued) = merge_discovered_tasks(vec![pending], vec![covered]);
        assert_eq!(queued, 0);
        assert_eq!(active[0].status, "active");
        assert_eq!(active[0].run.as_deref(), Some("run-in-progress"));
    }

    #[test]
    fn repeated_human_review_decision_requeues_completed_task() {
        let completed = task("github-pr-merge-abc", "done");
        let decision = task("github-pr-merge-abc", "pending");
        let (merged, queued) = merge_discovered_tasks(vec![completed], vec![decision]);
        assert_eq!(queued, 1);
        assert_eq!(merged[0].status, "pending");
        assert!(merged[0].run.is_none());
        assert!(merged[0].done_at.is_none());
    }

    #[test]
    fn repo_authorization_and_limits_are_enforced() {
        let granted = (0..=MAX_REPOS)
            .map(|i| format!("acme/repo-{i}"))
            .collect::<Vec<_>>();
        let too_many = PutEngineeringSourceRequest {
            enabled: true,
            auto_run: true,
            repos: granted.clone(),
            signals: vec![EngineeringSignal::DependabotPr],
            poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECONDS,
        };
        assert!(validate_request(&too_many, &granted).is_err());

        let unauthorized = PutEngineeringSourceRequest {
            enabled: true,
            auto_run: true,
            repos: vec!["other/private".into()],
            signals: vec![EngineeringSignal::DependabotPr],
            poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECONDS,
        };
        assert!(validate_request(&unauthorized, &granted).is_err());

        let invalid_interval = PutEngineeringSourceRequest {
            enabled: true,
            auto_run: true,
            repos: vec!["acme/repo-0".into()],
            signals: vec![EngineeringSignal::DependabotPr],
            poll_interval_seconds: MIN_POLL_INTERVAL_SECONDS - 1,
        };
        assert!(validate_request(&invalid_interval, &granted).is_err());
    }

    #[test]
    fn config_cursor_and_status_serialize_round_trip() {
        let config = EngineeringSourceConfig {
            version: 1,
            team_namespace: "kars-system".into(),
            team_name: "platform".into(),
            owner_sub: "subject-1".into(),
            connection_config_map_ref: "kars-github-connection-deadbeef".into(),
            enabled: true,
            auto_run: true,
            repos: vec!["acme/api".into()],
            signals: vec![EngineeringSignal::DependabotPr],
            poll_interval_seconds: 900,
        };
        let cursor = EngineeringCursor {
            repository_updated_at: BTreeMap::from([(
                "acme/api".into(),
                "2026-07-20T12:00:00Z".into(),
            )]),
        };
        let status = EngineeringSourceStatus {
            state: EngineeringSyncState::Ok,
            last_sync_at: Some("2026-07-20T12:00:00Z".into()),
            last_success_at: Some("2026-07-20T12:00:00Z".into()),
            last_error: None,
            items_discovered: 2,
            items_queued: 1,
            total_items_queued: 4,
            next_poll_at: Some("2026-07-20T12:15:00Z".into()),
            ..Default::default()
        };
        let data = source_data(&config, &cursor, &status).unwrap();
        let cm = ConfigMap {
            data: Some(data),
            ..Default::default()
        };
        let round_trip = parse_source(&cm).unwrap();
        assert_eq!(round_trip, (config, cursor, status));
    }

    fn clean_pull_status() -> serde_json::Value {
        serde_json::json!({
            "state": "open",
            "merged": false,
            "draft": false,
            "mergeable": true,
            "mergeable_state": "clean"
        })
    }

    #[test]
    fn review_readiness_only_flags_green_clean_prs() {
        let (state, _, total, passed) = classify_review_readiness(
            &clean_pull_status(),
            &serde_json::json!({
                "check_runs": [
                    {"status":"completed","conclusion":"success"},
                    {"status":"completed","conclusion":"neutral"}
                ]
            }),
            &serde_json::json!({"state":"success","statuses":[]}),
        );
        assert_eq!(state, EngineeringReviewState::ReadyForReview);
        assert_eq!((total, passed), (2, 2));

        let (state, _, _, _) = classify_review_readiness(
            &clean_pull_status(),
            &serde_json::json!({
                "check_runs": [{"status":"in_progress","conclusion":null}]
            }),
            &serde_json::json!({"state":"pending","statuses":[]}),
        );
        assert_eq!(state, EngineeringReviewState::WaitingForCi);

        let (state, _, _, _) = classify_review_readiness(
            &clean_pull_status(),
            &serde_json::json!({
                "check_runs": [{"status":"completed","conclusion":"failure"}]
            }),
            &serde_json::json!({"state":"failure","statuses":[]}),
        );
        assert_eq!(state, EngineeringReviewState::CiFailed);

        let (state, _, _, _) = classify_review_readiness(
            &clean_pull_status(),
            &serde_json::json!({
                "total_count": 101,
                "check_runs": (0..100).map(|_| serde_json::json!({
                    "status":"completed","conclusion":"success"
                })).collect::<Vec<_>>()
            }),
            &serde_json::json!({"state":"success","statuses":[]}),
        );
        assert_eq!(state, EngineeringReviewState::WaitingForCi);
    }

    #[test]
    fn red_pr_creates_deterministic_followup_work() {
        let item = EngineeringReviewItem {
            repo: "acme/api".into(),
            pr_number: 42,
            pr_url: "https://github.com/acme/api/pull/42".into(),
            title: "Fix dependency".into(),
            run: "run-1".into(),
            source_id: "github:acme/api:pull:42".into(),
            work_id: "dependabot-pr-example".into(),
            task_status: "done".into(),
            run_state: Some("Completed".into()),
            selected_roles: vec!["reviewer".into()],
            delivered_roles: vec!["reviewer".into()],
            artifact_count: Some(1),
            head_sha: "abc123".into(),
            state: EngineeringReviewState::CiFailed,
            detail: "test failed".into(),
            checks_total: 2,
            checks_passed: 1,
            observed_at: "2026-07-20T12:00:00Z".into(),
        };
        let first = review_followup_task(&item, "2026-07-20T12:00:00Z").unwrap();
        let second = review_followup_task(&item, "2026-07-20T13:00:00Z").unwrap();
        assert_eq!(first.id, second.id);
        let mut changed_head = item.clone();
        changed_head.head_sha = "def456".into();
        let changed = review_followup_task(&changed_head, "2026-07-20T14:00:00Z").unwrap();
        assert_eq!(first.id, changed.id);
        assert_ne!(first.description, changed.description);
        assert!(first.description.contains("Never claim green"));
        assert!(first.description.contains("superseded"));
        assert!(first.description.contains("do not repair or rebase it"));
    }

    #[test]
    fn duplicate_prs_create_one_canonical_retirement_task() {
        let first = pull("agent", "fix-js-yaml", 18);
        let second = pull("agent", "fix-js-yaml-again", 24);
        let task = dedupe_followup_task(
            "acme/api",
            "dependency-remediation-abc",
            &[&second, &first],
            "2026-07-20T12:00:00Z",
        )
        .unwrap();
        assert!(task.title.contains("PR #18"));
        assert!(task.description.contains("#24"));
        assert!(task.description.contains("never merge"));
    }
}
