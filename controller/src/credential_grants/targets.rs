// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::super::*;
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};

pub(super) async fn read(
    client: &Client,
    target: &CredentialTarget,
) -> Result<DynamicObject, String> {
    if !["KarsSandbox", "KarsTask", "KarsTeam"].contains(&target.kind.as_str())
        || target.namespace.is_empty()
        || target.name.is_empty()
        || target.uid.is_empty()
    {
        return Err("Credential target requires a complete supported UID-bound identity".into());
    }
    let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
        "kars.azure.com",
        "v1alpha1",
        &target.kind,
    ));
    let object =
        Api::<DynamicObject>::namespaced_with(client.clone(), &target.namespace, &resource)
            .get(&target.name)
            .await
            .map_err(|e| api_error("Read credential target", e))?;
    if identity(&object.metadata)?.0 != target.uid {
        return Err("Credential target was replaced".into());
    }
    Ok(object)
}

pub(super) async fn owner_allowed(
    client: &Client,
    target: &CredentialTarget,
    owner: &CredentialTarget,
    candidate: Option<&crate::kars_task::KarsTask>,
) -> Result<(), String> {
    if owner.namespace != target.namespace {
        return Err("Credential owners cannot cross workspaces".into());
    }
    read(client, owner).await?;
    if owner == target {
        return Ok(());
    }
    if target.kind != "KarsTask" {
        return Err("Credential owner is not the target".into());
    }
    let tasks: Api<crate::kars_task::KarsTask> = Api::namespaced(client.clone(), &target.namespace);
    let mut name = target.name.clone();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..64 {
        let task = tasks
            .get(&name)
            .await
            .map_err(|e| api_error("Read credential delegation ancestor", e))?;
        let uid = identity(&task.metadata)?.0;
        let checking_target = candidate.is_some_and(|candidate| {
            task.metadata.uid == candidate.metadata.uid
                && task.metadata.generation == candidate.metadata.generation
                && task.metadata.namespace == candidate.metadata.namespace
                && task.metadata.name == candidate.metadata.name
                && task.uid().as_deref() == Some(target.uid.as_str())
        });
        if !seen.insert(uid.to_string())
            || (!checking_target && !crate::kars_task_reconciler::task_is_ready(&task))
        {
            return Err("Credential delegation ancestry is stale or cyclic".into());
        }
        if owner.kind == "KarsTask" && task.name_any() == owner.name && uid == owner.uid {
            return Ok(());
        }
        if owner.kind == "KarsTeam"
            && task.metadata.owner_references.as_ref().is_some_and(|refs| {
                refs.iter().any(|r| {
                    r.api_version == "kars.azure.com/v1alpha1"
                        && r.kind == "KarsTeam"
                        && r.name == owner.name
                        && r.uid == owner.uid
                        && r.controller == Some(true)
                })
            })
        {
            return Ok(());
        }
        name = task
            .spec
            .parent_ref
            .as_ref()
            .ok_or("Credential owner is outside the authorized ancestry")?
            .name
            .clone();
    }
    Err("Credential ancestry exceeds the supported depth".into())
}
