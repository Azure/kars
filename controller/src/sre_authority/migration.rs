// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{api_error, check_secret_denial};
use crate::{
    crd::KarsSandbox,
    sre_registration::{CONTROL_VERSION, EPOCH, KarsSRERegistration, OWNER, RUNTIME_NAMESPACE},
};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Pod, Secret},
};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams},
};
use serde_json::json;

const STOPPED: &str = "kars.azure.com/sre-migration-stopped";
pub(super) const WAITING_FOR_CONSUMERS: &str =
    "Waiting for legacy SRE consumers to terminate; no private credentials issued";
const WAITING_FOR_ROTATION: &str =
    "Waiting for owned control credential consumers to restart on the new privacy epoch";
const WAITING_FOR_ROLLOUT: &str =
    "Waiting for prior-epoch or prior-control-version consumers to terminate";

pub(super) fn is_waiting(detail: &str) -> bool {
    matches!(
        detail,
        WAITING_FOR_CONSUMERS | WAITING_FOR_ROTATION | WAITING_FOR_ROLLOUT
    )
}

pub(super) async fn stop_registered_consumer_for_retirement(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<(), String> {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let Some(namespace) = namespaces
        .get_opt(RUNTIME_NAMESPACE)
        .await
        .map_err(|e| api_error("Read retiring SRE namespace", e))?
    else {
        return Ok(());
    };
    if namespace.metadata.uid.as_deref() != Some(reg.spec.runtime_namespace.uid.as_str()) {
        return Ok(());
    }
    let api: Api<Deployment> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    let Some(deployment) = api
        .get_opt("sre")
        .await
        .map_err(|e| api_error("Read retiring SRE consumer", e))?
    else {
        return Ok(());
    };
    let ours = deployment
        .spec
        .as_ref()
        .and_then(|s| s.template.metadata.as_ref())
        .and_then(|m| m.annotations.as_ref())
        .and_then(|a| a.get(OWNER))
        == reg.metadata.uid.as_ref();
    if !ours
        && reg
            .status
            .as_ref()
            .and_then(|s| s.router_service_account_uid.as_ref())
            .is_none()
    {
        return Ok(());
    }
    stop_legacy_consumer(client, reg).await
}

fn current_boundary(deployment: &Deployment, reg: &KarsSRERegistration) -> bool {
    deployment.spec.as_ref().is_some_and(|spec| {
        spec.template
            .metadata
            .as_ref()
            .and_then(|m| m.annotations.as_ref())
            .is_some_and(|a| {
                a.get(OWNER) == reg.metadata.uid.as_ref() && a.get(EPOCH) == Some(&reg.epoch())
            })
    })
}

pub(super) async fn validate_consumer(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<(), String> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    let Some(deployment) = api
        .get_opt("sre")
        .await
        .map_err(|e| api_error("Read SRE consumer", e))?
    else {
        return Ok(());
    };
    if current_boundary(&deployment, reg) {
        return Ok(());
    }
    if deployment
        .spec
        .as_ref()
        .and_then(|s| s.template.metadata.as_ref())
        .and_then(|m| m.annotations.as_ref())
        .and_then(|a| a.get(OWNER))
        == reg.metadata.uid.as_ref()
    {
        return Ok(());
    }
    let reviewed = reg
        .spec
        .legacy_consumer
        .as_ref()
        .ok_or("Existing SRE consumer requires an explicit UID/resourceVersion review")?;
    let was_stopped = deployment
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(STOPPED))
        == reg.metadata.uid.as_ref();
    if deployment.metadata.uid.as_deref() != Some(reviewed.uid.as_str())
        || deployment.metadata.deletion_timestamp.is_some()
        || (!was_stopped
            && deployment.metadata.resource_version.as_deref()
                != Some(reviewed.resource_version.as_str()))
    {
        return Err("Reviewed SRE consumer was changed or replaced".into());
    }
    Ok(())
}

