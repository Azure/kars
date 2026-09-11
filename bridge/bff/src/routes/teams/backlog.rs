// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{ListParams, Patch, PatchParams};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::tasks::require_cluster;

use super::require_owned_team;

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
