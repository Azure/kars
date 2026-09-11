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
    /// Live activity counts from the persisted trace (rounds + tool calls).
    pub rounds: usize,
    pub tool_calls: usize,
    pub last_action: Option<String>,
    /// True when the agent's sandbox pod is still running (working now) vs a
    /// recently-completed run (its ephemeral sandbox already torn down).
    pub live: bool,
    /// Real token cost of the run (from the mission output), when known.
    pub tokens: Option<i64>,
    /// The run's token budget ceiling (from the envelope), when set — so the UI
    /// can render spend against limit ("spent / budget") rather than a bare
    /// number. `None` for an uncapped run.
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
/// Un-launched drafts and team structural tasks (a non-taskforce `team-role`)
/// are NOT agents yet, so they stay out. Without the direct-mission arm the
/// flagship "Active agents" page was empty for the single most common action —
/// launch a mission and watch it — because a direct mission carries neither the
/// taskforce role nor a `-run-` suffix.
fn is_surfaceable_run(t: &KarsTask) -> bool {
    let name = t.name_any();
    let role = t
        .annotations()
        .get("kars.azure.com/team-role")
        .map(String::as_str);
    if role == Some("taskforce") || name.contains("-run-") {
        return true;
    }
    // A launched direct mission: no team structural role, and it was launched.
    let launched = t.spec.execution.as_ref().map(|e| e.launch).unwrap_or(false);
    role.is_none() && launched
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
        .filter(|task| is_surfaceable_run(task) && is_task_owner(task, &principal))
        .collect();
    // Freshest first.
    runs.sort_by(|a, b| {
        let ta = a.metadata.creation_timestamp.as_ref().map(|t| t.0);
        let tb = b.metadata.creation_timestamp.as_ref().map(|t| t.0);
        tb.cmp(&ta)
    });

    let mut out = Vec::new();
    for task in runs.into_iter().take(24) {
        let name = task.name_any();
        let team = task
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/team").cloned());

        // Mission output: tokens, status, finished, round/tool rollup.
        let output = cluster.read_mission_output(&name).await;
        let (tokens, status, finished_at, mut rounds, mut tool_calls) = match &output {
            Some(d) => (
                d.get("totalTokens").and_then(|v| v.parse::<i64>().ok()),
                d.get("status").cloned(),
                d.get("finishedAt").cloned(),
                d.get("rounds")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0),
                d.get("toolCalls")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0),
            ),
            None => (None, None, None, 0, 0),
        };

        // Live iff the run's sandbox pod is running AND it hasn't delivered a
        // terminal result yet. A DIRECT mission's sandbox lingers (Running) after
        // it delivers, so "Running pod" alone would mislabel a finished, idle
        // mission as live and inflate the fleet's "working now" count. A delivered
        // (or errored) run is Recent, not Live — its outcome and telemetry show in
        // the recent list. (Team run sandboxes tear down on delivery, so this is a
        // no-op for them.)
        let delivered = status.is_some();
        let live = !delivered && cluster.running_pod_for_sandbox(&name).await.is_some();
        // Honest pod health for a live agent (readiness/restarts/uptime/node).
        let health = if live {
            cluster.sandbox_pod_health(&name).await
        } else {
            None
        };

        // For a live run, the trace's last tool tells "what it's doing now";
        // also a more current round/tool count than the (post-hoc) output.
        let mut last_action = None;
        if live {
            // A live run has no persisted trace CM yet (it's written at delivery),
            // so fall back to the router's live trace — otherwise a working agent
            // reports 0 rounds / 0 tool calls / no current action.
            let trace: Vec<serde_json::Value> = match cluster
                .read_mission_trace(&name)
                .await
                .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
            {
                Some(t) if !t.is_empty() => t,
                _ => cluster.sandbox_live_trace(&name).await,
            };
            if !trace.is_empty() {
                let r = trace
                    .iter()
                    .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("round"))
                    .count();
                let tc = trace
                    .iter()
                    .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("tool"))
                    .count();
                if r > 0 {
                    rounds = r;
                }
                if tc > 0 {
                    tool_calls = tc;
                }
                last_action = trace
                    .last()
                    .and_then(|e| e.get("name").and_then(|n| n.as_str()).map(String::from));
            }
        }

        let phase = if live {
            Some("Running".into())
        } else if status.as_deref() == Some("ok") {
            Some("Delivered".into())
        } else if status.is_some() {
            Some("Errored".into())
        } else {
            Some("Idle".into())
        };

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
}

