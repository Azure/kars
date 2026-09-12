// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Task recovery requires the current materialized Sandbox caller, not just a
//! Task-kind target. Only executionPhase/executionDetail and its proven
//! sandboxRef may change; governance status, generation, spec and owners cannot.

use super::*;
use crate::{crd::KarsSandbox, kars_task::KarsTask};

fn typed_task(object: &DynamicObject) -> Result<KarsTask, String> {
    serde_json::from_value(
        serde_json::to_value(object).map_err(|_| "Task recovery serialization failed")?,
    )
    .map_err(|_| "Task recovery target is malformed".into())
}

fn ready(
    task: &KarsTask,
    target: &CredentialTarget,
    bindings: &CredentialBindings,
) -> Result<(), String> {
    if target.kind != "KarsTask"
        || identity(&task.metadata)?.0 != target.uid
        || task.name_any() != target.name
        || task.namespace().as_deref() != Some(target.namespace.as_str())
        || !task
            .metadata
            .generation
            .is_some_and(|generation| generation > 0)
        || !crate::kars_task_reconciler::task_is_ready(task)
        || !task.spec.execution.launch
        || task
            .spec
            .blueprint
            .as_ref()
            .and_then(|blueprint| blueprint.credential_bindings.as_ref())
            != Some(bindings)
    {
        return Err(
            "Task bundle recovery requires unchanged current Task authorization and bindings"
                .into(),
        );
    }
    Ok(())
}

fn status_fields(mut status: Value, target: &CredentialTarget) -> Result<Value, String> {
    let fields = status
        .as_object_mut()
        .ok_or("Task recovery status is malformed")?;
    if fields.get("executionPhase").is_some_and(|value| {
        !value.is_null() && !matches!(value.as_str(), Some("Launching" | "Running" | "Degraded"))
    }) || fields
        .get("executionDetail")
        .is_some_and(|value| !value.is_null() && !value.is_string())
        || fields
            .get("sandboxRef")
            .is_some_and(|value| !value.is_null() && value != &json!({"name":target.name}))
    {
        return Err("Task bundle recovery encountered an unsupported execution transition".into());
    }
    fields.remove("executionPhase");
    fields.remove("executionDetail");
    fields.remove("sandboxRef");
    Ok(status)
}

fn status_authority(task: &KarsTask, target: &CredentialTarget) -> Result<Value, String> {
    status_fields(
        serde_json::to_value(task.status.as_ref().ok_or("Task recovery status missing")?)
            .map_err(|_| "Task recovery status serialization failed")?,
        target,
    )
}

fn same_task(
    before: &KarsTask,
    after: &KarsTask,
    target: &CredentialTarget,
    bindings: &CredentialBindings,
) -> Result<(), String> {
    ready(before, target, bindings)?;
    ready(after, target, bindings)?;
    let view = |task: &KarsTask| -> Result<(kube::api::ObjectMeta, Value), String> {
        let object: DynamicObject = serde_json::from_value(
            serde_json::to_value(task).map_err(|_| "Task recovery serialization failed")?,
        )
        .map_err(|_| "Task recovery object is malformed")?;
        authority_view(&object, target, false)
    };
    if before.metadata.generation != after.metadata.generation
        || view(before)? != view(after)?
        || status_authority(before, target)? != status_authority(after, target)?
    {
        return Err("Task bundle authority changed during anchor recovery".into());
    }
    Ok(())
}

pub(super) fn verify_transition(
    before: &DynamicObject,
    after: &DynamicObject,
    target: &CredentialTarget,
    bindings: &CredentialBindings,
) -> Result<(), String> {
    if authority_view(before, target, false)? != authority_view(after, target, false)?
        || status_fields(before.data["status"].clone(), target)?
            != status_fields(after.data["status"].clone(), target)?
    {
        return Err("Task bundle target metadata or spec changed during anchor recovery".into());
    }
    same_task(&typed_task(before)?, &typed_task(after)?, target, bindings)
}

fn consumer_authority(
    sandbox: &KarsSandbox,
    target: &CredentialTarget,
    bindings: &CredentialBindings,
    bundle_uid: Option<&str>,
) -> Result<(kube::api::ObjectMeta, Value), String> {
    identity(&sandbox.metadata)?;
    if sandbox.name_any() != target.name
        || sandbox.namespace().as_deref() != Some(target.namespace.as_str())
        || !sandbox
            .metadata
            .generation
            .is_some_and(|generation| generation > 0)
        || sandbox.spec.credential_bindings.as_ref() != Some(bindings)
        || sandbox
            .spec
            .credentials_ref
            .as_ref()
            .is_some_and(|reference| {
                reference.name != bundle_name(target) || Some(reference.uid.as_str()) != bundle_uid
            })
        || !sandbox
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| {
                owners.len() == 1
                    && owners[0].kind == "KarsTask"
                    && owners[0].api_version == "kars.azure.com/v1alpha1"
                    && owners[0].name == target.name
                    && owners[0].uid == target.uid
                    && owners[0].controller == Some(true)
            })
    {
        return Err("Task bundle recovery caller is not its exact v2 materialized Sandbox".into());
    }
    let mut metadata = sandbox.metadata.clone();
    metadata.resource_version = None;
    metadata.managed_fields = None;
    Ok((
        metadata,
        serde_json::to_value(&sandbox.spec)
            .map_err(|_| "Task bundle consumer spec serialization failed")?,
    ))
}

pub(super) async fn verify_caller(
    client: &Client,
    consumer: TaskConsumer<'_>,
    target_object: &DynamicObject,
    target: &CredentialTarget,
    bindings: &CredentialBindings,
) -> Result<(), String> {
    same_task(consumer.task, &typed_task(target_object)?, target, bindings)?;
    let bundle_uid = annotation(&target_object.metadata, UID_ANNOTATION);
    let expected = consumer_authority(consumer.sandbox, target, bindings, bundle_uid)?;
    let current = Api::<KarsSandbox>::namespaced(client.clone(), &target.namespace)
        .get(&target.name)
        .await
        .map_err(|error| api_error("Recheck materialized Task bundle caller", error))?;
    if consumer_authority(&current, target, bindings, bundle_uid)? != expected {
        return Err("Materialized Task bundle caller changed identity, ownership or spec".into());
    }
    Ok(())
}
