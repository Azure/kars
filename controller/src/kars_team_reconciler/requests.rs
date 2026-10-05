// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Manual requests reserve an unlaunched Task UID before enabling execution.

use super::{
    ANNOT_RUN_REQUESTED, ANNOT_TEAM, ANNOT_TEAM_ROLE, MAX_CONCURRENT_RUNS, ReconcileError,
    capabilities, namespace, resource_version, specs, tasks,
};
use crate::kars_task::KarsTask;
use crate::kars_team::{KarsTeam, TeamRunAdmission};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams, PostParams},
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

pub(super) const REQUEST: &str = "kars.azure.com/run-now";
const SOURCE: &str = "kars.azure.com/team-run-request";
const STATE: &str = "kars.azure.com/team-request-state";
const AUTHORITY: &str = "kars.azure.com/team-request-authority";
const SPEC: &str = "kars.azure.com/team-request-spec";

fn invalid(message: &str) -> ReconcileError {
    ReconcileError::Invalid(format!("Team run admission: {message}"))
}

pub(super) fn pending(team: &KarsTeam) -> Result<Option<&str>, ReconcileError> {
    let Some(request) = team.annotations().get(REQUEST) else {
        return Ok(None);
    };
    sequence(request)?;
    Ok(Some(request))
}

fn sequence(request: &str) -> Result<u64, ReconcileError> {
    let parsed = request
        .strip_prefix("manual-")
        .and_then(|value| value.split_once('-'));
    if let Some((number, identity)) = parsed
        && let Ok(sequence) = number.parse::<u64>()
        && sequence > 0
        && sequence <= i64::MAX as u64
        && number == sequence.to_string()
        && identity.len() == 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(sequence);
    }
    Err(invalid(
        "request must have canonical manual-<sequence>-<sha256> identity",
    ))
}

fn digest(value: &impl Serialize) -> Result<String, ReconcileError> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

fn task_digest(task: &KarsTask) -> Result<String, ReconcileError> {
    let mut spec = task.spec.clone();
    if let Some(execution) = &mut spec.execution {
        execution.launch = false;
    }
    digest(&spec)
}

fn task_name(team: &KarsTeam, request: &str) -> Result<String, ReconcileError> {
    let owner = tasks::owner_ref(team)?;
    let key = digest(&("manual-team-run-v1", namespace(team)?, owner.uid, request))?;
    let prefix: String = team.name_any().chars().take(180).collect();
    Ok(format!(
        "{}-run-{}",
        prefix.trim_end_matches('-'),
        &key[..32]
    ))
}

fn reservation(team: &KarsTeam) -> Option<&TeamRunAdmission> {
    team.status.as_ref()?.run_admission.as_ref()
}

pub(super) fn awaiting_ack(team: &KarsTeam, task: &KarsTask) -> bool {
    reservation(team).is_some_and(|record| {
        team.annotations().get(REQUEST) == Some(&record.request)
            && !record.task_uid.is_empty()
            && task.metadata.uid.as_deref() == Some(record.task_uid.as_str())
            && task.name_any() == record.task_name
            && tasks::owned(&task.metadata, team)
    })
}

fn validate_task(
    task: &KarsTask,
    team: &KarsTeam,
    record: &TeamRunAdmission,
) -> Result<(), ReconcileError> {
    if !tasks::owned(&task.metadata, team)
        || task.name_any() != record.task_name
        || task.metadata.uid.as_deref() != Some(&record.task_uid)
        || task.metadata.uid.as_ref().is_none_or(String::is_empty)
        || task
            .metadata
            .resource_version
            .as_ref()
            .is_none_or(String::is_empty)
        || task.metadata.deletion_timestamp.is_some()
        || task.annotations().get(ANNOT_TEAM_ROLE).map(String::as_str) != Some("taskforce")
        || task.annotations().get(ANNOT_RUN_REQUESTED) != Some(&record.task_name)
        || task.annotations().get(SOURCE) != Some(&record.request)
        || task.annotations().get(AUTHORITY) != Some(&record.authority_digest)
        || task.annotations().get(SPEC) != Some(&record.task_spec_digest)
        || task_digest(task)? != record.task_spec_digest
    {
        return Err(invalid(
            "reserved Task ownership, UID, authority or request identity changed",
        ));
    }
    match task.annotations().get(STATE).map(String::as_str) {
        Some("admitted") => Ok(()),
        Some("staged")
            if task
                .spec
                .execution
                .as_ref()
                .is_some_and(|execution| !execution.launch)
                && ["kars.azure.com/run-ack", "kars.azure.com/run-completed"]
                    .iter()
                    .all(|key| !task.annotations().contains_key(*key)) =>
        {
            Ok(())
        }
        _ => Err(invalid(
            "Task has invalid staging state or already-executed staging evidence",
        )),
    }
}

