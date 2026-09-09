// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

pub(crate) const PENDING: &str = "kars.azure.com/credential-rebind-pending";
pub(crate) const PAUSED: &str = "CredentialsPaused";
pub(crate) const HOLD: &str = "kars.azure.com/credential-rebind-task-uid";

pub(crate) fn pending(task: &KarsTask) -> bool {
    task.annotations()
        .get(PENDING)
        .is_some_and(|value| value == "true")
}

pub(super) async fn reconcile(task: &KarsTask, ctx: &Ctx) -> Result<(), ReconcileError> {
    let namespace = task.namespace().unwrap_or_else(|| "default".into());
    let api = Api::<KarsTask>::namespaced(ctx.client.clone(), &namespace);
    let mut status = task.status.clone().unwrap_or_default();
    status.phase = Some(PHASE_PENDING.into());
    status.observed_generation = task.metadata.generation;
    status.envelope_digest = None;
    status.execution_phase = Some("PausingCredentials".into());
    status.execution_detail =
        Some("Credential rebind requested; preserving owned runtime state".into());
    let condition = conditions::preserve_transition_time(
        status
            .conditions
            .as_ref()
            .and_then(|values| conditions::find(values, TYPE_READY)),
        TYPE_READY,
        cond_status::FALSE,
        "CredentialRebindPending",
        "Credential authority is paused until current owned consumers have stopped",
        task.metadata.generation,
    );
    conditions::set(status.conditions.get_or_insert_with(Vec::new), condition);
    let mut serialized = serde_json::to_value(&status)?;
    serialized["envelopeDigest"] = serde_json::Value::Null;
    let paused=api.patch_status(&task.name_any(),&PatchParams::default(),&Patch::Merge(json!({
        "metadata":{"uid":task.metadata.uid,"resourceVersion":task.metadata.resource_version},"status":serialized,
    }))).await?;
    // Retract the old attestation before replacing credential authority.
    reconcile_receipt(&ctx.client, &namespace, &paused, &status, &ctx.signer).await;
    let stopped = async {
        crate::kars_task_execution::hold_credential_runtime(&ctx.client, &paused).await?;
        crate::kars_task_execution::pause_credentials(&ctx.client, &paused).await?;
        crate::kars_task_execution::credentials_quiescent(&ctx.client, &paused).await
    }
    .await;
    match stopped {
        Ok(true) => {
            status.execution_phase = Some(PAUSED.into());
            status.execution_detail = Some(
                "Owned credential consumers stopped; Sandbox and namespace data retained".into(),
            );
        }
        Ok(false) => {
            status.execution_detail =
                Some("Waiting for old credential consumers, including terminating Pods".into())
        }
        Err(error) => {
            status.execution_detail = Some(format!("Owned credential pause is blocked: {error}"))
        }
    }
    let mut serialized = serde_json::to_value(&status)?;
    serialized["envelopeDigest"] = serde_json::Value::Null;
    api.patch_status(&task.name_any(),&PatchParams::default(),&Patch::Merge(json!({
        "metadata":{"uid":paused.metadata.uid,"resourceVersion":paused.metadata.resource_version},"status":serialized,
    }))).await?;
    Ok(())
}

pub(super) async fn resume(client: &Client, task: &KarsTask) -> Result<(), String> {
    use crate::{crd::KarsSandbox, kars_receipt::KarsReceipt};
    if !crate::credential_grants::readiness::selected(task) {
        return Ok(());
    }
    let workspace = task
        .namespace()
        .ok_or("Credential resume workspace missing")?;
    let tasks = Api::<KarsTask>::namespaced(client.clone(), &workspace);
    let current = tasks
        .get(&task.name_any())
        .await
        .map_err(|_| "Credential resume Task unavailable")?;
    if current.uid() != task.uid()
        || !task_is_ready(&current)
        || !current
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch)
    {
        return Ok(());
    }
    let mut team_pin = None;
    if let Some(owner) = current
        .metadata
        .owner_references
        .as_ref()
        .and_then(|owners| {
            owners
                .iter()
                .find(|owner| owner.kind == "KarsTeam" && owner.controller == Some(true))
        })
    {
        let team = Api::<crate::kars_team::KarsTeam>::namespaced(client.clone(), &workspace)
            .get(&owner.name)
            .await
            .map_err(|_| "Credential resume Team unavailable")?;
        if team.uid().as_deref() != Some(owner.uid.as_str())
            || team.spec.paused
            || team.metadata.deletion_timestamp.is_some()
        {
            return Ok(());
        }
        let configured = json!({
            "credentialBindings":current.spec.blueprint.as_ref().and_then(|b|b.credential_bindings.as_ref()),
            "githubBinding":current.spec.blueprint.as_ref().and_then(|b|b.github_binding.as_ref()),
        });
        if crate::kars_team_reconciler::credential_bindings::desired(&team, &current).as_ref()
            != Some(&configured)
        {
            return Ok(());
        }
        team_pin = Some((team.name_any(), team.uid(), team.metadata.generation));
    }
    let sandboxes = Api::<KarsSandbox>::namespaced(client.clone(), &workspace);
    let Some(sandbox) = sandboxes
        .get_opt(&current.name_any())
        .await
        .map_err(|_| "Credential resume Sandbox unavailable")?
    else {
        return Ok(());
    };
    if sandbox.annotations().get(HOLD) != current.metadata.uid.as_ref() {
        return Ok(());
    }
    if sandbox
        .metadata
        .owner_references
        .as_ref()
        .is_none_or(|owners| {
            !owners.iter().any(|owner| {
                owner.kind == "KarsTask"
                    && owner.controller == Some(true)
                    && Some(&owner.uid) == current.metadata.uid.as_ref()
            })
        })
        || sandbox.metadata.deletion_timestamp.is_some()
    {
        return Err("Credential resume Sandbox ownership changed".into());
    }
    let desired = crate::kars_task::blueprint::effective_blueprint(&current.spec);
    if sandbox.spec.credential_bindings != desired.credential_bindings
        || sandbox.spec.github_binding != desired.github_binding
    {
        return Ok(());
    }
    let receipt = Api::<KarsReceipt>::namespaced(client.clone(), &workspace)
        .get_opt(&current.name_any())
        .await
        .map_err(|_| "Credential resume attestation unavailable")?;
    let Some(receipt) = receipt else {
        return Ok(());
    };
    if receipt.metadata.deletion_timestamp.is_some()
        || receipt
            .metadata
            .owner_references
            .as_ref()
            .is_none_or(|owners| {
                !owners.iter().any(|owner| {
                    owner.kind == "KarsTask"
                        && owner.controller == Some(true)
                        && Some(&owner.uid) == current.metadata.uid.as_ref()
                })
            })
        || receipt.spec.envelope_digest != current.envelope_digest()
    {
        return Ok(());
    }
    if !crate::kars_task_execution::credentials_quiescent(client, &current).await? {
        return Ok(());
    }
    let latest = tasks
        .get(&current.name_any())
        .await
        .map_err(|_| "Credential resume Task recheck failed")?;
    if latest.resource_version() != current.resource_version() || !task_is_ready(&latest) {
        return Ok(());
    }
    if let Some((name, uid, generation)) = team_pin {
        let team = Api::<crate::kars_team::KarsTeam>::namespaced(client.clone(), &workspace)
            .get(&name)
            .await
            .map_err(|_| "Credential resume Team recheck failed")?;
        if team.uid() != uid
            || team.metadata.generation != generation
            || team.spec.paused
            || team.metadata.deletion_timestamp.is_some()
        {
            return Ok(());
        }
    }
    sandboxes.patch_metadata(&current.name_any(),&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":sandbox.metadata.uid,"resourceVersion":sandbox.metadata.resource_version,"annotations":{HOLD:null}}
            }))).await.map_err(|_|"Credential runtime resume conflicted")?;
    Ok(())
}

