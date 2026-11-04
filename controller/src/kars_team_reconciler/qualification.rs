// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ReconcileError, namespace, resource_version, tasks, work};
use crate::{kars_task::KarsTask, kars_team::KarsTeam};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};
use serde_json::json;

#[path = "../../../shared/team_qualification.rs"]
pub(super) mod contract;

fn invalid(message: impl Into<String>) -> ReconcileError {
    ReconcileError::Invalid(message.into())
}

pub(super) fn marked(team: &KarsTeam) -> bool {
    [contract::PENDING, contract::RECORD, contract::CONSUMED]
        .iter()
        .any(|key| team.annotations().contains_key(*key))
}

pub(super) fn authorization(
    team: &KarsTeam,
    request: &str,
) -> Result<Option<contract::Record>, ReconcileError> {
    if team.annotations().contains_key(contract::PENDING) {
        return Err(invalid(
            "Qualification Team has no approved, exact work attempt",
        ));
    }
    let record = contract::record(team).map_err(invalid)?;
    if let Some(record) = &record {
        if record.request != request || record.team_uid != tasks::owner_ref(team)?.uid {
            return Err(invalid(
                "Team qualification does not authorize this request or Team UID",
            ));
        }
        if let Some(uid) = team.annotations().get(contract::CONSUMED)
            && !team
                .status
                .as_ref()
                .and_then(|s| s.run_admission.as_ref())
                .is_some_and(|a| &a.task_uid == uid && a.request == request)
        {
            return Err(invalid(
                "Team qualification has already been consumed by another attempt",
            ));
        }
    } else if team.annotations().contains_key(contract::CONSUMED) {
        return Err(invalid(
            "Consumed qualification Team lacks its authorization",
        ));
    }
    Ok(record)
}

pub(super) async fn before_admission(
    client: &Client,
    team: &KarsTeam,
    request: &str,
) -> Result<Option<contract::Record>, ReconcileError> {
    let record = authorization(team, request)?;
    if let Some(record) = &record {
        contract::check(
            client.clone(),
            namespace(team)?,
            &team.name_any(),
            record,
            chrono::Utc::now(),
        )
        .await
        .map_err(invalid)?;
        if team
            .spec
            .envelope
            .budget
            .as_ref()
            .and_then(|b| b.tokens)
            .is_none_or(|v| v <= 0)
            || !crate::inference_budget::team::governed(team)
        {
            return Err(invalid(
                "Qualification requires a positive finite governed Team token budget",
            ));
        }
    }
    Ok(record)
}

pub(super) fn binding(
    record: &contract::Record,
    binding: &work::contract::Binding,
) -> Result<(), ReconcileError> {
    if record.backlog_uid != binding.backlog_uid
        || record.item_id != binding.item_id
        || record.work_sha256 != binding.request_digest
    {
        return Err(invalid(
            "Qualification work item, backlog UID or reviewed intent changed",
        ));
    }
    Ok(())
}

pub(super) fn task_binding(
    team: &KarsTeam,
    task: &KarsTask,
    request: &str,
) -> Result<(), ReconcileError> {
    if let Some(record) = authorization(team, request)? {
        if task.annotations().get(contract::RECORD) != team.annotations().get(contract::RECORD) {
            return Err(invalid(
                "Qualification approval changed after Task reservation",
            ));
        }
        let bound = task
            .annotations()
            .get(work::contract::ANNOTATION)
            .ok_or_else(|| invalid("Qualification Task lacks reviewed work"))?;
        binding(&record, &serde_json::from_str(bound)?)?;
        if team
            .annotations()
            .get(contract::CONSUMED)
            .is_some_and(|uid| Some(uid) != task.metadata.uid.as_ref())
        {
            return Err(invalid(
                "Qualification Task UID differs from the consumed attempt",
            ));
        }
    } else if task.annotations().contains_key(contract::RECORD) {
        return Err(invalid("Qualification approval was removed from its Team"));
    }
    Ok(())
}

