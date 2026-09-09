// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::observation_privacy::{self as wire, Endpoint};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret, Service, ServiceAccount};
use kube::{Api, Client, ResourceExt};

pub(super) fn owned(
    meta: &kube::api::ObjectMeta,
    namespace_uid: &str,
    controller_uid: &str,
) -> bool {
    meta.uid.as_deref().is_some_and(|uid| !uid.is_empty())
        && meta
            .resource_version
            .as_deref()
            .is_some_and(|rv| !rv.is_empty())
        && meta.deletion_timestamp.is_none()
        && meta.annotations.as_ref().is_some_and(|a| {
            a.get(wire::NAMESPACE_UID).map(String::as_str) == Some(namespace_uid)
                && a.get(wire::CONTROLLER_UID).map(String::as_str) == Some(controller_uid)
        })
}

pub(crate) async fn current(client: &Client) -> Result<Endpoint, String> {
    if std::env::var("KARS_OBSERVATION_PRIVACY_RPC_ENABLED").as_deref() != Ok("true") {
        return Err("Private observation verifier is not enabled by this controller".into());
    }
    let namespace =
        std::env::var("POD_NAMESPACE").map_err(|_| "Privacy controller namespace unavailable")?;
    let cm = Api::<ConfigMap>::namespaced(client.clone(), &namespace)
        .get(wire::DESCRIPTOR)
        .await
        .map_err(|_| "Private observation verifier capability unavailable")?;
    let endpoint: Endpoint = serde_json::from_str(
        cm.data
            .as_ref()
            .and_then(|d| d.get("config.json"))
            .ok_or("Private observation verifier capability absent")?,
    )
    .map_err(|_| "Private observation verifier capability invalid")?;
    if endpoint.namespace != namespace {
        return Err("Privacy verifier namespace mismatch".into());
    }
    validate(client, &endpoint).await?;
    Ok(endpoint)
}

pub(super) async fn validate(client: &Client, endpoint: &Endpoint) -> Result<(), String> {
    const ERROR: &str = "Private observation verifier identity is unavailable";
    if !endpoint.valid(chrono::Utc::now().timestamp()) {
        return Err(ERROR.into());
    }
    let ns = Api::<Namespace>::all(client.clone())
        .get(&endpoint.namespace)
        .await
        .map_err(|_| ERROR)?;
    let sa = Api::<ServiceAccount>::namespaced(client.clone(), &endpoint.namespace)
        .get("kars-controller")
        .await
        .map_err(|_| ERROR)?;
    if ns.uid().as_deref() != Some(endpoint.namespace_uid.as_str())
        || ns.metadata.deletion_timestamp.is_some()
        || sa.uid().as_deref() != Some(endpoint.controller_uid.as_str())
        || sa.metadata.deletion_timestamp.is_some()
    {
        return Err(ERROR.into());
    }
    let secret = Api::<Secret>::namespaced(client.clone(), &endpoint.namespace)
        .get_metadata(wire::SECRET)
        .await
        .map_err(|_| ERROR)?;
    if !owned(
        &secret.metadata,
        &endpoint.namespace_uid,
        &endpoint.controller_uid,
    ) || secret.metadata.uid.as_deref() != Some(endpoint.tls_uid.as_str())
        || secret.metadata.resource_version.as_deref() != Some(endpoint.tls_version.as_str())
    {
        return Err(ERROR.into());
    }
    let descriptor = Api::<ConfigMap>::namespaced(client.clone(), &endpoint.namespace)
        .get(wire::DESCRIPTOR)
        .await
        .map_err(|_| ERROR)?;
    if !owned(
        &descriptor.metadata,
        &endpoint.namespace_uid,
        &endpoint.controller_uid,
    ) || descriptor.uid().as_deref() != Some(endpoint.descriptor_uid.as_str())
        || descriptor
            .data
            .as_ref()
            .and_then(|d| d.get("config.json"))
            .and_then(|raw| serde_json::from_str::<Endpoint>(raw).ok())
            .as_ref()
            != Some(endpoint)
    {
        return Err(ERROR.into());
    }
    let service = Api::<Service>::namespaced(client.clone(), &endpoint.namespace)
        .get(wire::SERVICE)
        .await
        .map_err(|_| ERROR)?;
    if !endpoint.service_matches(&service) {
        return Err(ERROR.into());
    }
    Ok(())
}
