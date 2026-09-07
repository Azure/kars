// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Durable harvest-before-retirement and bounded cadence identity.

use super::{ANNOT_RUN_REQUESTED, ANNOT_TEAM_ROLE, ReconcileError, tasks};
use crate::kars_task::KarsTask;
use crate::kars_team::KarsTeam;
use crate::providers::signing::sha256_hex;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, ResourceExt, api::ListParams};

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
    let prefix: String = team.name_any().chars().take(180).collect();
    Ok(format!("{prefix}-run-{}", &sha256_hex(&key)[..32]))
}

pub(super) async fn harvest_and_retire_runs(
    client: &Client,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
) -> Result<RunStats, ReconcileError> {
    let mut stats = RunStats::default();
    let list = tasks_api.list(&ListParams::default()).await?;
    let cms = Api::<ConfigMap>::namespaced(client.clone(), super::namespace(team)?);
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
        let terminal = matches!(
            (task.annotations().get(ANNOT_RUN_REQUESTED), task.annotations().get("kars.azure.com/run-completed")),
            (Some(requested), Some(completed)) if requested == completed
        );
        let Some(cm) = cms.get_opt(&format!("kars-mission-output-{run}")).await? else {
            if launched {
                stats.active += 1;
            }
            continue;
        };
        // In-progress output can still change: do not record an incomplete first
        // result under an idempotent commons key and lose the final result.
        if !terminal {
            if launched {
                stats.active += 1;
            }
            continue;
        }
        let data = cm.data.unwrap_or_default();
        let tokens = parse_count(data.get("totalTokens"), "totalTokens", &run)?;
        let artifacts = parse_count(data.get("artifactCount"), "artifactCount", &run)?;
        stats.tokens_total = stats.tokens_total.saturating_add(tokens);
        let output = data.get("output").map(String::as_str).unwrap_or_default();
        if (tokens > 0 || artifacts > 0)
            && data.get("status").map(String::as_str) == Some("ok")
            && !output.trim().is_empty()
        {
            let title = team
                .spec
                .charter
                .lines()
                .next()
                .unwrap_or(&team.spec.charter);
            // UID provenance prevents name reuse from aliasing an older run.
            let id = task
                .metadata
                .uid
                .as_deref()
                .filter(|uid| !uid.is_empty())
                .ok_or_else(|| ReconcileError::Invalid(format!("run '{run}' has no UID")))?;
            crate::team_commons::record_entry(client, team, id, title, &run, &run, output).await?;
            stats.succeeded += 1;
            if let Some(finished) = data.get("finishedAt") {
                if super::parse_rfc3339(finished).is_none() {
                    return Err(ReconcileError::Invalid(format!(
                        "run '{run}' has malformed finishedAt"
                    )));
                }
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
