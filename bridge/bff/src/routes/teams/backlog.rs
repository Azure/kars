// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, Path, State};
use kube::ResourceExt;
use kube::api::{Patch, PatchParams};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::tasks::require_cluster;

pub use super::work::TeamTask as TeamTaskDto;
use super::{require_owned_team, work};

pub(crate) fn validate_work_intent(
    team: &crate::kars::team::KarsTeam,
    item: &TeamTaskDto,
) -> AppResult<()> {
    work::validate_run_intent(
        team.spec
            .display_name
            .as_deref()
            .unwrap_or_else(|| team.metadata.name.as_deref().unwrap_or_default()),
        &team.spec.charter,
        item,
    )
    .map_err(|error| AppError::BadRequest(error.to_string()))
}

pub(crate) fn validate_revision_plan(
    team: &crate::kars::team::KarsTeam,
    items: &[TeamTaskDto],
) -> AppResult<()> {
    if work::next_pending(items).is_some_and(|item| {
        work::revision_source(item).is_some() || work::revision_diagnostic_source(item).is_some()
    })
        && let Some(plan) = team
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.execution_plan.as_ref())
    {
        crate::kars::task::execution_plan::validate_retained_input_execution(Some(plan), 1)
            .map_err(AppError::BadRequest)?;
    }
    Ok(())
}

fn validate_rejection_feedback(
    team: &crate::kars::team::KarsTeam,
    item: &TeamTaskDto,
    feedback: Option<&str>,
) -> AppResult<()> {
    let feedback = feedback
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("requesting changes requires written feedback".into())
        })?;
    let mut next = item.clone();
    next.feedback = Some(feedback.to_owned());
    validate_work_intent(team, &next)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmendFeedbackRequest {
    pub team_uid: String,
    pub expected: TeamTaskDto,
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    pub feedback: Option<String>,
}

fn require_feedback_team(team: &crate::kars::team::KarsTeam, expected_uid: &str) -> AppResult<()> {
    work_owner(team)?;
    if expected_uid.is_empty() || team.metadata.uid.as_deref() != Some(expected_uid) {
        return Err(AppError::Conflict(
            "Team identity changed; refresh before editing feedback".into(),
        ));
    }
    if !team.spec.paused {
        return Err(AppError::Conflict(
            "Pause the Team before editing queued feedback".into(),
        ));
    }
    Ok(())
}

/// PATCH only next-run feedback; never rewrite the prior checkpoint decision.
pub async fn amend_team_task_feedback(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, task_id)): Path<(String, String, String)>,
    Json(body): Json<AmendFeedbackRequest>,
) -> AppResult<Json<TeamTaskDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    require_feedback_team(&team, &body.team_uid)?;
    if body.expected.id != task_id {
        return Err(AppError::BadRequest(
            "expected item ID must match the route".into(),
        ));
    }
    let mut proposed = body.expected.clone();
    proposed.feedback = body.feedback.clone();
    validate_work_intent(&team, &proposed)?;
    let owner = work_owner(&team)?;
    let api = cluster.configmaps(&ns);
    let (cm, _) = work::read(&api, &owner)
        .await
        .map_err(work_error)?
        .ok_or(AppError::NotFound)?;
    let updated = work::update_guarded(
        &api,
        &owner,
        cm.metadata.uid.as_deref(),
        false,
        |items| work::amend_pending_feedback(items, &body.expected, body.feedback.clone()),
        || async {
            let fresh = require_owned_team(cluster, &ns, &name, &principal)
                .await
                .map_err(|error| work::Error::Conflict(error.to_string()))?;
            require_feedback_team(&fresh, &body.team_uid)
                .map_err(|error| work::Error::Conflict(error.to_string()))?;
            if fresh.metadata.resource_version != team.metadata.resource_version {
                return Err(work::Error::Conflict(
                    "Team changed while editing; refresh before retrying".into(),
                ));
            }
            Ok(())
        },
    )
    .await
    .map_err(work_error)?;
    Ok(Json(updated))
}

