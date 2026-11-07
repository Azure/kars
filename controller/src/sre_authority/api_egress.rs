// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{api_error, live};
use crate::{
    crd::KarsSandbox,
    reconciler::{namespace_ownership as claim, sre_egress},
    sre_registration::{EPOCH, KarsSRERegistration, NAME, OWNER},
};
use k8s_openapi::api::core::v1::Namespace;
use kube::{
    Api, Client, ResourceExt,
    api::{ApiResource, DeleteParams, DynamicObject, GroupVersionKind, PostParams, Preconditions},
};
use serde_json::{Value, json};

const POLICY: &str = "kars-sre-api";

fn resource() -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(
        "cilium.io",
        "v2",
        "CiliumNetworkPolicy",
    ))
}

fn owner(reg: &KarsSRERegistration) -> Value {
    json!([{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
        "name":NAME,"uid":reg.metadata.uid,"controller":true,"blockOwnerDeletion":false}])
}

fn owned(object: &DynamicObject, reg: &KarsSRERegistration) -> Result<(), String> {
    let annotations = object.metadata.annotations.as_ref();
    if object.name_any() != POLICY
        || object.namespace().as_deref() != Some(reg.spec.runtime_namespace.name.as_str())
        || object.uid().is_none_or(|uid| uid.is_empty())
        || object
            .resource_version()
            .is_none_or(|version| version.is_empty())
        || annotations.and_then(|a| a.get(OWNER)) != reg.metadata.uid.as_ref()
        || annotations.and_then(|a| a.get(claim::SOURCE_UID)) != Some(&reg.spec.sandbox.uid)
        || annotations.and_then(|a| a.get(claim::NAMESPACE_UID))
            != Some(&reg.spec.runtime_namespace.uid)
        || serde_json::to_value(&object.metadata.owner_references)
            .map_err(|_| "SRE API policy ownership cannot be serialized")?
            != owner(reg)
    {
        return Err("Foreign SRE API policy preserved".into());
    }
    Ok(())
}

async fn ready(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<KarsSRERegistration, String> {
    let reg = Api::<KarsSRERegistration>::all(client.clone())
        .get(NAME)
        .await
        .map_err(|error| api_error("Read SRE API path registration", error))?;
    if !live::currently_qualified(&reg)
        || sandbox.uid().as_deref() != Some(reg.spec.sandbox.uid.as_str())
        || sandbox.namespace().as_deref() != Some(reg.spec.sandbox.namespace.as_str())
        || sandbox.name_any() != reg.spec.sandbox.name
        || namespace.uid().as_deref() != Some(reg.spec.runtime_namespace.uid.as_str())
        || namespace.name_any() != reg.spec.runtime_namespace.name
    {
        return Err("SRE API path requires the currently qualified registered occupant".into());
    }
    live::verify(client, &reg).await?;
    Ok(reg)
}

pub(crate) async fn ensure(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    rules: &[Value],
) -> Result<(), String> {
    let reg = ready(client, sandbox, namespace).await?;
    if !sre_egress::cilium_installed(client).await? {
        return Ok(());
    }
    let spec = sre_egress::cilium_spec(&sandbox.name_any(), &namespace.name_any(), rules)?;
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &namespace.name_any(), &resource());
    let current = api
        .get_opt(POLICY)
        .await
        .map_err(|error| api_error("Read SRE API policy", error))?;
    if let Some(current) = &current {
        owned(current, &reg)?;
        if current.metadata.deletion_timestamp.is_some() {
            return Err("Owned SRE API policy is terminating".into());
        }
    }
    let rechecked = ready(client, sandbox, namespace).await?;
    if rechecked.uid() != reg.uid() || rechecked.metadata.generation != reg.metadata.generation {
        return Err("SRE API authority changed before policy issuance".into());
    }
    let annotations = json!({
        OWNER:reg.metadata.uid,EPOCH:reg.epoch(),
        claim::SOURCE_UID:reg.spec.sandbox.uid,
        claim::NAMESPACE_UID:reg.spec.runtime_namespace.uid,
    });
    if let Some(mut current) = current {
        let epoch_matches = current
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(EPOCH))
            == Some(&reg.epoch());
        if current.data["spec"] != spec || current.data.get("specs").is_some() || !epoch_matches {
            current.data["spec"] = spec;
            current
                .data
                .as_object_mut()
                .ok_or("SRE API policy is malformed")?
                .remove("specs");
            current
                .metadata
                .annotations
                .get_or_insert_default()
                .insert(EPOCH.into(), reg.epoch());
            api.replace(POLICY, &PostParams::default(), &current)
                .await
                .map_err(|error| api_error("Update owned SRE API policy", error))?;
        }
    } else {
        let desired: DynamicObject = serde_json::from_value(json!({
            "apiVersion":"cilium.io/v2","kind":"CiliumNetworkPolicy",
            "metadata":{"name":POLICY,"namespace":namespace.name_any(),
                "annotations":annotations,"ownerReferences":owner(&reg)},
            "spec":spec,
        }))
        .map_err(|_| "SRE API policy serialization failed")?;
        api.create(&PostParams::default(), &desired)
            .await
            .map_err(|error| api_error("Create registered SRE API policy", error))?;
    }
    let checked = ready(client, sandbox, namespace).await;
    match checked {
        Ok(current)
            if current.uid() == reg.uid()
                && current.metadata.generation == reg.metadata.generation =>
        {
            Ok(())
        }
        result => {
            let detail = result
                .err()
                .unwrap_or_else(|| "SRE API authority changed during issuance".into());
            match retire(client, &reg).await {
                Ok(()) => Err(detail),
                Err(error) => Err(format!(
                    "{detail}; owned API policy retirement failed: {error}"
                )),
            }
        }
    }
}

pub(super) async fn retire(client: &Client, reg: &KarsSRERegistration) -> Result<(), String> {
    if !sre_egress::cilium_installed(client).await? {
        return Ok(());
    }
    let namespace = Api::<Namespace>::all(client.clone())
        .get_opt(&reg.spec.runtime_namespace.name)
        .await
        .map_err(|error| api_error("Read retiring SRE API namespace", error))?;
    let Some(namespace) = namespace else {
        return Ok(());
    };
    if namespace.uid().as_deref() != Some(reg.spec.runtime_namespace.uid.as_str()) {
        return Err("Replaced SRE API namespace preserved".into());
    }
    let api: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        &reg.spec.runtime_namespace.name,
        &resource(),
    );
    let Some(object) = api
        .get_opt(POLICY)
        .await
        .map_err(|error| api_error("Read retiring SRE API policy", error))?
    else {
        return Ok(());
    };
    owned(&object, reg)?;
    let current_namespace = Api::<Namespace>::all(client.clone())
        .get(&reg.spec.runtime_namespace.name)
        .await
        .map_err(|error| api_error("Recheck retiring SRE API namespace", error))?;
    if current_namespace.uid() != namespace.uid() {
        return Err("SRE API namespace changed before retirement".into());
    }
    api.delete(
        POLICY,
        &DeleteParams {
            preconditions: Some(Preconditions {
                uid: object.uid(),
                resource_version: object.resource_version(),
            }),
            ..Default::default()
        },
    )
    .await
    .map_err(|error| api_error("Retire owned SRE API policy", error))?;
    if api
        .get_opt(POLICY)
        .await
        .map_err(|error| api_error("Verify SRE API policy retirement", error))?
        .is_some()
    {
        return Err("Owned SRE API policy retirement is pending".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