pub(super) struct Prepared {
    pub record: TeamRunAdmission,
    pub newly_reserved: bool,
}

pub(super) async fn prepare(
    client: &Client,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
    can_admit: bool,
) -> Result<Option<Prepared>, ReconcileError> {
    let Some(request) = pending(team)? else {
        return Ok(None);
    };
    let requested_sequence = sequence(request)?;
    let previous = reservation(team);
    let prior_sequence = previous
        .map(|record| sequence(&record.request))
        .transpose()?
        .unwrap_or(0);
    let reserved = previous.filter(|record| record.request == request);
    if reserved.is_none() && requested_sequence != prior_sequence + 1 {
        return Err(invalid(
            "request sequence is stale or skips the next admission; refusing replay",
        ));
    }
    let name = task_name(team, request)?;
    let existing = tasks_api.get_opt(&name).await?;
    if let Some(record) = reserved {
        if record.task_name != name {
            return Err(invalid("reservation names a different Task"));
        }
        let task = existing
            .ok_or_else(|| invalid("reserved Task is missing; refusing to recreate execution"))?;
        validate_task(&task, team, record)?;
        if task.annotations().get(STATE).map(String::as_str) != Some("admitted") && !can_admit {
            return Ok(None);
        }
        return Ok(Some(Prepared {
            record: record.clone(),
            newly_reserved: false,
        }));
    }
    if !can_admit {
        return Ok(None);
    }
    let authority_digest = digest(&team.spec)?;
    let task = match existing {
        Some(task) => task,
        None => {
            let allowance = specs::run_knowledge_budget(team).map_err(ReconcileError::Invalid)?;
            let knowledge = crate::team_commons::prior_knowledge(client, team, allowance).await?;
            let mut spec = specs::run_spec(team, &knowledge).map_err(ReconcileError::Invalid)?;
            spec.execution
                .as_mut()
                .ok_or_else(|| invalid("run spec lacks execution"))?
                .launch = false;
            let mut task = KarsTask::new(&name, spec);
            task.metadata.namespace = Some(namespace(team)?.into());
            task.metadata.owner_references = Some(vec![tasks::owner_ref(team)?]);
            task.metadata.labels = Some([(ANNOT_TEAM.into(), team.name_any())].into());
            task.metadata.annotations = Some(
                [
                    (ANNOT_TEAM.into(), team.name_any()),
                    (ANNOT_TEAM_ROLE.into(), "taskforce".into()),
                    (ANNOT_RUN_REQUESTED.into(), name.clone()),
                    (SOURCE.into(), request.into()),
                    (STATE.into(), "staged".into()),
                    (AUTHORITY.into(), authority_digest.clone()),
                    (SPEC.into(), task_digest(&task)?),
                ]
                .into(),
            );
            tasks_api.create(&PostParams::default(), &task).await?
        }
    };
    let record = TeamRunAdmission {
        request: request.into(),
        task_name: name,
        task_uid: task
            .metadata
            .uid
            .clone()
            .ok_or_else(|| invalid("created Task has no UID"))?,
        authority_digest,
        task_spec_digest: task_digest(&task)?,
    };
    validate_task(&task, team, &record)?;
    if task.annotations().get(STATE).map(String::as_str) != Some("staged") {
        return Err(invalid(
            "already-admitted Task lacks its matching reservation; refusing replay",
        ));
    }
    Ok(Some(Prepared {
        record,
        newly_reserved: true,
    }))
}

