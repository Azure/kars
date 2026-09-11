// kars Bridge BFF — queue helpers for engineering intake.

use std::collections::BTreeMap;

use chrono::Utc;

use crate::kars::cluster::Cluster;
use crate::routes::teams::read_task_list;

use super::remediation::match_remediation_task;
use super::{EngineeringSourceConfig, MAX_ITEMS_PER_SYNC, TeamTaskDto};

pub(super) fn merge_discovered_tasks(
    mut existing: Vec<TeamTaskDto>,
    discovered: Vec<TeamTaskDto>,
) -> (Vec<TeamTaskDto>, usize) {
    for task in &mut existing {
        if engineering_task_requires_review(&task.id) {
            task.review_required = true;
        }
    }
    let mut positions = existing
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut added = 0;
    for mut task in discovered {
        let (matching_id, _) = match_remediation_task(&mut task, |id| {
            positions
                .get(id)
                .map(|index| existing[*index].description.as_str())
        });
        if let Some(index) = positions.get(&matching_id).copied() {
            let current = &mut existing[index];
            let renewable_alert = task.id.starts_with("dependabot-alert-")
                || task.id.starts_with("code-scanning-alert-")
                || task.id.starts_with("secret-scanning-alert-");
            let renewable_human_decision = task.id.starts_with("github-pr-merge-")
                || task.id.starts_with("github-pr-feedback-");
            let renewable_pr_control =
                task.id.starts_with("github-pr-fix-") || task.id.starts_with("github-pr-dedupe-");
            if renewable_alert && current.status == "pending" && task.status == "done" {
                current.title = task.title;
                current.description = task.description;
                current.status = "done".into();
                current.run = None;
                current.done_at = task.done_at;
                current.stuck_since = None;
            } else if current.status == "done"
                && (renewable_human_decision
                    || ((renewable_alert || renewable_pr_control)
                        && current.description != task.description))
            {
                current.title = task.title;
                current.description = task.description;
                current.status = "pending".into();
                current.run = None;
                current.done_at = None;
                current.created_at = task.created_at;
                added += 1;
            } else if (renewable_alert || renewable_pr_control)
                && matches!(current.status.as_str(), "pending" | "active")
                && current.description != task.description
            {
                current.title = task.title;
                current.description = task.description;
            }
            current.review_required |= task.review_required;
            continue;
        }
        positions.insert(task.id.clone(), existing.len());
        existing.push(task);
        added += 1;
    }
    (existing, added)
}

fn engineering_task_requires_review(task_id: &str) -> bool {
    task_id.starts_with("dependabot-pr-")
        || task_id.starts_with("dependency-remediation-")
        || task_id.starts_with("dependabot-alert-")
        || task_id.starts_with("code-scanning-alert-")
        || task_id.starts_with("secret-scanning-alert-")
        || task_id.starts_with("github-pr-fix-")
        || task_id.starts_with("github-pr-dedupe-")
        || task_id.starts_with("github-pr-feedback-")
}

pub(super) fn append_bounded_tasks(
    target: &mut Vec<TeamTaskDto>,
    known_tasks: &mut BTreeMap<String, (String, String)>,
    incoming: Vec<TeamTaskDto>,
    queued_slots_used: &mut usize,
    attempt_cap: usize,
) -> bool {
    let mut queue_candidates = Vec::new();
    for mut task in incoming {
        let (matching_id, _) = match_remediation_task(&mut task, |id| {
            known_tasks
                .get(id)
                .map(|(_, description)| description.as_str())
        });
        let renewable_alert = task.id.starts_with("dependabot-alert-")
            || task.id.starts_with("code-scanning-alert-")
            || task.id.starts_with("secret-scanning-alert-");
        match known_tasks.get(&matching_id) {
            None => {
                known_tasks.insert(
                    task.id.clone(),
                    (task.status.clone(), task.description.clone()),
                );
                queue_candidates.push(task);
            }
            Some((status, description)) => {
                let reopen =
                    renewable_alert && status == "done" && description != &task.description;
                if reopen {
                    known_tasks.insert(
                        task.id.clone(),
                        ("pending".into(), task.description.clone()),
                    );
                    queue_candidates.push(task);
                } else if renewable_alert
                    && matches!(status.as_str(), "pending" | "active")
                    && description != &task.description
                {
                    known_tasks.insert(task.id.clone(), (status.clone(), task.description.clone()));
                    target.push(task);
                }
            }
        }
    }
    let remaining = MAX_ITEMS_PER_SYNC
        .saturating_sub(*queued_slots_used)
        .min(attempt_cap);
    let truncated = queue_candidates.len() > remaining;
    queue_candidates.truncate(remaining);
    *queued_slots_used += queue_candidates.len();
    target.extend(queue_candidates);
    truncated
}

pub(super) async fn merge_into_backlog(
    cluster: &Cluster,
    team: &str,
    discovered: Vec<TeamTaskDto>,
) -> Result<usize, String> {
    let queued = std::sync::atomic::AtomicUsize::new(0);
    let name = format!("kars-team-tasks-{team}");
    cluster
        .update_configmap_data(&name, &[("kars.azure.com/team-tasks", team)], |data| {
            let existing = data
                .get("tasks.json")
                .map(|raw| read_task_list(raw))
                .unwrap_or_default();
            let (merged, added) = merge_discovered_tasks(existing, discovered.clone());
            queued.store(added, std::sync::atomic::Ordering::Relaxed);
            data.insert(
                "tasks.json".to_string(),
                serde_json::to_string(&merged).unwrap_or_else(|_| "[]".into()),
            );
        })
        .await
        .map_err(|e| format!("updating the team backlog failed: {e}"))?;
    Ok(queued.load(std::sync::atomic::Ordering::Relaxed))
}

pub(super) async fn request_team_run(
    cluster: &Cluster,
    namespace: &str,
    team: &str,
) -> Result<bool, String> {
    let team_object = cluster
        .teams(namespace)
        .get_opt(team)
        .await
        .map_err(|error| format!("checking team run state failed: {error}"))?
        .ok_or_else(|| "the standing team no longer exists".to_string())?;
    if team_object.spec.paused {
        return Ok(false);
    }
    cluster
        .teams(namespace)
        .patch(
            team,
            &kube::api::PatchParams::default(),
            &kube::api::Patch::Merge(serde_json::json!({
                "metadata": {
                    "annotations": {
                        "kars.azure.com/backlog-run-now": Utc::now().to_rfc3339()
                    }
                }
            })),
        )
        .await
        .map(|_| true)
        .map_err(|error| format!("queued work but could not request a team run: {error}"))
}

pub(super) async fn ensure_auto_run_for_backlog(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
) -> Result<bool, String> {
    if !config.enabled || !config.auto_run {
        return Ok(false);
    }
    let has_pending = read_task_list(&cluster.read_team_tasks(&config.team_name).await)
        .iter()
        .any(|task| task.status == "pending");
    if !has_pending {
        return Ok(false);
    }
    request_team_run(cluster, &config.team_namespace, &config.team_name).await
}
