// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Preserve reviewed workload structure across controller-owned credential and model rollouts.

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

const ROLLOUT_FIELDS: [&str; 5] = [
    "kars.azure.com/inference-budget-ca-version",
    "kars.azure.com/inference-providers-version",
    "kars.azure.com/services-credential-version",
    "kars.azure.com/sre-private-credential-uid",
    "kars.azure.com/sre-tls-expiry",
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
    if let Some(annotations) = template.pointer_mut("/metadata/annotations") {
        let annotations = annotations.as_object_mut().ok_or(ERROR)?;
        for name in ROLLOUT_FIELDS {
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
    structure(template)
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
        || receipt.structure != structure(template.clone())?
        || receipt
            .model_structure
            .as_ref()
            .is_some_and(|digest| model_structure(template.clone()).as_ref() != Ok(digest))
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
/// permits only credential annotations and policy-verified model values, never
/// other spec/label changes or a replacement Deployment/Sandbox.
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
    let Some(activation) = grant.spec.private_activation else {
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
    let actual_structure = structure(template.clone())?;
    let actual_template = template_digest(template.clone())?;
    let prior = stored(namespace, &consumer.object.uid)?;
    let mut qualified = actual_template == consumer.template_digest
        || prior.as_ref().is_some_and(|prior| {
            prior.epoch == epoch
                && prior.baseline == consumer.template_digest
                && Some(&prior.sandbox_uid) == sandbox.metadata.uid.as_ref()
                && prior.structure == actual_structure
        });
    let route_structure = model_structure(template.clone())?;
    let mut model_transition = !qualified
        && prior.as_ref().is_some_and(|prior| {
            prior.epoch == epoch
                && prior.baseline == consumer.template_digest
                && Some(&prior.sandbox_uid) == sandbox.metadata.uid.as_ref()
                && prior.model_structure.as_ref() == Some(&route_structure)
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
mod tests {
    use super::*;
    use serde_json::json;

    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const NS: &str = "/api/v1/namespaces/kars-runtime";
    const SOURCE: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/runtime";
    const DEPLOYMENT: &str = "/apis/apps/v1/namespaces/kars-runtime/deployments/runtime";
    const REPLICAS: &str = "/apis/apps/v1/namespaces/kars-runtime/replicasets";

    async fn fixture() -> (
        MockServer,
        Client,
        Arc<Mutex<BTreeMap<String, Value>>>,
        ReviewedConsumer,
    ) {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut objects = BTreeMap::new();
        let mut activation = super::super::test_support::install(
            &mut objects,
            "core",
            "core-uid",
            "controller",
            &[("kars-runtime", "runtime-ns")],
        );
        let namespace = objects.get_mut(NS).unwrap();
        for (name, value) in [
            ("kars.azure.com/namespace-claim-version", "v1"),
            ("kars.azure.com/sandbox-name", "runtime"),
            ("kars.azure.com/sandbox-namespace", "work"),
            ("kars.azure.com/sandbox-uid", "sandbox"),
        ] {
            namespace["metadata"]["annotations"][name] = value.into();
        }
        let epoch = namespace["metadata"]["annotations"][EPOCH].clone();
        let template = json!({"metadata":{"labels":{"app":"runtime"},"annotations":{
            EPOCH:epoch,ROLLOUT_FIELDS[1]:"old-providers",ROLLOUT_FIELDS[2]:"old-credential"}},
            "spec":{"containers":[{"name":"router","image":"reviewed"}]}});
        let consumer: ReviewedConsumer = serde_json::from_value(json!({"kind":"Deployment",
            "object":{"name":"runtime","uid":"deployment","resourceVersion":"1"},
            "templateDigest":template_digest(template.clone()).unwrap()}))
        .unwrap();
        activation["namespaces"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|scope| scope["namespace"]["name"] == "kars-runtime")
            .unwrap()["consumers"] = json!([consumer]);
        objects.insert("/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace".into(),
            json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
                "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1"},
                "spec":{"workspaceUid":"work-uid","writers":[],"privateActivation":activation}}));
        objects.insert(SOURCE.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"runtime","namespace":"work","uid":"sandbox","resourceVersion":"1","generation":1,
                "annotations":{"kars.azure.com/namespace-uid":"runtime-ns"}},
            "spec":{"inferenceRef":{"name":"policy"}}}));
        objects.insert(DEPLOYMENT.into(),json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"runtime","namespace":"kars-runtime","uid":"deployment","resourceVersion":"1"},
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"runtime"}},"template":template}}));
        objects.insert(
            REPLICAS.into(),
            json!({"apiVersion":"apps/v1","kind":"ReplicaSetList","metadata":{},"items":[]}),
        );
        let objects = Arc::new(Mutex::new(objects));
        let snapshot = objects.clone();
        let server = MockServer::start().await;
        Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
            assert_eq!(request.method.as_str(), "GET", "qualification must be read-only");
            snapshot.lock().unwrap().get(request.url.path()).map(|value|
                ResponseTemplate::new(200).set_body_json(value)).unwrap_or_else(||
                ResponseTemplate::new(404).set_body_json(json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"NotFound","code":404})))
        }).mount(&server).await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        (server, client, objects, consumer)
    }

    #[tokio::test]
    async fn credential_rollout_reuses_qualified_structure_without_history_or_new_grant() {
        let (_server, client, objects, consumer) = fixture().await;
        let original = objects.lock().unwrap().clone();
        let mut namespace: Namespace = serde_json::from_value(original[NS].clone()).unwrap();
        let source: KarsSandbox = serde_json::from_value(original[SOURCE].clone()).unwrap();
        let baseline: Deployment = serde_json::from_value(original[DEPLOYMENT].clone()).unwrap();
        let epoch = original[NS]["metadata"]["annotations"][EPOCH]
            .as_str()
            .unwrap();
        namespace
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(format!("{PREFIX}parent-deployment"), epoch.into());
        let scope: crate::credential_grant::activation::NamespaceReview =
            serde_json::from_value(json!({
            "namespace":{"name":"kars-runtime","uid":"runtime-ns","resourceVersion":"1"},
            "consumers":[consumer],"epoch":epoch}))
            .unwrap();
        let (name, value) = record(&client, &source, &namespace, &baseline, &baseline, epoch)
            .await
            .unwrap()
            .unwrap();
        namespace
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(name.clone(), value);
        objects
            .lock()
            .unwrap()
            .insert(NS.into(), serde_json::to_value(&namespace).unwrap());
        for index in 0..15 {
            let mut changed = original[DEPLOYMENT].clone();
            for field in ROLLOUT_FIELDS {
                changed["spec"]["template"]["metadata"]["annotations"][field] =
                    json!(format!("rotation-{index}"));
            }
            objects
                .lock()
                .unwrap()
                .insert(DEPLOYMENT.into(), changed.clone());
            let applied: Deployment = serde_json::from_value(changed.clone()).unwrap();
            let (_, value) = record(&client, &source, &namespace, &applied, &applied, epoch)
                .await
                .unwrap()
                .unwrap();
            namespace
                .metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert(name.clone(), value);
            objects
                .lock()
                .unwrap()
                .insert(NS.into(), serde_json::to_value(&namespace).unwrap());
            assert!(
                matches(
                    &client,
                    &namespace,
                    &consumer,
                    &serde_json::from_value(changed).unwrap(),
                    epoch
                )
                .await
                .unwrap()
            );
            super::super::verification::reviewed_consumers(&client, &namespace, &scope, epoch)
                .await
                .unwrap();
        }
        for fault in [
            "image",
            "label",
            "unknown-annotation",
            "epoch",
            "source-uid",
            "source-generation",
            "namespace-uid",
            "record-field",
        ] {
            let mut state = objects.lock().unwrap().clone();
            let valid = state.clone();
            match fault {
                "image" => {
                    state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"]["containers"]
                        [0]["image"] = "unreviewed".into()
                }
                "label" => {
                    state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["labels"]["app"] =
                        "unreviewed".into()
                }
                "unknown-annotation" => {
                    state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]
                        ["unreviewed"] = "new".into()
                }
                "epoch" => {
                    state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]
                        [EPOCH] = "stale".into()
                }
                "source-uid" => {
                    state.get_mut(SOURCE).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "source-generation" => {
                    state.get_mut(SOURCE).unwrap()["metadata"]["generation"] = 2.into()
                }
                "namespace-uid" => {
                    state.get_mut(NS).unwrap()["metadata"]["uid"] = "replacement".into()
                }
                "record-field" => {
                    let mut receipt: Value = serde_json::from_str(
                        state[NS]["metadata"]["annotations"][&name]
                            .as_str()
                            .unwrap(),
                    )
                    .unwrap();
                    receipt["unreviewed"] = true.into();
                    state.get_mut(NS).unwrap()["metadata"]["annotations"][&name] =
                        receipt.to_string().into();
                }
                _ => unreachable!(),
            }
            *objects.lock().unwrap() = state.clone();
            let ns = serde_json::from_value(state[NS].clone()).unwrap();
            let current = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
            assert!(
                !matches(&client, &ns, &consumer, &current, epoch)
                    .await
                    .unwrap_or(false),
                "{fault}"
            );
            assert!(
                super::super::verification::reviewed_consumers(&client, &ns, &scope, epoch)
                    .await
                    .is_err(),
                "{fault}"
            );
            let applied: Deployment = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
            assert!(
                !matches!(
                    record(&client, &source, &ns, &applied, &applied, epoch).await,
                    Ok(Some(_))
                ),
                "{fault}"
            );
            *objects.lock().unwrap() = valid;
        }
        for (field, replacement) in [
            ("uid", "replacement"),
            ("name", "foreign"),
            ("namespace", "foreign"),
        ] {
            let valid = objects.lock().unwrap().clone();
            objects.lock().unwrap().get_mut(DEPLOYMENT).unwrap()["metadata"][field] =
                replacement.into();
            assert!(
                super::super::verification::reviewed_consumers(&client, &namespace, &scope, epoch)
                    .await
                    .is_err(),
                "{field}"
            );
            *objects.lock().unwrap() = valid;
        }
        namespace
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .remove(&name);
        assert!(
            super::super::verification::reviewed_consumers(&client, &namespace, &scope, epoch)
                .await
                .is_err(),
            "unrecorded consumers must remain strict"
        );
    }

    #[tokio::test]
    async fn upgrade_recovery_requires_complete_pinned_template_and_owned_history() {
        for fault in [
            "none",
            "missing",
            "foreign-owner",
            "history-image",
            "current-image",
            "unexpected-rollout",
        ] {
            let (_server, client, objects, consumer) = fixture().await;
            let mut state = objects.lock().unwrap().clone();
            let mut template = state[DEPLOYMENT]["spec"]["template"].clone();
            template["metadata"]["labels"]["pod-template-hash"] = "rs-hash".into();
            let mut replica = json!({"apiVersion":"apps/v1","kind":"ReplicaSet",
                "metadata":{"name":"runtime-old","namespace":"kars-runtime","uid":"rs","resourceVersion":"1",
                    "ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":"runtime","uid":"deployment","controller":true}]},
                "spec":{"template":template}});
            if fault == "foreign-owner" {
                replica["metadata"]["ownerReferences"][0]["uid"] = "foreign".into();
            }
            if fault == "history-image" {
                replica["spec"]["template"]["spec"]["containers"][0]["image"] = "foreign".into();
            }
            if fault != "missing" {
                state.get_mut(REPLICAS).unwrap()["items"] = json!([replica]);
            }
            state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["metadata"]["annotations"]
                [ROLLOUT_FIELDS[1]] = "new".into();
            if fault == "current-image" {
                state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"]["containers"][0]["image"] =
                    "foreign".into();
            }
            let ns = serde_json::from_value(state[NS].clone()).unwrap();
            let source = serde_json::from_value(state[SOURCE].clone()).unwrap();
            let applied: Deployment = serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
            let mut desired = applied.clone();
            if fault == "unexpected-rollout" {
                desired
                    .spec
                    .as_mut()
                    .unwrap()
                    .template
                    .metadata
                    .as_mut()
                    .unwrap()
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(ROLLOUT_FIELDS[1].into(), "different".into());
            }
            let epoch = state[NS]["metadata"]["annotations"][EPOCH]
                .as_str()
                .unwrap()
                .to_string();
            *objects.lock().unwrap() = state;
            let result = record(&client, &source, &ns, &desired, &applied, &epoch).await;
            assert_eq!(result.is_ok(), fault == "none", "{fault}");
            if let Ok(Some((_, value))) = result {
                let receipt: Reconciliation = serde_json::from_str(&value).unwrap();
                assert_eq!(receipt.baseline, consumer.template_digest);
            }
        }
    }

    #[tokio::test]
    async fn model_rollouts_require_pinned_structure_and_current_policy_for_both_runtimes() {
        const GRANT: &str =
            "/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace";
        const POLICY: &str =
            "/apis/kars.azure.com/v1alpha1/namespaces/work/inferencepolicies/policy";
        for runtime in ["agent", "openclaw"] {
            for fault in [
                "none",
                "missing-history",
                "foreign-history",
                "image",
                "label",
                "other-env",
                "value-from",
                "duplicate",
                "extra-field",
                "missing-model",
                "desired",
                "policy",
                "missing-policy",
                "source",
            ] {
                let (_server, client, objects, mut consumer) = fixture().await;
                let mut state = objects.lock().unwrap().clone();
                let mut baseline = state[DEPLOYMENT]["spec"]["template"].clone();
                baseline["spec"]["containers"] = json!([
                    {"name":runtime,"image":"reviewed-agent","env":[{"name":"KARS_MODEL","value":"old"},{"name":"OTHER","value":"fixed"}]},
                    {"name":"inference-router","image":"reviewed-router","env":[{"name":"AZURE_OPENAI_DEPLOYMENT","value":"old"},{"name":"DEFAULT_MODEL","value":"fixed"}]}
                ]);
                if runtime == "openclaw" {
                    baseline["spec"]["containers"][0]["env"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"name":"OPENCLAW_MODEL","value":"old"}));
                }
                consumer.template_digest = template_digest(baseline.clone()).unwrap();
                let legacy = Reconciliation {
                    epoch: state[NS]["metadata"]["annotations"][EPOCH]
                        .as_str()
                        .unwrap()
                        .into(),
                    baseline: consumer.template_digest.clone(),
                    structure: structure(baseline.clone()).unwrap(),
                    template: consumer.template_digest.clone(),
                    sandbox_uid: "sandbox".into(),
                    generation: 1,
                    model_structure: None,
                };
                state.get_mut(NS).unwrap()["metadata"]["annotations"][key("deployment")] =
                    serde_json::to_string(&legacy).unwrap().into();
                state.get_mut(GRANT).unwrap()["spec"]["privateActivation"]["namespaces"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|scope| scope["namespace"]["name"] == "kars-runtime")
                    .unwrap()["consumers"] = json!([consumer]);
                let mut replica = json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{"name":"old","namespace":"kars-runtime",
                    "ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":"runtime","uid":"deployment","controller":true}]},"spec":{"template":baseline}});
                replica["spec"]["template"]["metadata"]["labels"]["pod-template-hash"] =
                    "old".into();
                if fault == "foreign-history" {
                    replica["metadata"]["ownerReferences"][0]["uid"] = "foreign".into();
                }
                if fault != "missing-history" {
                    state.get_mut(REPLICAS).unwrap()["items"] = json!([replica]);
                }
                let mut changed = baseline;
                changed["spec"]["containers"][0]["env"][0]["value"] = "new".into();
                changed["spec"]["containers"][1]["env"][0]["value"] = "new".into();
                if runtime == "openclaw" {
                    changed["spec"]["containers"][0]["env"][2]["value"] = "new".into();
                }
                let desired_template = changed.clone();
                match fault {
                    "image" => changed["spec"]["containers"][0]["image"] = "foreign".into(),
                    "label" => changed["metadata"]["labels"]["app"] = "foreign".into(),
                    "other-env" => {
                        changed["spec"]["containers"][1]["env"][1]["value"] = "foreign".into()
                    }
                    "value-from" => {
                        changed["spec"]["containers"][0]["env"][0]["valueFrom"] =
                            json!({"fieldRef":{"fieldPath":"metadata.name"}})
                    }
                    "duplicate" => {
                        let entry = changed["spec"]["containers"][0]["env"][0].clone();
                        changed["spec"]["containers"][0]["env"]
                            .as_array_mut()
                            .unwrap()
                            .push(entry);
                    }
                    "extra-field" => {
                        changed["spec"]["containers"][0]["env"][0]["unexpected"] = true.into()
                    }
                    "missing-model" => {
                        changed["spec"]["containers"][0]["env"]
                            .as_array_mut()
                            .unwrap()
                            .remove(0);
                    }
                    _ => {}
                }
                state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"] = changed;
                state.insert(POLICY.into(), json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy",
                    "metadata":{"name":"policy","namespace":"work","uid":"policy","resourceVersion":"1","generation":1},
                    "spec":{"appliesTo":{},"modelPreference":{"primary":{"provider":"foundry","deployment":if fault == "policy" {"raced"} else {"new"}}}}}));
                if fault == "missing-policy" {
                    state.remove(POLICY);
                }
                let source: KarsSandbox = serde_json::from_value(state[SOURCE].clone()).unwrap();
                if fault == "source" {
                    state.get_mut(SOURCE).unwrap()["metadata"]["generation"] = 2.into();
                }
                let mut namespace: Namespace = serde_json::from_value(state[NS].clone()).unwrap();
                let epoch = state[NS]["metadata"]["annotations"][EPOCH]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let applied: Deployment =
                    serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
                let mut desired = applied.clone();
                desired.spec.as_mut().unwrap().template =
                    serde_json::from_value(desired_template).unwrap();
                if fault == "desired" {
                    desired
                        .spec
                        .as_mut()
                        .unwrap()
                        .template
                        .spec
                        .as_mut()
                        .unwrap()
                        .containers[0]
                        .env
                        .as_mut()
                        .unwrap()[0]
                        .value = Some("raced".into());
                }
                *objects.lock().unwrap() = state;
                let result = record(&client, &source, &namespace, &desired, &applied, &epoch).await;
                assert_eq!(
                    result.is_ok(),
                    fault == "none",
                    "{runtime}/{fault}: {result:?}"
                );
                if fault != "none" {
                    continue;
                }
                let (key, value) = result.unwrap().unwrap();
                namespace
                    .metadata
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert(key.clone(), value);
                objects.lock().unwrap().get_mut(REPLICAS).unwrap()["items"] = json!([]);
                // Once recorded, repeated model changes need no retained ReplicaSet or new grant.
                for model in ["third", "fourth", "old"] {
                    let mut state = objects.lock().unwrap().clone();
                    for container in state.get_mut(DEPLOYMENT).unwrap()["spec"]["template"]["spec"]
                        ["containers"]
                        .as_array_mut()
                        .unwrap()
                    {
                        for entry in container["env"].as_array_mut().unwrap() {
                            if matches!(
                                entry["name"].as_str(),
                                Some("KARS_MODEL" | "OPENCLAW_MODEL" | "AZURE_OPENAI_DEPLOYMENT")
                            ) {
                                entry["value"] = model.into();
                            }
                        }
                    }
                    state.get_mut(POLICY).unwrap()["spec"]["modelPreference"]["primary"]["deployment"] =
                        model.into();
                    let applied: Deployment =
                        serde_json::from_value(state[DEPLOYMENT].clone()).unwrap();
                    *objects.lock().unwrap() = state;
                    let (_, value) =
                        record(&client, &source, &namespace, &applied, &applied, &epoch)
                            .await
                            .unwrap()
                            .unwrap();
                    namespace
                        .metadata
                        .annotations
                        .as_mut()
                        .unwrap()
                        .insert(key.clone(), value);
                    let current =
                        serde_json::from_value(objects.lock().unwrap()[DEPLOYMENT].clone())
                            .unwrap();
                    assert!(
                        matches(&client, &namespace, &consumer, &current, &epoch)
                            .await
                            .unwrap()
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn replaced_consumers_and_resource_version_races_cannot_be_recorded() {
        for fault in ["replacement", "version", "absent-review", "absent-grant"] {
            let (_server, client, objects, _) = fixture().await;
            let original = objects.lock().unwrap().clone();
            let ns = serde_json::from_value(original[NS].clone()).unwrap();
            let source = serde_json::from_value(original[SOURCE].clone()).unwrap();
            let mut applied: Deployment =
                serde_json::from_value(original[DEPLOYMENT].clone()).unwrap();
            let epoch = original[NS]["metadata"]["annotations"][EPOCH]
                .as_str()
                .unwrap();
            match fault {
                "replacement" => applied.metadata.uid = Some("replacement".into()),
                "version" => applied.metadata.resource_version = Some("different".into()),
                "absent-review" => objects.lock().unwrap().get_mut("/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace").unwrap()["spec"]["privateActivation"]["namespaces"] = json!([]),
                "absent-grant" => { objects.lock().unwrap().remove("/apis/kars.azure.com/v1alpha1/namespaces/work/karscredentialgrants/workspace"); }
                _ => unreachable!(),
            }
            assert!(
                !matches!(
                    record(&client, &source, &ns, &applied, &applied, epoch).await,
                    Ok(Some(_))
                ),
                "{fault}"
            );
        }
    }

    #[test]
    fn credential_rollout_structure_retains_all_non_rollout_fields() {
        let baseline = json!({"metadata":{"labels":{"app":"sre"},"annotations":{
            EPOCH:"old", "kars.azure.com/inference-providers-version":"old", "unrelated":"fixed"}},
            "spec":{"containers":[{"name":"router","image":"reviewed"}]}});
        for name in ROLLOUT_FIELDS.into_iter().chain([EPOCH]) {
            let mut changed = baseline.clone();
            changed["metadata"]["annotations"][name] = "new".into();
            assert_eq!(
                structure(baseline.clone()).unwrap(),
                structure(changed).unwrap()
            );
        }
        for path in [
            "/spec/containers/0/image",
            "/metadata/labels/app",
            "/metadata/annotations/unrelated",
        ] {
            let mut changed = baseline.clone();
            *changed.pointer_mut(path).unwrap() = "changed".into();
            assert_ne!(
                structure(baseline.clone()).unwrap(),
                structure(changed).unwrap()
            );
        }
        let mut invalid = baseline;
        invalid["metadata"]["annotations"][ROLLOUT_FIELDS[0]] = json!({"not":"a string"});
        assert!(structure(invalid).is_err());
    }
}