/// An admitted Task may survive an acknowledgement failure followed by a
/// readiness failure. Recover only its sealed reservation, before readiness
/// can prevent consumption and terminal pause; never activate a staged Task.
pub(super) async fn recover_consumption(
    client: &Client,
    teams: &Api<KarsTeam>,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
) -> Result<bool, ReconcileError> {
    if !team.annotations().contains_key(contract::RECORD)
        || team.annotations().contains_key(contract::CONSUMED)
    {
        return Ok(false);
    }
    let Some(record) = team.status.as_ref().and_then(|s| s.run_admission.as_ref()) else {
        return Ok(false);
    };
    if team.annotations().get(super::requests::REQUEST) != Some(&record.request) {
        return Ok(false);
    }
    let Some(task) = tasks_api.get_opt(&record.task_name).await? else {
        return Ok(false);
    };
    if task
        .annotations()
        .get("kars.azure.com/team-request-state")
        .map(String::as_str)
        != Some("admitted")
    {
        return Ok(false);
    }
    let prepared = super::requests::Prepared {
        record: record.clone(),
        newly_reserved: false,
    };
    super::requests::finish(client, teams, tasks_api, team, &prepared).await?;
    Ok(true)
}

/// Consumption survives request acknowledgement and Task retirement. Only the
/// exact admitted attempt may finish; no cadence or manual successor is allowed.
pub(super) async fn pause_finished(
    teams: &Api<KarsTeam>,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
) -> Result<bool, ReconcileError> {
    let Some(uid) = team.annotations().get(contract::CONSUMED) else {
        return Ok(false);
    };
    if team.spec.paused {
        return Ok(false);
    }
    let admission = team
        .status
        .as_ref()
        .and_then(|s| s.run_admission.as_ref())
        .filter(|a| &a.task_uid == uid)
        .ok_or_else(|| invalid("Consumed qualification lost its durable admission"))?;
    let task = tasks_api.get_opt(&admission.task_name).await?;
    let finished = match task {
        None => true,
        Some(task) => {
            if task.uid().as_deref() != Some(uid.as_str()) || !tasks::owned(&task.metadata, team) {
                return Err(invalid("Consumed qualification Task was replaced"));
            }
            task.annotations()
                .get("kars.azure.com/run-requested")
                .is_some_and(|nonce| {
                    !nonce.is_empty()
                        && task.annotations().get("kars.azure.com/run-completed") == Some(nonce)
                })
                || !task.spec.execution.as_ref().is_some_and(|e| e.launch)
        }
    };
    if !finished {
        return Ok(false);
    }
    teams
        .patch(
            &team.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata":{"uid":team.uid(),"resourceVersion":resource_version(team)?,
                    "annotations":{"kars.azure.com/run-now":null}},"spec":{"paused":true}
            })),
        )
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_backlog_binding_rejects_replacement_and_reviewed_intent_changes() {
        let now = chrono::Utc::now();
        let record = contract::Record {
            approved_by: "operator".into(),
            approved_by_sub: "operator".into(),
            approved_at: now,
            expires_at: now + chrono::Duration::minutes(45),
            team_uid: "t".into(),
            package_sha256: "p".into(),
            resources: Default::default(),
            backlog_uid: "b".into(),
            item_id: "i".into(),
            work_sha256: "d".into(),
            request: "r".into(),
        };
        let bound = work::contract::Binding {
            backlog_uid: "b".into(),
            item_id: "i".into(),
            request_digest: "d".into(),
        };
        assert!(binding(&record, &bound).is_ok());
        for changed in [
            work::contract::Binding {
                backlog_uid: "other".into(),
                ..bound.clone()
            },
            work::contract::Binding {
                item_id: "other".into(),
                ..bound.clone()
            },
            work::contract::Binding {
                request_digest: "other".into(),
                ..bound
            },
        ] {
            assert!(binding(&record, &changed).is_err());
        }
    }
}
