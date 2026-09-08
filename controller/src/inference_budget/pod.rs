// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    binding,
    config::{AUDIENCE, PRIVATE_MOUNT, Settings, TOKEN_VOLUME},
    store::{Store, StoreError},
};
use crate::{
    crd::KarsSandbox,
    inference_budget_contract::{BudgetError, ResourceIdentity, RouterBinding},
    kars_task::KarsTask,
};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::{Value, json};

const CA_NAME: &str = "kars-inference-budget-ca";
const CA_VOLUME: &str = "kars-inference-budget-ca";

#[cfg(test)]
#[path = "pod_tests.rs"]
mod tests;

pub fn egress(sandbox: &KarsSandbox) -> Result<Option<Value>, StoreError> {
    let Some(reference) = &sandbox.spec.inference_budget_ref else {
        return Ok(None);
    };
    let settings = Settings::from_env()?.ok_or(BudgetError::Contract)?;
    if reference.account.namespace != settings.accounting_namespace {
        return Err(BudgetError::Identity.into());
    }
    Ok(Some(json!({
        "to": [{
            "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": settings.accounting_namespace}},
            "podSelector": {"matchLabels": {
                "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "controller"
            }}
        }],
        "ports": [{"protocol": "TCP", "port": 9447}]
    })))
}

pub struct Plan {
    pub binding: RouterBinding,
    pub ca_version: String,
    pub endpoint: String,
    pub router_image_digest: String,
}

fn api_error(stage: &'static str, error: kube::Error) -> StoreError {
    StoreError::Api {
        stage,
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

pub async fn prepare(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Option<Plan>, StoreError> {
    let owners = sandbox
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default();
    let Some(owner) = owners.iter().find(|owner| {
        owner.controller == Some(true)
            && owner.kind == "KarsTask"
            && owner.api_version == "kars.azure.com/v1alpha1"
    }) else {
        if sandbox.spec.inference_budget_ref.is_some() {
            return Err(BudgetError::Authorization.into());
        }
        return Ok(None);
    };
    let workspace = sandbox.namespace().ok_or(BudgetError::Identity)?;
    let tasks: Api<KarsTask> = Api::namespaced(client.clone(), &workspace);
    let task = tasks
        .get(&owner.name)
        .await
        .map_err(|error| api_error("read sandbox budget authority", error))?;
    if task.metadata.uid.as_deref() != Some(owner.uid.as_str())
        || task.name_any() != sandbox.name_any()
    {
        return Err(BudgetError::Identity.into());
    }
    if !binding::needs_account(client, &task).await? {
        return Ok(None);
    }
    let settings = Settings::from_env()?.ok_or(BudgetError::Contract)?;
    super::admission::verify(client, &settings.accounting_namespace).await?;
    let bound = task
        .status
        .as_ref()
        .and_then(|status| status.inference_budget.clone())
        .ok_or(StoreError::Missing)?;
    if sandbox.spec.inference_budget_ref.as_ref() != Some(&bound)
        || bound.task_uid != owner.uid
        || bound.authorization_digest != task.envelope_digest()
        || bound.account.namespace != settings.accounting_namespace
    {
        return Err(BudgetError::Authorization.into());
    }
    let store = Store::new(client.clone(), &settings.accounting_namespace);
    let account = store.read(&bound.root, &bound.account.uid).await?;
    let ledger = account
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or(StoreError::Missing)?;
    if !ledger.requires_enforcement(&bound.task_uid)? {
        return Ok(None);
    }
    let node = ledger
        .nodes
        .get(&bound.task_uid)
        .ok_or(BudgetError::Authorization)?;
    if !node.active
        || node.authority.authorization_digest != bound.authorization_digest
        || !task
            .spec
            .execution
            .as_ref()
            .is_some_and(|execution| execution.launch)
    {
        return Err(BudgetError::Authorization.into());
    }
    crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| BudgetError::Identity)?;
    fence_namespace(client, sandbox, namespace).await?;
    let epoch = crate::sre_authority::privacy_epoch(client, &namespace.name_any())
        .await
        .map_err(|_| BudgetError::Authorization)?;
    let (ca, ca_version) = settings.public_ca(client).await?;
    mirror_public_ca(client, sandbox, namespace, ca).await?;
    Ok(Some(Plan {
        binding: RouterBinding {
            task: bound,
            sandbox: ResourceIdentity {
                namespace: workspace,
                name: sandbox.name_any(),
                uid: sandbox.uid().ok_or(BudgetError::Identity)?,
            },
            runtime_namespace: namespace.name_any(),
            runtime_namespace_uid: namespace.uid().ok_or(BudgetError::Identity)?,
            privacy_epoch: epoch,
        },
        ca_version,
        endpoint: format!(
            "https://kars-inference-budget.{}.svc:9447",
            settings.accounting_namespace
        ),
        router_image_digest: settings.router_image_digest,
    }))
}

async fn fence_namespace(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), StoreError> {
    let live = crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| BudgetError::Identity)?;
    let key = "kars.azure.com/inference-budget";
    if let Some(value) = live.labels().get(key) {
        return if value == "v1" {
            Ok(())
        } else {
            Err(BudgetError::Identity.into())
        };
    }
    let api: Api<Namespace> = Api::all(client.clone());
    let updated = api
        .patch(
            &live.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {"uid": live.uid(), "resourceVersion": live.resource_version(),
                    "labels": {key: "v1"}}
            })),
        )
        .await
        .map_err(|error| api_error("activate budget namespace fence", error))?;
    if updated.metadata.uid != live.metadata.uid
        || updated.labels().get(key).map(String::as_str) != Some("v1")
    {
        return Err(BudgetError::Identity.into());
    }
    Ok(())
}