/// The generic Inbox must validate the same exact next-run intent as Team review.
pub(crate) async fn preflight_checkpoint_rejection(
    cluster: &crate::kars::cluster::Cluster,
    ns: &str,
    approval: &crate::kars::approval::KarsApproval,
    feedback: Option<&str>,
) -> AppResult<()> {
    if approval.spec.action.kind != "checkpoint" {
        return Ok(());
    }
    let Some(raw) = approval.annotations().get(work::ANNOTATION) else {
        // Milestone labels without a binding cannot safely identify queued work.
        if approval.labels().contains_key("kars.azure.com/milestone")
            || approval
                .annotations()
                .contains_key("kars.azure.com/milestone")
        {
            return Err(AppError::Conflict(
                "Team checkpoint has no work binding; refresh before deciding".into(),
            ));
        }
        return Ok(());
    };
    let binding: work::Binding = serde_json::from_str(raw)
        .map_err(|_| AppError::Conflict("invalid Team checkpoint work binding".into()))?;
    let owner_ref = approval
        .metadata
        .owner_references
        .as_ref()
        .and_then(|owners| {
            owners.iter().find(|owner| {
                owner.controller == Some(true)
                    && owner.kind == "KarsTeam"
                    && owner.api_version == "kars.azure.com/v1alpha1"
            })
        })
        .ok_or_else(|| AppError::Conflict("Team checkpoint has no Team owner".into()))?;
    let team = cluster
        .teams(ns)
        .get(&owner_ref.name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?;
    let owner = work_owner(&team)?;
    let (cm, items) = work::read(&cluster.configmaps(ns), &owner)
        .await
        .map_err(work_error)?
        .ok_or(AppError::NotFound)?;
    let item = items
        .iter()
        .find(|item| item.id == binding.item_id)
        .ok_or_else(|| AppError::Conflict("reviewed work disappeared".into()))?;
    if cm.uid().as_deref() != Some(binding.backlog_uid.as_str())
        || work::request_digest(item).map_err(|error| AppError::Upstream(error.to_string()))?
            != binding.request_digest
        || item.status != "awaiting_review"
        || !checkpoint_matches(approval, &team, item, raw)
        || !review_ready(approval, item)
    {
        return Err(AppError::Conflict(
            "Team checkpoint or work changed; refresh before deciding".into(),
        ));
    }
    validate_rejection_feedback(&team, item, feedback)
}

#[derive(Debug, Serialize)]
pub struct WorkReviewDto {
    pub task_id: String,
    pub assignment_nonce: String,
    pub ready: bool,
    pub decision_pending: bool,
}

fn checkpoint_matches(
    approval: &crate::kars::approval::KarsApproval,
    team: &crate::kars::team::KarsTeam,
    item: &TeamTaskDto,
    encoded_binding: &str,
) -> bool {
    approval.spec.action.kind == "checkpoint"
        && super::lifecycle::owned_run(&approval.metadata, team)
        && approval.metadata.deletion_timestamp.is_none()
        && item.run.as_deref() == Some(approval.spec.task_ref.name.as_str())
        && approval
            .annotations()
            .get(work::ANNOTATION)
            .map(String::as_str)
            == Some(encoded_binding)
        && approval.annotations().get(work::TASK_UID) == item.run_uid.as_ref()
        && approval.annotations().get(work::RUN_NONCE) == item.assignment_nonce.as_ref()
}

fn review_ready(approval: &crate::kars::approval::KarsApproval, item: &TeamTaskDto) -> bool {
    approval.spec.decision.is_none()
        && approval.status.as_ref().is_some_and(|status| {
            status.phase.as_deref() == Some("Pending")
                && status.bound_task_uid == item.run_uid
                && status
                    .bound_request
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                    == Some(serde_json::json!({
                        "taskRef": approval.spec.task_ref,
                        "action": approval.spec.action,
                        "ttl": approval.spec.ttl,
                    }))
                && status.expires_at.as_deref().is_some_and(|expiry| {
                    chrono::DateTime::parse_from_rfc3339(expiry)
                        .is_ok_and(|expiry| expiry > chrono::Utc::now())
                })
        })
}

pub(crate) async fn read_work_for_detail(
    cluster: &crate::kars::cluster::Cluster,
    team: &crate::kars::team::KarsTeam,
) -> AppResult<(Vec<TeamTaskDto>, Vec<WorkReviewDto>)> {
    let owner = work_owner(team)?;
    let Some((cm, items)) = work::read(&cluster.configmaps(&owner.namespace), &owner)
        .await
        .map_err(work_error)?
    else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut reviews = Vec::new();
    for item in items.iter().filter(|item| item.status == "awaiting_review") {
        let binding = work::Binding {
            backlog_uid: cm
                .uid()
                .ok_or_else(|| AppError::Conflict("backlog has no UID".into()))?,
            item_id: item.id.clone(),
            request_digest: work::request_digest(item)
                .map_err(|error| AppError::Upstream(error.to_string()))?,
        };
        let name = work::approval_name(&owner, &binding, item).map_err(work_error)?;
        let encoded = serde_json::to_string(&binding)
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        let approval = cluster
            .approvals(&owner.namespace)
            .get_opt(&name)
            .await
            .map_err(|error| AppError::Upstream(error.to_string()))?;
        if approval
            .as_ref()
            .is_some_and(|approval| !checkpoint_matches(approval, team, item, &encoded))
        {
            return Err(AppError::Conflict(
                "work checkpoint binding changed; review is unavailable".into(),
            ));
        }
        reviews.push(WorkReviewDto {
            task_id: item.id.clone(),
            assignment_nonce: item
                .assignment_nonce
                .clone()
                .ok_or_else(|| AppError::Conflict("work has no run nonce".into()))?,
            ready: approval
                .as_ref()
                .is_some_and(|approval| review_ready(approval, item)),
            decision_pending: approval.as_ref().is_some_and(|approval| {
                approval.spec.decision.is_some()
                    || approval.status.as_ref().is_some_and(|status| {
                        matches!(
                            status.phase.as_deref(),
                            Some("Approved" | "Denied" | "Expired" | "Stale")
                        )
                    })
            }),
        });
    }
    Ok((items, reviews))
}

#[derive(Debug, Default, Deserialize)]
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

pub(crate) fn work_owner(team: &crate::kars::team::KarsTeam) -> AppResult<work::Owner> {
    if team.metadata.deletion_timestamp.is_some() {
        return Err(AppError::Conflict("Team is being deleted".into()));
    }
    Ok(work::Owner {
        name: team.name_any(),
        namespace: team
            .namespace()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::Conflict("Team has no namespace".into()))?,
        uid: team
            .uid()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::Conflict("Team has no UID".into()))?,
    })
}

