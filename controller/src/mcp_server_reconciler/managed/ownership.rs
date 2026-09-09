// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::plan::*;
use crate::mcp_server::McpServer;
use k8s_openapi::{api::core::v1::Namespace, apimachinery::pkg::apis::meta::v1::ObjectMeta};
use kube::{
    Api, Client, Resource, ResourceExt,
    api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::fmt::Debug;

pub(super) fn api_error(stage: &str, error: kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!("{stage}: Kubernetes status {}", status.code),
        _ => format!("{stage}: Kubernetes transport/serialization failure"),
    }
}

pub(super) async fn source_current(
    client: &Client,
    source: &McpServer,
    deleting: bool,
) -> Result<(), String> {
    let namespace = source
        .namespace()
        .ok_or("McpServer source namespace is missing")?;
    let current = Api::<McpServer>::namespaced(client.clone(), &namespace)
        .get(&source.name_any())
        .await
        .map_err(|error| api_error("Verify live McpServer", error))?;
    if current.uid() != source.uid()
        || source.uid().is_none()
        || current.metadata.generation != source.metadata.generation
        || current
            .metadata
            .generation
            .is_none_or(|generation| generation <= 0)
        || current.metadata.deletion_timestamp.is_some() != deleting
    {
        return Err(
            "McpServer incarnation/generation changed; no resource mutation authorized".into(),
        );
    }
    Ok(())
}

pub(super) fn namespace_owned(
    namespace: &Namespace,
    config: &Config,
    controller_uid: &str,
) -> bool {
    let meta = &namespace.metadata;
    let annotation = |key: &str| {
        meta.annotations
            .as_ref()
            .and_then(|annotations| annotations.get(key))
            .map(String::as_str)
    };
    meta.name.as_deref() == Some(config.namespace.as_str())
        && meta.uid.as_deref().is_some_and(|uid| !uid.is_empty())
        && meta
            .resource_version
            .as_deref()
            .is_some_and(|rv| !rv.is_empty())
        && meta.deletion_timestamp.is_none()
        && meta.owner_references.as_ref().is_none_or(Vec::is_empty)
        && annotation(CLAIM) == Some("v1")
        && annotation(CONTROLLER_NS) == Some(config.controller_namespace.as_str())
        && annotation(CONTROLLER_UID) == Some(controller_uid)
}

pub(super) async fn namespace(
    client: &Client,
    config: &Config,
    source: &McpServer,
    create: bool,
) -> Result<Option<Namespace>, String> {
    config.validate()?;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let controller = namespaces
        .get(&config.controller_namespace)
        .await
        .map_err(|error| api_error("Read managed MCP controller namespace", error))?;
    let controller_uid = controller
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or("Managed MCP controller namespace UID is missing")?;
    if controller.metadata.deletion_timestamp.is_some() {
        return Err("Managed MCP controller namespace is terminating".into());
    }
    let existing = namespaces
        .get_opt(&config.namespace)
        .await
        .map_err(|error| api_error("Read managed MCP namespace", error))?;
    let namespace = match existing {
        Some(namespace) => namespace,
        None if !create => return Ok(None),
        None => {
            if source
                .status
                .as_ref()
                .and_then(|status| status.managed_namespace_uid.as_ref())
                .is_some()
            {
                return Err(
                    "Managed MCP namespace disappeared; explicit incarnation review required"
                        .into(),
                );
            }
            source_current(client, source, false).await?;
            let body: Namespace = serde_json::from_value(json!({"apiVersion":"v1","kind":"Namespace",
                "metadata":{"name":config.namespace,"annotations":{
                    CLAIM:"v1",CONTROLLER_NS:config.controller_namespace,CONTROLLER_UID:controller_uid},
                    "labels":{"app.kubernetes.io/name":"kars-managed-mcp","app.kubernetes.io/managed-by":"kars-controller",
                        "pod-security.kubernetes.io/enforce":"restricted","pod-security.kubernetes.io/audit":"restricted",
                        "pod-security.kubernetes.io/warn":"restricted"}}}))
                .map_err(|_| "Managed MCP namespace serialization failed")?;
            match namespaces.create(&PostParams::default(), &body).await {
                Ok(namespace) => namespace,
                Err(kube::Error::Api(status)) if status.code == 409 => namespaces
                    .get(&config.namespace)
                    .await
                    .map_err(|error| api_error("Inspect raced managed MCP namespace", error))?,
                Err(error) => return Err(api_error("Create managed MCP namespace", error)),
            }
        }
    };
    if !namespace_owned(&namespace, config, controller_uid)
        || source
            .status
            .as_ref()
            .and_then(|status| status.managed_namespace_uid.as_ref())
            .is_some_and(|uid| namespace.metadata.uid.as_ref() != Some(uid))
    {
        return Err(
            "Managed MCP namespace is unowned, replaced or terminating; no adoption permitted"
                .into(),
        );
    }
    let restricted = namespace
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("pod-security.kubernetes.io/enforce"));
    if restricted.map(String::as_str) != Some("restricted") {
        return Err("Managed MCP namespace no longer enforces restricted Pod security".into());
    }
    Ok(Some(namespace))
}

