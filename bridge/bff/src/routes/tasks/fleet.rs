// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::Json;
use axum::extract::{Extension, State};
use kube::ResourceExt;
use serde::Serialize;

use crate::auth::Principal;
use crate::error::AppResult;
use crate::kars::task::KarsTask;
use crate::state::AppState;

use super::mapping::is_task_owner;
use super::{clean_display_name, clean_objective, map_kube_err, require_cluster};

#[path = "fleet_observations.rs"]
mod observations;

/// One running agent + what it is doing now, for the lifecycle view.
#[derive(Debug, Serialize)]
pub struct AgentLifecycleDto {
    pub sandbox: String,
    pub namespace: String,
    pub phase: Option<String>,
    pub parent: Option<String>,
    /// The task this agent is executing (label-derived), if any.
    pub task: Option<String>,
    pub objective: Option<String>,
    pub tier: Option<i32>,
    /// Observed counts only; absent activity is not a measured zero.
    pub rounds: Option<usize>,
    pub tool_calls: Option<usize>,
    pub last_action: Option<String>,
    /// True when the agent's sandbox pod is still running (working now) vs a
    /// recently-completed run (its ephemeral sandbox already torn down).
    pub live: bool,
    /// Root-task mission-output tokens, when known. Excludes reviewed specialist
    /// phases; not a full-run or settled account total.
    pub tokens: Option<i64>,
    /// The root's declared token ceiling, when set. Comparing only root receipts
    /// against it does not establish remaining full-run budget. `None` is uncapped.
    pub budget_tokens: Option<i64>,
    /// Run outcome: `ok` | `error` (from the mission output), when finished.
    pub status: Option<String>,
    /// When the run delivered (from the mission output).
    pub finished_at: Option<String>,
    /// Owning standing team, if this is a team run.
    pub team: Option<String>,
    /// Human label for the run.
    pub display_name: Option<String>,
    /// Live pod health (readiness, restarts, uptime, node) — only for live
    /// agents; `None` for finished runs whose sandbox was torn down.
    pub health: Option<crate::kars::cluster::PodHealth>,
}

/// Whether a `KarsTask` should surface on the "Active agents" fleet views. A
/// surfaceable run is either a team taskforce run, a `*-run-<ts>` scheduled run,
/// OR a launched direct mission (a one-off the user kicked off from `/new`).
/// The recent-runs list also retains direct missions with a stopped run identity.
/// Un-launched drafts and team structural tasks (a non-taskforce `team-role`)
/// are NOT agents yet, so they stay out. Without the direct-mission arm the
/// flagship "Active agents" page was empty for the single most common action —
/// launch a mission and watch it — because a direct mission carries neither the
/// taskforce role nor a `-run-` suffix.
fn is_surfaceable_run(t: &KarsTask, include_stopped: bool) -> bool {
    if t.metadata.deletion_timestamp.is_some()
        || t.annotations().contains_key("kars.azure.com/reviewed-root")
    {
        return false;
    }
    let name = t.name_any();
    let role = t
        .annotations()
        .get("kars.azure.com/team-role")
        .map(String::as_str);
    if role == Some("taskforce") || name.contains("-run-") {
        return true;
    }
    let launched = t.spec.execution.as_ref().map(|e| e.launch).unwrap_or(false);
    role.is_none()
        && (launched
            || (include_stopped && super::run_evidence::live::can_read_stopped_activity(t)))
}

fn root_receipt_tokens(output: &std::collections::BTreeMap<String, String>) -> Option<i64> {
    super::run_evidence::mission_result(output).total_tokens
}