pub(crate) fn work_error(error: work::Error) -> AppError {
    match error {
        work::Error::Conflict(message) => AppError::Conflict(message),
        other => AppError::Upstream(other.to_string()),
    }
}

pub(crate) async fn read_tasks(
    cluster: &crate::kars::cluster::Cluster,
    team: &crate::kars::team::KarsTeam,
) -> AppResult<Vec<TeamTaskDto>> {
    let owner = work_owner(team)?;
    Ok(work::read(&cluster.configmaps(&owner.namespace), &owner)
        .await
        .map_err(work_error)?
        .map(|(_, items)| items)
        .unwrap_or_default())
}

/// `GET /api/namespaces/:ns/teams/:name/tasks` — the team's task backlog.
pub async fn list_team_tasks(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<Vec<TeamTaskDto>>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    Ok(Json(read_tasks(cluster, &team).await?))
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
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    if b.title.trim().is_empty() {
        return Err(AppError::BadRequest("task title is required".into()));
    }
    if b.acceptance_criteria.len() > 20 {
        return Err(AppError::BadRequest(format!(
            "acceptance criteria actual: {}, allowed: 20; reduce the criteria count",
            b.acceptance_criteria.len(),
        )));
    }
    let existing_tasks = read_tasks(cluster, &team).await?;
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
            .collect(),
        review_required: b.review_required,
        status: "pending".into(),
        run: None,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
        ..Default::default()
    };
    validate_work_intent(&team, &task)?;
    let owner = work_owner(&team)?;
    work::update(&cluster.configmaps(&ns), &owner, None, true, |items| {
        if items.iter().any(|existing| existing.id == task.id) {
            return Err(work::Error::Conflict(format!(
                "task id '{}' already exists",
                task.id
            )));
        }
        if let Some(dependency) = task
            .depends_on
            .iter()
            .find(|id| !items.iter().any(|item| &item.id == *id))
        {
            return Err(work::Error::Conflict(format!(
                "task dependency '{dependency}' disappeared"
            )));
        }
        items.push(task.clone());
        Ok(())
    })
    .await
    .map_err(work_error)?;
    Ok(Json(task))
}

/// `DELETE /api/namespaces/:ns/teams/:name/tasks/:task_id` — remove a task from
/// the backlog before it has been assigned. Attempt history remains durable.
pub async fn delete_team_task(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, task_id)): Path<(String, String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let owner = work_owner(&team)?;
    let api = cluster.configmaps(&ns);
    let (cm, _) = work::read(&api, &owner)
        .await
        .map_err(work_error)?
        .ok_or(AppError::NotFound)?;
    work::update(&api, &owner, cm.metadata.uid.as_deref(), false, |items| {
        let item = items
            .iter()
            .find(|item| item.id == task_id)
            .ok_or_else(|| work::Error::Conflict("task no longer exists".into()))?;
        if item.status != "pending" || !item.history.is_empty() {
            return Err(work::Error::Conflict(
                "assigned work and its attempt history cannot be removed".into(),
            ));
        }
        if items.iter().any(|item| item.depends_on.contains(&task_id)) {
            return Err(work::Error::Conflict(
                "dependent work still references this task".into(),
            ));
        }
        items.retain(|item| item.id != task_id);
        Ok(())
    })
    .await
    .map_err(work_error)?;

    Ok(Json(serde_json::json!({ "removed": true })))
}

