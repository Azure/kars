// kars Bridge BFF — Teams API DTOs + handlers.
//
// A Team (KarsTeam) is a standing org with a charter and a cadence loop that
// mints task-force KarsTasks autonomously (design note §11). These endpoints
// project the typed CRD into stable, browser-facing JSON so the Teams surface
// never depends on raw Kubernetes envelopes. Read-only: the controller is the
// sole writer of team membership + generated tasks.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::team::KarsTeam;
use crate::routes::options::{ModelOption, Options, RefOption, build_options};
use crate::routes::tasks::{
    clean_display_name, clean_objective, deliverable_excerpt, deliverable_text,
    extract_pull_requests, is_failure_shaped_output, is_no_change_output, require_cluster,
};
use kube::ResourceExt;
use kube::api::{Api, ListParams, Patch, PatchParams};

/// The on-disk commons index entry (mirrors the controller's `CommonsEntry`).
#[derive(Debug, Deserialize)]
pub struct CommonsIndexEntry {
    pub id: String,
    pub title: String,
    pub author: String,
    pub source_task: String,
    pub created_at: String,
    pub digest: String,
    pub size_bytes: i64,
}

/// Browser-facing commons entry — the index record plus resolved content.
#[derive(Debug, Serialize)]
pub struct CommonsEntryDto {
    pub id: String,
    pub title: String,
    pub author: String,
    pub source_task: String,
    pub created_at: String,
    pub digest: String,
    pub size_bytes: i64,
    pub content: Option<String>,
}

/// Browser-facing commons response.
#[derive(Debug, Serialize)]
pub struct CommonsResponse {
    pub commons: String,
    pub count: i64,
    pub entries: Vec<CommonsEntryDto>,
}

fn commons_entry_key(id: &str) -> String {
    format!(
        "entry-{}",
        id.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            },)
            .collect::<String>()
    )
}

/// One seat in the team roster (the org chart).
#[derive(Debug, Serialize)]
pub struct TeamRoleDto {
    pub name: String,
    pub system_prompt: Option<String>,
    pub tier: Option<i32>,
    /// The materialized member task name, when reconciled.
    pub member_task: Option<String>,
    /// Skills (KarsSkill names) this role acquires (§13).
    pub skills: Vec<String>,
    /// Per-member harness (runtime kind) — the differentiator: members can each
    /// run a different harness. `None` means inherit the team default.
    pub runtime: Option<String>,
    /// Per-member model deployment — `None` means inherit the team default.
    pub model: Option<String>,
}

/// Browser-facing team summary for the Teams index.
#[derive(Debug, Serialize)]
pub struct TeamSummaryDto {
    pub name: String,
    pub display_name: Option<String>,
    pub charter: String,
    pub phase: String,
    pub reporting_to: Option<String>,
    pub tier: i32,
    pub member_count: i64,
    pub generated_task_count: i64,
    pub every_minutes: Option<u32>,
    pub lifecycle_mode: String,
    pub warm_idle_seconds: Option<i64>,
    pub runtime_state: Option<String>,
    pub current_assignment_task: Option<String>,
    pub idle_deadline_at: Option<String>,
    pub paused: bool,
    pub created_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub next_run_at: Option<String>,
    pub health: Option<String>,
    pub detail: Option<String>,
    /// Delivered standing-run count — shown alongside generated so a card
    /// surfaces actual yield, not just scheduling activity.
    pub runs_succeeded: i64,
    pub retained_delivered: i64,
    pub retained_no_action: i64,
    pub retained_failed: i64,
}

/// Browser-facing team detail (charter, org chart, watching status, history).
#[derive(Debug, Serialize)]
pub struct TeamDetailDto {
    pub name: String,
    pub display_name: Option<String>,
    pub charter: String,
    pub phase: String,
    pub reporting_to: Option<String>,
    pub knowledge_commons: Option<String>,
    pub tier: i32,
    pub authority_ceiling: i32,
    pub delegation_depth: i32,
    pub paused: bool,
    pub every_minutes: Option<u32>,
    pub lifecycle_mode: String,
    pub warm_idle_seconds: Option<i64>,
    pub runtime_state: Option<String>,
    pub current_assignment_nonce: Option<String>,
    pub current_assignment_task: Option<String>,
    pub idle_deadline_at: Option<String>,
    pub envelope_digest: Option<String>,
    pub principal_task: Option<String>,
    pub roster: Vec<TeamRoleDto>,
    pub member_count: i64,
    pub generated_task_count: i64,
    pub last_generated_task: Option<String>,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub detail: Option<String>,
    pub health: Option<String>,
    pub runs_succeeded: i64,
    pub tokens_spent_total: i64,
    pub commons_entry_count: i64,
    pub last_success_at: Option<String>,
    pub created_at: Option<String>,
    pub last_activity_at: Option<String>,
    /// The task-force tasks the charter loop has minted, newest first.
    pub generated_tasks: Vec<String>,
    /// Customer-facing outcomes for retained runs, newest first. A cadence tick
    /// that correctly finds no work is a resolved outcome, not a failed delivery.
    pub recent_outcomes: Vec<TeamOutcomeDto>,
    pub recent_outcome_summary: TeamOutcomeSummaryDto,
    /// What the team can reach/use — surfaced for management at a glance.
    /// `*_default` flags mark values inherited from the cluster (no explicit
    /// blueprint override) so the UI can label them honestly rather than
    /// implying the operator chose them.
    pub tool_policy: Option<String>,
    pub tool_policy_default: bool,
    pub mcp_servers: Vec<String>,
    pub git_write_repos: Vec<String>,
    pub egress: Vec<String>,
    pub egress_mode: Option<String>,
    /// Domains the team's agents have ACTUALLY reached, aggregated live from the
    /// learn-mode observation buffer of its currently-running run sandboxes. This
    /// is the concrete "what has it touched so far" — distinct from the declared
    /// `egress` allowlist. Empty when no run is active (the buffer is per-run).
    pub learned_egress: Vec<String>,
    /// Network reachability posture when no explicit egress is declared — the
    /// kernel-level default-deny stance every sandbox runs under.
    pub network_posture: String,
    pub model: Option<String>,
    pub model_fallbacks: Vec<String>,
    pub model_default: bool,
    pub memory: Option<String>,
    /// The harness every run this team mints executes on (blueprint override,
    /// else the sandbox default OpenClaw). `runtime_default` marks the inherited
    /// case so the UI can badge it honestly.
    pub runtime: Option<String>,
    pub runtime_default: bool,
    pub isolation: Option<String>,
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    /// The team's assigned task backlog (pending/active/done), newest last.
    pub tasks: Vec<TeamTaskDto>,
    /// Communication channels enabled on this team's envelope (telegram/slack/…).
    pub channels: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TeamOutcomeDto {
    pub run: String,
    pub disposition: String,
    pub headline: String,
    pub detail: String,
    pub objective: String,
    pub finished_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub tokens: Option<i64>,
    pub model: Option<String>,
    pub pull_requests: Vec<crate::routes::tasks::PullRequestRef>,
    pub artifact_count: i64,
}

#[derive(Debug, Default, Serialize)]
pub struct TeamOutcomeSummaryDto {
    pub change_proposed: i64,
    pub no_action_needed: i64,
    pub completed: i64,
    pub failed: i64,
}

fn run_started_at(run: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let epoch = run.rsplit("-run-").next()?.get(..10)?.parse::<i64>().ok()?;
    chrono::DateTime::from_timestamp(epoch, 0)
}

fn is_internal_artifact_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    normalized.contains("collaboration.jsonl")
        || normalized.ends_with("role-plan.json")
        || normalized.ends_with("research-evidence.jsonl")
        || normalized.ends_with("activity.jsonl")
        || normalized.ends_with("subagent-telemetry.jsonl")
        || normalized.ends_with("execution-contract.json")
        || normalized.ends_with("task-checkpoint.json")
}

fn outcome_work_item(raw: &str) -> String {
    let objective = clean_objective(raw);
    if let Some(task) = objective
        .split_once("TASK:")
        .map(|(_, rest)| rest)
        .map(|rest| rest.split("DETAILS:").next().unwrap_or(rest))
        .and_then(|rest| rest.lines().next())
        .map(str::trim)
        .filter(|task| !task.is_empty())
    {
        return task.chars().take(180).collect();
    }
    clean_display_name(&None, &objective).unwrap_or(objective)
}

fn outcome_from_output(
    run: String,
    data: &std::collections::BTreeMap<String, String>,
) -> TeamOutcomeDto {
    let status = data.get("status").map(String::as_str);
    let raw_output = data.get("output").map(String::as_str).unwrap_or("");
    let output = deliverable_text(raw_output);
    let objective = outcome_work_item(data.get("objective").map(String::as_str).unwrap_or(""));
    let pull_requests = extract_pull_requests(&output);
    let no_action = is_no_change_output(&output);
    let disposition = if status == Some("error") || is_failure_shaped_output(&output) {
        "failed"
    } else if !pull_requests.is_empty() {
        "change_proposed"
    } else if no_action {
        "no_action_needed"
    } else {
        "completed"
    };
    let detail = deliverable_excerpt(&output);
    let headline = if let Some(pr) = pull_requests.first() {
        format!("Change proposed in {} PR #{}", pr.repo, pr.number)
    } else if disposition == "no_action_needed" {
        if detail.is_empty() {
            "No action needed".to_string()
        } else {
            detail.clone()
        }
    } else if disposition == "failed" {
        let lower = output.to_ascii_lowercase();
        if lower.contains("unexpected tokens remaining in message header") {
            "Agent response parser failed".to_string()
        } else if lower.contains("kars sandbox - secure ai runtime")
            && lower.contains("how can i help")
        {
            "Agent returned its runtime banner instead of work".to_string()
        } else if lower.contains("llm request failed")
            || lower.contains("network connection")
            || lower.contains("connection refused")
        {
            "Model or network request failed".to_string()
        } else if lower.contains("now await")
            || lower.contains("awaiting handback")
            || lower.contains("waiting for") && lower.contains("handback")
        {
            "Team run ended before all selected roles returned".to_string()
        } else if detail.is_empty() {
            "Run failed before producing an outcome".to_string()
        } else {
            detail.clone()
        }
    } else {
        clean_display_name(&None, &detail)
            .or_else(|| clean_display_name(&None, &objective))
            .unwrap_or_else(|| "Completed work".to_string())
    };
    let finished_at = data.get("finishedAt").cloned();
    let duration_seconds = finished_at
        .as_deref()
        .and_then(|finished| chrono::DateTime::parse_from_rfc3339(finished).ok())
        .and_then(|finished| {
            run_started_at(&run).map(|started| {
                finished
                    .with_timezone(&chrono::Utc)
                    .signed_duration_since(started)
                    .num_seconds()
                    .max(0)
            })
        });
    let artifact_count = data
        .get("artifacts")
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(raw).ok())
        .map(|artifacts| {
            artifacts
                .iter()
                .filter(|artifact| {
                    artifact
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|name| !is_internal_artifact_name(name))
                })
                .count() as i64
        })
        .unwrap_or(0);
    TeamOutcomeDto {
        run,
        disposition: disposition.to_string(),
        headline,
        detail,
        objective,
        finished_at,
        duration_seconds,
        tokens: data.get("totalTokens").and_then(|value| value.parse().ok()),
        model: data.get("model").cloned(),
        pull_requests,
        artifact_count,
    }
}

fn phase_of(team: &KarsTeam) -> String {
    team.status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Forming".to_string())
}

fn is_team_owner(team: &KarsTeam, principal: &Principal) -> bool {
    team.annotations()
        .get("kars.azure.com/owner-sub")
        .is_some_and(|subject| subject == &principal.sub)
}

