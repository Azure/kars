// kars Bridge BFF — synchronization helpers for engineering intake.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::Utc;
use kube::ResourceExt;

use crate::error::{AppError, AppResult};
use crate::kars::cluster::Cluster;
use crate::routes::github::{
    authorize_repo_set, connection_config_map_name, installation_token, mint_app_jwt,
};
use crate::routes::teams::read_task_list;
use crate::state::AppState;

use super::config::{is_due, next_poll_at, parse_source, sync_claim_active, verify_source_owner};
use super::github::{
    list_code_scanning_alerts, list_dependabot_alerts, list_open_pulls,
    list_secret_scanning_alerts, repository_features, signal_result, truncate_error,
    unavailable_security_product,
};
use super::intake::{
    backlog_task, code_scanning_task, dependabot_alert_task, is_dependabot_pr,
    legacy_alert_retirement, open_pull_may_address_dependabot_alert, secret_scanning_task,
};
use super::queue::{append_bounded_tasks, ensure_auto_run_for_backlog, merge_into_backlog};
use super::remediation::{
    description_matches_remediation, match_remediation_task, note_candidate_pulls,
};
use super::review::{collect_review_items, dedupe_followup_task};
use super::{
    CONFIG_KEY, CURSOR_KEY, EngineeringCursor, EngineeringReviewState, EngineeringSignal,
    EngineeringSignalSyncState, EngineeringSourceConfig, EngineeringSourceStatus,
    EngineeringSyncState, MAX_ALERTS_PER_SIGNAL, MAX_GITHUB_PAGES, MAX_ITEMS_PER_SYNC,
    MAX_OPEN_PRS_PER_REPO, MAX_SOURCES_PER_SWEEP, STATUS_KEY, SyncOutcome, source_config_map_name,
};

