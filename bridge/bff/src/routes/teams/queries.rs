// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{Api, ListParams};
use serde::Serialize;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::team::KarsTeam;
use crate::routes::tasks::{
    clean_display_name, clean_objective, deliverable_excerpt, deliverable_text,
    extract_pull_requests, is_failure_shaped_output, is_no_change_output, require_cluster,
};

use super::{
    TeamDetailDto, TeamRoleDto, TeamSummaryDto, channels_from_keys, effective_lifecycle_mode,
    is_team_owner, phase_of, read_task_list, require_owned_team, runtime_state,
};

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

#[cfg(test)]
mod tests {
    use super::*;

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