pub(crate) async fn require_owned_team(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    name: &str,
    principal: &Principal,
) -> AppResult<KarsTeam> {
    let team = cluster
        .teams(ns)
        .get_opt(name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or(AppError::NotFound)?;
    if !is_team_owner(&team, principal) {
        return Err(AppError::NotFound);
    }
    Ok(team)
}

fn to_summary(team: &KarsTeam) -> TeamSummaryDto {
    let st = team.status.as_ref();
    let created_at = team
        .metadata
        .creation_timestamp
        .as_ref()
        .map(|timestamp| timestamp.0.to_rfc3339());
    let last_run_at = st.and_then(|status| status.last_run_at.clone());
    let last_success_at = st.and_then(|status| status.last_success_at.clone());
    let last_activity_at = [
        st.and_then(|status| status.last_activity_at.clone()),
        last_run_at.clone(),
        last_success_at.clone(),
        created_at.clone(),
    ]
    .into_iter()
    .flatten()
    .max();
    TeamSummaryDto {
        name: team.name_any(),
        display_name: team.spec.display_name.clone(),
        charter: team.spec.charter.clone(),
        phase: phase_of(team),
        reporting_to: team.spec.reporting_to.clone(),
        tier: team.spec.envelope.tier,
        member_count: st.and_then(|s| s.member_count).unwrap_or(0),
        generated_task_count: st.and_then(|s| s.generated_task_count).unwrap_or(0),
        every_minutes: team.spec.cadence.as_ref().and_then(|c| c.every_minutes),
        lifecycle_mode: effective_lifecycle_mode(team),
        warm_idle_seconds: team.spec.warm_idle_seconds,
        runtime_state: runtime_state(team),
        current_assignment_task: st.and_then(|s| s.current_assignment_task.clone()),
        idle_deadline_at: st.and_then(|s| s.idle_deadline_at.clone()),
        paused: team.spec.paused,
        created_at,
        last_run_at,
        last_success_at,
        last_activity_at,
        next_run_at: st.and_then(|s| s.next_run_at.clone()),
        health: st.and_then(|s| s.health.clone()),
        detail: st.and_then(|s| s.detail.clone()),
        runs_succeeded: st.and_then(|s| s.runs_succeeded).unwrap_or(0),
        retained_delivered: 0,
        retained_no_action: 0,
        retained_failed: 0,
    }
}

fn effective_lifecycle_mode(team: &KarsTeam) -> String {
    team.status
        .as_ref()
        .and_then(|status| status.lifecycle_mode.clone())
        .or_else(|| team.spec.lifecycle_mode.clone())
        .unwrap_or_else(|| "ephemeral".into())
}

fn runtime_state(team: &KarsTeam) -> Option<String> {
    team.status
        .as_ref()
        .and_then(|status| status.runtime_state.clone())
}

/// `GET /api/namespaces/:ns/teams` — list standing teams in a namespace.
pub async fn list_teams(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
) -> AppResult<Json<Vec<TeamSummaryDto>>> {
    let cluster = require_cluster(&state)?;
    let api: Api<KarsTeam> = cluster.teams(&ns);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let task_list = cluster
        .tasks(&ns)
        .list(&ListParams::default())
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let mut retained_runs: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    let visible_team_names = list
        .items
        .iter()
        .filter(|team| is_team_owner(team, &principal))
        .map(ResourceExt::name_any)
        .collect::<Vec<_>>();
    let mut retained_outcomes: std::collections::HashMap<String, (i64, i64, i64)> =
        std::collections::HashMap::new();
    for record in cluster.list_mission_output_evidence().await {
        let data = record.data;
        let run = data
            .get("assignmentNonce")
            .cloned()
            .unwrap_or(record.evidence_key);
        let team_name = data
            .get("team")
            .filter(|team| visible_team_names.contains(team))
            .or_else(|| {
                visible_team_names
                    .iter()
                    .find(|team_name| run.starts_with(&format!("{team_name}-run-")))
            });
        let Some(team_name) = team_name else {
            continue;
        };
        let counts = retained_outcomes.entry(team_name.clone()).or_default();
        let status = data.get("status").map(String::as_str);
        let output = data.get("output").map(String::as_str).unwrap_or("");
        if status == Some("error") || is_failure_shaped_output(output) {
            counts.2 += 1;
        } else if is_no_change_output(output) {
            counts.1 += 1;
        } else if crate::routes::tasks::is_real_deliverable(status, output) {
            counts.0 += 1;
        }
    }
    for task in task_list.items {
        let is_run = task
            .annotations()
            .get("kars.azure.com/team-role")
            .is_some_and(|role| role == "taskforce");
        if !is_run {
            continue;
        }
        if let Some(team_name) = task.labels().get("kars.azure.com/team") {
            *retained_runs.entry(team_name.clone()).or_default() += 1;
        }
    }
    let mut summaries = list
        .items
        .iter()
        .filter(|team| is_team_owner(team, &principal))
        .map(|team| {
            let mut summary = to_summary(team);
            summary.generated_task_count = summary
                .generated_task_count
                .max(*retained_runs.get(&summary.name).unwrap_or(&0));
            if let Some((delivered, no_action, failed)) = retained_outcomes.get(&summary.name) {
                summary.retained_delivered = *delivered;
                summary.retained_no_action = *no_action;
                summary.retained_failed = *failed;
            }
            summary
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| right.last_activity_at.cmp(&left.last_activity_at));
    Ok(Json(summaries))
}

/// Derive a meaningful, distinct title for a commons entry. The controller
/// historically titled every entry by the team charter's first line, so the
/// Knowledge tab showed 50+ identical rows. We recover a real headline from the
/// (already envelope-unwrapped) content: the first markdown heading, else the
/// first substantive line, capped. Falls back to the stored title only when the
/// content yields nothing usable. `charter_line` is passed so we can recognize
/// (and replace) the legacy charter-as-title rows.
fn commons_title(stored: &str, content: &str, charter_line: &str) -> String {
    let derive = || -> Option<String> {
        let lines: Vec<&str> = content.lines().collect();
        let clean = |line: &str| -> Option<String> {
            let heading = line
                .trim()
                .trim_start_matches('#')
                .trim()
                .trim_start_matches("**")
                .trim_end_matches("**")
                .trim();
            // Drop leading noise — stray "?" placeholders (where an emoji was
            // stripped upstream), bullets, dashes — so the title starts on a word.
            let heading = heading
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .trim();
            if !heading.chars().any(char::is_alphanumeric) {
                return None;
            }
            let lower = heading.to_ascii_lowercase();
            if [
                "kars sandbox - secure ai runtime",
                "foundry project",
                "model:",
                "sandbox id",
                "security summary",
                "capabilities",
                "role plan",
                "role roster",
                "roles spawned",
            ]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            {
                return None;
            }

            let title: String = heading.chars().take(90).collect();
            Some(if heading.chars().count() > 90 {
                format!("{}…", title.trim_end())
            } else {
                title
            })
        };
        // Prefer the first real markdown heading near the top — briefings lead
        // with a status sentence then a "## …" headline, which reads far better
        // as a title than the preamble line.
        for line in lines.iter().take(14) {
            if line.trim_start().starts_with('#')
                && let Some(t) = clean(line)
            {
                return Some(t);
            }
        }
        // Otherwise the first substantive line.
        lines.iter().find_map(|l| clean(l))
    };
    // Replace the legacy "title == charter" rows and any empty title. The
    // controller stored the title as the charter's first line *truncated to 160
    // chars*, so we match by prefix rather than equality.
    let stored_t = stored.trim();
    let stored_lower = stored_t.to_ascii_lowercase();
    let cl = charter_line.trim();
    let legacy = stored_t.is_empty()
        || stored_t == cl
        || (stored_t.len() >= 24 && cl.starts_with(stored_t))
        || (cl.len() >= 24 && stored_t.starts_with(cl))
        || ["kars sandbox", "role plan", "role roster", "current state"]
            .iter()
            .any(|prefix| stored_lower.starts_with(prefix));
    if legacy {
        derive().unwrap_or_else(|| stored_t.to_string())
    } else {
        stored_t.to_string()
    }
}

/// `GET /api/namespaces/:ns/teams/:name/commons` — the team's shared,
/// provenance-tracked knowledge commons (design note §14). Each entry records
/// which run authored it, when, and a content digest.
pub async fn get_team_commons(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<CommonsResponse>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    // Commons name defaults to the team name when unset.
    let commons = team
        .spec
        .knowledge_commons
        .clone()
        .unwrap_or_else(|| name.clone());

    let data = cluster.read_commons(&commons).await.unwrap_or_default();
    let index: Vec<CommonsIndexEntry> = data
        .get("index.json")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    // The charter's first line is what the controller historically used as every
    // entry's title; we use it to recognize and replace those duplicate rows.
    let charter_line = team
        .spec
        .charter
        .lines()
        .next()
        .unwrap_or(&team.spec.charter)
        .to_string();

    // Newest first, with content resolved from the companion keys. We *heal* two
    // legacy defects here so the Knowledge tab is readable for entries written
    // before the source-side fixes: (1) content stored as the raw agent JSON
    // envelope is unwrapped to its prose deliverable; (2) the duplicate
    // charter-as-title is replaced with a real headline derived from that prose.
    let mut entries: Vec<CommonsEntryDto> = index
        .into_iter()
        .rev()
        .map(|e| {
            let key = format!(
                "entry-{}",
                e.id.chars()
                    .map(
                        |c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                            c
                        } else {
                            '_'
                        }
                    )
                    .collect::<String>()
            );
            let content = data.get(&key).map(|c| deliverable_text(c));
            let title = match content.as_deref() {
                Some(c) => commons_title(&e.title, c, &charter_line),
                None => e.title.clone(),
            };
            CommonsEntryDto {
                id: e.id,
                title,
                author: e.author,
                source_task: e.source_task,
                created_at: e.created_at,
                digest: e.digest,
                size_bytes: e.size_bytes,
                content,
            }
        })
        .collect();
    let total_entries = entries.len() as i64;
    entries.truncate(50);

    Ok(Json(CommonsResponse {
        commons,
        count: total_entries,
        entries,
    }))
}

/// `GET /api/namespaces/:ns/teams/:name/runs/:run/archive` — retrieve one
/// durable archived run directly from the full commons index.
pub async fn get_archived_run(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, run)): Path<(String, String, String)>,
) -> AppResult<Json<CommonsEntryDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let taskforce = run.starts_with(&format!("{name}-run-"));
    let persistent = run.starts_with(&format!("{name}-principal-assign-"));
    if !taskforce && !persistent {
        return Err(AppError::NotFound);
    }
    let commons_name = team
        .spec
        .knowledge_commons
        .as_deref()
        .filter(|commons| !commons.trim().is_empty())
        .unwrap_or(&name);
    let data = cluster
        .read_commons(commons_name)
        .await
        .ok_or(AppError::NotFound)?;
    let entry = data
        .get("index.json")
        .and_then(|raw| serde_json::from_str::<Vec<CommonsIndexEntry>>(raw).ok())
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|entry| entry.id == run || entry.source_task == run)
        })
        .ok_or(AppError::NotFound)?;
    let content = data
        .get(&commons_entry_key(&entry.id))
        .map(|content| deliverable_text(content));
    let charter_line = team
        .spec
        .charter
        .lines()
        .next()
        .unwrap_or(&team.spec.charter);
    let title = content
        .as_deref()
        .map(|content| commons_title(&entry.title, content, charter_line))
        .unwrap_or(entry.title);
    Ok(Json(CommonsEntryDto {
        id: entry.id,
        title,
        author: entry.author,
        source_task: entry.source_task,
        created_at: entry.created_at,
        digest: entry.digest,
        size_bytes: entry.size_bytes,
        content,
    }))
}

#[derive(Debug, serde::Deserialize)]
pub struct PromoteRequest {
    pub tier: i32,
}

/// `POST /api/namespaces/:ns/teams/:name/promote` — request a governed
/// promotion to a higher autonomy tier (§12). Sets `spec.requestedTier`; the
/// controller opens a human approval and only widens the envelope on approval.
/// The BFF never raises the envelope directly (the envelope-write VAP forbids
/// it) — it only records the request.
pub async fn promote_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(body): Json<PromoteRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    if !(1..=5).contains(&body.tier) {
        return Err(AppError::BadRequest("tier must be in 1..5".into()));
    }
    let api: Api<KarsTeam> = cluster.teams(&ns);
    let patch = serde_json::json!({ "spec": { "requestedTier": body.tier } });
    api.patch(
        &name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(patch),
    )
    .await
    .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "requested": true,
        "tier": body.tier,
        "note": "A human approval has been opened. The team is promoted only once it is approved."
    })))
}

/// `POST /api/namespaces/:ns/teams/:name/run` — trigger an immediate run
/// ("Run now"). Sets the `kars.azure.com/run-now` annotation; the controller
/// mints one taskforce run under the normal readiness gates and clears the
/// annotation. This is the only way to make a cadence-less ("on demand") team
/// act, and a manual kick for cadenced teams.
pub async fn run_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    Ok(Json(
        request_team_run(cluster, &ns, &name, &principal).await?,
    ))
}

pub(crate) async fn request_team_run(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    name: &str,
    principal: &Principal,
) -> AppResult<serde_json::Value> {
    let api: Api<KarsTeam> = cluster.teams(ns);
    let team = require_owned_team(cluster, ns, name, principal).await?;
    if team.spec.paused {
        return Err(AppError::BadRequest(
            "team is paused — resume it before running".into(),
        ));
    }
    if team
        .annotations()
        .get("kars.azure.com/run-now")
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err(AppError::BadRequest(
            "a run request is already pending for this team".into(),
        ));
    }
    let active_run = cluster
        .tasks(ns)
        .list(&ListParams::default().labels(&format!("kars.azure.com/team={name}")))
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .items
        .into_iter()
        .any(|task| {
            task.annotations()
                .get("kars.azure.com/team-role")
                .is_some_and(|role| role == "taskforce")
                && task
                    .spec
                    .execution
                    .as_ref()
                    .is_some_and(|execution| execution.launch)
        });
    if active_run {
        return Err(AppError::BadRequest(
            "this team already has a run in progress".into(),
        ));
    }
    let patch = serde_json::json!({
        "metadata": { "annotations": { "kars.azure.com/run-now": chrono::Utc::now().to_rfc3339() } }
    });
    api.patch(
        name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(patch),
    )
    .await
    .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(serde_json::json!({
        "triggered": true,
        "note": "A run has been requested. It appears under the team's runs once the principal launches."
    }))
}

#[derive(Debug, Deserialize)]
pub struct HaltTeamRunRequest {
    pub reason: Option<String>,
}

/// Governed emergency stop for a standing-team run. The team is paused first
/// so cadence/intake cannot immediately mint replacement work, then the active
/// task is un-launched while its trace, output, and halt decision remain.
pub async fn halt_team_run(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, run)): Path<(String, String, String)>,
    Json(body): Json<HaltTeamRunRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    Ok(Json(
        request_team_run_halt(
            cluster,
            &ns,
            &name,
            &run,
            body.reason.as_deref(),
            &principal,
        )
        .await?,
    ))
}

