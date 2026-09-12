// kars Bridge BFF — durable GitHub engineering intake for standing teams.
//
// This is intentionally Bridge-owned integration workflow. Source configuration,
// cursors, and status live in an owner-annotated ConfigMap; discovered work is
// merged into the controller's existing durable team task ConfigMap.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::routes::teams::TeamTaskDto;

mod config;
mod endpoints;
mod github;
mod intake;
mod queue;
mod remediation;
mod review;
mod synchronization;

pub(crate) use config::source_config_map_name;
use config::{default_poll_interval, default_true};
pub use endpoints::{decide_review_item, delete_source, get_source, put_source, sync_now};
pub use synchronization::spawn_poller;

#[cfg(test)]
use config::{parse_source, source_data, validate_request};
#[cfg(test)]
use github::{next_link, unavailable_security_product};
#[cfg(test)]
use intake::{
    alert_backlog_task, alert_source_id, alert_work_id, backlog_task, code_scanning_task,
    dependabot_alert_task, is_dependabot_pr, open_pull_may_address_dependabot_alert,
    secret_scanning_task, source_id, work_id,
};
#[cfg(test)]
use queue::{append_bounded_tasks, merge_discovered_tasks};
#[cfg(test)]
use remediation::{description_matches_remediation, remediation_work_id};
#[cfg(test)]
use review::{classify_review_readiness, dedupe_followup_task, review_followup_task};

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

struct GithubAlertRef<'a> {
    repo: &'a str,
    number: u64,
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

struct ReviewExecution<'a> {
    run: &'a str,
    work_id: &'a str,
    task_status: &'a str,
    run_state: Option<String>,
    selected_roles: Vec<String>,
    delivered_roles: Vec<String>,
    artifact_count: Option<usize>,
}

#[cfg(test)]
mod tests;