async fn perform_sync(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
    mut cursor: EngineeringCursor,
    claim_id: &str,
) -> Result<SyncOutcome, String> {
    let expected_connection = connection_config_map_name(&config.owner_sub);
    if config.connection_config_map_ref != expected_connection {
        return Err("source connection reference does not match its owner".into());
    }

    let team = cluster
        .teams(&config.team_namespace)
        .get_opt(&config.team_name)
        .await
        .map_err(|e| format!("reading the standing team failed: {e}"))?
        .ok_or_else(|| "the standing team no longer exists".to_string())?;
    if team
        .annotations()
        .get("kars.azure.com/owner-sub")
        .is_none_or(|owner| owner != &config.owner_sub)
    {
        return Err("the engineering source owner no longer owns this team".into());
    }

    let (installation_id, _account, granted_repos) = cluster
        .read_github_connection_result(&config.team_namespace, &config.connection_config_map_ref)
        .await
        .map_err(|e| format!("reading the GitHub connection failed: {e}"))?
        .ok_or_else(|| "the owner's GitHub connection is no longer available".to_string())?;
    authorize_repo_set(&config.repos, &granted_repos)
        .map_err(|e| format!("repository authorization changed: {e}"))?;

    let (app_id, private_key) = cluster
        .github_app_creds()
        .await
        .map_err(|error| format!("GitHub credential authority unavailable: {error}"))?
        .ok_or_else(|| "the shared GitHub App is not configured".to_string())?;
    let app_jwt = mint_app_jwt(&app_id, &private_key).map_err(|e| e.to_string())?;
    let token = installation_token(&app_jwt, &installation_id)
        .await
        .map_err(|e| e.to_string())?;

    let now = Utc::now().to_rfc3339();
    let mut tasks = Vec::new();
    let mut errors = Vec::new();
    let mut completed_attempts = 0;
    let mut signal_results = Vec::new();
    let client = reqwest::Client::new();
    let existing_backlog = read_task_list(&cluster.read_team_tasks(&config.team_name).await);
    let mut known_tasks = existing_backlog
        .iter()
        .map(|task| {
            (
                task.id.clone(),
                (task.status.clone(), task.description.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let attempt_count = config
        .repos
        .len()
        .saturating_mul(config.signals.len())
        .max(1);
    let attempt_cap = (MAX_ITEMS_PER_SYNC / attempt_count).max(1);
    let mut queued_slots_used = 0;
    for repo in &config.repos {
        let features = repository_features(&client, &token, repo).await;
        let open_pull_coverage = if config.signals.contains(&EngineeringSignal::DependabotAlert) {
            list_open_pulls(&client, &token, repo)
                .await
                .map(|pulls| pulls.items)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut dedupe_seen = BTreeSet::new();
        for signal in config.signals.iter().copied() {
            let (result, api_truncated, queue_truncated) = match signal {
                EngineeringSignal::DependabotPr => {
                    match list_open_pulls(&client, &token, repo).await {
                        Ok(pulls) => {
                            completed_attempts += 1;
                            if let Some(updated_at) =
                                pulls.items.iter().map(|pr| pr.updated_at.as_str()).max()
                            {
                                cursor
                                    .repository_updated_at
                                    .insert(repo.clone(), updated_at.to_string());
                            }
                            let signal_tasks = pulls
                                .items
                                .iter()
                                .filter(|pr| is_dependabot_pr(pr))
                                .map(|pr| backlog_task(repo, pr, &now))
                                .collect::<Vec<_>>();
                            let discovered = signal_tasks.len();
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), pulls.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
                EngineeringSignal::DependabotAlert => {
                    match list_dependabot_alerts(&client, &token, repo).await {
                        Ok(alerts) => {
                            completed_attempts += 1;
                            let discovered = alerts.items.len();
                            let mut signal_tasks = Vec::new();
                            for alert in &alerts.items {
                                let mut task = dependabot_alert_task(repo, alert, &now);
                                let (matching_id, identity_warning) =
                                    match_remediation_task(&mut task, |id| {
                                        known_tasks
                                            .get(id)
                                            .map(|(_, description)| description.as_str())
                                    });
                                if let Some(warning) = identity_warning {
                                    errors.push(warning);
                                }
                                let legacy_ids = known_tasks
                                    .iter()
                                    .filter(|(id, (status, description))| {
                                        id.starts_with("dependabot-alert-")
                                            && status == "pending"
                                            && description_matches_remediation(
                                                description,
                                                repo,
                                                alert.dependency.manifest_path.as_deref(),
                                                &alert.dependency.package.name,
                                            )
                                    })
                                    .map(|(id, _)| id.clone())
                                    .collect::<Vec<_>>();
                                for legacy_id in legacy_ids {
                                    let retirement =
                                        legacy_alert_retirement(&legacy_id, &matching_id, &now);
                                    known_tasks.insert(
                                        legacy_id,
                                        ("done".into(), retirement.description.clone()),
                                    );
                                    tasks.push(retirement);
                                }
                                let covering_pulls = open_pull_coverage
                                    .iter()
                                    .filter(|pull| {
                                        open_pull_may_address_dependabot_alert(pull, alert)
                                    })
                                    .collect::<Vec<_>>();
                                if dedupe_seen.insert(matching_id.clone())
                                    && let Some(dedupe) = dedupe_followup_task(
                                        repo,
                                        &matching_id,
                                        &covering_pulls,
                                        &now,
                                    )
                                {
                                    tasks.push(dedupe);
                                }
                                note_candidate_pulls(&mut task, &covering_pulls);
                                signal_tasks.push(task);
                            }
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), alerts.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
                EngineeringSignal::CodeScanningAlert => {
                    match list_code_scanning_alerts(&client, &token, repo).await {
                        Ok(alerts) => {
                            completed_attempts += 1;
                            let signal_tasks = alerts
                                .items
                                .iter()
                                .map(|alert| code_scanning_task(repo, alert, &now))
                                .collect::<Vec<_>>();
                            let discovered = signal_tasks.len();
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), alerts.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
                EngineeringSignal::SecretScanningAlert => {
                    match list_secret_scanning_alerts(&client, &token, repo).await {
                        Ok(alerts) => {
                            completed_attempts += 1;
                            let signal_tasks = alerts
                                .items
                                .iter()
                                .map(|alert| secret_scanning_task(repo, alert, &now))
                                .collect::<Vec<_>>();
                            let discovered = signal_tasks.len();
                            let bounded = append_bounded_tasks(
                                &mut tasks,
                                &mut known_tasks,
                                signal_tasks,
                                &mut queued_slots_used,
                                attempt_cap,
                            );
                            (Ok(discovered), alerts.truncated, bounded)
                        }
                        Err(error) => (Err(error), false, false),
                    }
                }
            };
            let mut truncation_reasons = Vec::new();
            if api_truncated {
                let item_limit = if signal == EngineeringSignal::DependabotPr {
                    MAX_OPEN_PRS_PER_REPO
                } else {
                    MAX_ALERTS_PER_SIGNAL
                };
                truncation_reasons.push(format!(
                    "GitHub returned more than the per-signal {item_limit}-item or {MAX_GITHUB_PAGES}-page scan cap."
                ));
            }
            if queue_truncated {
                truncation_reasons.push(format!(
                    "The sync found more new work than this source's fair {attempt_cap}-item allocation; remaining items will be retried on later polls."
                ));
            }
            let truncation_detail =
                (!truncation_reasons.is_empty()).then(|| truncation_reasons.join(" "));
            let (result, expected_unavailable) = match result {
                Err(error) => match unavailable_security_product(features.as_ref(), signal, &error)
                {
                    Some(unavailable) => (Err(unavailable), true),
                    None => (Err(error), false),
                },
                Ok(discovered) => (Ok(discovered), false),
            };
            if expected_unavailable {
                completed_attempts += 1;
            }
            let signal_status = signal_result(repo, signal, result, truncation_detail);
            if signal_status.state != EngineeringSignalSyncState::Ok && !expected_unavailable {
                errors.push(format!(
                    "{} {:?}: {}",
                    repo, signal_status.signal, signal_status.detail
                ));
            }
            signal_results.push(signal_status);
        }
    }

    let (review_items, review_followups, review_errors) =
        collect_review_items(cluster, &client, &token, config, &now).await;
    tasks.extend(review_followups);
    errors.extend(review_errors);
    let discovered = tasks.len();
    revalidate_claimed_source(cluster, config, claim_id).await?;
    let queued = merge_into_backlog(cluster, &config.team_name, tasks).await?;
    if let Err(error) = ensure_auto_run_for_backlog(cluster, config).await {
        errors.push(error);
    }
    Ok(SyncOutcome {
        cursor,
        discovered,
        queued,
        completed_attempts,
        errors,
        review_items,
        signal_results,
    })
}

async fn patch_runtime_state(
    cluster: &Cluster,
    name: &str,
    cursor: &EngineeringCursor,
    status: &EngineeringSourceStatus,
) -> AppResult<()> {
    let data = BTreeMap::from([
        (
            CURSOR_KEY.to_string(),
            serde_json::to_string(cursor).map_err(|e| AppError::Internal(e.into()))?,
        ),
        (
            STATUS_KEY.to_string(),
            serde_json::to_string(status).map_err(|e| AppError::Internal(e.into()))?,
        ),
    ]);
    cluster
        .patch_engineering_source_data(name, &data)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))
}

async fn finalize_source_claim(
    cluster: &Cluster,
    name: &str,
    claimed_status: &str,
    cursor: &EngineeringCursor,
    status: &EngineeringSourceStatus,
) -> AppResult<()> {
    let cursor = serde_json::to_string(cursor).map_err(|e| AppError::Internal(e.into()))?;
    let status = serde_json::to_string(status).map_err(|e| AppError::Internal(e.into()))?;
    let completed = cluster
        .complete_engineering_source_claim(name, claimed_status, &cursor, &status)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if !completed {
        return Err(AppError::Conflict(
            "engineering sync lost its claim before completion".into(),
        ));
    }
    Ok(())
}

async fn revalidate_claimed_source(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
    claim_id: &str,
) -> Result<(), String> {
    let name = source_config_map_name(&config.team_namespace, &config.team_name);
    let source = cluster
        .read_engineering_source(&name)
        .await
        .map_err(|error| format!("re-reading engineering source failed: {error}"))?
        .ok_or_else(|| "engineering source was deleted during sync".to_string())?;
    let (current_config, _, current_status) = parse_source(&source)?;
    if &current_config != config
        || !sync_claim_active(&current_status, Utc::now())
        || current_status.sync_claim_id.as_deref() != Some(claim_id)
    {
        return Err("engineering source changed or lost its sync claim before queueing".into());
    }
    Ok(())
}

pub(super) async fn synchronize_source(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
    _cursor: EngineeringCursor,
    _status: EngineeringSourceStatus,
) -> AppResult<EngineeringSourceStatus> {
    let name = source_config_map_name(&config.team_namespace, &config.team_name);
    let current = cluster
        .read_engineering_source(&name)
        .await
        .map_err(|error| AppError::Upstream(error.to_string()))?
        .ok_or_else(|| AppError::Conflict("engineering source no longer exists".into()))?;
    let current_data = current
        .data
        .as_ref()
        .ok_or_else(|| AppError::Conflict("engineering source has no data".into()))?;
    let expected_config = current_data
        .get(CONFIG_KEY)
        .cloned()
        .ok_or_else(|| AppError::Conflict("engineering source config is missing".into()))?;
    let expected_status = current_data
        .get(STATUS_KEY)
        .cloned()
        .unwrap_or_else(|| "{}".into());
    let current_cursor = current_data
        .get(CURSOR_KEY)
        .map(|value| serde_json::from_str::<EngineeringCursor>(value))
        .transpose()
        .map_err(|error| {
            AppError::Conflict(format!("engineering source cursor is invalid: {error}"))
        })?
        .unwrap_or_default();
    let mut status =
        serde_json::from_str::<EngineeringSourceStatus>(&expected_status).map_err(|error| {
            AppError::Conflict(format!("engineering source status is invalid: {error}"))
        })?;
    let stored_config =
        serde_json::from_str::<EngineeringSourceConfig>(&expected_config).map_err(|error| {
            AppError::Conflict(format!("engineering source config is invalid: {error}"))
        })?;
    if &stored_config != config {
        return Err(AppError::Conflict(
            "engineering source was reconfigured before sync".into(),
        ));
    }
    if sync_claim_active(&status, Utc::now()) {
        return Err(AppError::Conflict(
            "another engineering sync still owns the active claim".into(),
        ));
    }
    let claim_id = format!(
        "{}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        std::process::id()
    );
    status.state = EngineeringSyncState::Syncing;
    status.sync_claim_id = Some(claim_id.clone());
    status.sync_claim_expires_at = Some((Utc::now() + chrono::Duration::minutes(10)).to_rfc3339());
    status.last_error = None;
    status.next_poll_at = Some(next_poll_at(config, Utc::now()));
    let claimed_status =
        serde_json::to_string(&status).map_err(|e| AppError::Internal(e.into()))?;
    let claimed = cluster
        .claim_engineering_source(&name, &expected_config, &expected_status, &claimed_status)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    if !claimed {
        return Err(AppError::Conflict(
            "this source was reconfigured or another sync already claimed it".into(),
        ));
    }

    let completed_at = Utc::now();
    match perform_sync(cluster, config, current_cursor.clone(), &claim_id).await {
        Ok(outcome) => {
            status.last_sync_at = Some(completed_at.to_rfc3339());
            status.items_discovered = outcome.discovered;
            status.items_queued = outcome.queued;
            status.total_items_queued = status
                .total_items_queued
                .saturating_add(outcome.queued as u64);
            status.next_poll_at = Some(next_poll_at(config, completed_at));
            status.review_items = outcome.review_items;
            status.signal_results = outcome.signal_results;
            status.ready_for_review = status
                .review_items
                .iter()
                .filter(|item| item.state == EngineeringReviewState::ReadyForReview)
                .count();
            status.waiting_for_ci = status
                .review_items
                .iter()
                .filter(|item| item.state == EngineeringReviewState::WaitingForCi)
                .count();
            status.ci_failed = status
                .review_items
                .iter()
                .filter(|item| {
                    matches!(
                        item.state,
                        EngineeringReviewState::CiFailed | EngineeringReviewState::Blocked
                    )
                })
                .count();
            status.last_error =
                (!outcome.errors.is_empty()).then(|| truncate_error(outcome.errors.join("; ")));
            status.state = if outcome.errors.is_empty() {
                status.last_success_at = Some(completed_at.to_rfc3339());
                EngineeringSyncState::Ok
            } else if outcome.completed_attempts > 0 {
                EngineeringSyncState::Partial
            } else {
                EngineeringSyncState::Error
            };
            status.sync_claim_id = None;
            status.sync_claim_expires_at = None;
            finalize_source_claim(cluster, &name, &claimed_status, &outcome.cursor, &status)
                .await?;
        }
        Err(error) => {
            status.state = EngineeringSyncState::Error;
            status.last_sync_at = Some(completed_at.to_rfc3339());
            status.last_error = Some(truncate_error(error));
            status.items_discovered = 0;
            status.items_queued = 0;
            status.signal_results = Vec::new();
            status.next_poll_at = Some(next_poll_at(config, completed_at));
            status.sync_claim_id = None;
            status.sync_claim_expires_at = None;
            finalize_source_claim(cluster, &name, &claimed_status, &current_cursor, &status)
                .await?;
        }
    }
    Ok(status)
}

/// Start the bounded best-effort source poller. Durable `next_poll_at` values
/// and a stable initial jitter spread GitHub traffic across teams.
pub fn spawn_poller(state: AppState, sweep_interval: Duration) {
    if state.cluster().is_none() {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut interval = tokio::time::interval(sweep_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let Some(cluster) = state.cluster() else {
                continue;
            };
            let sources = match cluster
                .list_engineering_sources(MAX_SOURCES_PER_SWEEP)
                .await
            {
                Ok(sources) => sources,
                Err(error) => {
                    tracing::error!(error = %error, "engineering intake source listing failed");
                    continue;
                }
            };
            for source in sources {
                let source_name = source.name_any();
                let (config, cursor, status) = match parse_source(&source) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        tracing::error!(source = %source_name, error = %error, "invalid engineering intake source");
                        let failed = EngineeringSourceStatus {
                            state: EngineeringSyncState::Error,
                            last_sync_at: Some(Utc::now().to_rfc3339()),
                            last_error: Some(truncate_error(error)),
                            ..EngineeringSourceStatus::default()
                        };
                        if let Err(patch_error) = patch_runtime_state(
                            cluster,
                            &source_name,
                            &EngineeringCursor::default(),
                            &failed,
                        )
                        .await
                        {
                            tracing::error!(source = %source_name, error = %patch_error, "failed to record engineering intake source error");
                        }
                        continue;
                    }
                };
                if !verify_source_owner(&source, &config, &config.owner_sub) {
                    let error = "engineering source owner annotations do not match its config";
                    tracing::error!(source = %source_name, error, "invalid engineering intake source");
                    let failed = EngineeringSourceStatus {
                        state: EngineeringSyncState::Error,
                        last_sync_at: Some(Utc::now().to_rfc3339()),
                        last_error: Some(error.into()),
                        ..status
                    };
                    if let Err(patch_error) =
                        patch_runtime_state(cluster, &source_name, &cursor, &failed).await
                    {
                        tracing::error!(source = %source_name, error = %patch_error, "failed to record engineering intake ownership error");
                    }
                    continue;
                }
                if let Err(error) = ensure_auto_run_for_backlog(cluster, &config).await {
                    tracing::warn!(source = %source_name, team = %config.team_name, %error, "engineering intake could not rearm queued work");
                }
                if !config.enabled || !is_due(&status, Utc::now()) {
                    continue;
                }
                match synchronize_source(cluster, &config, cursor, status).await {
                    Ok(updated) => {
                        if let Some(error) = updated.last_error.as_deref() {
                            tracing::warn!(source = %source_name, team = %config.team_name, error, "engineering intake sync completed with errors");
                        } else {
                            tracing::info!(source = %source_name, team = %config.team_name, discovered = updated.items_discovered, queued = updated.items_queued, "engineering intake sync complete");
                        }
                    }
                    Err(error) => {
                        tracing::error!(source = %source_name, team = %config.team_name, error = %error, "engineering intake sync failed")
                    }
                }
            }
        }
    });
}