/// `GET /api/agents` — recent and live agent runs with their real work. Sources
/// from run KarsTasks + their persisted mission telemetry (not idle pods), so
/// the page answers "what have my agents been doing, and what's working now" —
/// live runs first, then recently completed. Ephemeral run sandboxes tear down
/// after delivering, so their work would otherwise vanish; here it persists.
pub async fn list_agents(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<Vec<AgentLifecycleDto>>> {
    let cluster = require_cluster(&state)?;
    let tasks_api = cluster.tasks("kars-system");
    let all = tasks_api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(map_kube_err)?;

    // Runs only: a team-owned run, a `*-run-<ts>` task, or a launched direct
    // mission. Members/principals (structural team roles) are standing authority,
    // not work to surface here.
    let mut runs: Vec<&KarsTask> = all
        .items
        .iter()
        .filter(|task| is_surfaceable_run(task, true) && is_task_owner(task, &principal))
        .collect();
    // Freshest first.
    runs.sort_by(|a, b| {
        let ta = a.metadata.creation_timestamp.as_ref().map(|t| t.0);
        let tb = b.metadata.creation_timestamp.as_ref().map(|t| t.0);
        tb.cmp(&ta)
    });

    let mut out = Vec::new();
    let mut terminal_budget = super::reviewed_summary::SummaryBudget::new();
    for task in runs.into_iter().take(24) {
        let name = task.name_any();
        let team = task
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/team").cloned());

        let snapshot =
            observations::read(cluster, task, &principal, Some(&mut terminal_budget)).await?;
        let (tokens, status, finished_at, mut rounds, mut tool_calls) = match &snapshot.output {
            Some(d) => (
                root_receipt_tokens(d),
                d.get("status").cloned(),
                d.get("finishedAt").cloned(),
                d.get("rounds").and_then(|v| v.parse::<usize>().ok()),
                d.get("toolCalls").and_then(|v| v.parse::<usize>().ok()),
            ),
            None => (None, None, None, None, None),
        };
        let live = !snapshot.workers.is_empty();
        // A root coordinator without a pod must not hide its reviewed workers.
        let health = if snapshot.workers.len() == 1 {
            cluster
                .sandbox_pod_health(&snapshot.workers[0].name)
                .await?
        } else {
            None
        };
        let mut last_action = None;
        for worker in &snapshot.workers {
            if let Some(events) = &worker.events {
                let counts = observations::counts(events);
                rounds = Some(rounds.unwrap_or(0) + counts.rounds);
                tool_calls = Some(tool_calls.unwrap_or(0) + counts.tools);
                last_action = events
                    .iter()
                    .rev()
                    .find(|event| event.get("kind").and_then(|v| v.as_str()) == Some("tool_result"))
                    .and_then(|event| event.get("name").and_then(|v| v.as_str()))
                    .map(String::from)
                    .or(last_action);
            }
        }
        let phase = Some(snapshot.phase);

        out.push(AgentLifecycleDto {
            sandbox: name.clone(),
            namespace: task.namespace().unwrap_or_default(),
            phase,
            parent: task.spec.parent_ref.as_ref().map(|p| p.name.clone()),
            task: Some(name.clone()),
            objective: Some(clean_objective(&task.spec.objective)),
            tier: Some(task.spec.envelope.tier),
            rounds,
            tool_calls,
            last_action,
            live,
            tokens,
            budget_tokens: task.spec.envelope.budget.as_ref().and_then(|b| b.tokens),
            status,
            finished_at,
            team,
            display_name: clean_display_name(&task.spec.display_name, &task.spec.objective),
            health,
        });
    }

    // Live runs first, then most-recent finished.
    out.sort_by(|a, b| b.live.cmp(&a.live).then(b.finished_at.cmp(&a.finished_at)));
    Ok(Json(out))
}

// ─── Fleet live telemetry (at-scale "what's happening now") ──────────────────

#[derive(serde::Serialize)]
pub struct FleetActivityItem {
    /// The run/agent this event came from.
    pub agent: String,
    pub display_name: Option<String>,
    pub team: Option<String>,
    /// "tool" | "round".
    pub kind: String,
    /// For a tool event, the tool name; for a round, the finish reason.
    pub label: String,
    /// Optional short argument/host preview for a tool event.
    pub detail: Option<String>,
    /// Whether a tool event failed (ok=false) — surfaced in red.
    pub failed: bool,
    /// Round index the event belongs to.
    pub round: i64,
    /// Monotonic sequence within the run's trace (for stable ordering).
    pub seq: i64,
    /// Milliseconds the step took, when known.
    pub ms: Option<i64>,
    pub worker: String,
    pub role: Option<String>,
    pub source: String,
    pub observed_at: Option<String>,
}

#[derive(serde::Serialize)]
pub struct FleetTelemetryDto {
    /// Agents whose sandbox pod is running right now.
    pub working: usize,
    /// Distinct standing teams with a live run.
    pub teams_active: usize,
    /// Sub-agents (runs with a parent) currently live.
    pub sub_agents: usize,
    /// Provisional observed usage, not settled spending or complete run totals.
    pub tokens_in_flight: Option<i64>,
    pub tool_calls: Option<usize>,
    pub rounds: Option<usize>,
    /// Live workers without a currently available, validated activity snapshot.
    pub activity_unavailable: usize,
    /// Timestamped observations first; undated events carry no cross-run ordering claim.
    pub feed: Vec<FleetActivityItem>,
}

/// `GET /api/agents/fleet` — partial, revision-bound observations from current
/// assignments, including reviewed workers whose coordinator has no sandbox.
pub async fn fleet_telemetry(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<FleetTelemetryDto>> {
    let cluster = require_cluster(&state)?;
    let tasks_api = cluster.tasks("kars-system");
    let all = tasks_api
        .list(&kube::api::ListParams::default())
        .await
        .map_err(map_kube_err)?;

    let runs: Vec<&KarsTask> = all
        .items
        .iter()
        .filter(|task| is_surfaceable_run(task, false) && is_task_owner(task, &principal))
        .collect();

    let mut working = 0usize;
    let mut teams: std::collections::BTreeSet<String> = Default::default();
    let mut sub_agents = 0usize;
    let mut tokens_in_flight = Some(0i64);
    let mut tool_calls = None;
    let mut rounds = None;
    let mut activity_unavailable = 0;
    let mut feed: Vec<FleetActivityItem> = Vec::new();

    for task in runs {
        let name = task.name_any();
        let snapshot = observations::read(cluster, task, &principal, None).await?;
        if snapshot.workers.is_empty() {
            continue;
        }
        let team = task.labels().get("kars.azure.com/team").cloned();
        if let Some(team) = &team {
            teams.insert(team.clone());
        }
        let display_name = clean_display_name(&task.spec.display_name, &task.spec.objective);
        for worker in snapshot.workers {
            working += 1;
            if worker.role.is_some() || task.spec.parent_ref.is_some() {
                sub_agents += 1;
            }
            let Some(events) = worker.events else {
                activity_unavailable += 1;
                tokens_in_flight = None;
                continue;
            };
            let counts = observations::counts(&events);
            rounds = Some(rounds.unwrap_or(0) + counts.rounds);
            tool_calls = Some(tool_calls.unwrap_or(0) + counts.tools);
            tokens_in_flight = tokens_in_flight
                .zip(counts.tokens)
                .and_then(|(total, tokens)| total.checked_add(tokens));
            for event in events {
                let kind = event
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if !matches!(kind, "round" | "tool_result") {
                    continue;
                }
                let is_tool = kind == "tool_result";
                feed.push(FleetActivityItem {
                    agent: name.clone(),
                    display_name: display_name.clone(),
                    team: team.clone(),
                    worker: worker.name.clone(),
                    role: worker.role.clone(),
                    source: event
                        .get("source")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown")
                        .into(),
                    observed_at: event
                        .get("observed_at")
                        .and_then(|value| value.as_str())
                        .map(String::from),
                    kind: if is_tool { "tool" } else { "round" }.into(),
                    label: event
                        .get(if is_tool { "name" } else { "model" })
                        .and_then(|value| value.as_str())
                        .unwrap_or(if is_tool { "tool" } else { "model response" })
                        .into(),
                    detail: None,
                    failed: if is_tool {
                        event.get("ok").and_then(|value| value.as_bool()) == Some(false)
                    } else {
                        event.get("accepted").and_then(|value| value.as_bool()) == Some(false)
                    },
                    round: event
                        .get("round")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0),
                    seq: event
                        .get("seq")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0),
                    ms: event.get("ms").and_then(|value| value.as_i64()),
                });
            }
        }
    }
    if working == 0 {
        tokens_in_flight = None;
    }
    feed.sort_by(|a, b| {
        let timestamp = |event: &FleetActivityItem| {
            event
                .observed_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        };
        timestamp(b)
            .cmp(&timestamp(a))
            .then(a.worker.cmp(&b.worker))
            .then(b.seq.cmp(&a.seq))
    });
    feed.truncate(40);

    Ok(Json(FleetTelemetryDto {
        working,
        teams_active: teams.len(),
        sub_agents,
        tokens_in_flight,
        tool_calls,
        rounds,
        activity_unavailable,
        feed,
    }))
}