pub(crate) async fn request_team_run_halt(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    name: &str,
    run: &str,
    reason: Option<&str>,
    principal: &Principal,
) -> AppResult<serde_json::Value> {
    require_owned_team(cluster, ns, name, principal).await?;
    let tasks = cluster.tasks(ns);
    let task = tasks
        .get(run)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    if task
        .labels()
        .get("kars.azure.com/team")
        .is_none_or(|team| team != name)
        || task
            .annotations()
            .get("kars.azure.com/team-role")
            .is_none_or(|role| role != "taskforce")
    {
        return Err(AppError::BadRequest(
            "the requested task is not a taskforce run owned by this team".into(),
        ));
    }
    if !task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch)
    {
        return Err(AppError::Conflict(
            "the requested team run is not active".into(),
        ));
    }

    let reason = reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("operator emergency-stop");
    let at = chrono::Utc::now().to_rfc3339();
    cluster
        .teams(ns)
        .patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({"spec": {"paused": true}})),
        )
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    tasks
        .patch(
            run,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({
                "metadata": {
                    "annotations": {
                        "kars.azure.com/halted": format!(
                            "halted by operator at {at}: {reason}"
                        )
                    }
                },
                "spec": {"execution": {"launch": false}}
            })),
        )
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;

    Ok(serde_json::json!({
        "halted": true,
        "team_paused": true,
        "run": run,
        "at": at,
        "reason": reason,
        "note": "The run sandbox is being torn down and the standing team is paused. Retained evidence remains available."
    }))
}

/// `DELETE /api/namespaces/:ns/teams/:name` — permanently delete a standing
/// team. Deleting the `KarsTeam` cascade-removes its runs + member sandboxes;
/// the BFF then sweeps the team's shared-memory commons, task backlog, and
/// channel secret so nothing is orphaned. Idempotent-ish: a not-found team is a
/// 404, but missing aux objects are ignored.
pub async fn delete_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    cluster
        .delete_team(
            &ns,
            &name,
            team.metadata
                .uid
                .as_deref()
                .ok_or_else(|| AppError::Conflict("Team UID missing".into()))?,
            team.metadata
                .resource_version
                .as_deref()
                .ok_or_else(|| AppError::Conflict("Team resourceVersion missing".into()))?,
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "deleted": true,
        "note": "Team deletion requested. Core garbage-collects sources bound to this exact Team UID; legacy credential stores are retained for operator review."
    })))
}

/// One backlog task (mirrors the controller's `team_tasks::TeamTask`).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TeamTaskDto {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub review_required: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_nonce: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddTaskRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub review_required: bool,
}

pub(crate) fn read_task_list(raw: &str) -> Vec<TeamTaskDto> {
    serde_json::from_str::<Vec<TeamTaskDto>>(raw).unwrap_or_default()
}

/// `GET /api/namespaces/:ns/teams/:name/tasks` — the team's task backlog.
pub async fn list_team_tasks(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<Vec<TeamTaskDto>>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    Ok(Json(read_task_list(&cluster.read_team_tasks(&name).await)))
}

/// `POST /api/namespaces/:ns/teams/:name/tasks` — append a task to the backlog.
/// The controller picks up the oldest `pending` task on its next run.
pub async fn add_team_task(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(b): Json<AddTaskRequest>,
) -> AppResult<Json<TeamTaskDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    if b.title.trim().is_empty() {
        return Err(AppError::BadRequest("task title is required".into()));
    }
    let existing_tasks = read_task_list(&cluster.read_team_tasks(&name).await);
    if let Some(missing) = b.depends_on.iter().find(|dependency| {
        !existing_tasks
            .iter()
            .any(|task| task.id.as_str() == dependency.as_str())
    }) {
        return Err(AppError::BadRequest(format!(
            "task dependency '{missing}' does not exist"
        )));
    }
    let requested_id =
        b.id.as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| {
                id.to_ascii_lowercase()
                    .chars()
                    .map(|character| {
                        if character.is_ascii_alphanumeric() || character == '-' {
                            character
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>()
                    .trim_matches('-')
                    .chars()
                    .take(63)
                    .collect::<String>()
            })
            .filter(|id| !id.is_empty());
    let task_id =
        requested_id.unwrap_or_else(|| format!("t-{}", chrono::Utc::now().timestamp_micros()));
    if existing_tasks.iter().any(|task| task.id == task_id) {
        return Err(AppError::BadRequest(format!(
            "task id '{task_id}' already exists"
        )));
    }
    let task = TeamTaskDto {
        id: task_id,
        title: b.title.trim().to_string(),
        description: b.description.trim().to_string(),
        depends_on: b.depends_on,
        acceptance_criteria: b
            .acceptance_criteria
            .into_iter()
            .map(|criterion| criterion.trim().to_string())
            .filter(|criterion| !criterion.is_empty())
            .take(20)
            .collect(),
        review_required: b.review_required,
        status: "pending".into(),
        run: None,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    };
    let task_for_write = task.clone();
    let duplicate = std::sync::atomic::AtomicBool::new(false);
    let missing_dependency = std::sync::Mutex::new(None::<String>);
    cluster
        .update_configmap_data(
            &format!("kars-team-tasks-{name}"),
            &[("kars.azure.com/team-tasks", name.as_str())],
            |data| {
                let mut tasks = data
                    .get("tasks.json")
                    .map(|raw| read_task_list(raw))
                    .unwrap_or_default();
                if tasks
                    .iter()
                    .any(|existing| existing.id == task_for_write.id)
                {
                    duplicate.store(true, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
                if let Some(dependency) = task_for_write.depends_on.iter().find(|dependency| {
                    !tasks
                        .iter()
                        .any(|task| task.id.as_str() == dependency.as_str())
                }) {
                    *missing_dependency.lock().expect("dependency lock") = Some(dependency.clone());
                    return;
                }
                tasks.push(task_for_write.clone());
                data.insert(
                    "tasks.json".into(),
                    serde_json::to_string(&tasks).unwrap_or_else(|_| "[]".into()),
                );
            },
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if duplicate.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(AppError::Conflict(format!(
            "task id '{}' already exists",
            task.id
        )));
    }
    if let Some(dependency) = missing_dependency.lock().expect("dependency lock").clone() {
        return Err(AppError::Conflict(format!(
            "task dependency '{dependency}' disappeared during update"
        )));
    }
    Ok(Json(task))
}

/// `DELETE /api/namespaces/:ns/teams/:name/tasks/:task_id` — remove a task from
/// the backlog (any status; removing an active task doesn't stop its run).
pub async fn delete_team_task(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, task_id)): Path<(String, String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let removed = std::sync::atomic::AtomicBool::new(false);
    let dependency_blocked = std::sync::atomic::AtomicBool::new(false);
    cluster
        .update_configmap_data(
            &format!("kars-team-tasks-{name}"),
            &[("kars.azure.com/team-tasks", name.as_str())],
            |data| {
                let mut tasks = data
                    .get("tasks.json")
                    .map(|raw| read_task_list(raw))
                    .unwrap_or_default();
                if tasks.iter().any(|task| {
                    task.id != task_id && task.depends_on.iter().any(|id| id == &task_id)
                }) {
                    dependency_blocked.store(true, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
                let before = tasks.len();
                tasks.retain(|task| task.id != task_id);
                removed.store(tasks.len() != before, std::sync::atomic::Ordering::Relaxed);
                data.insert(
                    "tasks.json".into(),
                    serde_json::to_string(&tasks).unwrap_or_else(|_| "[]".into()),
                );
            },
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if dependency_blocked.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(AppError::Conflict(
            "cannot delete a milestone that is referenced by dependent work".into(),
        ));
    }
    if !removed.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(AppError::NotFound);
    }

    Ok(Json(serde_json::json!({ "removed": true })))
}

#[derive(Debug, Deserialize)]
pub struct ReviewTeamTaskRequest {
    pub decision: String,
    pub feedback: Option<String>,
}

pub async fn review_team_task(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, task_id)): Path<(String, String, String)>,
    Json(body): Json<ReviewTeamTaskRequest>,
) -> AppResult<Json<TeamTaskDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    if !matches!(body.decision.as_str(), "approve" | "request_changes") {
        return Err(AppError::BadRequest(
            "decision must be approve or request_changes".into(),
        ));
    }
    let current = read_task_list(&cluster.read_team_tasks(&name).await);
    let existing = current
        .iter()
        .find(|task| task.id == task_id)
        .cloned()
        .ok_or(AppError::NotFound)?;
    if existing.status != "awaiting_review" {
        return Err(AppError::BadRequest(
            "only an awaiting_review milestone can be decided".into(),
        ));
    }
    let feedback = body
        .feedback
        .as_deref()
        .map(str::trim)
        .filter(|feedback| !feedback.is_empty())
        .map(str::to_string);
    if body.decision == "request_changes" && feedback.is_none() {
        return Err(AppError::BadRequest(
            "request_changes requires written feedback".into(),
        ));
    }
    let approvals = cluster.approvals(&ns);
    let selector = format!("kars.azure.com/team={name},kars.azure.com/milestone={task_id}");
    let approval = approvals
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
        .into_iter()
        .find(|approval| {
            approval.spec.action.kind == "checkpoint"
                && approval.spec.decision.is_none()
                && approval
                    .status
                    .as_ref()
                    .and_then(|status| status.phase.as_deref())
                    .is_none_or(|phase| phase == "Pending")
        })
        .ok_or_else(|| {
            AppError::Conflict(
                "checkpoint approval is not pending yet; refresh before deciding".into(),
            )
        })?;
    approvals
        .patch(
            &approval.name_any(),
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({
                "spec": {
                    "decision": {
                        "verdict": if body.decision == "approve" { "approve" } else { "deny" },
                        "decider": principal.name,
                        "deciderSubject": principal.sub,
                        "deciderRoles": principal.roles,
                        "reason": feedback,
                    }
                }
            })),
        )
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;

    let updated = read_task_list(&cluster.read_team_tasks(&name).await)
        .into_iter()
        .find(|task| task.id == task_id)
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

// ─── Communication channels (part of a team's envelope) ──────────────────────
// A standing team can report to its operator over Telegram / Slack / Discord /
// WhatsApp. Tokens live ONLY in the K8s Secret `kars-team-channel-<team>`,
// propagated by the controller into each ephemeral run sandbox. SECURITY: the
// API is write-only for tokens — GET never returns a token, only which channels
// are enabled.

/// Map a channel id → the env keys the sandbox entrypoint reads for it.
pub(crate) fn channel_env_keys(channel: &str) -> &'static [&'static str] {
    match channel {
        "telegram" => &["TELEGRAM_BOT_TOKEN", "TELEGRAM_ALLOW_FROM"],
        "slack" => &["SLACK_BOT_TOKEN"],
        "discord" => &["DISCORD_BOT_TOKEN"],
        "whatsapp" => &["WHATSAPP_ENABLED"],
        // Teams uses a dedicated Secret (kars-bridge-teams), not workspace channels.
        // Only a non-secret marker key goes in workspace-channels for enabled detection.
        "teams" => &["TEAMS_ENABLED"],
        _ => &[],
    }
}

/// Derive which channels are enabled from the present secret keys (no values).
pub(crate) const SUPPORTED_CHANNELS: &[&str] =
    &["telegram", "slack", "discord", "whatsapp", "teams"];

pub(crate) fn channels_from_keys(keys: &[String]) -> Vec<String> {
    SUPPORTED_CHANNELS
        .iter()
        .copied()
        .filter(|ch| {
            // A channel is "enabled" if its primary token/flag key is present.
            let primary = channel_env_keys(ch).first().copied().unwrap_or("");
            keys.iter().any(|k| k == primary)
        })
        .map(String::from)
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelQualificationDto {
    pub channel: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChannelsDto {
    /// Channel ids currently enabled (e.g. ["telegram","slack"]).
    pub enabled: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<ChannelQualificationDto>,
}

async fn effective_team_route(
    cluster: &crate::kars::cluster::Cluster,
    team: &KarsTeam,
) -> Option<(String, String, String)> {
    let runtime = team
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.runtime.clone())
        .filter(|runtime| !runtime.is_empty())
        .unwrap_or_else(|| "OpenClaw".to_string());
    if let Some(model) = team
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.model.as_ref())
    {
        return Some((runtime, model.provider.clone(), model.deployment.clone()));
    }
    let deployment = cluster.controller_default_model().await?;
    let provider = cluster.controller_provider().await.map(|(id, _, _)| id);
    Some((
        runtime,
        crate::routes::options::provider_for(&deployment, None, provider.as_deref()),
        deployment,
    ))
}

