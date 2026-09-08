// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{NAMESPACE_UID, SECRET, SOURCE_UID, api_error};
use crate::{crd::KarsSandbox, sre_registration::EPOCH};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Pod, Secret},
};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams, PostParams},
};
use serde_json::json;

pub(super) const REVISION: &str = "kars.azure.com/services-privacy-revision";
pub(super) const VERSION: &str = crate::sre_registration::CONTROL_VERSION;
pub(super) const RETIRED: &str = "kars.azure.com/services-credential-retired";

pub(super) struct Projection {
    pub(super) version: String,
}

impl Projection {
    pub(super) fn decorate(&self, deployment: &mut Deployment) {
        deployment
            .spec
            .as_mut()
            .expect("controller Deployment spec")
            .template
            .metadata
            .get_or_insert_with(Default::default)
            .annotations
            .get_or_insert_with(Default::default)
            .insert(VERSION.into(), self.version.clone());
    }

    pub(super) async fn consumers_current(
        &self,
        client: &Client,
        namespace: &str,
        name: &str,
    ) -> Result<bool, String> {
        let pods = Api::<Pod>::namespaced(client.clone(), namespace)
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={name}")))
            .await
            .map_err(api_error)?;
        // Include terminating Pods: Deployment availability alone can hide a
        // still-running router that accepts its old startup-cached token.
        Ok(pods.iter().all(|pod| {
            pod.metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(VERSION))
                == Some(&self.version)
        }))
    }
}

fn validate(secret: &Secret, source_uid: &str, namespace: &Namespace) -> Result<(), String> {
    let annotations = secret.metadata.annotations.as_ref();
    let matches = |key, value: &str| {
        annotations
            .and_then(|annotations| annotations.get(key))
            .map(String::as_str)
            == Some(value)
    };
    if secret.metadata.uid.as_deref().is_none_or(str::is_empty)
        || secret
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || secret.metadata.deletion_timestamp.is_some()
        || secret.metadata.name.as_deref() != Some(SECRET)
        || secret.metadata.namespace != namespace.metadata.name
        || secret
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| !owners.is_empty())
        || secret
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("app.kubernetes.io/managed-by"))
            .map(String::as_str)
            != Some("kars-controller")
        || !matches(SOURCE_UID, source_uid)
        || !matches(
            NAMESPACE_UID,
            namespace.metadata.uid.as_deref().unwrap_or_default(),
        )
        || secret.type_.as_deref().is_some_and(|kind| kind != "Opaque")
        || secret
            .data
            .as_ref()
            .and_then(|data| data.get("control-token"))
            .is_none_or(|value| {
                value.0.len() != 64 || value.0.iter().any(|byte| !byte.is_ascii_graphic())
            })
    {
        return Err(
            "Existing governed service credential has conflicting ownership or invalid data".into(),
        );
    }
    Ok(())
}

fn current(secret: &Secret, epoch: Option<&str>) -> bool {
    let annotations = secret.metadata.annotations.as_ref();
    if annotations.is_some_and(|annotations| annotations.contains_key(RETIRED)) {
        return false;
    }
    match epoch {
        // A Ready v2 registration has already rotated its owned credentials
        // and retired the old cached consumers before publishing this epoch.
        Some(epoch) => annotations.and_then(|a| a.get(EPOCH)).map(String::as_str) == Some(epoch),
        None => {
            annotations
                .and_then(|a| a.get(REVISION))
                .map(String::as_str)
                == Some(crate::sre_privacy::REVISION)
        }
    }
}

async fn review_consumer(
    client: &Client,
    namespace: &str,
    name: &str,
) -> Result<Option<Deployment>, String> {
    let deployment = Api::<Deployment>::namespaced(client.clone(), namespace)
        .get_opt(name)
        .await
        .map_err(api_error)?;
    if let Some(deployment) = deployment.as_ref() {
        if deployment.metadata.name.as_deref() != Some(name)
            || deployment.metadata.namespace.as_deref() != Some(namespace)
            || deployment.metadata.uid.as_deref().is_none_or(str::is_empty)
            || deployment
                .metadata
                .resource_version
                .as_deref()
                .is_none_or(str::is_empty)
            || deployment.metadata.deletion_timestamp.is_some()
            || !crate::sre_authority::controller_managed(deployment, name)
        {
            return Err(
                "Governed service credential consumer is not a live controller-owned Deployment"
                    .into(),
            );
        }
    } else if !Api::<Pod>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await
        .map_err(api_error)?
        .items
        .is_empty()
    {
        return Err(
            "Governed service credential has unreviewed consumers without its Deployment".into(),
        );
    }
    Ok(deployment)
}

async fn quarantine(
    client: &Client,
    namespace: &str,
    name: &str,
    secret: &Secret,
) -> Result<(), String> {
    if secret
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(RETIRED))
        .map(String::as_str)
        != Some("true")
    {
        Api::<Secret>::namespaced(client.clone(), namespace)
            .patch(SECRET, &PatchParams::default(), &Patch::Merge(json!({
                "metadata":{"uid":secret.metadata.uid,"resourceVersion":secret.metadata.resource_version,
                    "annotations":{RETIRED:"true",REVISION:null,EPOCH:null}},
            }))).await.map_err(api_error)?;
    }
    let consumer = review_consumer(client, namespace, name).await?;
    if let Some(deployment) = consumer
        && deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.replicas)
            .unwrap_or(1)
            != 0
    {
        Api::<Deployment>::namespaced(client.clone(), namespace)
            .patch(name, &PatchParams::default(), &Patch::Merge(json!({
                "metadata":{"uid":deployment.metadata.uid,"resourceVersion":deployment.metadata.resource_version},
                "spec":{"replicas":0},
            }))).await.map_err(api_error)?;
    }
    Ok(())
}

