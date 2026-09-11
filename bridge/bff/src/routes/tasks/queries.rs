use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{Api, ListParams};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::KarsTask;
use crate::state::AppState;

use super::artifacts::build_artifact_set;
use super::evidence::{
    merge_trace_total_tokens, select_task_checkpoint, subagent_trace_from_artifacts,
};
use super::history::synth_detail_from_output;
use super::mapping::{
    composition_from_materialized, is_task_owner, to_detail, to_sub_agent, to_summary,
};
use super::presentation::deliverable_pull_requests;
use super::{
    MissionResultDto, MissionTelemetryDto, TaskDetailDto, TaskSummaryDto, classify_blocked,
    deliverable_text, map_kube_err, require_cluster,
};

/// `GET /api/namespaces/:ns/tasks` — list tasks in a namespace.
pub async fn list_tasks(
    State(state): State<AppState>,
    principal: Option<Extension<Principal>>,
    Path(ns): Path<String>,
) -> AppResult<Json<Vec<TaskSummaryDto>>> {
    let cluster = require_cluster(&state)?;
    let principal = principal
        .map(|Extension(principal)| principal)
        .ok_or_else(|| AppError::Forbidden("signed-in principal required".into()))?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    // Cross-reference delivered + failed missions in ONE pass over the persisted
    // outputs, so the list can show "Delivered" / "Run failed" instead of
    // misreading an idle delivered run — or a hung errored run — as "drafting".
    let outputs = cluster.list_mission_outputs().await;
    let mut terminal = std::collections::HashMap::<String, &'static str>::new();
    for record in &outputs {
        match record.data.get("status").map(String::as_str) {
            Some("ok")
                if record
                    .data
                    .get("output")
                    .is_some_and(|output| !output.trim().is_empty()) =>
            {
                terminal
                    .entry(record.task_name.clone())
                    .or_insert("delivered");
            }
            Some("error") => {
                terminal.entry(record.task_name.clone()).or_insert("failed");
            }
            _ => {}
        }
    }
    let delivered: std::collections::HashSet<String> = terminal
        .iter()
        .filter(|(_, status)| **status == "delivered")
        .map(|(task, _)| task.clone())
        .collect();
    let failed: std::collections::HashSet<String> = terminal
        .iter()
        .filter(|(_, status)| **status == "failed")
        .map(|(task, _)| task.clone())
        .collect();
    let mut summaries: Vec<TaskSummaryDto> = list
        .items
        .iter()
        .filter(|task| is_task_owner(task, &principal))
        .map(|t| {
            let mut s = to_summary(t);
            s.delivered = delivered.contains(&s.name);
            s.failed = failed.contains(&s.name);
            s
        })
        .collect();

    // Persist history: a mission whose KarsTask CR has been garbage-collected
    // (retired-run GC) still has its delivered/errored output ConfigMap. Without
    // this, completed missions silently vanish from the list mid-session and
    // their direct URLs 404 ("data loss", audit BUG-8). Re-add any output-only
    // mission that isn't already represented by a live CR. Team-run machinery
    // (`<team>-run-<epoch>`) is excluded — those belong to the Team view, which
    // is exactly what the live-CR path already hides.
    let live_names: std::collections::HashSet<String> =
        summaries.iter().map(|s| s.name.clone()).collect();
    for record in &outputs {
        let task = &record.task_name;
        let d = &record.data;
        if d.get("ownerSub").map(String::as_str) != Some(principal.sub.as_str()) {
            continue;
        }
        if live_names.contains(task) || regex_lite_is_team_run(task) {
            continue;
        }
        let is_ok = delivered.contains(task);
        let is_err = failed.contains(task);
        // Only surface a genuinely terminal output (delivered or errored); skip
        // stray/empty outputs so we don't invent phantom missions.
        if !is_ok && !is_err {
            continue;
        }
        summaries.push(TaskSummaryDto {
            name: task.clone(),
            namespace: ns.clone(),
            objective: d.get("objective").cloned().unwrap_or_default(),
            display_name: d
                .get("displayName")
                .cloned()
                .filter(|s| !s.trim().is_empty()),
            created_at: d.get("startedAt").cloned(),
            tier: d.get("tier").and_then(|v| v.parse().ok()).unwrap_or(0),
            phase: if is_err {
                "Failed".into()
            } else {
                "Delivered".into()
            },
            envelope_digest: None,
            team: d.get("team").cloned(),
            delivered: is_ok,
            failed: is_err,
            launched: true,
            execution_phase: Some("Idle".into()),
        });
    }
    Ok(Json(summaries))
}

