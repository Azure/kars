// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Preserve reviewed workload structure across controller-owned credential, observer and model rollouts.

use super::{EPOCH, ERROR, PREFIX, live, verification::template_digest};
use crate::{
    crd::KarsSandbox,
    credential_grant::{KarsCredentialGrant, activation::ReviewedConsumer},
};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Namespace};
use kube::{
    Api, Client, ResourceExt,
    api::ListParams,
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[path = "reconciliation_observer.rs"]
mod observer;

const ROLLOUT_FIELDS: [&str; 6] = [
    "kars.azure.com/inference-budget-ca-version",
    "kars.azure.com/inference-providers-version",
    "kars.azure.com/services-credential-version",
    "kars.azure.com/sre-private-credential-uid",
    "kars.azure.com/sre-tls-expiry",
    crate::credential_source::POD_VERSION,
];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reconciliation {
    epoch: String,
    baseline: String,
    structure: String,
    template: String,
    sandbox_uid: String,
    generation: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_structure: Option<String>,
}

fn key(uid: &str) -> String {
    format!("{PREFIX}reconciled-{uid}")
}

fn structure(mut template: Value) -> Result<String, String> {
    observer::project(&mut template)?;
    rollout_structure(template, true)
}

fn rollout_structure(mut template: Value, project_credentials: bool) -> Result<String, String> {
    if let Some(annotations) = template.pointer_mut("/metadata/annotations") {
        let annotations = annotations.as_object_mut().ok_or(ERROR)?;
        for name in ROLLOUT_FIELDS {
            if !project_credentials && name == crate::credential_source::POD_VERSION {
                continue;
            }
            if annotations
                .get(name)
                .is_some_and(|value| !value.is_string())
            {
                return Err(ERROR.into());
            }
            annotations.remove(name);
        }
    }
    template_digest(template)
}

fn model_values(template: &Value) -> Result<Vec<(String, String, String)>, String> {
    let mut values = Vec::new();
    for container in template
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .ok_or(ERROR)?
    {
        let name = container["name"].as_str().ok_or(ERROR)?;
        for entry in container["env"].as_array().into_iter().flatten() {
            let variable = entry["name"].as_str().ok_or(ERROR)?;
            if matches!(
                (name, variable),
                ("agent" | "openclaw", "KARS_MODEL")
                    | ("openclaw", "OPENCLAW_MODEL")
                    | ("inference-router", "AZURE_OPENAI_DEPLOYMENT")
            ) {
                let value = entry["value"]
                    .as_str()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or(ERROR)?;
                if entry.get("valueFrom").is_some()
                    || values.iter().any(|(c, e, _)| c == name && e == variable)
                {
                    return Err(ERROR.into());
                }
                values.push((name.to_owned(), variable.to_owned(), value.to_owned()));
            }
        }
    }
    Ok(values)
}

fn model_structure(mut template: Value) -> Result<String, String> {
    observer::project(&mut template)?;
    rollout_model_structure(template, true)
}

fn rollout_model_structure(
    mut template: Value,
    project_credentials: bool,
) -> Result<String, String> {
    let values = model_values(&template)?;
    for container in template["spec"]["containers"].as_array_mut().ok_or(ERROR)? {
        let name = container["name"].as_str().ok_or(ERROR)?.to_owned();
        for entry in container
            .get_mut("env")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if values
                .iter()
                .any(|(c, e, _)| c == &name && entry["name"].as_str() == Some(e))
            {
                entry["value"] = Value::String("controller-policy-model".into());
            }
        }
    }
    rollout_structure(template, project_credentials)
}

