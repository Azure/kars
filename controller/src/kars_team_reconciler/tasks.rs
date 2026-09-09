// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Ownership-checked, optimistic-concurrency-controlled Task lifecycle.

use super::{ANNOT_RUN_REQUESTED, ANNOT_TEAM, ANNOT_TEAM_ROLE, ReconcileError, specs};
use crate::kars_task::{KarsTask, KarsTaskSpec, spec_attenuation_violations};
use crate::kars_team::KarsTeam;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::{
    Api, ResourceExt,
    api::{DeleteParams, ListParams, PostParams, Preconditions},
};

pub(super) fn owner_ref(team: &KarsTeam) -> Result<OwnerReference, ReconcileError> {
    let uid = team
        .metadata
        .uid
        .as_ref()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| ReconcileError::Invalid("Team has no UID".into()))?;
    Ok(OwnerReference {
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsTeam".into(),
        name: team.name_any(),
        uid: uid.clone(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    })
}

pub(super) fn owned(metadata: &ObjectMeta, team: &KarsTeam) -> bool {
    let Some(uid) = team.metadata.uid.as_deref().filter(|uid| !uid.is_empty()) else {
        return false;
    };
    metadata.namespace == team.metadata.namespace
        && metadata.owner_references.as_ref().is_some_and(|owners| {
            owners
                .iter()
                .filter(|owner| owner.controller == Some(true))
                .count()
                == 1
                && owners.iter().any(|owner| {
                    owner.controller == Some(true)
                        && owner.uid == uid
                        && owner.kind == "KarsTeam"
                        && owner.api_version == "kars.azure.com/v1alpha1"
                        && Some(owner.name.as_str()) == team.metadata.name.as_deref()
                })
        })
}

fn require_version(task: &KarsTask) -> Result<(), ReconcileError> {
    if task.metadata.uid.as_ref().is_none_or(String::is_empty)
        || task
            .metadata
            .resource_version
            .as_ref()
            .is_none_or(String::is_empty)
    {
        return Err(ReconcileError::Invalid(format!(
            "task '{}' lacks UID/resourceVersion",
            task.name_any()
        )));
    }
    Ok(())
}

pub(super) async fn idle(
    tasks: &Api<KarsTask>,
    task: &KarsTask,
) -> Result<KarsTask, ReconcileError> {
    if !task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch)
    {
        return Ok(task.clone());
    }
    require_version(task)?;
    let mut updated = task.clone();
    if let Some(execution) = &mut updated.spec.execution {
        execution.launch = false;
    }
    Ok(tasks
        .replace(&task.name_any(), &PostParams::default(), &updated)
        .await?)
}

