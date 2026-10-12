// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    crd::{KarsSandbox, RuntimeKind},
    skill_delivery as delivery,
};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{Api, Client, ResourceExt, api::PostParams};
use serde_json::json;

pub(super) async fn project(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Option<String>, String> {
    let names = match sandbox.annotations().get(delivery::SELECTION) {
        None => Vec::new(),
        Some(value) => value.split(',').map(String::from).collect(),
    };
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), &namespace.name_any());
    let existing = api
        .get_opt(delivery::CATALOG_NAME)
        .await
        .map_err(|e| e.to_string())?;
    if names.is_empty() {
        if existing.is_some() {
            return Err(
                "Skill selection changed on an existing runtime; recreate the sandbox".into(),
            );
        }
        return Ok(None);
    }
    if !matches!(
        sandbox.spec.runtime.kind,
        RuntimeKind::OpenClaw | RuntimeKind::Hermes
    ) {
        return Err("Selected skill packages require OpenClaw or Hermes".into());
    }
    let governance = sandbox
        .spec
        .governance
        .as_ref()
        .ok_or("Skills require governance")?;
    let policy = Some(governance.tool_policy_ref.name.as_str());
    let servers = governance
        .mcp_server_refs
        .iter()
        .map(|r| r.name.clone())
        .collect::<Vec<_>>();
    let source_namespace = sandbox
        .namespace()
        .ok_or("Sandbox source namespace missing")?;
    let skills = delivery::resolve(client, &source_namespace, &names, policy, &servers).await?;
    let uid = sandbox
        .metadata
        .uid
        .as_deref()
        .ok_or("Sandbox UID missing")?;
    let namespace_uid = namespace
        .metadata
        .uid
        .as_deref()
        .ok_or("Namespace UID missing")?;
    let (text, digest) = delivery::catalog(uid, skills)?;
    let desired: ConfigMap = serde_json::from_value(json!({
        "apiVersion":"v1","kind":"ConfigMap",
        "metadata":{
            "name":delivery::CATALOG_NAME,"namespace":namespace.name_any(),
            "annotations":{"kars.azure.com/sandbox-uid":uid},
            "ownerReferences":[{
                "apiVersion":"v1","kind":"Namespace","name":namespace.name_any(),
                "uid":namespace_uid,"controller":true,"blockOwnerDeletion":false
            }]
        },
        "immutable":true,"data":{"catalog.json":text}
    }))
    .map_err(|e| e.to_string())?;
    super::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|e| e.to_string())?;
    match existing {
        Some(old) => {
            if old.metadata.deletion_timestamp.is_some()
                || old.immutable != Some(true)
                || old.metadata.owner_references != desired.metadata.owner_references
                || old.metadata.annotations != desired.metadata.annotations
                || old.data != desired.data
                || old.binary_data.is_some()
            {
                return Err("Approved skill snapshot changed or has foreign ownership; recreate the sandbox after review".into());
            }
        }
        None => {
            api.create(&PostParams::default(), &desired)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(Some(digest))
}
