// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use kube::api::{ListParams, Patch, PatchParams};
use serde_json::json;

use crate::kars_task_reconciler::rebind::{PAUSED, PENDING};

pub(crate) fn desired(team: &KarsTeam, task: &KarsTask) -> Option<serde_json::Value> {
    let blueprint = match task.annotations().get(ANNOT_TEAM_ROLE).map(String::as_str) {
        Some("principal") => specs::principal_spec(team).blueprint,
        Some("member") => {
            specs::member_spec(
                team,
                team.spec
                    .roster
                    .iter()
                    .find(|role| specs::member_name(team, role) == task.name_any())?,
            )
            .blueprint
        }
        Some("taskforce") => team.spec.blueprint.clone(),
        _ => return None,
    };
    Some(
        json!({"credentialBindings":blueprint.as_ref().and_then(|b|b.credential_bindings.as_ref()),
        "githubBinding":blueprint.as_ref().and_then(|b|b.github_binding.as_ref())}),
    )
}

pub(crate) async fn reconcile(
    client: &Client,
    api: &Api<KarsTask>,
    team: &KarsTeam,
) -> Result<(), ReconcileError> {
    for task in api.list(&ListParams::default()).await? {
        if !tasks::owned(&task.metadata, team) || task.metadata.deletion_timestamp.is_some() {
            continue;
        }
        if task
            .annotations()
            .get("kars.azure.com/run-completed")
            .is_some_and(|completed| task.annotations().get(ANNOT_RUN_REQUESTED) == Some(completed))
        {
            continue;
        }
        let Some(desired) = desired(team, &task) else {
            continue;
        };
        let pending = task
            .annotations()
            .get(PENDING)
            .is_some_and(|value| value == "true");
        let active = task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch);
        if !active {
            if pending {
                api.patch(&task.name_any(),&PatchParams::default(),&Patch::Merge(json!({
                    "metadata":{"uid":task.metadata.uid,"resourceVersion":task.metadata.resource_version,"annotations":{PENDING:null}}
                }))).await?;
            }
            continue;
        }
        let current = json!({
            "credentialBindings":task.spec
                .blueprint
                .as_ref()
                .and_then(|blueprint| blueprint.credential_bindings.as_ref()),
            "githubBinding":task.spec.blueprint.as_ref().and_then(|blueprint|blueprint.github_binding.as_ref()),
        });
        if current == desired && !pending {
            continue;
        }
        let uid = task
            .uid()
            .ok_or_else(|| ReconcileError::Invalid("Credential run UID missing".into()))?;
        let version = task.resource_version().ok_or_else(|| {
            ReconcileError::Invalid("Credential run resourceVersion missing".into())
        })?;
        if team.spec.paused {
            continue;
        }
        if !pending {
            api.patch(
                &task.name_any(),
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata":{"uid":uid,"resourceVersion":version,"annotations":{PENDING:"true"}},
                })),
            )
            .await?;
            continue;
        }
        if task.status.as_ref().is_none_or(|status| {
            status.execution_phase.as_deref() != Some(PAUSED)
                || status.observed_generation != task.metadata.generation
                || status.envelope_digest.is_some()
                || status.conditions.as_ref().is_none_or(|conditions| {
                    !conditions
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "False")
                })
        }) {
            continue;
        }
        if !crate::kars_task_execution::credentials_quiescent(client, &task)
            .await
            .map_err(ReconcileError::Invalid)?
        {
            continue;
        }
        if current["credentialBindings"].is_object() && !desired["credentialBindings"].is_object() {
            return Err(ReconcileError::Invalid(
                "Governed credential removal requires explicit retirement; runtime remains paused"
                    .into(),
            ));
        }
        let namespace = team
            .namespace()
            .ok_or_else(|| ReconcileError::Invalid("Team workspace missing".into()))?;
        let latest = Api::<KarsTeam>::namespaced(client.clone(), &namespace)
            .get(&team.name_any())
            .await?;
        if latest.uid() != team.uid()
            || latest.metadata.generation != team.metadata.generation
            || latest.metadata.deletion_timestamp.is_some()
            || latest.spec.paused
        {
            continue;
        }
        api.patch(
            &task.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata":{"uid":uid,"resourceVersion":version,"annotations":{PENDING:null}},
                "spec":{"blueprint":desired}
            })),
        )
        .await?;
    }
    Ok(())
}