#[derive(Debug, Deserialize)]
pub struct ReviewTeamTaskRequest {
    pub decision: String,
    pub feedback: Option<String>,
    pub assignment_nonce: String,
    pub run_uid: String,
}

pub async fn review_team_task(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, task_id)): Path<(String, String, String)>,
    Json(body): Json<ReviewTeamTaskRequest>,
) -> AppResult<Json<TeamTaskDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    if !matches!(body.decision.as_str(), "approve" | "request_changes") {
        return Err(AppError::BadRequest(
            "decision must be approve or request_changes".into(),
        ));
    }
    let owner = work_owner(&team)?;
    let (cm, current) = work::read(&cluster.configmaps(&ns), &owner)
        .await
        .map_err(work_error)?
        .ok_or(AppError::NotFound)?;
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
    if existing.assignment_nonce.as_deref() != Some(body.assignment_nonce.as_str())
        || existing.run_uid.as_deref() != Some(body.run_uid.as_str())
    {
        return Err(AppError::Conflict(
            "work revision changed since it was displayed; refresh before reviewing".into(),
        ));
    }
    let feedback = body
        .feedback
        .as_deref()
        .filter(|feedback| !feedback.trim().is_empty())
        .map(str::to_string);
    if body.decision == "request_changes" {
        validate_rejection_feedback(&team, &existing, feedback.as_deref())?;
    } else if let Some(text) = &feedback {
        let actual = text.chars().count();
        let allowed = work::MAX_OBJECTIVE_CHARS;
        if actual > allowed {
            return Err(AppError::BadRequest(format!(
                "review feedback actual: {actual} characters, allowed: {allowed}; shorten the feedback"
            )));
        }
    }
    let approvals = cluster.approvals(&ns);
    let binding = work::Binding {
        backlog_uid: cm
            .uid()
            .ok_or_else(|| AppError::Conflict("backlog has no UID".into()))?,
        item_id: existing.id.clone(),
        request_digest: work::request_digest(&existing)
            .map_err(|error| AppError::Upstream(error.to_string()))?,
    };
    let approval_name = work::approval_name(&owner, &binding, &existing).map_err(work_error)?;
    let encoded_binding =
        serde_json::to_string(&binding).map_err(|error| AppError::Upstream(error.to_string()))?;
    let approval = approvals
        .get_opt(&approval_name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
        .filter(|approval| {
            checkpoint_matches(approval, &team, &existing, &encoded_binding)
                && review_ready(approval, &existing)
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
                "metadata": {
                    "uid": approval.uid().ok_or_else(|| AppError::Conflict("approval has no UID".into()))?,
                    "resourceVersion": approval.resource_version().ok_or_else(|| AppError::Conflict("approval has no resourceVersion".into()))?,
                },
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
        .map_err(|error| match error {
            kube::Error::Api(response) if matches!(response.code, 409 | 422) => {
                AppError::Conflict("checkpoint changed while deciding; refresh before retrying".into())
            }
            other => AppError::Upstream(other.to_string()),
        })?;

    let updated = read_tasks(cluster, &team)
        .await?
        .into_iter()
        .find(|task| task.id == task_id)
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated))
}