fn receipt_structure_matches(receipt: &Reconciliation, template: &Value) -> Result<bool, String> {
    if receipt.structure == structure(template.clone())?
        && receipt
            .model_structure
            .as_ref()
            .is_none_or(|digest| model_structure(template.clone()).as_ref() == Ok(digest))
    {
        return Ok(true);
    }
    // Pre-upgrade receipts pinned the observer rendering and/or projection revision. Read them until the
    // next verified controller reconciliation replaces them with the new form.
    for project_credentials in [true, false] {
        if receipt.structure == rollout_structure(template.clone(), project_credentials)?
            && receipt.model_structure.as_ref().is_none_or(|digest| {
                rollout_model_structure(template.clone(), project_credentials).as_ref()
                    == Ok(digest)
            })
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn verify_model_transition(
    client: &Client,
    sandbox: &KarsSandbox,
    actual: &Value,
    desired: &Value,
) -> Result<(), String> {
    let values = model_values(actual)?;
    if values.is_empty() || values != model_values(desired)? {
        return Err("Private model rollout differs from the controller-rendered route".into());
    }
    let policy = Api::<crate::inference_policy::InferencePolicy>::namespaced(
        client.clone(),
        &sandbox.namespace().ok_or(ERROR)?,
    )
    .get(&sandbox.spec.inference_ref.name)
    .await
    .map_err(|_| ERROR)?;
    live(&policy.metadata)?;
    let primary = &policy.spec.model_preference.as_ref().ok_or(ERROR)?.primary;
    if values
        .iter()
        .any(|(_, _, value)| value != &primary.deployment)
    {
        return Err(
            "Private model rollout differs from the current same-namespace inference policy".into(),
        );
    }
    Ok(())
}

fn stored(namespace: &Namespace, uid: &str) -> Result<Option<Reconciliation>, String> {
    namespace
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(&key(uid)))
        .map(|raw| serde_json::from_str(raw).map_err(|_| ERROR.into()))
        .transpose()
}

fn deployment_api(client: &Client, namespace: &str) -> Api<DynamicObject> {
    Api::namespaced_with(
        client.clone(),
        namespace,
        &ApiResource::from_gvk(&GroupVersionKind::gvk("apps", "v1", "Deployment")),
    )
}

async fn source(client: &Client, namespace: &Namespace, name: &str) -> Result<KarsSandbox, String> {
    let annotations = namespace.metadata.annotations.as_ref().ok_or(ERROR)?;
    let workspace = annotations
        .get("kars.azure.com/sandbox-namespace")
        .ok_or(ERROR)?;
    if annotations
        .get("kars.azure.com/sandbox-name")
        .map(String::as_str)
        != Some(name)
    {
        return Err(ERROR.into());
    }
    let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), workspace)
        .get(name)
        .await
        .map_err(|_| ERROR)?;
    crate::reconciler::namespace_ownership::recheck(client, &sandbox, namespace)
        .await
        .map_err(|_| ERROR)?;
    live(&sandbox.metadata)?;
    if sandbox
        .metadata
        .generation
        .is_none_or(|generation| generation < 1)
    {
        return Err(ERROR.into());
    }
    Ok(sandbox)
}

pub(super) async fn matches(
    client: &Client,
    namespace: &Namespace,
    consumer: &ReviewedConsumer,
    current: &DynamicObject,
    epoch: &str,
) -> Result<bool, String> {
    if consumer.kind != "Deployment" {
        return Ok(false);
    }
    let Some(receipt) = stored(namespace, &consumer.object.uid)? else {
        return Ok(false);
    };
    let template = &current.data["spec"]["template"];
    if receipt.epoch != epoch
        || receipt.baseline != consumer.template_digest
        || receipt.template != template_digest(template.clone())?
        || !receipt_structure_matches(&receipt, template)?
        || template["metadata"]["annotations"][EPOCH] != epoch
    {
        return Ok(false);
    }
    let sandbox = source(client, namespace, &consumer.object.name).await?;
    Ok(
        sandbox.uid().as_deref() == Some(receipt.sandbox_uid.as_str())
            && sandbox.metadata.generation == Some(receipt.generation),
    )
}