async fn channel_statuses_for_team(
    cluster: &crate::kars::cluster::Cluster,
    team: &KarsTeam,
    enabled: &[String],
) -> Vec<ChannelQualificationDto> {
    let route = effective_team_route(cluster, team).await;
    SUPPORTED_CHANNELS
        .iter()
        .copied()
        .map(|channel| {
            let enabled = enabled.iter().any(|configured| configured == channel);
            match route.as_ref() {
                Some((runtime, provider, deployment)) => {
                    let qualification = crate::routes::options::channel_adapter_qualified_for_route(
                        runtime,
                        provider,
                        deployment,
                        channel,
                    );
                    match qualification {
                        Ok(qualified) => ChannelQualificationDto {
                            channel: channel.to_string(),
                            enabled,
                            qualified: Some(qualified),
                            detail: Some(if qualified {
                                format!(
                                    "Retained channel-adapter evidence exists for {}.",
                                    crate::routes::options::route_label(
                                        runtime, provider, deployment
                                    )
                                )
                            } else {
                                format!(
                                    "No retained channel-adapter qualification matches {}. Credentials can be configured later, but generic route records do not prove this channel adapter.",
                                    crate::routes::options::route_label(
                                        runtime, provider, deployment
                                    )
                                )
                            }),
                        },
                        Err(error) => ChannelQualificationDto {
                            channel: channel.to_string(),
                            enabled,
                            qualified: None,
                            detail: Some(format!(
                                "Channel qualification could not be evaluated: {error}"
                            )),
                        },
                    }
                }
                None => ChannelQualificationDto {
                    channel: channel.to_string(),
                    enabled,
                    qualified: None,
                    detail: Some(
                        "The team has no effective runtime/model route yet, so channel qualification cannot be evaluated."
                            .into(),
                    ),
                },
            }
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct SetChannelRequest {
    /// Channel id: telegram | slack | discord | whatsapp.
    pub channel: String,
    /// The channel's bot token / OAuth token. For whatsapp send "true".
    pub token: String,
    /// Telegram only: comma-separated allowed numeric user IDs.
    #[serde(default)]
    pub allow_from: Option<String>,
}

/// `GET /api/namespaces/:ns/teams/:name/channels` — which channels are enabled.
pub async fn get_team_channels(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let keys = cluster
        .team_channel_keys(&ns, &name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&keys);
    Ok(Json(ChannelsDto {
        statuses: channel_statuses_for_team(cluster, &team, &enabled).await,
        enabled,
    }))
}

/// `POST /api/namespaces/:ns/teams/:name/channels` — enable/update a channel.
/// The token is written straight into the team's channel Secret and never
/// echoed back.
pub async fn set_team_channel(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(b): Json<SetChannelRequest>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let keys = channel_env_keys(b.channel.as_str());
    if keys.is_empty() {
        return Err(AppError::BadRequest(format!(
            "unknown channel '{}': use telegram|slack|discord|whatsapp",
            b.channel
        )));
    }
    if b.token.trim().is_empty() {
        return Err(AppError::BadRequest("token is required".into()));
    }
    let mut data = std::collections::BTreeMap::new();
    // whatsapp uses a presence flag, not a token.
    let primary = keys[0];
    data.insert(primary.to_string(), b.token.trim().to_string());
    if b.channel == "telegram"
        && let Some(allow) = b
            .allow_from
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        data.insert("TELEGRAM_ALLOW_FROM".to_string(), allow.to_string());
    }
    cluster
        .merge_team_channel(&ns, &name, data)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let after = cluster
        .team_channel_keys(&ns, &name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&after);
    Ok(Json(ChannelsDto {
        statuses: channel_statuses_for_team(cluster, &team, &enabled).await,
        enabled,
    }))
}

/// `DELETE /api/namespaces/:ns/teams/:name/channels/:channel` — disable a channel.
pub async fn delete_team_channel(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, channel)): Path<(String, String, String)>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let keys: Vec<String> = channel_env_keys(channel.as_str())
        .iter()
        .map(|s| s.to_string())
        .collect();
    if keys.is_empty() {
        return Err(AppError::BadRequest(format!("unknown channel '{channel}'")));
    }
    cluster
        .remove_team_channel_keys(&ns, &name, &keys)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let after = cluster
        .team_channel_keys(&ns, &name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&after);
    Ok(Json(ChannelsDto {
        statuses: channel_statuses_for_team(cluster, &team, &enabled).await,
        enabled,
    }))
}

