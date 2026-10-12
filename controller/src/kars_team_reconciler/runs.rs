// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Durable harvest-before-retirement and bounded cadence identity.

use super::{ANNOT_RUN_REQUESTED, ANNOT_TEAM_ROLE, ReconcileError, tasks};
use crate::kars_task::KarsTask;
use crate::kars_team::KarsTeam;
use crate::providers::signing::sha256_hex;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, ResourceExt, api::ListParams};

#[path = "../../../shared/team_run_names.rs"]
mod run_names;
pub(super) use run_names::prefix as run_prefix;

#[derive(Default)]
pub(super) struct RunStats {
    pub active: usize,
    pub succeeded: i64,
    pub barren: i64,
    pub tokens_total: i64,
    pub last_success_at: Option<String>,
}

/// The same predecessor status always produces the same slot, even when create
/// succeeded and the subsequent Team status write failed.
pub(super) fn cadence_name(team: &KarsTeam) -> Result<String, ReconcileError> {
    let uid = tasks::owner_ref(team)?.uid;
    let previous = team.status.clone().unwrap_or_default();
    let key = serde_json::to_vec(&(
        uid,
        team.metadata.generation,
        previous.last_run_at,
        previous.generated_task_count,
        team.spec
            .cadence
            .as_ref()
            .and_then(|cadence| cadence.every_minutes),
    ))?;
    Ok(format!(
        "{}{}",
        run_prefix(&team.name_any()),
        &sha256_hex(&key)[..32]
    ))
}

fn current_output(cm: &ConfigMap, task: &KarsTask) -> bool {
    let (Some(name), Some(namespace), Some(uid)) = (
        task.metadata.name.as_deref(),
        task.metadata.namespace.as_deref(),
        task.metadata.uid.as_deref().filter(|uid| !uid.is_empty()),
    ) else {
        return false;
    };
    let Some(nonce) = task
        .annotations()
        .get(ANNOT_RUN_REQUESTED)
        .filter(|nonce| !nonce.is_empty())
    else {
        return false;
    };
    let Some(data) = cm.data.as_ref() else {
        return false;
    };
    task.metadata.deletion_timestamp.is_none()
        && cm.metadata.deletion_timestamp.is_none()
        && cm.metadata.name.as_deref() == Some(format!("kars-mission-output-{name}").as_str())
        && cm.metadata.namespace.as_deref() == Some(namespace)
        && task.annotations().get("kars.azure.com/run-completed") == Some(nonce)
        && cm
            .annotations()
            .get("kars.azure.com/mission-task-uid")
            .map(String::as_str)
            == Some(uid)
        && cm.annotations().get("kars.azure.com/mission-run-nonce") == Some(nonce)
        && cm
            .annotations()
            .get("kars.azure.com/mission-principal-name")
            .map(String::as_str)
            == Some(name)
        && cm.metadata.owner_references.as_ref().is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner.api_version == "kars.azure.com/v1alpha1"
                    && owner.kind == "KarsTask"
                    && owner.name == name
                    && owner.uid == uid
                    && owner.controller == Some(true)
            })
        })
        && data.get("taskName").map(String::as_str) == Some(name)
        && data.get("taskUid").map(String::as_str) == Some(uid)
        && data.get("assignmentNonce") == Some(nonce)
}

