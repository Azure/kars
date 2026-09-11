// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{Api, ListParams, Patch, PatchParams};
use serde::Deserialize;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::team::KarsTeam;
use crate::routes::tasks::require_cluster;

use super::require_owned_team;

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