pub(super) async fn stop_legacy_consumer(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<(), String> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(deployment) = api
        .get_opt("sre")
        .await
        .map_err(|e| api_error("Read SRE consumer for retirement", e))?
    {
        if reg.spec.enabled && current_boundary(&deployment, reg) {
            return Ok(());
        }
        validate_consumer(client, reg).await?;
        if deployment.spec.as_ref().and_then(|spec| spec.replicas) != Some(0) {
            api.patch("sre",&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":deployment.metadata.uid,"resourceVersion":deployment.metadata.resource_version,
                    "annotations":{STOPPED:reg.metadata.uid}},
                "spec":{"replicas":0},
            }))).await.map_err(|e|api_error("Stop reviewed SRE consumer",e))?;
        }
    }
    let pods: Api<Pod> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    let pods = pods
        .list(&ListParams::default())
        .await
        .map_err(|e| api_error("Wait for old SRE consumers", e))?;
    if pods.iter().any(|pod| {
        pod.spec
            .as_ref()
            .and_then(|spec| spec.service_account_name.as_deref())
            == Some("sandbox")
    }) {
        return Err(WAITING_FOR_CONSUMERS.into());
    }
    Ok(())
}

pub(crate) fn controller_managed(deployment: &Deployment, sandbox: &str) -> bool {
    deployment
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("kars.azure.com/sandbox"))
        .map(String::as_str)
        == Some(sandbox)
        && deployment
            .metadata
            .managed_fields
            .as_ref()
            .is_some_and(|fields| {
                fields.iter().any(|field| {
                    field.manager.as_deref() == Some(crate::field_managers::CLAWSANDBOX)
                        && field
                            .fields_v1
                            .as_ref()
                            .is_some_and(|fields| fields.0.get("f:spec").is_some())
                })
            })
}