async fn mirror_public_ca(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    ca: String,
) -> Result<(), StoreError> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace.name_any());
    let existing = api
        .get_opt(CA_NAME)
        .await
        .map_err(|error| api_error("read budget CA projection", error))?;
    if let Some(existing) = existing {
        if existing
            .annotations()
            .get("kars.azure.com/budget-sandbox-uid")
            != sandbox.metadata.uid.as_ref()
            || existing
                .annotations()
                .get("kars.azure.com/budget-namespace-uid")
                != namespace.metadata.uid.as_ref()
            || existing.metadata.uid.is_none()
            || existing.metadata.resource_version.is_none()
        {
            return Err(BudgetError::Identity.into());
        }
        if existing.data.as_ref().and_then(|data| data.get("ca.crt")) == Some(&ca) {
            return Ok(());
        }
        api.patch(
            CA_NAME,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {"uid": existing.uid(), "resourceVersion": existing.resource_version()},
                "data": {"ca.crt": ca}
            })),
        )
        .await
        .map_err(|error| api_error("update budget CA projection", error))?;
    } else {
        let map: ConfigMap = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {
                "name": CA_NAME, "namespace": namespace.name_any(),
                "annotations": {
                    "kars.azure.com/budget-sandbox-uid": sandbox.uid(),
                    "kars.azure.com/budget-namespace-uid": namespace.uid()
                },
                "ownerReferences": [{
                    "apiVersion": "v1", "kind": "Namespace", "name": namespace.name_any(),
                    "uid": namespace.uid(), "controller": true, "blockOwnerDeletion": false
                }]
            }, "data": {"ca.crt": ca}
        }))
        .map_err(|_| BudgetError::Corrupt)?;
        api.create(&PostParams::default(), &map)
            .await
            .map_err(|error| api_error("create budget CA projection", error))?;
    }
    Ok(())
}

pub async fn decorate(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    pod: &mut Value,
) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
    let mut annotations = std::collections::BTreeMap::new();
    if let Some(plan) = prepare(client, sandbox, namespace).await? {
        plan.apply(pod, &mut annotations)?;
    }
    Ok(annotations)
}

impl Plan {
    pub fn apply(
        &self,
        pod: &mut Value,
        annotations: &mut std::collections::BTreeMap<String, String>,
    ) -> Result<(), StoreError> {
        let volumes = pod
            .get_mut("volumes")
            .and_then(Value::as_array_mut)
            .ok_or(BudgetError::Corrupt)?;
        if volumes.iter().any(|volume| {
            matches!(
                volume.get("name").and_then(Value::as_str),
                Some(TOKEN_VOLUME | CA_VOLUME)
            )
        }) {
            return Err(BudgetError::Corrupt.into());
        }

        volumes.push(json!({
            "name": TOKEN_VOLUME,
            "projected": {"sources": [{"serviceAccountToken": {"audience": AUDIENCE, "expirationSeconds": 600, "path": "token"}}]}
        }));
        volumes.push(json!({"name": CA_VOLUME, "configMap": {"name": CA_NAME}}));
        let containers = pod
            .get_mut("containers")
            .and_then(Value::as_array_mut)
            .ok_or(BudgetError::Corrupt)?;
        let router = containers
            .iter_mut()
            .find(|container| {
                container.get("name").and_then(Value::as_str) == Some("inference-router")
            })
            .ok_or(BudgetError::Corrupt)?;
        let env = router
            .get_mut("env")
            .and_then(Value::as_array_mut)
            .ok_or(BudgetError::Corrupt)?;
        env.extend([
            json!({"name": "KARS_INFERENCE_BUDGET_REQUIRED", "value": "true"}),
            json!({"name": "KARS_INFERENCE_BUDGET_BINDING", "value": serde_json::to_string(&self.binding).map_err(|_| BudgetError::Corrupt)?}),
            json!({"name": "KARS_INFERENCE_BUDGET_ENDPOINT", "value": self.endpoint}),
            json!({"name": "KARS_INFERENCE_BUDGET_CA", "value": "/etc/kars/inference-budget-ca/ca.crt"}),
            json!({"name": "POD_NAME", "valueFrom": {"fieldRef": {"fieldPath": "metadata.name"}}}),
            json!({"name": "POD_UID", "valueFrom": {"fieldRef": {"fieldPath": "metadata.uid"}}}),
        ]);
        let mounts = router
            .get_mut("volumeMounts")
            .and_then(Value::as_array_mut)
            .ok_or(BudgetError::Corrupt)?;
        mounts.push(json!({"name": TOKEN_VOLUME, "mountPath": PRIVATE_MOUNT, "readOnly": true}));
        mounts.push(json!({"name": CA_VOLUME, "mountPath": "/etc/kars/inference-budget-ca", "readOnly": true}));
        router["readinessProbe"] = json!({
            "httpGet": {"path": "/readyz", "port": "inference"}, "initialDelaySeconds": 3, "periodSeconds": 5
        });
        let image = router
            .get("image")
            .and_then(Value::as_str)
            .ok_or(BudgetError::Contract)?;
        let base = image.split('@').next().ok_or(BudgetError::Contract)?;
        router["image"] = json!(format!("{base}@{}", self.router_image_digest));
        annotations.insert(
            "kars.azure.com/inference-budget-ca-version".into(),
            self.ca_version.clone(),
        );
        annotations.insert(
            "kars.azure.com/inference-budget-binding".into(),
            crate::providers::signing::sha256_hex(
                &serde_json::to_vec(&self.binding).map_err(|_| BudgetError::Corrupt)?,
            ),
        );
        Ok(())
    }
}