#[cfg(test)]
#[path = "feedback_tests.rs"]
mod feedback_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (
        crate::kars::team::KarsTeam,
        TeamTaskDto,
        crate::kars::approval::KarsApproval,
    ) {
        let team = serde_json::from_value(json!({
            "metadata":{"name":"eng","namespace":"work","uid":"team-uid"},
            "spec":{"charter":"Review work","envelope":{"tier":2,"authorityCeiling":2}}
        }))
        .unwrap();
        let item = TeamTaskDto {
            id: "checklist".into(),
            title: "Checklist".into(),
            status: "awaiting_review".into(),
            run: Some("run".into()),
            run_uid: Some("run-uid".into()),
            assignment_nonce: Some("nonce".into()),
            review_required: true,
            ..Default::default()
        };
        let mut raw = json!({
            "metadata":{"name":"checkpoint","namespace":"work","uid":"approval-uid","resourceVersion":"1",
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam","name":"eng","uid":"team-uid","controller":true}],
                "annotations":{(work::ANNOTATION):"binding",(work::TASK_UID):"run-uid",(work::RUN_NONCE):"nonce"}},
            "spec":{"taskRef":{"name":"run"},"action":{"kind":"checkpoint","summary":"Review result"},"ttl":"P7D"},
            "status":{"phase":"Pending","boundTaskUid":"run-uid",
                "expiresAt":(chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()}
        });
        raw["status"]["boundRequest"] = json!(
            r#"{"taskRef":{"name":"run"},"action":{"kind":"checkpoint","summary":"Review result"},"ttl":"P7D"}"#
        );
        (team, item, serde_json::from_value(raw).unwrap())
    }

    #[test]
    fn checkpoint_review_requires_exact_team_work_run_and_request_binding() {
        let (team, item, approval) = fixture();
        assert!(checkpoint_matches(&approval, &team, &item, "binding"));
        assert!(review_ready(&approval, &item));
        let raw = serde_json::to_value(approval).unwrap();
        for (path, value) in [
            ("/metadata/namespace", json!("elsewhere")),
            ("/metadata/ownerReferences/0/uid", json!("other-team")),
            (
                "/metadata/annotations/kars.azure.com~1team-work",
                json!("other-work"),
            ),
            (
                "/metadata/annotations/kars.azure.com~1team-work-task-uid",
                json!("replacement"),
            ),
            (
                "/metadata/annotations/kars.azure.com~1team-work-run-nonce",
                json!("older"),
            ),
            ("/spec/taskRef/name", json!("other-run")),
            ("/spec/action/kind", json!("egress")),
        ] {
            let mut altered = raw.clone();
            *altered.pointer_mut(path).unwrap() = value;
            assert!(
                !checkpoint_matches(
                    &serde_json::from_value(altered).unwrap(),
                    &team,
                    &item,
                    "binding"
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn stale_unbound_expired_and_submitted_decisions_are_not_actionable_after_reload() {
        let (_, item, approval) = fixture();
        let raw = serde_json::to_value(approval).unwrap();
        for (path, value) in [
            ("/status/phase", json!("Approved")),
            ("/status/boundTaskUid", json!("other")),
            ("/status/boundRequest", json!("previous request")),
            ("/status/expiresAt", json!("2020-01-01T00:00:00Z")),
            ("/status/expiresAt", json!("bad timestamp")),
            ("/status/expiresAt", json!(null)),
            ("/spec/action/summary", json!("Different request")),
        ] {
            let mut altered = raw.clone();
            *altered.pointer_mut(path).unwrap() = value;
            assert!(
                !review_ready(&serde_json::from_value(altered).unwrap(), &item),
                "{path}"
            );
        }
        let mut submitted = raw;
        submitted["spec"]["decision"] = json!({"verdict":"approve","decider":"Reviewer"});
        assert!(!review_ready(
            &serde_json::from_value(submitted).unwrap(),
            &item
        ));
    }

    #[test]
    fn checkpoint_request_allows_only_json_ordering_not_changed_or_extra_fields() {
        let (_, item, mut approval) = fixture();
        for raw in [
            r#"{"ttl":"P7D","action":{"summary":"Review result","kind":"checkpoint"},"taskRef":{"name":"run"}}"#,
            r#"{ "taskRef": {"name":"run"}, "action": {"kind":"checkpoint","summary":"Review result"}, "ttl":"P7D" }"#,
        ] {
            approval.status.as_mut().unwrap().bound_request = Some(raw.into());
            assert!(review_ready(&approval, &item));
        }
        for raw in [
            r#"{"taskRef":{"name":"other"},"action":{"kind":"checkpoint","summary":"Review result"},"ttl":"P7D"}"#,
            r#"{"taskRef":{"name":"run"},"action":{"kind":"checkpoint","summary":"Review result","detail":"new"},"ttl":"P7D"}"#,
            r#"{"taskRef":{"name":"run"},"action":{"kind":"checkpoint","summary":"Review result"},"ttl":"P8D"}"#,
            r#"{"taskRef":{"name":"run"},"action":{"kind":"checkpoint","summary":"Review result"},"ttl":"P7D","extra":null}"#,
        ] {
            approval.status.as_mut().unwrap().bound_request = Some(raw.into());
            assert!(!review_ready(&approval, &item), "{raw}");
        }
    }
}