#[derive(serde::Serialize)]
pub struct FleetTelemetryDto {
    /// Agents whose sandbox pod is running right now.
    pub working: usize,
    /// Distinct standing teams with a live run.
    pub teams_active: usize,
    /// Sub-agents (runs with a parent) currently live.
    pub sub_agents: usize,
    /// Live token burn summed across working agents (from their in-flight trace).
    pub tokens_in_flight: i64,
    /// Tool calls summed across working agents this run.
    pub tool_calls: i64,
    /// Model rounds summed across working agents this run.
    pub rounds: i64,
    /// The most recent activity across ALL live agents, newest first — a single
    /// chronological fleet feed of what every working agent is doing right now.
    pub feed: Vec<FleetActivityItem>,
}

/// `GET /api/agents/fleet` — aggregate LIVE telemetry across every working
/// agent, plus a single merged activity feed of what they're all doing right
/// now. This is the "at scale" view: instead of drilling into one mission, see
/// the whole fleet's live tool-by-tool work in one stream. Sourced from each
/// live run's real execution trace — never fabricated; an idle fleet returns
/// zeros and an empty feed.
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
        .filter(|task| is_surfaceable_run(task) && is_task_owner(task, &principal))
        .collect();

    let mut working = 0usize;
    let mut teams: std::collections::BTreeSet<String> = Default::default();
    let mut sub_agents = 0usize;
    let mut tokens_in_flight = 0i64;
    let mut tool_calls = 0i64;
    let mut rounds = 0i64;
    let mut feed: Vec<FleetActivityItem> = Vec::new();

    for task in runs {
        let name = task.name_any();
        // Only agents that are actually running right now contribute trace.
        if cluster.running_pod_for_sandbox(&name).await.is_none() {
            continue;
        }
        // A delivered direct mission keeps a lingering Running pod but is idle —
        // its historical tokens are NOT "in flight". Skip it here so the live
        // counters reflect only work happening now (it still shows, with its
        // outcome, in the Recent runs list from /api/agents).
        if cluster
            .read_mission_output(&name)
            .await
            .and_then(|d| d.get("status").cloned())
            .is_some()
        {
            continue;
        }
        working += 1;
        for sub in cluster.sub_agent_sandbox_names("kars-system", &name).await {
            if cluster.running_pod_for_sandbox(&sub).await.is_some() {
                working += 1;
                sub_agents += 1;
            }
        }
        let team = task
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("kars.azure.com/team").cloned());
        if let Some(t) = &team {
            teams.insert(t.clone());
        }
        let display_name = clean_display_name(&task.spec.display_name, &task.spec.objective);

        // Prefer the persisted trace (delivered runs); for a LIVE run the trace
        // CM doesn't exist yet, so fall back to the router's live trace — else
        // every actively-working agent shows zero rounds/tokens/tools (the exact
        // opposite of "what's happening now"). Mirrors get_task's live fallback.
        let trace: Vec<serde_json::Value> = match cluster
            .read_mission_trace(&name)
            .await
            .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
        {
            Some(t) if !t.is_empty() => t,
            _ => cluster.sandbox_live_trace(&name).await,
        };
        if trace.is_empty() {
            continue;
        }

        for e in &trace {
            let kind = e.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let round = e.get("round").and_then(|v| v.as_i64()).unwrap_or(0);
            let seq = e.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
            let ms = e.get("ms").and_then(|v| v.as_i64());
            match kind {
                "round" => {
                    rounds += 1;
                    tokens_in_flight += e.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    feed.push(FleetActivityItem {
                        agent: name.clone(),
                        display_name: display_name.clone(),
                        team: team.clone(),
                        kind: "round".into(),
                        label: e
                            .get("finish_reason")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or("model round")
                            .to_string(),
                        detail: None,
                        failed: false,
                        round,
                        seq,
                        ms,
                    });
                }
                "tool" => {
                    tool_calls += 1;
                    let failed = e.get("ok").and_then(|v| v.as_bool()) == Some(false);
                    feed.push(FleetActivityItem {
                        agent: name.clone(),
                        display_name: display_name.clone(),
                        team: team.clone(),
                        kind: "tool".into(),
                        label: e
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string(),
                        detail: e
                            .get("args_preview")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.chars().take(80).collect()),
                        failed,
                        round,
                        seq,
                        ms,
                    });
                }
                _ => {}
            }
        }
    }

    // Newest activity first, capped so a busy fleet stays responsive.
    feed.sort_by(|a, b| b.round.cmp(&a.round).then(b.seq.cmp(&a.seq)));
    feed.truncate(40);

    Ok(Json(FleetTelemetryDto {
        working,
        teams_active: teams.len(),
        sub_agents,
        tokens_in_flight,
        tool_calls,
        rounds,
        feed,
    }))
}