/// Existing #550 control credentials may have been exposed by the old SRE
/// identity. Rotate only proven controller-owned instances and restart their
/// owned consumer; the old router caches the token at startup.
pub(super) async fn rotate_owned_control_credentials(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<(), String> {
    let all: Api<Secret> = Api::all(client.clone());
    let list = all
        .list(&ListParams::default().fields("metadata.name=router-services-admin"))
        .await
        .map_err(|e| api_error("Inventory owned control credentials", e))?;
    for secret in list {
        if secret
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
            .map(String::as_str)
            != Some("kars-controller")
        {
            continue;
        }
        if secret.metadata.uid.as_deref().is_none_or(str::is_empty)
            || secret
                .metadata
                .resource_version
                .as_deref()
                .is_none_or(str::is_empty)
            || secret.metadata.deletion_timestamp.is_some()
            || secret
                .metadata
                .owner_references
                .as_ref()
                .is_some_and(|owners| !owners.is_empty())
        {
            return Err("Owned control credential identity is incomplete or terminating".into());
        }
        let Some(namespace_name) = secret.namespace() else {
            continue;
        };
        let annotations = secret.metadata.annotations.as_ref();
        let Some(source_uid) = annotations.and_then(|a| a.get("kars.azure.com/sandbox-uid")) else {
            continue;
        };
        let Some(namespace_uid) = annotations.and_then(|a| a.get("kars.azure.com/namespace-uid"))
        else {
            continue;
        };
        let namespaces: Api<Namespace> = Api::all(client.clone());
        let namespace = namespaces
            .get(&namespace_name)
            .await
            .map_err(|e| api_error("Read control credential namespace", e))?;
        if namespace.metadata.uid.as_deref() != Some(namespace_uid)
            || namespace.metadata.deletion_timestamp.is_some()
        {
            return Err("Control credential namespace changed".into());
        }
        let ns_annotations = namespace
            .metadata
            .annotations
            .as_ref()
            .ok_or("Control namespace claim is absent")?;
        let workspace = ns_annotations
            .get("kars.azure.com/sandbox-namespace")
            .ok_or("Control namespace source is absent")?;
        let name = ns_annotations
            .get("kars.azure.com/sandbox-name")
            .ok_or("Control namespace owner is absent")?;
        let sandboxes: Api<KarsSandbox> = Api::namespaced(client.clone(), workspace);
        let sandbox = sandboxes
            .get(name)
            .await
            .map_err(|e| api_error("Read control credential owner", e))?;
        if sandbox.metadata.uid.as_deref() != Some(source_uid)
            || sandbox.metadata.deletion_timestamp.is_some()
            || !crate::reconciler::namespace_ownership::claimed(&namespace, &sandbox)
                .map_err(|_| "Control namespace ownership is invalid")?
        {
            return Err("Control credential ownership changed; no rotation was authorized".into());
        }
        check_secret_denial(client, &namespace_name).await?;
        let deployments: Api<Deployment> = Api::namespaced(client.clone(), &namespace_name);
        let Some(deployment) = deployments
            .get_opt(name)
            .await
            .map_err(|e| api_error("Read control credential consumer", e))?
        else {
            return Err("Owned control credential has no verified consumer Deployment".into());
        };
        if !controller_managed(&deployment, name) {
            return Err("Control credential consumer is not controller-owned".into());
        }
        let epoch = reg.epoch();
        let rotated = annotations.and_then(|a| a.get(EPOCH)) != Some(&epoch);
        let secret = if rotated {
            let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace_name);
            secrets.patch("router-services-admin",&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":secret.metadata.uid,"resourceVersion":secret.metadata.resource_version,
                    "annotations":{EPOCH:epoch}},
                "stringData":{"control-token":crate::providers::signing::generate_service_token()},
            }))).await.map_err(|e|api_error("Rotate owned control credential",e))?
        } else {
            secret
        };
        let version = format!(
            "{}:{}",
            secret
                .metadata
                .uid
                .as_deref()
                .ok_or("Control credential UID missing")?,
            secret
                .metadata
                .resource_version
                .as_deref()
                .ok_or("Control credential version missing")?
        );
        let template_annotations = deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|meta| meta.annotations.as_ref());
        let require_version = rotated
            || !super::live::currently_qualified(reg)
            || template_annotations
                .is_some_and(|annotations| annotations.contains_key(CONTROL_VERSION));
        if template_annotations.and_then(|a| a.get(EPOCH)) != Some(&epoch)
            || (require_version
                && template_annotations.and_then(|a| a.get(CONTROL_VERSION)) != Some(&version))
        {
            deployments.patch(name,&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":deployment.metadata.uid,"resourceVersion":deployment.metadata.resource_version},
                "spec":{"template":{"metadata":{"annotations":{EPOCH:epoch,CONTROL_VERSION:version}}}},
            }))).await.map_err(|e|api_error("Restart owned control credential consumer",e))?;
            return Err(WAITING_FOR_ROTATION.into());
        }
        let selector = deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.match_labels.as_ref())
            .filter(|labels| !labels.is_empty())
            .ok_or("Owned control credential consumer has no bounded Pod selector")?
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        let pods = Api::<Pod>::namespaced(client.clone(), &namespace_name)
            .list(&ListParams::default().labels(&selector))
            .await
            .map_err(|e| api_error("Verify old control credential consumers have terminated", e))?;
        if pods.iter().any(|pod| {
            let annotations = pod.metadata.annotations.as_ref();
            annotations.and_then(|annotations| annotations.get(EPOCH)) != Some(&epoch)
                || (require_version
                    && annotations.and_then(|annotations| annotations.get(CONTROL_VERSION))
                        != Some(&version))
        }) {
            // Rollout availability alone can exclude a still-terminating old
            // router which continues accepting its startup-cached control token.
            return Err(WAITING_FOR_ROLLOUT.into());
        }
        // Privacy completion is credential/template identity plus termination
        // of old caches, not workload availability. In particular, the SRE
        // readiness endpoint itself requires this authority to become Ready.
    }
    Ok(())
}