pub async fn get_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<TeamDetailDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;

    // Resolve generated task-force tasks: KarsTasks in the namespace owned by
    // this team whose name carries the `<team>-run-` standing-operation prefix.
    let tasks: Api<crate::kars::task::KarsTask> = cluster.tasks(&ns);
    let run_prefix = format!("{name}-run-");
    let mut generated_tasks: Vec<String> = tasks
        .list(&ListParams::default())
        .await
        .map(|l| {
            l.items
                .iter()
                .map(kube::ResourceExt::name_any)
                .filter(|n| n.starts_with(&run_prefix))
                .collect()
        })
        .unwrap_or_default();
    generated_tasks.sort();
    generated_tasks.reverse();
    let retained_run_count = generated_tasks.len() as i64;
    let newest_retained_run = generated_tasks.first().cloned();
    let retained_run_names = generated_tasks
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut recent_outcomes = cluster
        .list_mission_output_evidence()
        .await
        .into_iter()
        .filter_map(|record| {
            let run = record
                .data
                .get("assignmentNonce")
                .cloned()
                .unwrap_or(record.evidence_key);
            (retained_run_names.contains(&run) || record.data.get("team") == Some(&name))
                .then(|| outcome_from_output(run, &record.data))
        })
        .collect::<Vec<_>>();
    recent_outcomes.sort_by(|left, right| {
        right
            .finished_at
            .cmp(&left.finished_at)
            .then_with(|| right.run.cmp(&left.run))
    });
    let mut recent_outcome_summary = TeamOutcomeSummaryDto::default();
    for outcome in &recent_outcomes {
        match outcome.disposition.as_str() {
            "change_proposed" => recent_outcome_summary.change_proposed += 1,
            "no_action_needed" => recent_outcome_summary.no_action_needed += 1,
            "failed" => recent_outcome_summary.failed += 1,
            _ => recent_outcome_summary.completed += 1,
        }
    }

    let st = team.status.as_ref();
    let member_names: Vec<String> = st
        .map(|s| s.member_refs.iter().map(|r| r.name.clone()).collect())
        .unwrap_or_default();

    // Build the org chart: pair each roster role with its materialized member
    // task (named `<team>-<role>` by the reconciler).
    let roster: Vec<TeamRoleDto> = team
        .spec
        .roster
        .iter()
        .map(|role| {
            let member_task = format!("{name}-{}", role.name);
            let materialized = member_names.iter().any(|m| m == &member_task);
            TeamRoleDto {
                name: role.name.clone(),
                system_prompt: role.system_prompt.clone(),
                tier: role.envelope.as_ref().map(|e| e.tier),
                member_task: materialized.then_some(member_task),
                skills: role.skills.clone(),
                runtime: role.blueprint.as_ref().and_then(|b| b.runtime.clone()),
                model: role
                    .blueprint
                    .as_ref()
                    .and_then(|b| b.model.as_ref())
                    .map(|m| format!("{}::{}", m.provider, m.deployment)),
            }
        })
        .collect();

    let bp = team.spec.blueprint.as_ref();
    // Effective tool policy: explicit blueprint override, else the system
    // default `kars-default` (applied to every run sandbox via the
    // `system-default=true` sandbox selector). Never "none" — a run without a
    // governing policy fails closed.
    let bp_tool_policy = bp.and_then(|b| b.tool_policy.clone());
    let tool_policy_default = bp_tool_policy.is_none();
    let tool_policy = bp_tool_policy.or_else(|| Some("kars-default".to_string()));
    // Effective model: explicit blueprint override, else the controller's
    // KARS_TASK_DEFAULT_MODEL that every run actually inherits.
    let bp_model = bp
        .and_then(|b| b.model.as_ref())
        .map(|m| format!("{}::{}", m.provider, m.deployment));
    let model_default = bp_model.is_none();
    let model = match bp_model {
        Some(m) => Some(m),
        None => cluster.controller_default_model().await,
    };
    // Effective harness: explicit blueprint override, else the sandbox default
    // (OpenClaw). Team runs inherit this via launched_run_blueprint.
    let bp_runtime = bp.and_then(|b| b.runtime.clone()).filter(|s| !s.is_empty());
    let runtime_default = bp_runtime.is_none();
    let runtime = bp_runtime.or_else(|| Some("OpenClaw".to_string()));
    let egress: Vec<String> = bp
        .map(|b| {
            b.egress
                .iter()
                .map(|e| {
                    if let Some(p) = e.port {
                        format!("{}:{}", e.host, p)
                    } else {
                        e.host.clone()
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Concrete "domains reached so far": aggregate the learn-mode observation
    // buffers of the team's currently-running run sandboxes (`<team>-run-<epoch>`
    // in kars-system). Per-run + best-effort — empty when no run is live.
    let mut learned_egress: Vec<String> = Vec::new();
    if let Ok(sandboxes) = cluster
        .list_kind_labeled("KarsSandbox", &format!("kars.azure.com/team={name}"))
        .await
    {
        let mut seen = std::collections::BTreeSet::new();
        for sb in sandboxes.iter().take(8) {
            let sb_name = sb.metadata.name.clone().unwrap_or_default();
            let running = sb
                .data
                .get("status")
                .and_then(|s| s.get("phase"))
                .and_then(|p| p.as_str())
                == Some("Running");
            if !running || sb_name.is_empty() {
                continue;
            }
            if let Ok(domains) = cluster.sandbox_learned_domains(&sb_name).await {
                for d in domains {
                    seen.insert(d);
                }
            }
        }
        learned_egress = seen.into_iter().collect();
    }

    Ok(Json(TeamDetailDto {
        name: team.name_any(),
        display_name: team.spec.display_name.clone(),
        charter: team.spec.charter.clone(),
        phase: phase_of(&team),
        reporting_to: team.spec.reporting_to.clone(),
        knowledge_commons: team.spec.knowledge_commons.clone(),
        tier: team.spec.envelope.tier,
        authority_ceiling: team.spec.envelope.authority_ceiling,
        delegation_depth: team.spec.envelope.delegation_depth,
        paused: team.spec.paused,
        every_minutes: team.spec.cadence.as_ref().and_then(|c| c.every_minutes),
        lifecycle_mode: effective_lifecycle_mode(&team),
        warm_idle_seconds: team.spec.warm_idle_seconds,
        runtime_state: runtime_state(&team),
        current_assignment_nonce: st.and_then(|s| s.current_assignment_nonce.clone()),
        current_assignment_task: st.and_then(|s| s.current_assignment_task.clone()),
        idle_deadline_at: st.and_then(|s| s.idle_deadline_at.clone()),
        envelope_digest: st.and_then(|s| s.envelope_digest.clone()),
        principal_task: st.and_then(|s| s.principal_ref.as_ref().map(|r| r.name.clone())),
        roster,
        member_count: st.and_then(|s| s.member_count).unwrap_or(0),
        generated_task_count: st
            .and_then(|s| s.generated_task_count)
            .unwrap_or(0)
            .max(retained_run_count),
        last_generated_task: newest_retained_run
            .or_else(|| st.and_then(|s| s.last_generated_task.clone())),
        last_run_at: st.and_then(|s| s.last_run_at.clone()),
        next_run_at: st.and_then(|s| s.next_run_at.clone()),
        detail: st.and_then(|s| s.detail.clone()),
        health: st.and_then(|s| s.health.clone()),
        runs_succeeded: st.and_then(|s| s.runs_succeeded).unwrap_or(0),
        tokens_spent_total: st.and_then(|s| s.tokens_spent_total).unwrap_or(0),
        commons_entry_count: st.and_then(|s| s.commons_entry_count).unwrap_or(0),
        last_success_at: st.and_then(|s| s.last_success_at.clone()),
        created_at: team
            .metadata
            .creation_timestamp
            .as_ref()
            .map(|timestamp| timestamp.0.to_rfc3339()),
        last_activity_at: [
            st.and_then(|status| status.last_activity_at.clone()),
            st.and_then(|status| status.last_run_at.clone()),
            st.and_then(|status| status.last_success_at.clone()),
            team.metadata
                .creation_timestamp
                .as_ref()
                .map(|timestamp| timestamp.0.to_rfc3339()),
        ]
        .into_iter()
        .flatten()
        .max(),
        generated_tasks,
        recent_outcomes,
        recent_outcome_summary,
        tool_policy,
        tool_policy_default,
        mcp_servers: bp.map(|b| b.mcp_servers.clone()).unwrap_or_default(),
        git_write_repos: bp
            .and_then(|b| b.git_write.as_ref())
            .map(|git_write| git_write.repos.clone())
            .unwrap_or_default(),
        egress,
        egress_mode: bp.and_then(|b| b.egress_mode.clone()),
        learned_egress,
        network_posture: "Default-deny egress (kernel-level). Only the inference router and AGT mesh relay are reachable; novel domains need an approved egress request.".to_string(),
        model,
        model_fallbacks: bp
            .map(|blueprint| {
                blueprint
                    .model_fallbacks
                    .iter()
                    .map(|model| format!("{}::{}", model.provider, model.deployment))
                    .collect()
            })
            .unwrap_or_default(),
        model_default,
        memory: bp.and_then(|blueprint| blueprint.memory.clone()),
        runtime,
        runtime_default,
        isolation: bp.and_then(|b| b.isolation.clone()),
        execution_plan: bp
            .and_then(|blueprint| blueprint.execution_plan.as_ref())
            .map(crate::routes::tasks::ExecutionPlanDto::from_crd),
        tasks: read_task_list(&cluster.read_team_tasks(&name).await),
        channels: channels_from_keys(&cluster.team_channel_keys(&ns,&name).await.map_err(|e|AppError::Upstream(e.to_string()))?),
    }))
}

/// One event in a team's continuous ledger.
#[derive(Debug, Serialize)]
pub struct LedgerEvent {
    pub at: String,
    pub kind: String,
    pub summary: String,
    pub task: Option<String>,
    pub tokens: Option<i64>,
}

/// `GET /api/namespaces/:ns/teams/:name/ledger` — the team's continuous ledger
/// (§14): a streaming, append-only timeline of everything the standing
/// operation has done, composed from the durable records the controller already
/// writes (generated runs + their deliverables/tokens + harvested knowledge +
/// published digests). Newest first.
pub async fn get_team_ledger(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<Vec<LedgerEvent>>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let mut events: Vec<LedgerEvent> = Vec::new();

    // Run deliverables (delivery events, with token cost).
    let run_prefix = format!("{name}-run-");
    for record in cluster.list_mission_output_evidence().await {
        let data = record.data;
        let task = data
            .get("assignmentNonce")
            .cloned()
            .unwrap_or(record.evidence_key);
        let belongs_to_team = data.get("team") == Some(&name)
            || task.starts_with(&run_prefix)
            || task.starts_with(&format!("{name}-principal-assign-"));
        if !belongs_to_team {
            continue;
        }
        let at = data.get("finishedAt").cloned().unwrap_or_default();
        let tokens = data.get("totalTokens").and_then(|t| t.parse::<i64>().ok());
        let ok = data.get("status").map(String::as_str) == Some("ok");
        events.push(LedgerEvent {
            at,
            kind: if ok {
                "delivery".into()
            } else {
                "delivery_error".into()
            },
            summary: data
                .get("output")
                .map(|o| {
                    deliverable_text(o)
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .chars()
                        .take(140)
                        .collect::<String>()
                })
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "run completed".into()),
            task: Some(task),
            tokens,
        });
    }

    // Harvested knowledge (commons entries). Heal the legacy charter-as-title so
    // the ledger reads "Learned: <real headline>" rather than the same charter
    // line on every knowledge event.
    if let Some(cm) = cluster.read_commons(&name).await
        && let Some(idx) = cm.get("index.json")
        && let Ok(entries) = serde_json::from_str::<Vec<CommonsIndexEntry>>(idx)
    {
        let charter_line = cluster
            .teams(&ns)
            .get_opt(&name)
            .await
            .ok()
            .flatten()
            .map(|t| {
                t.spec
                    .charter
                    .lines()
                    .next()
                    .unwrap_or(&t.spec.charter)
                    .to_string()
            })
            .unwrap_or_default();
        for e in entries {
            let key = format!(
                "entry-{}",
                e.id.chars()
                    .map(
                        |c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                            c
                        } else {
                            '_'
                        }
                    )
                    .collect::<String>()
            );
            let title = match cm.get(&key) {
                Some(c) => commons_title(&e.title, &deliverable_text(c), &charter_line),
                None => e.title.clone(),
            };
            events.push(LedgerEvent {
                at: e.created_at,
                kind: "knowledge".into(),
                summary: format!("Learned: {title}"),
                task: Some(e.source_task),
                tokens: None,
            });
        }
    }

    // Published digests (report events).
    for d in cluster.list_team_digests().await {
        if d.get("team").and_then(|v| v.as_str()) != Some(name.as_str()) {
            continue;
        }
        events.push(LedgerEvent {
            at: d
                .get("at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            kind: "digest".into(),
            summary: d
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            task: None,
            tokens: None,
        });
    }

    events.sort_by(|a, b| b.at.cmp(&a.at));
    events.truncate(100);
    Ok(Json(events))
}

#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    pub charter: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub tier: Option<i32>,
    pub authority_ceiling: Option<i32>,
    pub delegation_depth: Option<i32>,
    pub reporting_to: Option<String>,
    pub knowledge_commons: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    pub cadence_minutes: Option<u32>,
    /// Runtime retention policy. Older clients omit this and retain ephemeral
    /// behavior; the Bridge composer recommends resource-optimized operation.
    #[serde(default)]
    pub lifecycle_mode: Option<String>,
    /// Idle window before a resource-optimized runtime is suspended.
    #[serde(default)]
    pub warm_idle_seconds: Option<i64>,
    /// Governance policy that bounds every run this team mints. When omitted the
    /// Bridge assigns the cluster default (`kars-default`) so the team's
    /// sandboxes are governed AND functional — an un-governed sandbox hangs
    /// because the agent's AGT engine fails closed on an empty policy set.
    pub tool_policy: Option<String>,
    /// The harness every run this team mints executes on (OpenClaw / Hermes /
    /// BYO). Written onto the team's run blueprint; the controller inherits it
    /// for each minted run. A bootstrap-only (non-autonomous) harness is
    /// corrected to OpenClaw. Absent => the sandbox default (OpenClaw).
    #[serde(default)]
    pub runtime: Option<String>,
    /// Model route for the team principal and minted runs, encoded as
    /// `provider::deployment` (for example
    /// `github-copilot::claude-opus-4.8`).
    #[serde(default)]
    pub model: Option<String>,
    /// Ordered provider::deployment routes used only after the primary route
    /// fails. Every entry must independently qualify the complete Team plan.
    #[serde(default)]
    pub model_fallbacks: Vec<String>,
    /// Network destinations inherited by every run.
    #[serde(default)]
    pub egress: Vec<CreateEgress>,
    /// `learning` for discovery or `strict` for allowlist enforcement.
    #[serde(default)]
    pub egress_mode: Option<String>,
    /// Connected MCP servers every run this team mints receives. Each name must
    /// resolve to an installed McpServer; preflight validates readiness before
    /// launch and the controller inherits the list onto every task-force run.
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// When false/omitted the team is created PAUSED (hibernating) so nothing
    /// runs until the operator explicitly launches it ("Run now" / resume) —
    /// the launch is a real human approval, not an automatic kickoff. Set true
    /// to opt into launching immediately on create.
    #[serde(default)]
    pub launch: Option<bool>,
    #[serde(default)]
    pub roles: Vec<CreateRole>,
    #[serde(default)]
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
    /// Repos every run may open PRs against, selected from the authenticated
    /// principal's connection. The server derives the typed connection reference.
    #[serde(default)]
    pub git_write_repos: Option<Vec<String>>,
    /// The identity creating this team (stamped `kars.azure.com/created-by`) for
    /// per-user budget attribution. Absent => "unattributed".
    #[serde(default)]
    pub created_by: Option<String>,
    /// Retention override, in seconds, for every task-force RUN this team
    /// mints — auto-delete a run's record this long after its deliverable
    /// lands. Does NOT apply to the standing principal/roster, which are
    /// never auto-deleted. `0` disables retention for this team's runs even
    /// if a cluster-wide default is set. Absent inherits the cluster default.
    #[serde(default)]
    pub run_retention_ttl_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEgress {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRole {
    pub name: String,
    pub system_prompt: Option<String>,
    pub runtime: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
}

/// Build a `spec.roster` array from create/update role inputs, shared by team
/// creation and roster editing so both paths produce identical role shapes
/// (per-member systemPrompt + blueprint{runtime, model} + skills).
/// Reject roster role names that collide with the reserved task names the
/// controller derives from the team (`<team>-principal`). A role named
/// "principal" would otherwise re-materialize the principal task as a member
/// parented to itself, deadlocking the whole team. The controller also skips
/// such a role defensively, but rejecting here gives the operator a clear error
/// instead of a silently dropped role.
fn reject_reserved_role_names(team: &str, roles: &[CreateRole]) -> AppResult<()> {
    let _ = team;
    for r in roles {
        let n = r.name.trim().to_ascii_lowercase();
        if n == "principal" {
            return Err(AppError::BadRequest(
                "role name 'principal' is reserved for the team's authority root — rename this role".into(),
            ));
        }
    }
    Ok(())
}

fn build_roster(roles: &[CreateRole]) -> Vec<serde_json::Value> {
    roles
        .iter()
        .filter(|r| !r.name.trim().is_empty())
        .map(|r| {
            let mut role = serde_json::json!({ "name": r.name.trim() });
            if let Some(sp) = &r.system_prompt
                && !sp.trim().is_empty()
            {
                role["systemPrompt"] = serde_json::json!(sp.trim());
            }
            let mut bp = serde_json::Map::new();
            if let Some(rt) = &r.runtime
                && !rt.is_empty()
            {
                // Harness capability, defense-in-depth: a bootstrap-only adapter
                // (no autonomous task loop) can't run a standing member — a
                // hand-composed team could still name one, so correct it to
                // OpenClaw here too. Hermes/BYO are autonomous and pass through.
                let rt = if crate::routes::compose::is_non_autonomous_harness(rt) {
                    "OpenClaw"
                } else {
                    rt.as_str()
                };
                bp.insert("runtime".into(), serde_json::json!(rt));
            }
            if let Some(m) = &r.model
                && let Some((provider, deployment)) = m.split_once("::")
            {
                bp.insert(
                    "model".into(),
                    serde_json::json!({ "provider": provider, "deployment": deployment }),
                );
            }
            if !bp.is_empty() {
                role["blueprint"] = serde_json::Value::Object(bp);
            }
            if !r.skills.is_empty() {
                role["skills"] = serde_json::json!(r.skills);
            }
            role
        })
        .collect()
}

fn normalize_mcp_servers(servers: &[String]) -> AppResult<Vec<String>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut normalized = Vec::new();
    for server in servers {
        let server = server.trim();
        if !server.is_empty() && seen.insert(server.to_string()) {
            normalized.push(server.to_string());
        }
    }
    if normalized.len() > 8 {
        return Err(AppError::BadRequest(
            "a team may connect at most 8 MCP servers".into(),
        ));
    }
    Ok(normalized)
}

fn normalize_autonomous_runtime(runtime: &mut Option<String>) {
    if let Some(value) = runtime.as_deref()
        && (value.trim().is_empty() || crate::routes::compose::is_non_autonomous_harness(value))
    {
        *runtime = Some("OpenClaw".into());
    }
}

fn normalize_model_fallback_routes(
    routes: &[String],
    primary: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut normalized = Vec::new();
    for route in routes {
        let route = route.trim();
        if route.is_empty()
            || primary.is_some_and(|primary| route == primary)
            || !seen.insert(route.to_string())
        {
            continue;
        }
        let valid = route
            .split_once("::")
            .is_some_and(|(provider, deployment)| {
                !provider.trim().is_empty() && !deployment.trim().is_empty()
            });
        if !valid {
            return Err(AppError::BadRequest(
                "model_fallbacks entries must be encoded as provider::deployment".into(),
            ));
        }
        normalized.push(route.to_string());
    }
    if normalized.len() > 8 {
        return Err(AppError::BadRequest(
            "model_fallbacks may contain at most 8 unique routes".into(),
        ));
    }
    Ok(normalized)
}

fn validate_model_route(models: &[ModelOption], route: &str) -> AppResult<()> {
    let route = route.trim();
    if route.is_empty() {
        return Ok(());
    }
    let valid = route
        .split_once("::")
        .is_some_and(|(provider, deployment)| {
            models
                .iter()
                .any(|model| model.provider == provider && model.deployment == deployment)
        });
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "model route `{route}` is not present in the live model catalogue"
        )))
    }
}

fn option_named<'a>(items: &'a [RefOption], namespace: &str, name: &str) -> Option<&'a RefOption> {
    items
        .iter()
        .find(|option| option.name == name && option.namespace == namespace)
}

fn team_role_qualification_requirements(
    plan: &crate::routes::tasks::ExecutionPlanDto,
    role_name: &str,
) -> std::collections::BTreeSet<String> {
    let mut required =
        std::collections::BTreeSet::from(["team".to_string(), "telemetry".to_string()]);
    if let Some(role) = plan.roles.iter().find(|role| role.name == role_name) {
        for phase in &role.phases {
            required.extend(phase.capabilities.iter().cloned());
        }
    }
    required
}

struct TeamModelRoutes<'a> {
    namespace: &'a str,
    runtime: Option<&'a str>,
    model: Option<&'a str>,
    model_fallbacks: &'a [String],
    roles: &'a [CreateRole],
    execution_plan: &'a crate::routes::tasks::ExecutionPlanDto,
    mcp_servers: &'a [String],
    memory: Option<&'a str>,
}