pub(crate) async fn fence_deployment(
    client: &Client,
    sandbox: &crate::crd::KarsSandbox,
    deployment: &mut k8s_openapi::api::apps::v1::Deployment,
    identity: &serde_json::Value,
) -> Result<(), String> {
    let Some(owner) = sandbox
        .metadata
        .owner_references
        .as_ref()
        .and_then(|owners| {
            owners
                .iter()
                .find(|owner| owner.kind == "KarsTask" && owner.controller == Some(true))
        })
    else {
        return Ok(());
    };
    let workspace = sandbox
        .namespace()
        .ok_or("Task runtime workspace missing")?;
    let runtime = format!("kars-{}", sandbox.name_any());
    let prior = Api::<k8s_openapi::api::apps::v1::Deployment>::namespaced(client.clone(), &runtime)
        .get_opt(&sandbox.name_any())
        .await
        .map_err(|_| "Task runtime deployment recheck failed")?;
    let live = Api::<crate::crd::KarsSandbox>::namespaced(client.clone(), &workspace)
        .get(&sandbox.name_any())
        .await
        .map_err(|_| "Task runtime source recheck failed")?;
    if live.uid() != sandbox.uid()
        || live.metadata.generation != sandbox.metadata.generation
        || live.metadata.deletion_timestamp.is_some()
    {
        return Err("Task runtime source changed before deployment apply".into());
    }
    let task = Api::<KarsTask>::namespaced(client.clone(), &workspace)
        .get(&owner.name)
        .await
        .map_err(|_| "Task runtime authority recheck failed")?;
    if task.uid().as_deref() != Some(owner.uid.as_str()) {
        return Err("Task runtime owner changed".into());
    }
    if pending(&task)
        || live.annotations().contains_key(HOLD)
        || live.spec.suspended.unwrap_or(false)
        || !task_is_ready(&task)
        || !task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch)
    {
        deployment
            .spec
            .as_mut()
            .ok_or("Task runtime deployment spec missing")?
            .replicas = Some(0);
    } else if identity["task_authorization"] != task.envelope_digest()
        || identity["task_generation"] != json!(task.metadata.generation)
    {
        return Err("Task runtime authorization changed before deployment apply".into());
    }
    if let Some(prior) = prior {
        let namespace = Api::<k8s_openapi::api::core::v1::Namespace>::all(client.clone())
            .get(&runtime)
            .await
            .map_err(|_| "Task runtime namespace recheck failed")?;
        crate::reconciler::namespace_ownership::recheck(client, &live, &namespace)
            .await
            .map_err(|e| e.to_string())?;
        crate::reconciler::credential_sources::validate_owned_deployment(&prior, &live, &namespace)
            .map_err(|e| e.to_string())?;
        deployment.metadata.uid = prior.metadata.uid;
        deployment.metadata.resource_version = prior.metadata.resource_version;
    }
    Ok(())
}
#[cfg(test)]
mod tests;

pub(crate) async fn apply_deployment(
    client: &Client,
    sandbox: &crate::crd::KarsSandbox,
    mut deployment: k8s_openapi::api::apps::v1::Deployment,
    identity: &serde_json::Value,
) -> Result<(), String> {
    fence_deployment(client, sandbox, &mut deployment, identity).await?;
    Api::<k8s_openapi::api::apps::v1::Deployment>::namespaced(
        client.clone(),
        &format!("kars-{}", sandbox.name_any()),
    )
    .patch(
        &sandbox.name_any(),
        &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
        &Patch::Apply(deployment),
    )
    .await
    .map_err(|e| crate::credential_grants::api_error("Apply current task credential runtime", e))?;
    Ok(())
}