pub(super) async fn harvest_and_retire_runs(
    client: &Client,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
) -> Result<RunStats, ReconcileError> {
    let mut stats = RunStats::default();
    let list = tasks_api.list(&ListParams::default()).await?;
    let namespace = super::namespace(team)?;
    let cms = Api::<ConfigMap>::namespaced(client.clone(), namespace);
    for task in list.items.iter().filter(|task| {
        tasks::owned(&task.metadata, team)
            && task
                .annotations()
                .get(ANNOT_TEAM_ROLE)
                .is_some_and(|role| role == "taskforce")
    }) {
        let run = task.name_any();
        let launched = task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch);
        let Some(cm) = cms.get_opt(&format!("kars-mission-output-{run}")).await? else {
            if launched {
                stats.active += 1;
            } else if let Some(current) = tasks_api.get_opt(&run).await?
                && current.metadata.uid == task.metadata.uid
                && current.metadata.namespace.as_deref() == Some(namespace)
                && current
                    .annotations()
                    .get(ANNOT_TEAM_ROLE)
                    .is_some_and(|role| role == "taskforce")
            {
                super::work::cancel_stopped(client, team, &current).await?;
            }
            continue;
        };
        // Projections are written before the Task completion marker. Refresh the
        // Task after reading output, and never harvest a replaced or revised run.
        let current = tasks_api.get_opt(&run).await?;
        let Some(task) = current.as_ref().filter(|current| {
            current.metadata.uid == task.metadata.uid
                && current.metadata.name == task.metadata.name
                && current.metadata.namespace.as_deref() == Some(namespace)
                && tasks::owned(&current.metadata, team)
                && current
                    .annotations()
                    .get(ANNOT_TEAM_ROLE)
                    .is_some_and(|role| role == "taskforce")
                && current_output(&cm, current)
        }) else {
            if launched {
                stats.active += 1;
            }
            continue;
        };
        let launched = task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch);
        let data = cm.data.unwrap_or_default();
        if data
            .get("finishedAt")
            .is_some_and(|finished| super::parse_rfc3339(finished).is_none())
        {
            return Err(ReconcileError::Invalid(format!(
                "run '{run}' has malformed finishedAt"
            )));
        }
        let tokens = parse_count(data.get("totalTokens"), "totalTokens", &run)?;
        let artifacts = parse_count(data.get("artifactCount"), "artifactCount", &run)?;
        stats.tokens_total = stats.tokens_total.saturating_add(tokens);
        let output = data.get("output").map(String::as_str).unwrap_or_default();
        let successful = (tokens > 0 || artifacts > 0)
            && data.get("status").map(String::as_str) == Some("ok")
            && !output.trim().is_empty();
        let id = format!(
            "run-{}",
            sha256_hex(&serde_json::to_vec(&(
                task.metadata.uid.as_deref(),
                task.annotations().get(ANNOT_RUN_REQUESTED),
            ))?)
        );
        let archive = if successful {
            match crate::team_commons::retained_archive(client, team, &id).await? {
                Some(archive) => Some(archive),
                None => crate::reviewed_execution::retained_team_run(client, team, task).await?,
            }
        } else {
            None
        };
        if let Some(archive) = &archive {
            let source = &archive.documents[0].source;
            if source.task_uid != task.metadata.uid.as_deref().unwrap_or_default()
                || Some(&source.run_nonce) != task.annotations().get(ANNOT_RUN_REQUESTED)
                || archive.output != output
            {
                return Err(ReconcileError::Invalid(
                    "retained archive differs from the completed run".into(),
                ));
            }
        }
        let may_harvest = super::work::harvest(client, team, task, &data, successful).await?;
        if successful {
            let title = team
                .spec
                .charter
                .lines()
                .next()
                .unwrap_or(&team.spec.charter);
            if may_harvest {
                crate::team_commons::record_run(
                    client,
                    team,
                    &id,
                    title,
                    &run,
                    &run,
                    output,
                    archive.as_ref(),
                )
                .await?;
            }
            stats.succeeded += 1;
            if let Some(finished) = data.get("finishedAt") {
                if stats
                    .last_success_at
                    .as_ref()
                    .is_none_or(|previous| previous < finished)
                {
                    stats.last_success_at = Some(finished.clone());
                }
            }
        } else {
            stats.barren += 1;
        }
        if launched {
            tasks::idle(tasks_api, task).await?;
        }
    }
    Ok(stats)
}

fn parse_count(value: Option<&String>, field: &str, run: &str) -> Result<i64, ReconcileError> {
    let Some(value) = value else { return Ok(0) };
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
        .ok_or_else(|| ReconcileError::Invalid(format!("run '{run}' has malformed {field}")))
}