fn validate_team_model_routes(options: &Options, routes: TeamModelRoutes<'_>) -> AppResult<()> {
    let TeamModelRoutes {
        namespace,
        runtime,
        model,
        model_fallbacks,
        roles,
        execution_plan,
        mcp_servers,
        memory,
    } = routes;
    let memory = memory.map(str::trim).filter(|memory| !memory.is_empty());
    let default_route = options
        .models
        .iter()
        .find(|model| model.is_default)
        .or_else(|| options.models.first())
        .map(|model| format!("{}::{}", model.provider, model.deployment))
        .unwrap_or_default();
    let principal_route = model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(default_route.as_str());
    validate_model_route(&options.models, principal_route)?;
    let principal_runtime = runtime
        .map(str::trim)
        .filter(|runtime| !runtime.is_empty())
        .unwrap_or("OpenClaw");
    let principal_model = principal_route.split_once("::").ok_or_else(|| {
        AppError::BadRequest(format!(
            "model route `{principal_route}` must use provider::deployment"
        ))
    })?;
    let principal_blueprint = crate::routes::tasks::BlueprintDto {
        runtime: Some(principal_runtime.to_string()),
        model: Some(crate::routes::tasks::ModelDto {
            provider: principal_model.0.to_string(),
            deployment: principal_model.1.to_string(),
        }),
        model_fallbacks: Vec::new(),
        instructions: None,
        tool_policy: None,
        mcp_servers: mcp_servers.to_vec(),
        egress: Vec::new(),
        egress_mode: None,
        isolation: None,
        memory: memory.map(str::to_string),
        skills: roles
            .iter()
            .flat_map(|role| role.skills.iter().cloned())
            .collect(),
        execution_plan: Some(execution_plan.clone()),
    };
    let (principal_required, principal_parallel) =
        crate::routes::validate::qualification_requirements(&principal_blueprint, Some("team"));
    validate_qualified_model_route(
        principal_runtime,
        principal_route,
        &principal_required,
        principal_parallel,
    )?;
    let principal_route_label = crate::routes::options::route_label(
        principal_runtime,
        principal_model.0,
        principal_model.1,
    );
    for server in mcp_servers {
        let option = option_named(&options.mcp_servers, namespace, server).ok_or_else(|| {
            AppError::BadRequest(format!(
                "MCP server `{server}` is not present in the live options catalogue"
            ))
        })?;
        match crate::routes::options::mcp_server_qualified_for_route(
            principal_runtime,
            principal_model.0,
            principal_model.1,
            option,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::BadRequest(format!(
                    "MCP server `{server}` lacks retained resource qualification for {principal_route_label} at current schema {}",
                    option.tool_schema_digest.as_deref().unwrap_or("missing")
                )));
            }
            Err(error) => {
                return Err(AppError::Upstream(format!(
                    "resource qualification configuration error: {error}"
                )));
            }
        }
    }
    if let Some(memory) = memory.map(str::trim).filter(|memory| !memory.is_empty()) {
        let option = option_named(&options.memories, namespace, memory).ok_or_else(|| {
            AppError::BadRequest(format!(
                "memory `{memory}` is not present in the live options catalogue"
            ))
        })?;
        match crate::routes::options::memory_binding_qualified_for_route(
            principal_runtime,
            principal_model.0,
            principal_model.1,
            option,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(AppError::BadRequest(format!(
                    "memory `{memory}` lacks retained resource qualification for {principal_route_label} at backend {} / compiled digest {}",
                    option.backend.as_deref().unwrap_or("missing"),
                    option.compiled_digest.as_deref().unwrap_or("missing")
                )));
            }
            Err(error) => {
                return Err(AppError::Upstream(format!(
                    "resource qualification configuration error: {error}"
                )));
            }
        }
    }
    for role in roles {
        let role_route = role
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or(principal_route);
        validate_model_route(&options.models, role_route)?;
        let role_runtime = role
            .runtime
            .as_deref()
            .map(str::trim)
            .filter(|runtime| !runtime.is_empty())
            .unwrap_or(principal_runtime);
        let role_required = team_role_qualification_requirements(execution_plan, &role.name);
        validate_qualified_model_route(role_runtime, role_route, &role_required, 1)?;
        let (provider, deployment) = role_route.split_once("::").ok_or_else(|| {
            AppError::BadRequest(format!(
                "model route `{role_route}` must use provider::deployment"
            ))
        })?;
        let role_route_label =
            crate::routes::options::route_label(role_runtime, provider, deployment);
        if role_required.contains("mcp") {
            for server in mcp_servers {
                let option =
                    option_named(&options.mcp_servers, namespace, server).ok_or_else(|| {
                        AppError::BadRequest(format!(
                            "MCP server `{server}` is not present in the live options catalogue"
                        ))
                    })?;
                match crate::routes::options::mcp_server_qualified_for_route(
                    role_runtime,
                    provider,
                    deployment,
                    option,
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(AppError::BadRequest(format!(
                            "role `{}` MCP server `{server}` lacks retained resource qualification for {role_route_label} at current schema {}",
                            role.name,
                            option.tool_schema_digest.as_deref().unwrap_or("missing")
                        )));
                    }
                    Err(error) => {
                        return Err(AppError::Upstream(format!(
                            "resource qualification configuration error: {error}"
                        )));
                    }
                }
            }
        }
        if role_required.contains("memory")
            && let Some(memory) = memory.map(str::trim).filter(|memory| !memory.is_empty())
        {
            let option = option_named(&options.memories, namespace, memory).ok_or_else(|| {
                AppError::BadRequest(format!(
                    "memory `{memory}` is not present in the live options catalogue"
                ))
            })?;
            match crate::routes::options::memory_binding_qualified_for_route(
                role_runtime,
                provider,
                deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "role `{}` memory `{memory}` lacks retained resource qualification for {role_route_label} at backend {} / compiled digest {}",
                        role.name,
                        option.backend.as_deref().unwrap_or("missing"),
                        option.compiled_digest.as_deref().unwrap_or("missing")
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
        for skill in &role.skills {
            let option = option_named(&options.skills, namespace, skill).ok_or_else(|| {
                AppError::BadRequest(format!(
                    "skill `{skill}` is not present in the approved live catalogue"
                ))
            })?;
            match crate::routes::options::skill_version_qualified_for_route(
                role_runtime,
                provider,
                deployment,
                option,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(AppError::BadRequest(format!(
                        "role `{}` skill `{skill}` lacks retained resource qualification for {role_route_label} at current version digest {}",
                        role.name,
                        option.version_digest.as_deref().unwrap_or("missing")
                    )));
                }
                Err(error) => {
                    return Err(AppError::Upstream(format!(
                        "resource qualification configuration error: {error}"
                    )));
                }
            }
        }
    }
    let mut seen_fallbacks = std::collections::BTreeSet::new();
    for fallback in model_fallbacks {
        let fallback = fallback.trim();
        if fallback.is_empty() {
            continue;
        }
        if !seen_fallbacks.insert(fallback.to_string()) {
            continue;
        }
        if seen_fallbacks.len() > 8 {
            return Err(AppError::BadRequest(
                "model_fallbacks may contain at most 8 unique routes".into(),
            ));
        }
        validate_model_route(&options.models, fallback)?;
        let fallback_roles = roles
            .iter()
            .cloned()
            .map(|mut role| {
                role.model = Some(fallback.to_string());
                role
            })
            .collect::<Vec<_>>();
        validate_team_model_routes(
            options,
            TeamModelRoutes {
                namespace,
                runtime,
                model: Some(fallback),
                model_fallbacks: &[],
                roles: &fallback_roles,
                execution_plan,
                mcp_servers,
                memory,
            },
        )?;
    }
    Ok(())
}

fn validate_qualified_model_route(
    runtime: &str,
    route: &str,
    required_capabilities: &std::collections::BTreeSet<String>,
    max_parallel: i32,
) -> AppResult<()> {
    let Some((provider, deployment)) = route.split_once("::") else {
        return Err(AppError::BadRequest(format!(
            "model route `{route}` must use provider::deployment"
        )));
    };
    match crate::routes::options::route_qualification(
        runtime,
        provider,
        deployment,
        required_capabilities,
        max_parallel,
        None,
    ) {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::BadRequest(format!(
            "runtime/model route `{runtime} · {provider}::{deployment}` has not passed the fresh E2E qualification matrix"
        ))),
        Err(error) => Err(AppError::Upstream(format!(
            "route qualification configuration error: {error}"
        ))),
    }
}

fn normalize_lifecycle_mode(mode: Option<&str>) -> AppResult<Option<&'static str>> {
    let Some(mode) = mode.map(str::trim).filter(|mode| !mode.is_empty()) else {
        return Ok(None);
    };
    match mode.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "ephemeral" => Ok(Some("ephemeral")),
        "resourceoptimized" => Ok(Some("resourceOptimized")),
        "persistent" => Ok(Some("persistent")),
        _ => Err(AppError::BadRequest(
            "lifecycle_mode must be 'ephemeral', 'resourceOptimized', or 'persistent'".into(),
        )),
    }
}

fn validate_warm_idle_seconds(seconds: Option<i64>) -> AppResult<Option<i64>> {
    match seconds {
        Some(seconds) if seconds < 0 => Err(AppError::BadRequest(
            "warm_idle_seconds must be non-negative".into(),
        )),
        value => Ok(value),
    }
}

fn apply_team_git_write(
    spec: &mut serde_json::Value,
    git_write: Option<&crate::kars::task::GitWriteConfig>,
) -> AppResult<()> {
    let Some(git_write) = git_write else {
        return Ok(());
    };
    if !spec["blueprint"].is_object() {
        spec["blueprint"] = serde_json::json!({});
    }
    spec["blueprint"]["gitWrite"] =
        serde_json::to_value(git_write).map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(())
}

async fn validate_mcp_servers(
    cluster: &crate::kars::cluster::Cluster,
    namespace: &str,
    servers: &[String],
) -> AppResult<()> {
    for server in servers {
        let Some(resource) = cluster
            .get_kind(namespace, "McpServer", server)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?
        else {
            return Err(AppError::BadRequest(format!(
                "MCP server `{server}` is not installed in namespace `{namespace}`"
            )));
        };
        let phase = resource
            .data
            .get("status")
            .and_then(|status| status.get("phase"))
            .and_then(|phase| phase.as_str());
        let observed_generation = resource
            .data
            .get("status")
            .and_then(|status| status.get("observedGeneration"))
            .and_then(|generation| generation.as_i64());
        if phase != Some("Ready") || observed_generation != resource.metadata.generation {
            return Err(AppError::BadRequest(format!(
                "MCP server `{server}` is not Ready for its current generation in namespace `{namespace}`"
            )));
        }
    }
    Ok(())
}

