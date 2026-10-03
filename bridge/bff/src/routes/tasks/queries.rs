// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{Api, ListParams};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::task::KarsTask;
use crate::state::AppState;

use super::artifacts::build_artifact_set;
use super::evidence::{select_task_checkpoint, subagent_trace_from_artifacts};
use super::mapping::{
    composition_from_materialized, is_task_owner, to_detail, to_sub_agent, to_summary,
};
use super::presentation::deliverable_pull_requests;
use super::revision::{ReadRevision, output_matches_task};
use super::run_evidence::{mission_result, run_telemetry, scope_activity};
use super::{TaskDetailDto, TaskSummaryDto, map_kube_err, require_cluster};

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
    let outputs = if ns == "kars-system" {
        cluster.list_mission_outputs().await
    } else {
        Vec::new()
    };
    // Re-read ownership and revision after evidence; never project onto an older list snapshot.
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(map_kube_err)?;
    let summaries = list
        .items
        .iter()
        .filter(|task| {
            is_task_owner(task, &principal)
                && task.namespace().as_deref() == Some(ns.as_str())
                && task.metadata.deletion_timestamp.is_none()
        })
        .map(|task| {
            let mut summary = to_summary(task);
            if let Some(record) = outputs
                .iter()
                .find(|record| output_matches_task(task, &record.data))
            {
                let result = mission_result(&record.data);
                summary.delivered = result.reviewable;
                summary.failed = !result.reviewable;
            }
            summary
        })
        .collect();
    // Retained output without a live Task is not current, authorized evidence.
    Ok(Json(summaries))
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
    let task = api
        .get_opt(&name)
        .await
        .map_err(map_kube_err)?
        .ok_or(AppError::NotFound)?;
    let revision = ReadRevision::capture(&task, &ns, &name, &principal)?;
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
    let output_data = if ns == "kars-system" {
        cluster
            .read_mission_output(&name)
            .await
            .filter(|output| output_matches_task(&task, output))
    } else {
        None
    };
    let result = output_data.as_ref().map(mission_result);

    // The mission's full artifact set: the manifest (name + size, incl. binary)
    // comes from the output ConfigMap; text contents come from the companion
    // artifacts ConfigMap. Merge them so the set is complete and honest.
    let artifacts = if output_data.is_some() {
        build_artifact_set(cluster, &name, output_data.as_ref()).await
    } else {
        Vec::new()
    };
    let successful_result = result.as_ref().is_some_and(|result| result.reviewable);
    // Name-only progress checkpoints cannot establish revision custody.
    let checkpoint = select_task_checkpoint(None, &artifacts, successful_result);

    // The mission's live execution activity — the real per-round + per-tool
    // trace the agent emitted, persisted by the controller as the clean audit
    // record. Parsed from the trace ConfigMap; empty when no trace exists.
    let mut activity: Vec<serde_json::Value> = if output_data.is_some() {
        cluster
            .read_current_mission_trace(&name)
            .await
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
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

    // Name-only router traces cannot establish Task UID, revision, or runtime
    // custody. Missing bound activity means unavailable detail, not zero work.

    let current_nonce = task
        .annotations()
        .get("kars.azure.com/run-requested")
        .map(String::as_str);
    activity = scope_activity(activity, current_nonce);
    let telemetry = run_telemetry(result.as_ref());

    // The running agent's real mesh identity, discovered from the AGT registry
    // (harness-neutral). Only meaningful once a sandbox is running.
    let agent_identity = match &sandbox_name {
        Some(sb) => cluster.discover_agent_identity(sb).await,
        None => None,
    };

    // Referenced PR URLs are not proof that this mission authored the PR.
    let pull_requests = output_data
        .as_ref()
        .map(deliverable_pull_requests)
        .unwrap_or_default();

    let task = revision.recheck(cluster, &principal).await?;
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
