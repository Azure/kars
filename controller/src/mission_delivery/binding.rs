// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    Error, Prepared, Result, annotation, api_error, executable, identity, roots, same_snapshot,
    task_owner, workload,
};
use crate::{
    crd::KarsSandbox,
    kars_task::KarsTask,
    reconciler::{credential_sources, namespace_ownership},
};
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{ConfigMap, Namespace},
    },
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{Api, Client, ResourceExt, api::PostParams};
use serde_json::json;
use std::collections::BTreeMap;

async fn recheck(client: &Client, prepared: &Prepared, runtime: &workload::Workload) -> Result<()> {
    workload::recheck(client, runtime).await?;
    roots::recheck(
        client,
        roots::Anchor::Runtime(&prepared.sandbox),
        &prepared.namespace,
        &prepared.runtime_root.uid,
    )
    .await?;
    workload::recheck(client, &prepared.dispatcher).await?;
    let helper_pin = annotation(&prepared.dispatcher.deployment.metadata, roots::PIN)
        .ok_or(Error::Invalid("missing dispatcher root pin"))?;
    roots::recheck(
        client,
        roots::Anchor::Dispatcher(&prepared.dispatcher.deployment),
        &prepared.dispatcher_namespace,
        helper_pin,
    )
    .await?;
    let namespace = Api::<Namespace>::all(client.clone())
        .get_metadata(&prepared.namespace.name_any())
        .await
        .map_err(|e| api_error("recheck binding namespace", e))?;
    same_snapshot(&prepared.namespace.metadata, &namespace.metadata)?;
    let workspace = prepared
        .task
        .namespace()
        .ok_or(Error::Invalid("missing Task namespace"))?;
    let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), &workspace)
        .get_metadata(&prepared.sandbox.name_any())
        .await
        .map_err(|e| api_error("recheck binding sandbox", e))?;
    same_snapshot(&prepared.sandbox.metadata, &sandbox.metadata)?;
    let task = Api::<KarsTask>::namespaced(client.clone(), &workspace)
        .get_metadata(&prepared.task.name_any())
        .await
        .map_err(|e| api_error("recheck binding Task", e))?;
    same_snapshot(&prepared.task.metadata, &task.metadata)
}

fn owned(map: &ConfigMap, desired: &ConfigMap) -> Result<()> {
    identity(&map.metadata)?;
    if map.metadata.name != desired.metadata.name
        || map.metadata.namespace != desired.metadata.namespace
        || map.metadata.owner_references != desired.metadata.owner_references
        || map.immutable == Some(true)
    {
        return Err(Error::Invalid(
            "existing mission binding is outside Task custody",
        ));
    }
    Ok(())
}

pub(super) async fn publish(client: &Client, prepared: &Prepared) -> Result<()> {
    if !executable(&prepared.task, &prepared.sandbox) {
        return Err(Error::Invalid("mission is no longer executable"));
    }
    let namespace = namespace_ownership::recheck(client, &prepared.sandbox, &prepared.namespace)
        .await
        .map_err(|_| Error::Invalid("binding namespace custody changed"))?;
    same_snapshot(&prepared.namespace.metadata, &namespace.metadata)?;
    let deployment = Api::<Deployment>::namespaced(client.clone(), &namespace.name_any())
        .get(&prepared.sandbox.name_any())
        .await
        .map_err(|e| api_error("read applied mission Deployment", e))?;
    credential_sources::validate_owned_deployment(&deployment, &prepared.sandbox, &namespace)
        .map_err(|_| Error::Invalid("applied mission Deployment is outside sandbox custody"))?;
    // Check the actual applied identity environment, not just the desired manifest.
    let mut expected = deployment.clone();
    let env = expected
        .spec
        .as_mut()
        .and_then(|s| s.template.spec.as_mut())
        .and_then(|p| p.containers.iter_mut().find(|c| c.name == "openclaw"))
        .ok_or(Error::Invalid("applied OpenClaw container missing"))?
        .env
        .get_or_insert_with(Vec::new);
    env.retain(|e| !e.name.starts_with("KARS_MISSION_"));
    super::project(
        &mut expected,
        &prepared.task,
        &prepared.sandbox,
        &prepared.runtime_root.uid,
        &prepared.dispatcher_did,
    )?;
    if deployment.spec != expected.spec {
        return Err(Error::NotReady(
            "applied mission identity projection differs",
        ));
    }
    let runtime = workload::read(client, deployment).await?;
    let binding = json!({
        "taskName": prepared.task.name_any(), "taskUid": identity(&prepared.task.metadata)?.0,
        "sandboxName": prepared.sandbox.name_any(), "sandboxUid": identity(&prepared.sandbox.metadata)?.0,
        "namespaceUid": identity(&namespace.metadata)?.0,
        "deploymentUid": identity(&runtime.deployment.metadata)?.0, "deploymentGeneration": runtime.deployment.metadata.generation,
        "replicaSetName": runtime.replica_set.name_any(), "replicaSetUid": identity(&runtime.replica_set.metadata)?.0,
        "podName": runtime.pod.name_any(), "podUid": identity(&runtime.pod.metadata)?.0,
        "agentDid": super::identity::did(&prepared.runtime_root.value, super::identity::Role::Runtime,
            identity(&prepared.sandbox.metadata)?.0, identity(&runtime.pod.metadata)?.0)?,
        "dispatcherDid": prepared.dispatcher_did,
        "admission": super::admission::for_task(&prepared.task)?,
    });
    let name = format!("kars-mission-binding-{}", prepared.task.name_any());
    if name.len() > 253 {
        return Err(Error::Invalid("mission binding name too long"));
    }
    let workspace = prepared
        .task
        .namespace()
        .ok_or(Error::Invalid("missing Task namespace"))?;
    let mut desired = ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(workspace.clone()),
            owner_references: Some(vec![task_owner(&prepared.task)?]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            "binding.json".into(),
            binding.to_string(),
        )])),
        ..Default::default()
    };
    let maps = Api::<ConfigMap>::namespaced(client.clone(), &workspace);
    let prior = maps
        .get_opt(&name)
        .await
        .map_err(|e| api_error("read mission binding", e))?;
    if let Some(prior) = &prior {
        owned(prior, &desired)?;
        if prior.data == desired.data && prior.binary_data.is_none() {
            recheck(client, prepared, &runtime).await?;
            return Ok(());
        }
        desired.metadata = prior.metadata.clone();
        desired.binary_data = None;
    }
    recheck(client, prepared, &runtime).await?;
    // Independent resources cannot be made transactional. Consumers repeat the
    // complete currentness fence; a stale binding never authorizes execution.
    let written = if prior.is_some() {
        maps.replace(&name, &PostParams::default(), &desired).await
    } else {
        maps.create(&PostParams::default(), &desired).await
    }
    .map_err(|e| api_error("commit mission binding; no conflict adoption", e))?;
    owned(&written, &desired)?;
    if written.data != desired.data {
        return Err(Error::Invalid("mission binding write was altered"));
    }
    Ok(())
}