#[cfg(test)]
mod tests {
    use super::{KarsTask, is_surfaceable_run, root_receipt_tokens};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn direct_mission() -> serde_json::Value {
        json!({
            "metadata":{"name":"briefing","namespace":"kars-system","uid":"task-uid",
                "annotations":{"kars.azure.com/run-requested":"run-1"}},
            "spec":{"objective":"Briefing","execution":{"launch":false},
                "envelope":{"tier":1,"delegationDepth":0,"authorityCeiling":1}},
            "status":{"executionPhase":"Idle"}
        })
    }

    #[test]
    fn stopped_direct_missions_remain_recent_without_becoming_live() {
        for completed in [false, true] {
            let mut value = direct_mission();
            if completed {
                value["metadata"]["annotations"]["kars.azure.com/run-completed"] = json!("run-1");
            }
            let task: KarsTask = serde_json::from_value(value.clone()).unwrap();
            assert!(is_surfaceable_run(&task, true));
            assert!(!is_surfaceable_run(&task, false));
            value["spec"]["execution"]["launch"] = json!(true);
            let task = serde_json::from_value(value).unwrap();
            assert!(is_surfaceable_run(&task, true));
            assert!(is_surfaceable_run(&task, false));
        }
    }

    #[test]
    fn recent_mission_selection_still_excludes_drafts_and_structural_tasks() {
        for (path, replacement) in [
            (
                "/metadata/annotations/kars.azure.com~1run-requested",
                json!(""),
            ),
            (
                "/metadata/annotations/kars.azure.com~1run-requested",
                json!("invalid nonce"),
            ),
            ("/metadata/annotations", json!({})),
            ("/spec/execution", json!(null)),
            (
                "/metadata/annotations",
                json!({"kars.azure.com/run-requested":"run-1", "kars.azure.com/team-role":"principal"}),
            ),
            (
                "/metadata/annotations",
                json!({"kars.azure.com/run-requested":"run-1", "kars.azure.com/reviewed-root":"root"}),
            ),
        ] {
            let mut value = direct_mission();
            *value.pointer_mut(path).unwrap() = replacement;
            let task = serde_json::from_value(value).unwrap();
            assert!(!is_surfaceable_run(&task, true), "{path}");
            assert!(!is_surfaceable_run(&task, false), "{path}");
        }
        let mut value = direct_mission();
        value["metadata"]["deletionTimestamp"] = json!("2026-11-03T00:00:00Z");
        assert!(!is_surfaceable_run(
            &serde_json::from_value(value).unwrap(),
            true
        ));
    }

    #[test]
    fn fleet_root_receipt_usage_preserves_unknown_and_measured_zero() {
        for (tokens, known, expected) in [
            ("0", "true", Some(0)),
            ("993479", "true", Some(993479)),
            ("0", "false", None),
            ("993479", "false", None),
            ("-1", "true", None),
            ("9007199254740992", "true", None),
            ("invalid", "true", None),
        ] {
            let output = BTreeMap::from([
                ("totalTokens".into(), tokens.into()),
                ("usageKnown".into(), known.into()),
            ]);
            assert_eq!(root_receipt_tokens(&output), expected, "{tokens}/{known}");
        }
        assert_eq!(root_receipt_tokens(&BTreeMap::new()), None);
    }

    #[test]
    fn fleet_root_receipt_usage_does_not_trust_invalid_terminal_evidence() {
        for evidence in ["{}", "not-json"] {
            let output = BTreeMap::from([
                ("totalTokens".into(), "993479".into()),
                ("usageKnown".into(), "true".into()),
                ("evidence.json".into(), evidence.into()),
            ]);
            assert_eq!(root_receipt_tokens(&output), None);
        }
    }
}
