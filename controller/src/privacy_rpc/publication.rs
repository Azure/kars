// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::observation_privacy::{self as wire, Endpoint};
use k8s_openapi::api::{
    core::v1::{ConfigMap, Pod, Service},
    networking::v1::NetworkPolicy,
};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams},
};
use serde_json::json;

async fn pod(client: &Client, namespace: &str, name: &str, uid: &str) -> Result<Pod, String> {
    let pod = Api::<Pod>::namespaced(client.clone(), namespace)
        .get(name)
        .await
        .map_err(|_| "Privacy RPC controller Pod unavailable")?;
    if pod.uid().as_deref() != Some(uid)
        || pod.metadata.deletion_timestamp.is_some()
        || pod
            .spec
            .as_ref()
            .and_then(|spec| spec.service_account_name.as_deref())
            != Some("kars-controller")
        || pod.metadata.labels.as_ref().is_none_or(|labels| {
            labels.get("app.kubernetes.io/name").map(String::as_str) != Some("kars")
                || labels
                    .get("app.kubernetes.io/component")
                    .map(String::as_str)
                    != Some("controller")
        })
    {
        return Err("Privacy RPC controller Pod identity changed".into());
    }
    Ok(pod)
}

pub(super) async fn withdraw(
    client: &Client,
    namespace: &str,
    name: &str,
    uid: &str,
) -> Result<(), String> {
    let current = pod(client, namespace, name, uid).await?;
    if current
        .metadata
        .labels
        .as_ref()
        .is_none_or(|labels| !labels.contains_key(wire::REVISION_LABEL))
    {
        return Ok(());
    }
    Api::<Pod>::namespaced(client.clone(), namespace).patch_metadata(name, &PatchParams::default(),
        &Patch::Merge(json!({"metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version,
            "labels":{wire::REVISION_LABEL:serde_json::Value::Null}}})))
        .await.map_err(|_| "Privacy RPC capability withdrawal failed")?;
    Ok(())
}

pub(super) async fn publish(
    client: &Client,
    endpoint: &Endpoint,
    pod_name: &str,
    pod_uid: &str,
) -> Result<(), String> {
    const ERROR: &str = "Privacy RPC capability publication unavailable";
    let current = pod(client, &endpoint.namespace, pod_name, pod_uid).await?;
    let policies = Api::<NetworkPolicy>::namespaced(client.clone(), &endpoint.namespace)
        .list(&ListParams::default())
        .await
        .map_err(|_| ERROR)?;
    let labels = current.metadata.labels.clone().unwrap_or_default();
    for direction in ["Ingress", "Egress"] {
        if !crate::credential_grants::observation_network::isolated(
            &policies.items,
            &labels,
            direction,
        ) {
            return Err(
                "Privacy RPC needs the operator namespace's approved baseline network isolation"
                    .into(),
            );
        }
    }
    let revision = endpoint.revision();
    if current
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get(wire::REVISION_LABEL))
        != Some(&revision)
    {
        Api::<Pod>::namespaced(client.clone(), &endpoint.namespace).patch_metadata(pod_name, &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version,
                "labels":{wire::REVISION_LABEL:revision},
                "annotations":{wire::CONTROLLER_UID:endpoint.controller_uid,wire::NAMESPACE_UID:endpoint.namespace_uid}}})))
            .await.map_err(|_| ERROR)?;
    }
    let services = Api::<Service>::namespaced(client.clone(), &endpoint.namespace);
    let service = services.get(wire::SERVICE).await.map_err(|_| ERROR)?;
    if service.uid().as_deref() != Some(endpoint.service_uid.as_str()) {
        return Err(ERROR.into());
    }
    if service
        .spec
        .as_ref()
        .and_then(|spec| spec.selector.as_ref())
        .and_then(|s| s.get(wire::REVISION_LABEL))
        != Some(&revision)
    {
        services.patch(wire::SERVICE,&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":service.metadata.uid,"resourceVersion":service.metadata.resource_version},
            "spec":{"selector":{wire::REVISION_LABEL:revision}}
        }))).await.map_err(|_| ERROR)?;
    }
    let descriptors = Api::<ConfigMap>::namespaced(client.clone(), &endpoint.namespace);
    let current = descriptors.get(wire::DESCRIPTOR).await.map_err(|_| ERROR)?;
    if current.uid().as_deref() != Some(endpoint.descriptor_uid.as_str())
        || !super::discovery::owned(
            &current.metadata,
            &endpoint.namespace_uid,
            &endpoint.controller_uid,
        )
    {
        return Err(ERROR.into());
    }
    let serialized = serde_json::to_string(endpoint).map_err(|_| ERROR)?;
    if current
        .data
        .as_ref()
        .and_then(|data| data.get("config.json"))
        != Some(&serialized)
    {
        descriptors.patch(wire::DESCRIPTOR,&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version},
            "data":{"config.json":serialized}
        }))).await.map_err(|_| ERROR)?;
    }
    Ok(())
}