/// Only the qualified projector can write these namespace annotations. A receipt
/// permits only credential annotations, exact current observer projections and
/// policy-verified model values, never other spec/label changes or a replacement
/// Deployment/Sandbox. Full current template digests remain mandatory.
pub(super) async fn record(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    desired: &Deployment,
    applied: &Deployment,
    epoch: &str,
) -> Result<Option<(String, String)>, String> {
    let workspace = sandbox.namespace().ok_or(ERROR)?;
    let Some(grant) = Api::<KarsCredentialGrant>::namespaced(client.clone(), &workspace)
        .get_opt("workspace")
        .await
        .map_err(|_| ERROR)?
    else {
        return Ok(None);
    };
    let Some(activation) = grant.spec.private_activation.as_ref() else {
        return Ok(None);
    };
    let Some(consumer) = activation
        .namespaces
        .iter()
        .find(|scope| {
            scope.namespace.name == namespace.name_any()
                && Some(&scope.namespace.uid) == namespace.metadata.uid.as_ref()
                && scope.epoch.as_deref() == Some(epoch)
        })
        .and_then(|scope| {
            scope.consumers.iter().find(|consumer| {
                consumer.kind == "Deployment"
                    && consumer.object.name == applied.name_any()
                    && Some(&consumer.object.uid) == applied.metadata.uid.as_ref()
            })
        })
    else {
        return Ok(None);
    };
    let fresh_source = source(client, namespace, &applied.name_any()).await?;
    if fresh_source.uid() != sandbox.uid()
        || fresh_source.metadata.generation != sandbox.metadata.generation
    {
        return Err(ERROR.into());
    }
    let current = deployment_api(client, &namespace.name_any())
        .get(&applied.name_any())
        .await
        .map_err(|_| ERROR)?;
    if current.uid() != applied.uid() || current.resource_version() != applied.resource_version() {
        return Err(ERROR.into());
    }
    let template = &current.data["spec"]["template"];
    let expected =
        serde_json::to_value(&desired.spec.as_ref().ok_or(ERROR)?.template).map_err(|_| ERROR)?;
    if template["metadata"]["annotations"][EPOCH] != epoch
        || ROLLOUT_FIELDS.iter().any(|name| {
            template["metadata"]["annotations"][name] != expected["metadata"]["annotations"][name]
        })
    {
        return Err(ERROR.into());
    }
    observer::verify_rendering(
        template,
        &expected,
        sandbox,
        &fresh_source,
        &grant,
        namespace,
        &consumer.object.uid,
    )?;
    let actual_structure = structure(template.clone())?;
    let observer_pinned_structure = rollout_structure(template.clone(), true)?;
    let legacy_structure = rollout_structure(template.clone(), false)?;
    let actual_template = template_digest(template.clone())?;
    let prior = stored(namespace, &consumer.object.uid)?;
    let mut qualified = actual_template == consumer.template_digest
        || prior.as_ref().is_some_and(|prior| {
            prior.epoch == epoch
                && prior.baseline == consumer.template_digest
                && Some(&prior.sandbox_uid) == sandbox.metadata.uid.as_ref()
                && (prior.structure == actual_structure
                    || prior.structure == observer_pinned_structure
                    || prior.structure == legacy_structure)
        });
    let route_structure = model_structure(template.clone())?;
    let observer_pinned_route_structure = rollout_model_structure(template.clone(), true)?;
    let legacy_route_structure = rollout_model_structure(template.clone(), false)?;
    let mut model_transition = !qualified
        && prior.as_ref().is_some_and(|prior| {
            prior.epoch == epoch
                && prior.baseline == consumer.template_digest
                && Some(&prior.sandbox_uid) == sandbox.metadata.uid.as_ref()
                && (prior.model_structure.as_ref() == Some(&route_structure)
                    || prior.model_structure.as_ref() == Some(&observer_pinned_route_structure)
                    || prior.model_structure.as_ref() == Some(&legacy_route_structure))
        });
    if !qualified && !model_transition {
        // Upgrade recovery can use a retained ReplicaSet only when its complete
        // raw template hashes to the original qualification; history is not trust.
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk("apps", "v1", "ReplicaSet"));
        let replicas =
            Api::<DynamicObject>::namespaced_with(client.clone(), &namespace.name_any(), &resource)
                .list(&ListParams::default())
                .await
                .map_err(|_| ERROR)?;
        for replica in replicas.items {
            let owners = replica
                .metadata
                .owner_references
                .as_deref()
                .unwrap_or_default();
            if owners.len() != 1
                || owners[0].uid != consumer.object.uid
                || owners[0].kind != "Deployment"
                || owners[0].api_version != "apps/v1"
                || owners[0].controller != Some(true)
                || replica.metadata.deletion_timestamp.is_some()
            {
                continue;
            }
            let mut baseline = replica.data["spec"]["template"].clone();
            if let Some(labels) = baseline
                .pointer_mut("/metadata/labels")
                .and_then(Value::as_object_mut)
            {
                labels.remove("pod-template-hash");
            }
            if template_digest(baseline.clone())? == consumer.template_digest {
                qualified = structure(baseline.clone())? == actual_structure;
                model_transition = !qualified && model_structure(baseline)? == route_structure;
                if qualified || model_transition {
                    break;
                }
            }
        }
    }
    if model_transition {
        verify_model_transition(client, &fresh_source, template, &expected).await?;
        qualified = true;
    }
    if !qualified {
        return Err(
            "Private credential or model rollout changed the reviewed workload structure".into(),
        );
    }
    let receipt = Reconciliation {
        epoch: epoch.into(),
        baseline: consumer.template_digest.clone(),
        structure: actual_structure,
        template: actual_template,
        sandbox_uid: live(&sandbox.metadata)?.0.into(),
        generation: sandbox.metadata.generation.ok_or(ERROR)?,
        model_structure: Some(route_structure),
    };
    Ok(Some((
        key(&consumer.object.uid),
        serde_json::to_string(&receipt).map_err(|_| ERROR)?,
    )))
}

#[cfg(test)]
#[path = "reconciliation_tests.rs"]
mod tests;