/// True when `name` looks like a standing-team run task (`<team>-run-<epoch>`),
/// which the Missions surface intentionally hides (they belong to the Team
/// view). A tiny hand-rolled check to avoid a regex dependency.
pub(super) fn regex_lite_is_team_run(name: &str) -> bool {
    if let Some(idx) = name.rfind("-run-") {
        let suffix = &name[idx + "-run-".len()..];
        return !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// `GET /api/namespaces/:ns/tasks/:name` — fetch one task, with its delegated
/// children resolved (tasks whose `parentRef` points at this task).
pub async fn get_task(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<TaskDetailDto>> {
    let cluster = require_cluster(&state)?;
    let api: Api<KarsTask> = cluster.tasks(&ns);
    let task = match api.get_opt(&name).await.map_err(map_kube_err)? {
        Some(t) => t,
        // The KarsTask CR was garbage-collected (retired-run GC) but the
        // mission's terminal output persists. Synthesize a read-only detail from
        // it so a delivered/failed mission's page — and the list link that now
        // shows it — doesn't 404 mid-session. Genuine unknowns still 404.
        None => return synth_detail_from_output(cluster, &ns, &name, &principal).await,
    };
    if !is_task_owner(&task, &principal) {
        return Err(AppError::NotFound);
    }
    // Resolve direct children by scanning the namespace for parentRef == name.
    // Exclude RUN INSTANCES (cadence/taskforce runs named `*-run-<epoch>` or
    // annotated team-role=taskforce): those are run history, not org-chart roles.
    // Without this the org chart floods with every historical run of a standing
    // team as a duplicate node. Same predicate list_agents uses to identify runs.
    let all = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    let children: Vec<TaskSummaryDto> = all
        .items
        .iter()
        .filter(|t| t.spec.parent_ref.as_ref().is_some_and(|r| r.name == name))
        .filter(|t| {
            let is_run = t.name_any().contains("-run-")
                || t.annotations()
                    .get("kars.azure.com/team-role")
                    .map(String::as_str)
                    == Some("taskforce");
            !is_run
        })
        .map(to_summary)
        .collect();

    // Runtime agents: the sub-agents this mission's agent spawned at run time.
    // The inference router labels each spawned KarsSandbox
    // `kars.azure.com/parent=<sandbox>`; surface them so the org chart reflects
    // the *running* agent/sub-agent tree, not only the governed role tree.
    let sandbox_name = task
        .status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .map(|r| r.name.clone());
    let sub_agents = match &sandbox_name {
        Some(sb) => cluster
            .sub_agent_sandboxes(&ns, sb)
            .await
            .iter()
            .map(to_sub_agent)
            .collect(),
        None => Vec::new(),
    };

    // Effective composition: once launched, show what the sandbox is ACTUALLY
    // running (read from the materialized InferencePolicy + KarsSandbox,
    // including any controller-defaulted model), not just the submitted
    // blueprint. Pre-launch, fall back to the blueprint (the planned config).
    let effective = match &sandbox_name {
        Some(sb) => {
            let ip = cluster
                .get_kind(&ns, "InferencePolicy", &format!("{name}-inference"))
                .await
                .ok()
                .flatten();
            let sandbox = cluster
                .get_kind(&ns, "KarsSandbox", sb)
                .await
                .ok()
                .flatten();
            composition_from_materialized(ip.as_ref(), sandbox.as_ref())
        }
        None => None,
    };

    // Live egress enforcement mode (Learn/Strict) read from the materialized
    // KarsSandbox — the real monitoring→enforced surface.
    let egress_mode = match &sandbox_name {
        Some(sb) => cluster.sandbox_egress_mode(sb).await,
        None => None,
    };

    // The mission's captured run result (persisted deliverable + real tokens).
    let output_data = cluster.read_mission_output(&name).await;
    let mut result = output_data.as_ref().and_then(|d| {
        let output = deliverable_text(d.get("output")?);
        let blocked = classify_blocked(d.get("status").map(String::as_str), &output);
        Some(MissionResultDto {
            output,
            status: d.get("status").cloned(),
            model: d.get("model").cloned(),
            total_tokens: d.get("totalTokens").and_then(|v| v.parse().ok()),
            prompt_tokens: d.get("promptTokens").and_then(|v| v.parse().ok()),
            completion_tokens: d.get("completionTokens").and_then(|v| v.parse().ok()),
            finished_at: d.get("finishedAt").cloned(),
            assignment_nonce: d.get("assignmentNonce").cloned(),
            source: d.get("source").cloned(),
            blocked,
            artifact_persistence: d.get("artifactPersistence").cloned(),
            artifact_count: d.get("artifactCount").and_then(|v| v.parse().ok()),
            declared_artifact_count: d.get("declaredArtifactCount").and_then(|v| v.parse().ok()),
        })
    });

    // The mission's full artifact set: the manifest (name + size, incl. binary)
    // comes from the output ConfigMap; text contents come from the companion
    // artifacts ConfigMap. Merge them so the set is complete and honest.
    let artifacts = build_artifact_set(cluster, &name, output_data.as_ref()).await;
    let successful_result = result.as_ref().is_some_and(|result| {
        result.status.as_deref() != Some("error") && result.blocked.is_none()
    });
    let checkpoint = select_task_checkpoint(
        cluster.read_mission_progress(&name).await,
        &artifacts,
        successful_result,
    );

    // The mission's live execution activity — the real per-round + per-tool
    // trace the agent emitted, persisted by the controller as the clean audit
    // record. Parsed from the trace ConfigMap; empty when no trace exists.
    let mut activity: Vec<serde_json::Value> = cluster
        .read_mission_trace(&name)
        .await
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
        .unwrap_or_default();
    activity.extend(subagent_trace_from_artifacts(&artifacts));
    activity.sort_by(|left, right| {
        left.get("ts")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .cmp(
                right
                    .get("ts")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            )
    });

    // LIVE fallback. The persisted trace ConfigMap is written only once, at
    // delivery — so a still-running mission would otherwise show an EMPTY
    // activity trace (blank deploy timeline, agent graph, and map, and a
    // "Waiting for the first model round" that lies while the agent is already
    // on round 3). When no persisted trace exists yet and the mission is
    // launched, pull the SAME live router telemetry the Activity SSE streams —
    // the principal sandbox plus every sub-agent it spawned — so the WHOLE
    // detail page is genuinely live on each poll, not just the SSE tab.
    if activity.is_empty()
        && let Some(principal) = &sandbox_name
    {
        let mut live: Vec<serde_json::Value> = Vec::new();
        for mut ev in cluster.sandbox_live_trace(principal).await {
            if let Some(obj) = ev.as_object_mut() {
                obj.insert("agent".into(), serde_json::json!(name));
                obj.insert("agentInstance".into(), serde_json::json!(principal));
                obj.insert("agentRole".into(), serde_json::json!("principal"));
            }
            live.push(ev);
        }
        let mut descendants = cluster
            .sub_agent_sandbox_names(&ns, principal)
            .await
            .into_iter();
        loop {
            let sub_batch = descendants.by_ref().take(8).collect::<Vec<_>>();
            if sub_batch.is_empty() {
                break;
            }
            let mut polling = tokio::task::JoinSet::new();
            for sub in sub_batch {
                let cluster = cluster.clone();
                polling.spawn(async move {
                    let events = cluster.sandbox_live_trace(&sub).await;
                    (sub, events)
                });
            }
            while let Some(result) = polling.join_next().await {
                let Ok((sub, events)) = result else {
                    continue;
                };
                for mut ev in events {
                    if let Some(obj) = ev.as_object_mut() {
                        obj.insert("agent".into(), serde_json::json!(sub.clone()));
                        obj.insert("agentInstance".into(), serde_json::json!(sub.clone()));
                        obj.insert("agentRole".into(), serde_json::json!("subagent"));
                    }
                    live.push(ev);
                }
            }
        }
        activity = live;
    }

    // Loop-shape telemetry (rounds, tool calls). Token totals live on `result`.
    // Derive rollups from the persisted per-round/per-tool trace when the run's
    // output ConfigMap didn't include them — some harnesses persist the trace
    // but not the totals, which left a DELIVERED mission's map reading
    // "Not run yet" / "No activity". The trace is the honest source either way.
    let trace_round_events = activity
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("round"))
        .count() as i64;
    let trace_tool_events = activity
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("tool"))
        .count() as i64;
    let trace_total_tokens: i64 = activity
        .iter()
        .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("round"))
        .filter_map(|e| e.get("total_tokens").and_then(serde_json::Value::as_i64))
        .sum();

    // Backfill the token total on the result from the trace when the output CM
    // didn't carry it (so token burn shows on a delivered run with a trace).
    merge_trace_total_tokens(&mut result, trace_total_tokens);

    let telemetry = {
        let mut rounds = output_data
            .as_ref()
            .and_then(|d| d.get("rounds").and_then(|v| v.parse::<i64>().ok()));
        let mut tool_calls = output_data
            .as_ref()
            .and_then(|d| d.get("toolCalls").and_then(|v| v.parse::<i64>().ok()));
        if trace_round_events > 0 {
            rounds = Some(rounds.unwrap_or_default().max(trace_round_events));
        }
        if trace_tool_events > 0 {
            tool_calls = Some(tool_calls.unwrap_or_default().max(trace_tool_events));
        }
        if rounds.is_some() || tool_calls.is_some() {
            Some(MissionTelemetryDto { rounds, tool_calls })
        } else {
            None
        }
    };

    // The running agent's real mesh identity, discovered from the AGT registry
    // (harness-neutral). Only meaningful once a sandbox is running.
    let agent_identity = match &sandbox_name {
        Some(sb) => cluster.discover_agent_identity(sb).await,
        None => None,
    };

    // Pull requests the mission opened, extracted from its raw output — a PR is a
    // first-class delivery type, surfaced on the Artifacts tab (not just prose).
    let pull_requests = output_data
        .as_ref()
        .map(deliverable_pull_requests)
        .unwrap_or_default();

    Ok(Json(to_detail(
        &task,
        children,
        sub_agents,
        effective,
        result,
        artifacts,
        pull_requests,
        activity,
        telemetry,
        checkpoint,
        agent_identity,
        egress_mode,
    )))
}
