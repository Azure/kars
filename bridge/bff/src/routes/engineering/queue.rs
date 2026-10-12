// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — queue helpers for engineering intake.

use std::collections::BTreeMap;

use kube::ResourceExt;

use crate::kars::cluster::Cluster;
use crate::kars::team::KarsTeam;
use crate::providers::signing::sha256;
use crate::routes::teams::{read_tasks, request_owned_team_run, work, work_owner};

use super::remediation::{RemediationUpdate, aggregate_remediation_tasks, plan_remediation_update};
use super::{EngineeringSourceConfig, MAX_ITEMS_PER_SYNC, TeamTaskDto};

fn plan_update(task: &mut TeamTaskDto, existing: &[TeamTaskDto]) -> RemediationUpdate {
    match plan_remediation_update(task, existing) {
        RemediationUpdate::NotRemediation => {}
        result => return result,
    }
    let Some(current) = existing.iter().find(|current| current.id == task.id) else {
        return RemediationUpdate::Write(Box::new(task.clone()));
    };
    if current.status == "pending"
        && current.run.is_none()
        && current.run_uid.is_none()
        && current.assignment_nonce.is_none()
    {
        let mut updated = current.clone();
        updated.title = task.title.clone();
        updated.description = task.description.clone();
        updated.review_required |= task.review_required;
        if task.status == "retired" {
            updated.status = task.status.clone();
            updated.done_at = task.done_at.clone();
            updated.stuck_since = None;
        }
        return if &updated == current {
            RemediationUpdate::Unchanged
        } else {
            RemediationUpdate::Write(Box::new(updated))
        };
    }
    if task.status != "pending"
        || (current.title == task.title && current.description == task.description)
    {
        return RemediationUpdate::Unchanged;
    }
    let digest = sha256(work::request(task).to_string().as_bytes());
    let mut followup = task.clone();
    followup.id = format!("engineering-followup-{}", hex::encode(&digest[..16]));
    if existing.iter().any(|item| item.id == followup.id) {
        return RemediationUpdate::Unchanged;
    }
    if !work::finished(current) && !followup.depends_on.contains(&current.id) {
        followup.depends_on.push(current.id.clone());
    }
    RemediationUpdate::Write(Box::new(followup))
}

pub(super) fn merge_discovered_tasks(
    mut existing: Vec<TeamTaskDto>,
    discovered: Vec<TeamTaskDto>,
) -> (Vec<TeamTaskDto>, usize) {
    for task in &mut existing {
        if task.status == "pending" && engineering_task_requires_review(&task.id) {
            task.review_required = true;
        }
    }
    let mut positions = existing
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let mut added = 0;
    for mut task in aggregate_remediation_tasks(discovered) {
        match plan_update(&mut task, &existing) {
            RemediationUpdate::Unchanged => continue,
            RemediationUpdate::Write(updated) => {
                let updated = *updated;
                if let Some(index) = positions.get(&updated.id).copied() {
                    existing[index] = updated;
                } else {
                    positions.insert(updated.id.clone(), existing.len());
                    existing.push(updated);
                    added += 1;
                }
                continue;
            }
            RemediationUpdate::NotRemediation => unreachable!("plan_update resolves ordinary work"),
        }
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
    known_tasks: &mut BTreeMap<String, TeamTaskDto>,
    incoming: Vec<TeamTaskDto>,
    queued_slots_used: &mut usize,
    attempt_cap: usize,
) -> bool {
    let remaining = MAX_ITEMS_PER_SYNC
        .saturating_sub(*queued_slots_used)
        .min(attempt_cap);
    let mut accepted = 0;
    let mut truncated = false;
    for mut task in aggregate_remediation_tasks(incoming) {
        let existing = known_tasks.values().cloned().collect::<Vec<_>>();
        match plan_update(&mut task, &existing) {
            RemediationUpdate::Unchanged => continue,
            RemediationUpdate::Write(updated) => {
                let updated = *updated;
                let needs_slot = !known_tasks.contains_key(&updated.id);
                if needs_slot && accepted >= remaining {
                    truncated = true;
                    continue;
                }
                accepted += usize::from(needs_slot);
                known_tasks.insert(updated.id.clone(), updated);
                // Carry the source observation, not the plan: the final CAS must recheck
                // assignment ownership and completed history against its fresh backlog.
                target.push(task);
                continue;
            }
            RemediationUpdate::NotRemediation => unreachable!("plan_update resolves ordinary work"),
        }
    }
    *queued_slots_used += accepted;
    truncated
}

pub(super) async fn merge_into_backlog(
    cluster: &Cluster,
    team: &KarsTeam,
    discovered: Vec<TeamTaskDto>,
) -> Result<usize, String> {
    let owner = work_owner(team).map_err(|error| error.to_string())?;
    work::update(
        &cluster.configmaps(&owner.namespace),
        &owner,
        None,
        true,
        |existing| {
            let (merged, added) = merge_discovered_tasks(existing.clone(), discovered.clone());
            *existing = merged;
            Ok(added)
        },
    )
    .await
    .map_err(|error| format!("updating the team backlog failed: {error}"))
}

pub(super) async fn request_team_run(cluster: &Cluster, team: &KarsTeam) -> Result<bool, String> {
    if team.spec.paused || team.annotations().contains_key("kars.azure.com/run-now") {
        return Ok(false);
    }
    let tasks = read_tasks(cluster, team)
        .await
        .map_err(|error| error.to_string())?;
    if work::next_pending(&tasks).is_none() {
        return Ok(false);
    }
    request_owned_team_run(cluster, team)
        .await
        .map(|_| true)
        .map_err(|error| format!("queued work but could not request a team run: {error}"))
}

pub(super) async fn source_team(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
) -> Result<KarsTeam, String> {
    let team = cluster
        .teams(&config.team_namespace)
        .get(&config.team_name)
        .await
        .map_err(|error| format!("reading the standing team failed: {error}"))?;
    if team.annotations().get("kars.azure.com/owner-sub") != Some(&config.owner_sub) {
        return Err("the engineering source owner no longer owns this team".into());
    }
    Ok(team)
}

pub(super) async fn ensure_auto_run_for_backlog(
    cluster: &Cluster,
    config: &EngineeringSourceConfig,
) -> Result<bool, String> {
    if !config.enabled || !config.auto_run {
        return Ok(false);
    }
    let team = source_team(cluster, config).await?;
    request_team_run(cluster, &team).await
}