/// `POST /api/namespaces/:ns/teams` — create a standing team. The controller
/// validates the envelope; cadence drives the autonomous tick. Defaults are
/// conservative (tier 3, ceiling=tier, depth 1) so a team can't self-amplify.
pub async fn create_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(mut b): Json<CreateTeamRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    cluster
        .credential_grant(&ns)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if b.name.trim().is_empty() || b.charter.trim().len() < 8 {
        return Err(AppError::BadRequest(
            "name and a real charter are required".into(),
        ));
    }
    reject_reserved_role_names(&b.name, &b.roles)?;
    let execution_plan = b
        .execution_plan
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("a typed execution_plan is required".into()))?;
    crate::routes::compose::validate_execution_plan(execution_plan)
        .map_err(AppError::BadRequest)?;
    let roster_names = b
        .roles
        .iter()
        .map(|role| role.name.trim())
        .collect::<std::collections::BTreeSet<_>>();
    let plan_names = execution_plan
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if roster_names != plan_names {
        return Err(AppError::BadRequest(
            "execution_plan role names must exactly match the team roster".into(),
        ));
    }
    b.mcp_servers = normalize_mcp_servers(&b.mcp_servers)?;
    validate_mcp_servers(cluster, &ns, &b.mcp_servers).await?;
    normalize_autonomous_runtime(&mut b.runtime);
    for role in &mut b.roles {
        normalize_autonomous_runtime(&mut role.runtime);
    }
    let options = build_options(cluster).await?;
    if b.model
        .as_deref()
        .is_none_or(|model| model.trim().is_empty())
    {
        b.model = options
            .models
            .iter()
            .find(|model| model.is_default)
            .or_else(|| options.models.first())
            .map(|model| format!("{}::{}", model.provider, model.deployment));
    }
    b.model_fallbacks = normalize_model_fallback_routes(&b.model_fallbacks, b.model.as_deref())?;
    validate_team_model_routes(
        &options,
        TeamModelRoutes {
            namespace: &ns,
            runtime: b.runtime.as_deref(),
            model: b.model.as_deref(),
            model_fallbacks: &b.model_fallbacks,
            roles: &b.roles,
            execution_plan,
            mcp_servers: &b.mcp_servers,
            memory: b.memory.as_deref(),
        },
    )?;
    b.created_by = Some(principal.name.clone());
    let created_by = principal.name.clone();
    let git_write = crate::routes::github::authorize_git_write(
        cluster,
        &ns,
        &principal,
        b.git_write_repos.as_deref(),
    )
    .await?;
    // Aggregate inference-budget gate (cluster + workspace + user): a launched
    // team immediately kicks off a run (token spend), so block starting new work
    // when a budget at any tier is strict/over-buffer. A paused team passes.
    if b.launch.unwrap_or(false) {
        crate::routes::budgets::enforce_launch_budget(cluster, &ns, &created_by).await?;
    }
    let tier = b.tier.unwrap_or(3).clamp(1, 5);
    let ceiling = b.authority_ceiling.unwrap_or(tier).clamp(1, tier);
    // Governance: create PAUSED unless the operator explicitly opts into
    // launching. A paused team does not auto-kickoff (the controller mints the
    // initial run only when `!paused`), so "Launch" is a genuine human approval
    // — clicking Run now / Resume — not an automatic side-effect of Create.
    let paused = !b.launch.unwrap_or(false);
    let mut spec = serde_json::json!({
        "charter": b.charter, "paused": paused, "envelope": { "tier": tier, "authorityCeiling": ceiling, "delegationDepth": b.delegation_depth.unwrap_or(1) },
    });
    if let Some(mode) = normalize_lifecycle_mode(b.lifecycle_mode.as_deref())? {
        spec["lifecycleMode"] = serde_json::json!(mode);
    }
    if let Some(seconds) = validate_warm_idle_seconds(b.warm_idle_seconds)? {
        spec["warmIdleSeconds"] = serde_json::json!(seconds);
    }
    if let Some(r) = &b.reporting_to {
        spec["reportingTo"] = serde_json::json!(r);
    }
    if let Some(d) = b
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        spec["displayName"] = serde_json::json!(d);
    }
    if let Some(c) = &b.knowledge_commons {
        spec["knowledgeCommons"] = serde_json::json!(c);
    }
    // cadence_minutes == 0 (or absent) means a cadence-LESS "run on demand" team:
    // the CRD requires everyMinutes >= 1 when the cadence field is present, so we
    // OMIT it entirely rather than write an invalid everyMinutes: 0 (which the
    // apiserver rejects 422). A cadence-less team is minted once on creation
    // (kickoff) and thereafter only runs via "Run now".
    if let Some(m) = b.cadence_minutes
        && m >= 1
    {
        spec["cadence"] = serde_json::json!({ "everyMinutes": m });
    }
    // Every team run must be governed by a real ToolPolicy. Without one the run
    // sandbox is created with governance disabled, the agent's AGT engine starts
    // with an empty policy set and fails closed, and the run hangs until the
    // dispatch times out. Resolve the requested policy (or the cluster default
    // `kars-default`) and pin it on the team's run blueprint.
    let tool_policy = resolve_team_tool_policy(cluster, &ns, b.tool_policy.as_deref()).await;
    if let Some(tp) = &tool_policy {
        spec["blueprint"] = serde_json::json!({ "toolPolicy": tp });
    }
    if !spec["blueprint"].is_object() {
        spec["blueprint"] = serde_json::json!({});
    }
    spec["blueprint"]["executionPlan"] = serde_json::to_value(execution_plan.clone().into_crd())
        .map_err(|error| {
            AppError::BadRequest(format!("execution_plan could not be serialized: {error}"))
        })?;
    // Team-level harness: the runtime every minted run executes on. Correct a
    // bootstrap-only adapter (no autonomous task loop) to OpenClaw — a standing
    // run must be able to run autonomously. Hermes/BYO are autonomous and pass
    // through. The controller inherits this via the team's run blueprint.
    if let Some(rt) = b
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let rt = if crate::routes::compose::is_non_autonomous_harness(rt) {
            "OpenClaw"
        } else {
            rt
        };
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["runtime"] = serde_json::json!(rt);
    }
    if let Some(model) = b.model.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && let Some((provider, deployment)) = model.split_once("::")
    {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["model"] =
            serde_json::json!({"provider": provider, "deployment": deployment});
    }
    let model_fallbacks = b
        .model_fallbacks
        .iter()
        .map(|route| route.trim())
        .filter(|route| !route.is_empty())
        .filter_map(|route| route.split_once("::"))
        .map(|(provider, deployment)| {
            serde_json::json!({"provider": provider, "deployment": deployment})
        })
        .collect::<Vec<_>>();
    spec["blueprint"]["modelFallbacks"] = serde_json::json!(model_fallbacks);
    if let Some(memory) = b.memory.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["memory"] = serde_json::json!(memory);
    }
    if !b.egress.is_empty() {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["egress"] = serde_json::json!(
            b.egress
                .iter()
                .filter_map(|entry| {
                    let host = entry.host.trim();
                    (!host.is_empty())
                        .then(|| serde_json::json!({"host": host, "port": entry.port}))
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(mode) = b.egress_mode.as_deref().map(str::trim) {
        let mode = match mode.to_ascii_lowercase().as_str() {
            "strict" => "Strict",
            "learning" | "learn" => "Learn",
            _ => {
                return Err(AppError::BadRequest(
                    "egress_mode must be 'learning' or 'strict'".into(),
                ));
            }
        };
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["egressMode"] = serde_json::json!(mode);
    }
    apply_team_git_write(&mut spec, git_write.as_ref().map(|(config, _)| config))?;
    if let Some((_, binding)) = &git_write {
        spec["blueprint"]["githubBinding"] =
            serde_json::to_value(binding).map_err(|error| AppError::Upstream(error.to_string()))?;
    }
    if !b.mcp_servers.is_empty() {
        if !spec["blueprint"].is_object() {
            spec["blueprint"] = serde_json::json!({});
        }
        spec["blueprint"]["mcpServers"] = serde_json::json!(
            b.mcp_servers
                .iter()
                .map(|server| server.trim())
                .filter(|server| !server.is_empty())
                .collect::<Vec<_>>()
        );
    }
    if !b.roles.is_empty() {
        spec["roster"] = serde_json::json!(build_roster(&b.roles));
    }
    if let Some(ttl) = b.run_retention_ttl_seconds {
        spec["runRetentionTtlSeconds"] = serde_json::json!(ttl);
    }
    let body = serde_json::json!({ "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTeam", "metadata": {"name": b.name.trim(), "namespace": ns}, "spec": spec });
    let mut body = body;
    // Stamp the creator for per-user budget attribution (propagated onto runs).
    if body["metadata"]["annotations"].is_null() {
        body["metadata"]["annotations"] = serde_json::json!({});
    }
    body["metadata"]["annotations"]["kars.azure.com/created-by"] = serde_json::json!(created_by);
    body["metadata"]["annotations"]["kars.azure.com/owner-sub"] = serde_json::json!(principal.sub);
    body["metadata"]["annotations"]["kars.azure.com/owner-name"] =
        serde_json::json!(principal.name);
    let active = !body["spec"]["paused"].as_bool().unwrap_or(false);
    body["spec"]["paused"] = serde_json::json!(true);
    let captured = cluster
        .create_kind(&ns, "KarsTeam", body)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    cluster
        .finish_created_credentials(
            &crate::kars::credentials::Target {
                kind: "KarsTeam".into(),
                namespace: ns.clone(),
                name: captured.name_any(),
                uid: captured
                    .uid()
                    .ok_or_else(|| AppError::Upstream("Team CREATE omitted UID".into()))?,
            },
            active,
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(
        serde_json::json!({"created": true, "name": b.name.trim()}),
    ))
}

/// Resolve the governance policy to pin on a team's run blueprint: the
/// requested policy when it exists, else the cluster default (`kars-default`),
/// else the first installed policy. Returns `None` only when the cluster has no
/// ToolPolicy at all (nothing we can assign).
async fn resolve_team_tool_policy(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    requested: Option<&str>,
) -> Option<String> {
    if let Some(r) = requested.map(str::trim).filter(|r| !r.is_empty())
        && cluster
            .get_kind(ns, "ToolPolicy", r)
            .await
            .ok()
            .flatten()
            .is_some()
    {
        return Some(r.to_string());
    }
    if cluster
        .get_kind(
            ns,
            "ToolPolicy",
            crate::routes::compose::DEFAULT_TOOL_POLICY,
        )
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return Some(crate::routes::compose::DEFAULT_TOOL_POLICY.to_string());
    }
    cluster
        .list_kind_all("ToolPolicy")
        .await
        .ok()?
        .into_iter()
        .find(|policy| policy.namespace().as_deref() == Some(ns))
        .map(|policy| policy.name_any())
}

#[derive(Debug, Deserialize)]
pub struct UpdateTeamRequest {
    pub charter: Option<String>,
    pub paused: Option<bool>,
    pub cadence_minutes: Option<u32>,
    pub reporting_to: Option<String>,
    /// Change how the standing runtime is retained between assignments.
    #[serde(default)]
    pub lifecycle_mode: Option<String>,
    /// Change the resource-optimized warm idle window.
    #[serde(default)]
    pub warm_idle_seconds: Option<i64>,
    /// When present, replaces the team's roster — editing the org post-create
    /// (add/remove roles, change per-member prompt/harness/model/skills). The
    /// controller reconciles member tasks to match.
    pub roles: Option<Vec<CreateRole>>,
    /// Change the harness every run this team mints executes on (OpenClaw /
    /// Hermes / BYO). A non-autonomous adapter is corrected to OpenClaw.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Change the principal/default model inherited by future runs.
    #[serde(default)]
    pub model: Option<String>,
    /// Replace the ordered fallback routes inherited by future runs.
    #[serde(default)]
    pub model_fallbacks: Option<Vec<String>>,
    /// Replace or clear the shared memory binding inherited by future runs.
    #[serde(default)]
    pub memory: Option<String>,
    /// Replace the connected MCP servers inherited by future team runs.
    /// `Some([])` explicitly clears the list; `None` leaves it unchanged.
    #[serde(default)]
    pub mcp_servers: Option<Vec<String>>,
    /// Replace the keyless GitHub write scope inherited by future team runs.
    /// `Some([])` revokes PR-write access; `None` leaves it unchanged.
    #[serde(default)]
    pub git_write_repos: Option<Vec<String>>,
    /// Replace the declared egress destinations for future runs.
    #[serde(default)]
    pub egress: Option<Vec<CreateEgress>>,
    /// Change future runs between learning and strict egress modes.
    #[serde(default)]
    pub egress_mode: Option<String>,
    /// Change the retention override for this team's future task-force runs.
    /// `0` disables retention; absent leaves the current setting unchanged.
    #[serde(default)]
    pub run_retention_ttl_seconds: Option<i64>,
    /// Replace the typed execution plan while preserving the team roster. The
    /// server validates structure, exact role-name parity, and route qualification.
    #[serde(default)]
    pub execution_plan: Option<crate::routes::tasks::ExecutionPlanDto>,
}

/// `PATCH /api/namespaces/:ns/teams/:name` — edit charter, cadence, reporting,
/// or pause. Envelope-raising fields are out of scope here (promote handles
/// governed tier changes); this is the non-amplifying day-to-day edit.
pub async fn update_team(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(mut b): Json<UpdateTeamRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    normalize_autonomous_runtime(&mut b.runtime);
    if let Some(roles) = &mut b.roles {
        for role in roles {
            normalize_autonomous_runtime(&mut role.runtime);
        }
    }
    let execution_plan_changed = b.execution_plan.is_some();
    if b.runtime.is_some()
        || b.model.is_some()
        || b.model_fallbacks.is_some()
        || b.memory.is_some()
        || b.roles.is_some()
        || b.mcp_servers.is_some()
        || b.execution_plan.is_some()
    {
        let options = build_options(cluster).await?;
        if b.model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty())
        {
            b.model = options
                .models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| options.models.first())
                .map(|model| format!("{}::{}", model.provider, model.deployment));
        }
        let existing_runtime = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.runtime.as_deref());
        let existing_model = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.model.as_ref())
            .map(|model| format!("{}::{}", model.provider, model.deployment));
        let existing_model_fallbacks = team
            .spec
            .blueprint
            .as_ref()
            .map(|blueprint| {
                blueprint
                    .model_fallbacks
                    .iter()
                    .map(|model| format!("{}::{}", model.provider, model.deployment))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if b.model.is_some() || b.model_fallbacks.is_some() {
            b.model_fallbacks = Some(normalize_model_fallback_routes(
                b.model_fallbacks
                    .as_deref()
                    .unwrap_or(existing_model_fallbacks.as_slice()),
                b.model.as_deref().or(existing_model.as_deref()),
            )?);
        }
        let existing_roles = team
            .spec
            .roster
            .iter()
            .map(|role| CreateRole {
                name: role.name.clone(),
                system_prompt: role.system_prompt.clone(),
                runtime: role
                    .blueprint
                    .as_ref()
                    .and_then(|blueprint| blueprint.runtime.clone()),
                model: role
                    .blueprint
                    .as_ref()
                    .and_then(|blueprint| blueprint.model.as_ref())
                    .map(|model| format!("{}::{}", model.provider, model.deployment)),
                skills: role.skills.clone(),
            })
            .collect::<Vec<_>>();
        let existing_execution_plan = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.execution_plan.as_ref())
            .map(crate::routes::tasks::ExecutionPlanDto::from_crd);
        let existing_mcp_servers = team
            .spec
            .blueprint
            .as_ref()
            .map(|blueprint| blueprint.mcp_servers.clone())
            .unwrap_or_default();
        let existing_memory = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.memory.as_deref());
        let execution_plan = b
            .execution_plan
            .as_ref()
            .or(existing_execution_plan.as_ref())
            .ok_or_else(|| {
            AppError::BadRequest(
                "this team cannot change runtime/model/roles/MCP until it has a typed execution_plan"
                    .into(),
            )
        })?;
        crate::routes::compose::validate_execution_plan(execution_plan)
            .map_err(AppError::BadRequest)?;
        let effective_roles = b.roles.as_deref().unwrap_or(existing_roles.as_slice());
        let roster_names = effective_roles
            .iter()
            .map(|role| role.name.trim())
            .collect::<std::collections::BTreeSet<_>>();
        let plan_names = execution_plan
            .roles
            .iter()
            .map(|role| role.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if roster_names != plan_names {
            return Err(AppError::BadRequest(
                "execution_plan role names must exactly match the team roster".into(),
            ));
        }
        validate_team_model_routes(
            &options,
            TeamModelRoutes {
                namespace: &ns,
                runtime: b.runtime.as_deref().or(existing_runtime),
                model: b.model.as_deref().or(existing_model.as_deref()),
                model_fallbacks: b
                    .model_fallbacks
                    .as_deref()
                    .unwrap_or(existing_model_fallbacks.as_slice()),
                roles: effective_roles,
                execution_plan,
                mcp_servers: b
                    .mcp_servers
                    .as_deref()
                    .unwrap_or(existing_mcp_servers.as_slice()),
                memory: b.memory.as_deref().or(existing_memory),
            },
        )?;
    }
    if b.paused == Some(false) {
        crate::routes::budgets::enforce_launch_budget(cluster, &ns, &principal.name).await?;
    }
    let mut spec = serde_json::Map::new();
    if let Some(c) = &b.charter {
        spec.insert("charter".into(), serde_json::json!(c));
    }
    if let Some(p) = b.paused {
        spec.insert("paused".into(), serde_json::json!(p));
    }
    if let Some(r) = &b.reporting_to {
        spec.insert("reportingTo".into(), serde_json::json!(r));
    }
    if let Some(mode) = normalize_lifecycle_mode(b.lifecycle_mode.as_deref())? {
        spec.insert("lifecycleMode".into(), serde_json::json!(mode));
    }
    if let Some(seconds) = validate_warm_idle_seconds(b.warm_idle_seconds)? {
        spec.insert("warmIdleSeconds".into(), serde_json::json!(seconds));
    }
    if let Some(m) = b.cadence_minutes {
        if m >= 1 {
            spec.insert("cadence".into(), serde_json::json!({"everyMinutes": m}));
        } else {
            // 0 = passive / run-on-demand: clear the cadence entirely (a merge
            // patch null removes the field) so the team actually stops auto-
            // running, honouring the "0 = passive" label instead of silently
            // leaving the previous cadence in place.
            spec.insert("cadence".into(), serde_json::Value::Null);
        }
    }
    if let Some(roles) = &b.roles {
        reject_reserved_role_names(&name, roles)?;
        spec.insert("roster".into(), serde_json::json!(build_roster(roles)));
    }
    let mut blueprint = serde_json::Map::new();
    if let Some(rt) = b
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let rt = if crate::routes::compose::is_non_autonomous_harness(rt) {
            "OpenClaw"
        } else {
            rt
        };
        blueprint.insert("runtime".into(), serde_json::json!(rt));
    }
    if let Some(model) = b.model.as_deref().map(str::trim) {
        if model.is_empty() {
            blueprint.insert("model".into(), serde_json::Value::Null);
        } else if let Some((provider, deployment)) = model.split_once("::") {
            blueprint.insert(
                "model".into(),
                serde_json::json!({"provider": provider, "deployment": deployment}),
            );
        } else {
            return Err(AppError::BadRequest(
                "model must be encoded as provider::deployment".into(),
            ));
        }
    }
    if let Some(fallbacks) = &b.model_fallbacks {
        let mut seen = std::collections::BTreeSet::new();
        let mut routes = Vec::new();
        for fallback in fallbacks {
            let fallback = fallback.trim();
            if fallback.is_empty() || !seen.insert(fallback.to_string()) {
                continue;
            }
            let Some((provider, deployment)) = fallback.split_once("::") else {
                return Err(AppError::BadRequest(
                    "model_fallbacks entries must be encoded as provider::deployment".into(),
                ));
            };
            routes.push(serde_json::json!({
                "provider": provider,
                "deployment": deployment,
            }));
        }
        if routes.len() > 8 {
            return Err(AppError::BadRequest(
                "model_fallbacks may contain at most 8 unique routes".into(),
            ));
        }
        blueprint.insert("modelFallbacks".into(), serde_json::json!(routes));
    }
    if let Some(memory) = b.memory.as_deref() {
        blueprint.insert(
            "memory".into(),
            if memory.trim().is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::json!(memory.trim())
            },
        );
    }
    if let Some(servers) = &b.mcp_servers {
        let servers = normalize_mcp_servers(servers)?;
        validate_mcp_servers(cluster, &ns, &servers).await?;
        blueprint.insert("mcpServers".into(), serde_json::json!(servers));
    }
    if let Some(repos) = &b.git_write_repos {
        let git_write =
            crate::routes::github::authorize_git_write(cluster, &ns, &principal, Some(repos))
                .await?;
        blueprint.insert(
            "githubBinding".into(),
            git_write
                .as_ref()
                .map(|(_, binding)| serde_json::to_value(binding))
                .transpose()
                .map_err(|error| AppError::Upstream(error.to_string()))?
                .unwrap_or(serde_json::Value::Null),
        );
        blueprint.insert(
            "gitWrite".into(),
            git_write
                .map(|(grant, _)| {
                    serde_json::to_value(grant).map_err(|e| AppError::Upstream(e.to_string()))
                })
                .transpose()?
                .unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(egress) = &b.egress {
        let entries = egress
            .iter()
            .filter_map(|entry| {
                let host = entry.host.trim();
                (!host.is_empty()).then(|| serde_json::json!({"host": host, "port": entry.port}))
            })
            .collect::<Vec<_>>();
        blueprint.insert("egress".into(), serde_json::json!(entries));
    }
    if let Some(mode) = b.egress_mode.as_deref().map(str::trim) {
        let mode = match mode.to_ascii_lowercase().as_str() {
            "strict" => "Strict",
            "learning" | "learn" => "Learn",
            _ => {
                return Err(AppError::BadRequest(
                    "egress_mode must be 'learning' or 'strict'".into(),
                ));
            }
        };
        blueprint.insert("egressMode".into(), serde_json::json!(mode));
    }
    if let Some(execution_plan) = &b.execution_plan {
        blueprint.insert(
            "executionPlan".into(),
            serde_json::to_value(execution_plan.clone().into_crd()).map_err(|error| {
                AppError::BadRequest(format!("execution_plan could not be serialized: {error}"))
            })?,
        );
    }
    if !blueprint.is_empty() {
        // Merge-patch the nested blueprint so runtime/MCP edits preserve the
        // team's existing toolPolicy/model and can be changed together.
        spec.insert("blueprint".into(), serde_json::Value::Object(blueprint));
    }
    if let Some(ttl) = b.run_retention_ttl_seconds {
        spec.insert("runRetentionTtlSeconds".into(), serde_json::json!(ttl));
    }
    let api: Api<KarsTeam> = cluster.teams(&ns);
    api.patch(
        &name,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(serde_json::json!({"spec": spec})),
    )
    .await
    .map_err(|e| AppError::Upstream(e.to_string()))?;
    if execution_plan_changed {
        cluster
            .merge_patch_kind(
                &ns,
                "KarsTask",
                &format!("{name}-principal"),
                serde_json::json!({
                    "metadata": {
                        "annotations": {
                            "kars.azure.com/retry-not-before": null
                        }
                    }
                }),
            )
            .await
            .map_err(|error| {
                AppError::Upstream(format!(
                    "team plan was updated but its retry park could not be cleared: {error}"
                ))
            })?;
    }
    Ok(Json(serde_json::json!({"updated": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_team_visibility_requires_exact_owner_even_for_operators() {
        let team: KarsTeam = serde_json::from_value(serde_json::json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsTeam",
            "metadata": {
                "name": "private-team",
                "annotations": {"kars.azure.com/owner-sub": "subject-a"}
            },
            "spec": {
                "charter": "Maintain private work.",
                "envelope": {"tier": 2, "authorityCeiling": 1},
                "paused": true
            }
        }))
        .expect("team");
        let owner = Principal {
            sub: "subject-a".into(),
            name: "owner".into(),
            roles: vec!["user".into()],
        };
        let other_operator = Principal {
            sub: "subject-b".into(),
            name: "operator".into(),
            roles: vec!["operator".into()],
        };

        assert!(is_team_owner(&team, &owner));
        assert!(!is_team_owner(&team, &other_operator));
    }

    #[test]
    fn team_create_and_update_accept_mcp_servers() {
        let create: CreateTeamRequest = serde_json::from_value(serde_json::json!({
            "name": "review-board",
            "charter": "Review the release",
            "model": "github-copilot::claude-opus-4.8",
            "model_fallbacks": [
                "local-inference::gpt-oss-120b",
                "foundry::gpt-5.4-pro"
            ],
            "mcp_servers": ["playwright", "everything"],
            "memory": "foundry-default",
            "lifecycle_mode": "resourceOptimized",
            "warm_idle_seconds": 900,
            "egress_mode": "strict",
            "egress": [{"host":"docs.example.com","port":443}]
        }))
        .expect("create request");
        assert_eq!(create.mcp_servers, vec!["playwright", "everything"]);
        assert_eq!(create.memory.as_deref(), Some("foundry-default"));
        assert_eq!(
            create.model.as_deref(),
            Some("github-copilot::claude-opus-4.8")
        );
        assert_eq!(
            create.model_fallbacks,
            vec!["local-inference::gpt-oss-120b", "foundry::gpt-5.4-pro"]
        );
        assert_eq!(create.egress_mode.as_deref(), Some("strict"));
        assert_eq!(create.egress[0].host, "docs.example.com");
        assert_eq!(create.egress[0].port, Some(443));
        assert_eq!(create.lifecycle_mode.as_deref(), Some("resourceOptimized"));
        assert_eq!(create.warm_idle_seconds, Some(900));

        let update: UpdateTeamRequest = serde_json::from_value(serde_json::json!({
            "mcp_servers": [],
            "model": "local-inference::gpt-oss-120b",
            "model_fallbacks": ["github-copilot::gpt-5.6-sol"],
            "lifecycle_mode": "persistent",
            "warm_idle_seconds": 1800,
            "egress_mode": "learning",
            "egress": []
        }))
        .expect("update request");
        assert_eq!(update.mcp_servers, Some(Vec::new()));
        assert_eq!(
            update.model.as_deref(),
            Some("local-inference::gpt-oss-120b")
        );
        assert_eq!(
            update.model_fallbacks,
            Some(vec!["github-copilot::gpt-5.6-sol".into()])
        );
        assert_eq!(update.egress_mode.as_deref(), Some("learning"));
        assert_eq!(update.egress.map(|entries| entries.len()), Some(0));
        assert_eq!(update.lifecycle_mode.as_deref(), Some("persistent"));
        assert_eq!(update.warm_idle_seconds, Some(1800));
    }

    #[test]
    fn team_lifecycle_modes_are_normalized_and_idle_window_is_bounded() {
        assert_eq!(
            normalize_lifecycle_mode(Some("resource-optimized")).expect("mode"),
            Some("resourceOptimized")
        );
        assert_eq!(
            normalize_lifecycle_mode(Some("persistent")).expect("mode"),
            Some("persistent")
        );
        assert!(normalize_lifecycle_mode(Some("always-on")).is_err());
        assert_eq!(
            validate_warm_idle_seconds(Some(900)).expect("idle"),
            Some(900)
        );
        assert_eq!(validate_warm_idle_seconds(Some(0)).expect("idle"), Some(0));
        assert!(validate_warm_idle_seconds(Some(-1)).is_err());
    }

    #[test]
    fn team_models_require_exact_live_catalogue_pairs() {
        let models = vec![ModelOption {
            provider: "github-copilot".into(),
            deployment: "shared-name".into(),
            is_default: true,
            detail: None,
        }];
        assert!(validate_model_route(&models, "github-copilot::shared-name").is_ok());
        assert!(validate_model_route(&models, "").is_ok());
        assert!(validate_model_route(&models, "local-inference::shared-name").is_err());
        assert!(validate_model_route(&models, "shared-name").is_err());
    }

    #[test]
    fn team_mcp_servers_are_deduplicated_and_bounded() {
        assert_eq!(
            normalize_mcp_servers(&[
                " playwright ".into(),
                "playwright".into(),
                "everything".into()
            ])
            .expect("normalize"),
            vec!["playwright", "everything"]
        );
        let too_many = (0..9).map(|i| format!("mcp-{i}")).collect::<Vec<_>>();
        assert!(normalize_mcp_servers(&too_many).is_err());
    }

    #[test]
    fn team_git_write_is_applied_without_mcp_servers() {
        let mut spec = serde_json::json!({"charter": "Deliver a feature"});
        let git_write = crate::kars::task::GitWriteConfig {
            connection_config_map_ref: crate::kars::task::LocalObjectRef {
                name: "kars-github-connection-0123456789abcdef".into(),
            },
            repos: vec!["owner/repo".into()],
        };
        apply_team_git_write(&mut spec, Some(&git_write)).expect("git write applies");
        assert_eq!(
            spec["blueprint"]["gitWrite"]["connectionConfigMapRef"]["name"],
            "kars-github-connection-0123456789abcdef"
        );
        assert_eq!(spec["blueprint"]["gitWrite"]["repos"][0], "owner/repo");
        assert!(spec["blueprint"].get("mcpServers").is_none());
    }

    #[test]
    fn team_outcomes_distinguish_change_no_action_and_failure() {
        let output = |status: &str, text: &str| {
            std::collections::BTreeMap::from([
                ("status".to_string(), status.to_string()),
                ("output".to_string(), text.to_string()),
                ("finishedAt".to_string(), "2026-07-22T20:12:27Z".to_string()),
            ])
        };
        let change = outcome_from_output(
            "team-run-1784750586".into(),
            &output(
                "ok",
                "Opened https://github.com/example/repo/pull/42 with the dependency fix.",
            ),
        );
        assert_eq!(change.disposition, "change_proposed");
        assert!(change.headline.contains("PR #42"));
        let change_with_old_sentinel = outcome_from_output(
            "team-run-1784750586".into(),
            &output(
                "ok",
                "Opened https://github.com/example/repo/pull/43.\nPrior run: [[NO_MATERIAL_CHANGE]]",
            ),
        );
        assert_eq!(change_with_old_sentinel.disposition, "change_proposed");

        let no_action = outcome_from_output(
            "team-run-1784750586".into(),
            &output(
                "ok",
                "[[NO_MATERIAL_CHANGE]] The dependency is already fixed on main.",
            ),
        );
        assert_eq!(no_action.disposition, "no_action_needed");
        assert!(no_action.headline.contains("already fixed"));

        let failed = outcome_from_output(
            "team-run-1784750586".into(),
            &output("error", "Parser failed before a handback was produced."),
        );
        assert_eq!(failed.disposition, "failed");
    }

    #[test]
    fn team_outcome_uses_assigned_task_as_work_item() {
        assert_eq!(
            outcome_work_item(
                "Assigned task for team 'maintenance'.\nTASK: [Dependabot alert] owner/repo #7: tar DETAILS: internal scaffolding"
            ),
            "[Dependabot alert] owner/repo #7: tar"
        );
    }
}