async fn current_team(
    teams: &Api<KarsTeam>,
    expected: &KarsTeam,
    record: &TeamRunAdmission,
) -> Result<KarsTeam, ReconcileError> {
    let fresh = teams.get(&expected.name_any()).await?;
    if tasks::owner_ref(&fresh)? != tasks::owner_ref(expected)?
        || fresh.metadata.namespace != expected.metadata.namespace
        || fresh.metadata.generation != expected.metadata.generation
        || resource_version(&fresh)? != resource_version(expected)?
        || fresh.metadata.deletion_timestamp.is_some()
        || pending(&fresh)? != Some(record.request.as_str())
        || reservation(&fresh) != Some(record)
    {
        return Err(invalid("Team or pending request changed after reservation"));
    }
    Ok(fresh)
}

async fn ready_to_activate(
    client: &Client,
    tasks_api: &Api<KarsTask>,
    team: &KarsTeam,
    record: &TeamRunAdmission,
) -> Result<(), ReconcileError> {
    let effective = capabilities::effective_team(client, team).await?;
    if effective.spec.paused
        || specs::unsupported_budget(&effective.spec.envelope)
        || !effective.validation_errors().is_empty()
        || digest(&effective.spec)? != record.authority_digest
    {
        return Err(invalid(
            "reviewed Team authority changed or no longer allows admission",
        ));
    }
    capabilities::capability_readiness(client, &effective).await?;
    crate::inference_budget::team::ready(client, &effective, &specs::principal_name(&effective))
        .await
        .map_err(|error| ReconcileError::Invalid(format!("Team run budget admission: {error}")))?;
    let active = tasks_api
        .list(&ListParams::default())
        .await?
        .items
        .into_iter()
        .filter(|task| {
            tasks::owned(&task.metadata, team)
                && task.name_any() != record.task_name
                && task.annotations().get(ANNOT_TEAM_ROLE).map(String::as_str) == Some("taskforce")
                && task
                    .spec
                    .execution
                    .as_ref()
                    .is_some_and(|execution| execution.launch)
        })
        .count();
    if active >= MAX_CONCURRENT_RUNS {
        return Err(invalid("Team concurrent-run capacity is unavailable"));
    }
    Ok(())
}

pub(super) async fn finish(
    client: &Client,
    teams: &Api<KarsTeam>,
    tasks_api: &Api<KarsTask>,
    persisted: &KarsTeam,
    prepared: &Prepared,
) -> Result<(), ReconcileError> {
    let record = &prepared.record;
    let fresh = current_team(teams, persisted, record).await?;
    let mut task = tasks_api.get(&record.task_name).await?;
    validate_task(&task, &fresh, record)?;
    if task.annotations().get(STATE).map(String::as_str) == Some("staged") {
        ready_to_activate(client, tasks_api, &fresh, record).await?;
        current_team(teams, &fresh, record).await?;
        task.spec
            .execution
            .as_mut()
            .ok_or_else(|| invalid("staged Task lacks execution"))?
            .launch = true;
        task.annotations_mut()
            .insert(STATE.into(), "admitted".into());
        let admitted = tasks_api
            .replace(&record.task_name, &PostParams::default(), &task)
            .await?;
        // Kubernetes has no cross-object transaction. Compensate if the Team fence
        // changed during activation; never overwrite a changed/replaced Task.
        if let Err(error) = current_team(teams, &fresh, record).await {
            tasks::idle(tasks_api, &admitted).await?;
            return Err(error);
        }
    }
    teams
        .patch(
            &fresh.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {
                    "uid": tasks::owner_ref(&fresh)?.uid,
                    "resourceVersion": resource_version(&fresh)?,
                    "annotations": { (REQUEST): null }
                }
            })),
        )
        .await?;
    Ok(())
}