async fn retire(tasks: &Api<KarsTask>, task: &KarsTask) -> Result<(), ReconcileError> {
    let stopped = idle(tasks, task).await?;
    require_version(&stopped)?;
    let params = DeleteParams {
        preconditions: Some(Preconditions {
            uid: stopped.metadata.uid.clone(),
            resource_version: stopped.metadata.resource_version.clone(),
        }),
        ..Default::default()
    };
    match tasks.delete(&task.name_any(), &params).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Never use labels as evidence of ownership, including when labels disappeared.
pub(super) async fn revoke_all(
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
) -> Result<(), ReconcileError> {
    let list = tasks.list(&ListParams::default()).await?;
    for task in list.items.iter().filter(|task| owned(&task.metadata, team)) {
        retire(tasks, task).await?;
    }
    Ok(())
}

pub(super) async fn reconcile_revocations(
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
) -> Result<(), ReconcileError> {
    let principal = specs::principal_spec(team);
    let list = tasks.list(&ListParams::default()).await?;
    for task in list.items.iter().filter(|task| owned(&task.metadata, team)) {
        let role = task.annotations().get(ANNOT_TEAM_ROLE).map(String::as_str);
        let rebinding = crate::kars_task_reconciler::rebind::pending(task);
        let within = |desired: &KarsTaskSpec| {
            within_seat(&task.spec, desired)
                || (rebinding
                    && within_seat(
                        &without_credentials(&task.spec),
                        &without_credentials(desired),
                    ))
        };
        let attenuated = spec_attenuation_violations(&task.spec, &principal).is_empty()
            || (rebinding
                && spec_attenuation_violations(
                    &without_credentials(&task.spec),
                    &without_credentials(&principal),
                )
                .is_empty());
        let authorized = match role {
            Some("principal") => task.name_any() == specs::principal_name(team),
            Some("member") => team.spec.roster.iter().any(|role| {
                specs::member_name(team, role) == task.name_any()
                    && task
                        .spec
                        .parent_ref
                        .as_ref()
                        .is_some_and(|reference| reference.name == specs::principal_name(team))
                    && within(&specs::member_spec(team, role))
                    && attenuated
            }),
            Some("taskforce") => {
                task.spec
                    .parent_ref
                    .as_ref()
                    .is_some_and(|reference| reference.name == specs::principal_name(team))
                    && specs::envelope_errors(&task.spec.envelope).is_empty()
                    && specs::policy_errors(&task.spec).is_empty()
                    && attenuated
            }
            _ => false,
        };
        if !authorized {
            retire(tasks, task).await?;
        } else if team.spec.paused
            || specs::has_positive_budget(&task.spec.envelope)
            || (role == Some("principal") && !within(&principal))
        {
            idle(tasks, task).await?;
        }

        fn without_credentials(spec: &KarsTaskSpec) -> KarsTaskSpec {
            let mut value = spec.clone();
            if let Some(blueprint) = value.blueprint.as_mut() {
                blueprint.credential_bindings = None;
                blueprint.github_binding = None;
            }
            value
        }
    }
    Ok(())
}

/// Compare authority to a seat's new bounds without consuming an extra hop:
/// the old and desired specs are the same seat, not a parent and child.
fn within_seat(old: &KarsTaskSpec, desired: &KarsTaskSpec) -> bool {
    let mut bound = desired.clone();
    bound.envelope.authority_ceiling = desired.envelope.tier;
    bound.envelope.delegation_depth = desired.envelope.delegation_depth.saturating_add(1);
    old.envelope.authority_ceiling <= desired.envelope.authority_ceiling
        && specs::envelope_errors(&old.envelope).is_empty()
        && specs::policy_errors(old).is_empty()
        && spec_attenuation_violations(old, &bound).is_empty()
}

pub(super) async fn apply_task(
    tasks: &Api<KarsTask>,
    team: &KarsTeam,
    name: &str,
    mut spec: KarsTaskSpec,
    role: &str,
) -> Result<KarsTask, ReconcileError> {
    if role == "taskforce" && specs::has_positive_budget(&spec.envelope) {
        return Err(ReconcileError::Invalid(
            "UnsupportedLaunchBudget: finite total/subtree and monetary budgets are planning-only until durable enforcement is available".into(),
        ));
    }
    let existing = tasks.get_opt(name).await?;
    if let Some(old) = &existing {
        if !owned(&old.metadata, team)
            || old.annotations().get(ANNOT_TEAM_ROLE).map(String::as_str) != Some(role)
        {
            return Err(ReconcileError::Invalid(format!(
                "refusing to adopt foreign or different-role task '{name}'"
            )));
        }
        require_version(old)?;
        if old.metadata.deletion_timestamp.is_some() {
            return Err(ReconcileError::Invalid(format!(
                "task '{name}' is still terminating"
            )));
        }
        // A retry after a lost Team status write must not launch a completed run again.
        if role == "taskforce" {
            if !within_seat(&old.spec, &spec)
                || old
                    .spec
                    .parent_ref
                    .as_ref()
                    .map(|reference| &reference.name)
                    != spec.parent_ref.as_ref().map(|reference| &reference.name)
                || old
                    .annotations()
                    .get(ANNOT_RUN_REQUESTED)
                    .map(String::as_str)
                    != Some(name)
            {
                return Err(ReconcileError::Invalid(format!(
                    "cadence slot '{name}' no longer matches its authority or run identity"
                )));
            }
            if team.spec.paused {
                return idle(tasks, old).await;
            }
            return Ok(old.clone());
        }
        if crate::kars_task_reconciler::rebind::pending(old) && !team.spec.paused {
            return Ok(old.clone());
        }
        if within_seat(&old.spec, &spec) {
            spec.execution = old.spec.execution.clone();
        } else if old
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch)
        {
            // Stop old authority before updating its envelope. The Task controller
            // will tear down the previous sandbox rather than keep it live.
            let mut execution = old.spec.execution.clone().unwrap_or_default();
            execution.launch = false;
            spec.execution = Some(execution);
        }
    }
    if (team.spec.paused || specs::has_positive_budget(&spec.envelope))
        && let Some(execution) = &mut spec.execution
    {
        execution.launch = false;
    }
    let mut updated = existing
        .clone()
        .unwrap_or_else(|| KarsTask::new(name, spec.clone()));
    updated.spec = spec;
    updated.metadata.namespace = team.metadata.namespace.clone();
    if existing.is_none() {
        updated.metadata.owner_references = Some(vec![owner_ref(team)?]);
    }
    let annotations = updated
        .metadata
        .annotations
        .get_or_insert_with(Default::default);
    annotations.insert(ANNOT_TEAM.into(), team.name_any());
    annotations.insert(ANNOT_TEAM_ROLE.into(), role.into());
    if role == "taskforce" {
        annotations.insert(ANNOT_RUN_REQUESTED.into(), name.into());
    }
    updated
        .metadata
        .labels
        .get_or_insert_with(Default::default)
        .insert(ANNOT_TEAM.into(), team.name_any());
    match existing {
        Some(old) => {
            if serde_json::to_value(&old.spec)? == serde_json::to_value(&updated.spec)?
                && old.metadata.annotations == updated.metadata.annotations
                && old.metadata.labels == updated.metadata.labels
            {
                return Ok(old);
            }
            Ok(tasks
                .replace(name, &PostParams::default(), &updated)
                .await?)
        }
        None => Ok(tasks.create(&PostParams::default(), &updated).await?),
    }
}
