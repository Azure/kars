// kars Bridge BFF — endpoints helpers for engineering intake.

use axum::Json;
use axum::extract::{Extension, Path, State};
use chrono::Utc;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::providers::signing::sha256;
use crate::routes::github::connection_config_map_name;
use crate::routes::tasks::require_cluster;
use crate::routes::teams::{read_task_list, require_owned_team};
use crate::state::AppState;

use super::config::{
    initial_jitter_seconds, parse_source, source_annotations, source_data, sync_claim_active,
    to_dto, validate_request, verify_source_owner,
};
use super::queue::{merge_into_backlog, request_team_run};
use super::synchronization::synchronize_source;
use super::{
    EngineeringCursor, EngineeringReviewDecisionRequest, EngineeringSourceConfig,
    EngineeringSourceDto, EngineeringSourceStatus, EngineeringSyncState,
    PutEngineeringSourceRequest, TeamTaskDto, source_config_map_name,
};

/// `GET /api/namespaces/:ns/teams/:name/engineering-source`.
pub async fn get_source(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let source_name = source_config_map_name(&ns, &name);
    let Some(cm) = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
    else {
        return Ok(Json(to_dto(
            false,
            None,
            EngineeringSourceStatus::default(),
        )));
    };
    let (config, _cursor, status) =
        parse_source(&cm).map_err(|e| AppError::Upstream(e.to_string()))?;
    if config.team_namespace != ns || config.team_name != name {
        return Err(AppError::NotFound);
    }
    if !verify_source_owner(&cm, &config, &principal.sub) {
        return Ok(Json(to_dto(
            false,
            None,
            EngineeringSourceStatus::default(),
        )));
    }
    Ok(Json(to_dto(true, Some(&config), status)))
}

/// `PUT /api/namespaces/:ns/teams/:name/engineering-source`.
pub async fn put_source(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(request): Json<PutEngineeringSourceRequest>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let connection_ref = connection_config_map_name(&principal.sub);
    let (_installation_id, _account, granted_repos) = cluster
        .read_github_connection_result(&ns, &connection_ref)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or_else(|| {
            AppError::Rejected(
                "connect GitHub for your user before configuring engineering intake".into(),
            )
        })?;
    let (repos, signals) = validate_request(&request, &granted_repos)?;

    let source_name = source_config_map_name(&ns, &name);
    let existing = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let (cursor, mut status, previous_config) = if let Some(cm) = existing.as_ref() {
        let (config, cursor, status) =
            parse_source(cm).map_err(|e| AppError::Upstream(e.to_string()))?;
        if config.team_namespace != ns || config.team_name != name {
            return Err(AppError::NotFound);
        }
        if sync_claim_active(&status, Utc::now()) {
            return Err(AppError::Conflict(
                "engineering intake is syncing; retry the configuration change shortly".into(),
            ));
        }
        if verify_source_owner(cm, &config, &principal.sub) {
            (cursor, status, Some(config))
        } else {
            (
                EngineeringCursor::default(),
                EngineeringSourceStatus::default(),
                None,
            )
        }
    } else {
        (
            EngineeringCursor::default(),
            EngineeringSourceStatus::default(),
            None,
        )
    };

    let config = EngineeringSourceConfig {
        version: 1,
        team_namespace: ns,
        team_name: name,
        owner_sub: principal.sub,
        connection_config_map_ref: connection_ref,
        enabled: request.enabled,
        auto_run: request.auto_run,
        repos,
        signals,
        poll_interval_seconds: request.poll_interval_seconds,
    };
    if config.enabled {
        let changed = previous_config.as_ref().is_none_or(|previous| {
            !previous.enabled
                || previous.repos != config.repos
                || previous.signals != config.signals
                || previous.auto_run != config.auto_run
                || previous.poll_interval_seconds != config.poll_interval_seconds
        });
        if changed || status.next_poll_at.is_none() {
            status.state = EngineeringSyncState::Idle;
            status.next_poll_at = Some(
                (Utc::now() + chrono::Duration::seconds(initial_jitter_seconds(&source_name)))
                    .to_rfc3339(),
            );
        }
    } else {
        status.state = EngineeringSyncState::Disabled;
        status.next_poll_at = None;
    }
    let data = source_data(&config, &cursor, &status)?;
    let annotations = source_annotations(&config);
    if let Some(current) = existing {
        cluster
            .replace_engineering_source(current, &annotations, &data)
            .await
            .map_err(|error| {
                if matches!(error, kube::Error::Api(ref response) if response.code == 409) {
                    AppError::Conflict(
                        "engineering intake changed concurrently; reload and retry".into(),
                    )
                } else {
                    AppError::Upstream(error.to_string())
                }
            })?;
    } else {
        cluster
            .create_engineering_source(&source_name, &annotations, &data)
            .await
            .map_err(|error| {
                if matches!(error, kube::Error::Api(ref response) if response.code == 409) {
                    AppError::Conflict(
                        "engineering intake was configured concurrently; reload and retry".into(),
                    )
                } else {
                    AppError::Upstream(error.to_string())
                }
            })?;
    }
    Ok(Json(to_dto(true, Some(&config), status)))
}