async fn checked_epoch(
    client: &Client,
    namespace: &str,
    name: &str,
    existing: Option<&Secret>,
) -> Result<Option<String>, String> {
    match crate::sre_authority::privacy_readiness(client, namespace).await {
        Ok(crate::sre_authority::PrivacyReadiness::Qualified(epoch)) => Ok(epoch),
        Ok(crate::sre_authority::PrivacyReadiness::Pending) => {
            Err("SRE privacy qualification is still pending; no credential issued or reused".into())
        }
        Err(error) => {
            if let Some(secret) = existing {
                quarantine(client, namespace, name, secret)
                    .await
                    .map_err(|failure| {
                        format!("{error}; owned control credential quarantine failed: {failure}")
                    })?;
            }
            Err(error)
        }
    }
}

pub(in crate::reconciler) async fn quarantine_on_privacy_loss(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), String> {
    let workspace = sandbox.namespace().ok_or("Sandbox workspace missing")?;
    let live = Api::<KarsSandbox>::namespaced(client.clone(), &workspace)
        .get(&sandbox.name_any())
        .await
        .map_err(api_error)?;
    if live.uid() != sandbox.uid() || live.metadata.deletion_timestamp.is_some() {
        return Err("Sandbox incarnation changed before credential quarantine".into());
    }
    let namespace = super::super::namespace_ownership::recheck(client, &live, namespace)
        .await
        .map_err(|_| "Namespace authority changed before credential quarantine")?;
    if crate::sre_authority::privacy_readiness(client, &namespace.name_any())
        .await
        .is_ok()
    {
        return Ok(());
    }
    let secret = Api::<Secret>::namespaced(client.clone(), &namespace.name_any())
        .get_opt(SECRET)
        .await
        .map_err(api_error)?;
    if let Some(secret) = secret {
        validate(
            &secret,
            live.metadata.uid.as_deref().ok_or("Sandbox UID missing")?,
            &namespace,
        )?;
        quarantine(client, &namespace.name_any(), &live.name_any(), &secret).await?;
    }
    Ok(())
}

pub(super) async fn ensure(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Projection, String> {
    if !super::super::namespace_ownership::claimed(namespace, sandbox)
        .map_err(|_| "Governed service credential namespace claim is invalid")?
        || sandbox.metadata.deletion_timestamp.is_some()
        || namespace.metadata.deletion_timestamp.is_some()
    {
        return Err("Governed service credential requires a live exact namespace claim".into());
    }
    let source_uid = sandbox
        .metadata
        .uid
        .as_deref()
        .ok_or("Sandbox UID missing")?;
    let namespace_name = namespace.name_any();
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace_name);
    let existing = secrets.get_opt(SECRET).await.map_err(api_error)?;
    if let Some(secret) = existing.as_ref() {
        validate(secret, source_uid, namespace)?;
    }
    let mut epoch = checked_epoch(
        client,
        &namespace_name,
        &sandbox.name_any(),
        existing.as_ref(),
    )
    .await?;
    let secret = if let Some(secret) = existing
        .as_ref()
        .filter(|secret| current(secret, epoch.as_deref()))
    {
        secret.clone()
    } else {
        if existing.is_some() {
            review_consumer(client, &namespace_name, &sandbox.name_any()).await?;
            // The ownership inventory awaited API calls. Recheck privacy at
            // the actual mint/write boundary, not only before that inventory.
            epoch = checked_epoch(
                client,
                &namespace_name,
                &sandbox.name_any(),
                existing.as_ref(),
            )
            .await?;
        }
        let mut annotations = json!({
            SOURCE_UID: source_uid, NAMESPACE_UID: namespace.metadata.uid,
            REVISION: crate::sre_privacy::REVISION,
        });
        if let Some(epoch) = epoch.as_ref() {
            annotations[EPOCH] = json!(epoch);
        }
        let material = crate::providers::signing::generate_service_token();
        if let Some(secret) = existing {
            annotations[RETIRED] = serde_json::Value::Null;
            if epoch.is_none() {
                annotations[EPOCH] = serde_json::Value::Null;
            }
            secrets.patch(SECRET, &PatchParams::default(), &Patch::Merge(json!({
                "metadata": {"uid": secret.metadata.uid, "resourceVersion": secret.metadata.resource_version,
                    "annotations": annotations},
                "stringData": {"control-token": material},
            }))).await.map_err(api_error)?
        } else {
            let definition: Secret = serde_json::from_value(json!({
                "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
                "metadata": {"name": SECRET, "namespace": namespace_name,
                    "labels": {"app.kubernetes.io/managed-by": "kars-controller"},
                    "annotations": annotations},
                "stringData": {"control-token": material},
            }))
            .map_err(|_| "Governed service credential serialization failed")?;
            secrets
                .create(&PostParams::default(), &definition)
                .await
                .map_err(api_error)?
        }
    };
    validate(&secret, source_uid, namespace)?;
    if !current(&secret, epoch.as_deref()) {
        return Err(
            "Governed service credential privacy stamp did not match the verified write".into(),
        );
    }
    Ok(Projection {
        version: format!(
            "{}:{}",
            secret.metadata.uid.unwrap(),
            secret.metadata.resource_version.unwrap()
        ),
    })
}