pub(super) fn owned(meta: &ObjectMeta, plan: &Owner, namespace_uid: &str) -> bool {
    let annotation = |key: &str| {
        meta.annotations
            .as_ref()
            .and_then(|annotations| annotations.get(key))
            .map(String::as_str)
    };
    meta.name.as_deref() == Some(plan.name.as_str())
        && meta.namespace.as_deref() == Some(plan.namespace.as_str())
        && meta.uid.as_deref().is_some_and(|uid| !uid.is_empty())
        && meta
            .resource_version
            .as_deref()
            .is_some_and(|rv| !rv.is_empty())
        && meta.owner_references.as_ref().is_none_or(Vec::is_empty)
        && annotation(SOURCE_NS) == Some(plan.source_namespace.as_str())
        && annotation(SOURCE_NAME) == Some(plan.source_name.as_str())
        && annotation(SOURCE_UID) == Some(plan.source_uid.as_str())
        && annotation(NAMESPACE_UID) == Some(namespace_uid)
        && meta
            .labels
            .as_ref()
            .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
            .map(String::as_str)
            == Some("kars-controller")
}

fn selector_matches<K: Serialize + Resource<DynamicType = ()>>(value: &K, plan: &Owner) -> bool {
    let Ok(value) = serde_json::to_value(value) else {
        return false;
    };
    let selector = match K::kind(&()).as_ref() {
        "Deployment" => &value["spec"]["selector"]["matchLabels"],
        "Service" => &value["spec"]["selector"],
        "NetworkPolicy" => &value["spec"]["podSelector"]["matchLabels"],
        _ => return false,
    };
    *selector == json!({SOURCE_UID:plan.source_uid})
}

fn contains_desired(current: &Value, desired: &Value) -> bool {
    match desired {
        Value::Object(entries) => entries
            .iter()
            .all(|(key, value)| contains_desired(&current[key], value)),
        Value::Array(items) if items.is_empty() && current.is_null() => true,
        Value::Array(items) => current.as_array().is_some_and(|values| {
            values.len() == items.len()
                && values
                    .iter()
                    .zip(items)
                    .all(|(current, desired)| contains_desired(current, desired))
        }),
        _ => current == desired,
    }
}

pub(super) async fn preflight<K>(
    api: &Api<K>,
    plan: &Owner,
    namespace_uid: &str,
) -> Result<Option<K>, String>
where
    K: Clone + Debug + DeserializeOwned + Serialize + Resource<DynamicType = ()>,
{
    let existing = api
        .get_opt(&plan.name)
        .await
        .map_err(|error| api_error("Inspect managed MCP resource", error))?;
    if let Some(value) = existing.as_ref()
        && (!owned(value.meta(), plan, namespace_uid)
            || value.meta().deletion_timestamp.is_some()
            || !selector_matches(value, plan))
    {
        return Err(
            "Managed MCP resource has conflicting ownership or selector; no mutation permitted"
                .into(),
        );
    }
    Ok(existing)
}

pub(super) async fn upsert<K>(
    client: &Client,
    source: &McpServer,
    api: &Api<K>,
    plan: &Owner,
    namespace_uid: &str,
    mut desired: Value,
) -> Result<K, String>
where
    K: Clone + Debug + DeserializeOwned + Serialize + Resource<DynamicType = ()>,
{
    let existing = preflight(api, plan, namespace_uid).await?;
    source_current(client, source, false).await?;
    let current_namespace = Api::<Namespace>::all(client.clone())
        .get(&plan.namespace)
        .await
        .map_err(|error| api_error("Recheck managed MCP namespace incarnation", error))?;
    if current_namespace.metadata.uid.as_deref() != Some(namespace_uid)
        || current_namespace.metadata.deletion_timestamp.is_some()
    {
        return Err("Managed MCP namespace changed before resource write".into());
    }
    let result = match existing {
        Some(existing) => {
            if contains_desired(
                &serde_json::to_value(&existing)
                    .map_err(|_| "Managed MCP resource comparison failed")?,
                &desired,
            ) {
                return Ok(existing);
            }
            desired["metadata"]["uid"] = json!(existing.meta().uid);
            desired["metadata"]["resourceVersion"] = json!(existing.meta().resource_version);
            api.patch(&plan.name, &PatchParams::default(), &Patch::Merge(desired))
                .await
                .map_err(|error| api_error("CAS update managed MCP resource", error))?
        }
        None => {
            let desired: K = serde_json::from_value(desired)
                .map_err(|_| "Managed MCP resource serialization failed")?;
            api.create(&PostParams::default(), &desired)
                .await
                .map_err(|error| api_error("Create managed MCP resource", error))?
        }
    };
    if !owned(result.meta(), plan, namespace_uid) || !selector_matches(&result, plan) {
        return Err(
            "Managed MCP resource response did not preserve exact ownership/selector".into(),
        );
    }
    Ok(result)
}

pub(super) async fn delete<K>(
    api: &Api<K>,
    plan: &Owner,
    namespace_uid: &str,
) -> Result<bool, String>
where
    K: Clone + Debug + DeserializeOwned + Serialize + Resource<DynamicType = ()>,
{
    let Some(current) = api
        .get_opt(&plan.name)
        .await
        .map_err(|error| api_error("Inspect managed MCP cleanup target", error))?
    else {
        return Ok(true);
    };
    if !owned(current.meta(), plan, namespace_uid) || !selector_matches(&current, plan) {
        return Err("Managed MCP cleanup target is not the exact owned resource; preserved".into());
    }
    if current.meta().deletion_timestamp.is_none() {
        api.delete(
            &plan.name,
            &DeleteParams {
                preconditions: Some(Preconditions {
                    uid: current.meta().uid.clone(),
                    resource_version: current.meta().resource_version.clone(),
                }),
                propagation_policy: Some(kube::api::PropagationPolicy::Foreground),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| api_error("Delete exact managed MCP resource", error))?;
    }
    Ok(api
        .get_opt(&plan.name)
        .await
        .map_err(|error| api_error("Wait for managed MCP resource removal", error))?
        .is_none())
}