/// `POST /api/namespaces/:ns/teams/:name/engineering-source/sync`.
pub async fn sync_now(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let source_name = source_config_map_name(&ns, &name);
    let cm = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
        .ok_or_else(|| AppError::Rejected("configure engineering intake first".into()))?;
    let (config, cursor, status) =
        parse_source(&cm).map_err(|e| AppError::Upstream(e.to_string()))?;
    if config.team_namespace != ns
        || config.team_name != name
        || !verify_source_owner(&cm, &config, &principal.sub)
    {
        return Err(AppError::NotFound);
    }
    if !config.enabled {
        return Err(AppError::Rejected(
            "enable engineering intake before syncing".into(),
        ));
    }
    let status = synchronize_source(cluster, &config, cursor, status).await?;
    Ok(Json(to_dto(true, Some(&config), status)))
}

/// `DELETE /api/namespaces/:ns/teams/:name/engineering-source`.
pub async fn delete_source(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<EngineeringSourceDto>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let source_name = source_config_map_name(&ns, &name);
    if let Some(cm) = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?
    {
        let (config, _, status) =
            parse_source(&cm).map_err(|e| AppError::Upstream(e.to_string()))?;
        if config.team_namespace != ns || config.team_name != name {
            return Err(AppError::NotFound);
        }
        if sync_claim_active(&status, Utc::now()) {
            return Err(AppError::Conflict(
                "engineering intake is syncing; retry disconnect shortly".into(),
            ));
        }
        let resource_version = cm
            .metadata
            .resource_version
            .clone()
            .ok_or_else(|| AppError::Conflict("engineering source has no version".into()))?;

        cluster
            .delete_engineering_source_if_version(&source_name, resource_version)
            .await
            .map_err(|error| {
                if matches!(error, kube::Error::Api(ref response) if response.code == 409) {
                    AppError::Conflict(
                        "engineering intake changed concurrently; reload and retry".into(),
                    )
                } else {
                    AppError::Upstream(error.to_string())
                }
            })?;
    }
    Ok(Json(to_dto(
        false,
        None,
        EngineeringSourceStatus::default(),
    )))
}

/// Turn a human PR decision into durable standing-team work. This preserves the
/// same source → backlog → run chain instead of trying to mutate a retired run.
pub async fn decide_review_item(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(request): Json<EngineeringReviewDecisionRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    require_owned_team(cluster, &ns, &name, &principal).await?;
    let decision = request.decision.trim();
    if decision != "request_changes" {
        return Err(AppError::BadRequest(
            "only request_changes is supported; merge remains a human GitHub action until a typed single-use merge grant exists".into(),
        ));
    }
    let comment = request
        .comment
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .chars()
        .take(1500)
        .collect::<String>();
    if decision == "request_changes" && comment.is_empty() {
        return Err(AppError::BadRequest(
            "request_changes requires concrete feedback".into(),
        ));
    }

    let source_name = source_config_map_name(&ns, &name);
    let source = cluster
        .read_engineering_source(&source_name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
        .ok_or_else(|| AppError::Rejected("configure engineering intake first".into()))?;
    let (config, _, status) =
        parse_source(&source).map_err(|error| AppError::Upstream(error.to_string()))?;
    if !verify_source_owner(&source, &config, &principal.sub)
        || !config
            .repos
            .iter()
            .any(|repo| repo.eq_ignore_ascii_case(&request.repo))
    {
        return Err(AppError::NotFound);
    }
    let _item = status
        .review_items
        .iter()
        .find(|item| {
            item.repo.eq_ignore_ascii_case(&request.repo)
                && item.pr_number == request.pr_number
                && item.head_sha == request.head_sha
                && item.run == request.run
        })
        .ok_or_else(|| {
            AppError::Conflict(
                "the PR changed since this card was rendered; sync before deciding".into(),
            )
        })?;
    let identity = format!(
        "engineering-review:{decision}:{}:{}:{}:{comment}",
        request.repo.to_ascii_lowercase(),
        request.pr_number,
        request.head_sha
    );
    let digest = sha256(identity.as_bytes());
    let task = TeamTaskDto {
        id: format!("github-pr-feedback-{}", hex::encode(&digest[..10])),
        title: format!(
            "[Review feedback] Revise {} PR #{}",
            request.repo, request.pr_number
        ),
        description: format!(
            "PR: {}\nREVIEWED SHA: {}\nSOURCE RUN: {}\n\nREQUESTED CHANGES:\n{}\n\nA human reviewed the team's PR and requested changes. Re-open the exact prior evidence, apply only the requested delta, run relevant tests, push a new commit, and wait for GitHub checks. Never merge and never reuse stale green evidence.",
            request.pr_url, request.head_sha, request.run, comment
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(Utc::now().to_rfc3339()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    };
    let task_id = task.id.clone();
    let queued = merge_into_backlog(cluster, &name, vec![task])
        .await
        .map_err(AppError::Upstream)?;
    let pending = read_task_list(&cluster.read_team_tasks(&name).await)
        .iter()
        .any(|task| task.id == task_id && task.status == "pending");
    let run_requested = if pending {
        request_team_run(cluster, &ns, &name)
            .await
            .map_err(AppError::Upstream)?
    } else {
        false
    };
    Ok(Json(serde_json::json!({
        "queued": queued > 0,
        "run_requested": run_requested,
        "decision": decision,
        "team": name,
    })))
}
