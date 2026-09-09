// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{api_error, check_secret_denial};
use crate::{
    crd::KarsSandbox,
    sre_registration::{EPOCH, KarsSRERegistration, OWNER, RUNTIME_NAMESPACE},
};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Pod, Secret},
};
use kube::{
    Api, Client, ResourceExt,
    api::{DeleteParams, ListParams, Patch, PatchParams, Preconditions},
};
use serde_json::json;

const STOPPED: &str = "kars.azure.com/sre-migration-stopped";
pub(super) const WAITING_FOR_CONSUMERS: &str =
    "Waiting for legacy SRE consumers to terminate; no private credentials issued";
const WAITING_FOR_ROTATION: &str =
    "Waiting for owned control credential consumers to restart on the new privacy epoch";
const WAITING_FOR_ROLLOUT: &str =
    "Owned control credential consumer has not completed its privacy-epoch rollout";
const WAITING_FOR_CONSUMER_REMOVAL: &str =
    "Waiting for the owned SRE consumer Deployment to be removed";

pub(super) fn is_waiting(detail: &str) -> bool {
    matches!(
        detail,
        WAITING_FOR_CONSUMERS
            | WAITING_FOR_ROTATION
            | WAITING_FOR_ROLLOUT
            | WAITING_FOR_CONSUMER_REMOVAL
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
    let registration_uid = reg
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or("SRE retirement requires the actual registration UID")?;
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
        .map(String::as_str)
        == Some(registration_uid);
    if !ours
        && reg
            .status
            .as_ref()
            .and_then(|s| s.router_service_account_uid.as_ref())
            .is_none()
    {
        return Ok(());
    }
    stop_legacy_consumer(client, reg).await?;
    if !ours {
        return Ok(());
    }
    // Namespace-controller intentionally lacks registrar power. Remove only
    // our stopped Deployment here instead of leaving protected content to GC.
    let Some(current) = api
        .get_opt("sre")
        .await
        .map_err(|error| api_error("Read stopped SRE consumer", error))?
    else {
        return Ok(());
    };
    let uid = deployment
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or("Retiring SRE consumer UID is missing")?;
    let still_owned = current
        .spec
        .as_ref()
        .and_then(|spec| spec.template.metadata.as_ref())
        .and_then(|metadata| metadata.annotations.as_ref())
        .and_then(|annotations| annotations.get(OWNER))
        .map(String::as_str)
        == Some(registration_uid);
    if current.metadata.uid.as_deref() != Some(uid)
        || !still_owned
        || current.spec.as_ref().and_then(|spec| spec.replicas) != Some(0)
    {
        return Err("Retiring SRE consumer changed after quiescence; preserved".into());
    }
    if current.metadata.deletion_timestamp.is_some() {
        return Err(WAITING_FOR_CONSUMER_REMOVAL.into());
    }
    let version = current
        .metadata
        .resource_version
        .filter(|version| !version.is_empty())
        .ok_or("Retiring SRE consumer resourceVersion is missing")?;
    match api
        .delete(
            "sre",
            &DeleteParams {
                preconditions: Some(Preconditions {
                    uid: Some(uid.into()),
                    resource_version: Some(version),
                }),
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => {}
        Err(kube::Error::Api(error)) if error.code == 404 => {}
        Err(error) => return Err(api_error("Remove stopped owned SRE consumer", error)),
    }
    if api
        .get_opt("sre")
        .await
        .map_err(|error| api_error("Verify owned SRE consumer removal", error))?
        .is_some()
    {
        return Err(WAITING_FOR_CONSUMER_REMOVAL.into());
    }
    Ok(())
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

fn controller_managed(deployment: &Deployment, sandbox: &str) -> bool {
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
        if annotations.and_then(|a| a.get(EPOCH)) != Some(&epoch) {
            let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace_name);
            secrets.patch("router-services-admin",&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":secret.metadata.uid,"resourceVersion":secret.metadata.resource_version,
                    "annotations":{EPOCH:epoch}},
                "stringData":{"control-token":crate::providers::signing::generate_service_token()},
            }))).await.map_err(|e|api_error("Rotate owned control credential",e))?;
        }
        if deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|meta| meta.annotations.as_ref())
            .and_then(|a| a.get(EPOCH))
            != Some(&epoch)
        {
            deployments.patch(name,&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":deployment.metadata.uid,"resourceVersion":deployment.metadata.resource_version},
                "spec":{"template":{"metadata":{"annotations":{EPOCH:epoch}}}},
            }))).await.map_err(|e|api_error("Restart owned control credential consumer",e))?;
            return Err(WAITING_FOR_ROTATION.into());
        }
        let desired = deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.replicas)
            .unwrap_or(1);
        let ready = deployment.status.as_ref().is_some_and(|status| {
            status.observed_generation == deployment.metadata.generation
                && status.updated_replicas.unwrap_or(0) == desired
                && status.available_replicas.unwrap_or(0) == desired
        });
        if !ready {
            return Err(WAITING_FOR_ROLLOUT.into());
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
            pod.metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(EPOCH))
                != Some(&epoch)
        }) {
            // Rollout availability alone can exclude a still-terminating old
            // router which continues accepting its startup-cached control token.
            return Err(WAITING_FOR_ROLLOUT.into());
        }
    }
    Ok(())
}
