// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Scoped issuance fences, owner-CAS application and pending protection.

use super::{EPOCH, ERROR, PREFIX, bundle_revision, inspect_namespace, live, namespace_epoch};
use crate::{crd::KarsSandbox, credential_grant::KarsCredentialGrant};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Namespace};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// Private activation is explicit; unrelated standalone runtimes stay unchanged.
pub(crate) async fn for_sandbox(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Option<String>, String> {
    let namespace = crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| ERROR)?;
    if let Some(epoch) = namespace_epoch(client, &namespace).await? {
        inspect_namespace(client, &namespace, &epoch).await?;
        return Ok(Some(epoch));
    }
    let workspace = sandbox.namespace().ok_or(ERROR)?;
    let Some(grant) = Api::<KarsCredentialGrant>::namespaced(client.clone(), &workspace)
        .get_opt("workspace")
        .await
        .map_err(|_| ERROR)?
    else {
        return Ok(None);
    };
    if !grant.spec.enabled || grant.spec.writers.is_empty() {
        return Ok(None);
    }
    let selected = grant.spec.observation_targets.iter().any(|target| {
        target.name == sandbox.name_any()
            && Some(target.uid.as_str()) == sandbox.metadata.uid.as_deref()
    }) || grant
        .spec
        .private_activation
        .as_ref()
        .is_some_and(|activation| {
            activation
                .namespaces
                .iter()
                .any(|scope| scope.namespace.name == namespace.name_any())
        });
    if !selected {
        return Ok(None);
    }
    Err(
        "Private target namespace requires reviewed grant activation before issuance or reuse"
            .into(),
    )
}

pub(crate) fn stamp_matches(
    secret: &k8s_openapi::api::core::v1::Secret,
    epoch: Option<&str>,
) -> bool {
    epoch.is_none_or(|epoch| {
        secret
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(EPOCH))
            .map(String::as_str)
            == Some(epoch)
    })
}

pub(crate) fn different_rsa_keys(old: &str, new: &str) -> Result<bool, String> {
    use rsa::{RsaPrivateKey, pkcs1::DecodeRsaPrivateKey, pkcs8::DecodePrivateKey};
    let parse = |value: &str| {
        RsaPrivateKey::from_pkcs8_pem(value)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(value))
            .map(|key| key.to_public_key())
            .map_err(|_| "Private App key cannot be qualified for rotation".to_string())
    };
    Ok(parse(old)? != parse(new)?)
}

pub(crate) fn approved_deployment(
    namespace: &Namespace,
    deployment: &Deployment,
    epoch: &str,
) -> bool {
    deployment.uid().is_some_and(|uid| {
        namespace
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(&format!("{PREFIX}parent-{uid}")))
            .map(String::as_str)
            == Some(epoch)
    })
}

pub(crate) async fn required_in_namespace(
    client: &Client,
    namespace: &Namespace,
) -> Result<bool, String> {
    if namespace
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(&format!("{PREFIX}enabled")))
        .map(String::as_str)
        == Some("true")
    {
        return Ok(true);
    }
    let Some(workspace) = namespace
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("kars.azure.com/sandbox-namespace"))
    else {
        return Ok(false);
    };
    Ok(
        Api::<KarsCredentialGrant>::namespaced(client.clone(), workspace)
            .get_opt("workspace")
            .await
            .map_err(|_| ERROR)?
            .is_some_and(|grant| {
                grant.spec.enabled
                    && !grant.spec.writers.is_empty()
                    && (grant
                        .spec
                        .observation_targets
                        .iter()
                        .any(|target| format!("kars-{}", target.name) == namespace.name_any())
                        || grant
                            .spec
                            .private_activation
                            .as_ref()
                            .is_some_and(|activation| {
                                activation
                                    .namespaces
                                    .iter()
                                    .any(|scope| scope.namespace.name == namespace.name_any())
                            }))
            }),
    )
}

pub(crate) async fn apply_deployment(
    client: &Client,
    sandbox: &KarsSandbox,
    deployment: &mut Deployment,
) -> Result<bool, String> {
    use kube::api::PostParams;
    let Some(epoch) = deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.template.metadata.as_ref())
        .and_then(|meta| meta.annotations.as_ref())
        .and_then(|a| a.get(EPOCH))
        .cloned()
    else {
        return Ok(false);
    };
    let namespace_name = format!("kars-{}", sandbox.name_any());
    let namespace = Api::<Namespace>::all(client.clone())
        .get(&namespace_name)
        .await
        .map_err(|_| ERROR)?;
    crate::reconciler::namespace_ownership::recheck(client, sandbox, &namespace)
        .await
        .map_err(|_| ERROR)?;
    if namespace_epoch(client, &namespace).await?.as_deref() != Some(epoch.as_str()) {
        return Err(ERROR.into());
    }
    let current =
        Api::<KarsSandbox>::namespaced(client.clone(), &sandbox.namespace().ok_or(ERROR)?)
            .get(&sandbox.name_any())
            .await
            .map_err(|_| ERROR)?;
    if current.uid() != sandbox.uid()
        || current.metadata.generation != sandbox.metadata.generation
        || current.metadata.deletion_timestamp.is_some()
    {
        return Err(ERROR.into());
    }
    let api = Api::<Deployment>::namespaced(client.clone(), &namespace_name);
    let previous = api.get_opt(&sandbox.name_any()).await.map_err(|_| ERROR)?;
    let applied = if let Some(previous) = previous {
        live(&previous.metadata)?;
        if !approved_deployment(&namespace, &previous, &epoch)
            || deployment
                .metadata
                .uid
                .as_ref()
                .is_some_and(|uid| Some(uid) != previous.metadata.uid.as_ref())
            || deployment
                .metadata
                .resource_version
                .as_ref()
                .is_some_and(|rv| Some(rv) != previous.metadata.resource_version.as_ref())
        {
            return Err("Unreviewed or changed private runtime Deployment preserved".into());
        }
        deployment.metadata.uid = previous.metadata.uid;
        deployment.metadata.resource_version = previous.metadata.resource_version;
        api.patch(
            &sandbox.name_any(),
            &PatchParams::apply(crate::field_managers::CLAWSANDBOX).force(),
            &Patch::Apply(deployment.clone()),
        )
        .await
        .map_err(|_| ERROR)?
    } else {
        if deployment.metadata.uid.is_some() || deployment.metadata.resource_version.is_some() {
            return Err("Reviewed private runtime disappeared; no replacement was adopted".into());
        }
        api.create(
            &PostParams {
                field_manager: Some(crate::field_managers::CLAWSANDBOX.into()),
                ..Default::default()
            },
            deployment,
        )
        .await
        .map_err(|_| "Private runtime CREATE conflicted; existing object preserved")?
    };
    let uid = live(&applied.metadata)?.0.to_string();
    let fresh = Api::<Namespace>::all(client.clone())
        .get(&namespace_name)
        .await
        .map_err(|_| ERROR)?;
    if fresh.uid() != namespace.uid()
        || namespace_epoch(client, &fresh).await?.as_deref() != Some(epoch.as_str())
    {
        return Err(ERROR.into());
    }
    let key = format!("{PREFIX}parent-{uid}");
    if fresh
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(&key))
        != Some(&epoch)
    {
        Api::<Namespace>::all(client.clone()).patch(&namespace_name, &PatchParams::default(), &Patch::Merge(json!({
            "metadata":{"uid":fresh.metadata.uid,"resourceVersion":fresh.metadata.resource_version,
                "annotations":{key:epoch}}
        }))).await.map_err(|_| ERROR)?;
    }
    Ok(true)
}

pub(crate) async fn protect_pending(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<(), String> {
    if !grant.spec.enabled || grant.spec.writers.is_empty() {
        return Ok(());
    }
    bundle_revision(client).await?;
    use k8s_openapi::api::authentication::v1::SelfSubjectReview;
    use kube::api::PostParams;
    let subject = Api::<SelfSubjectReview>::all(client.clone())
        .create(&PostParams::default(), &SelfSubjectReview::default())
        .await
        .map_err(|_| ERROR)?;
    let subject = serde_json::to_value(subject).map_err(|_| ERROR)?;
    let user = subject["status"]["userInfo"]["username"]
        .as_str()
        .ok_or(ERROR)?;
    let uid = subject["status"]["userInfo"]["uid"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or(ERROR)?;
    let (root, account) = user
        .strip_prefix("system:serviceaccount:")
        .and_then(|v| v.split_once(':'))
        .ok_or(ERROR)?;
    if account != "kars-controller" {
        return Err(ERROR.into());
    }
    let workspace = grant.namespace().ok_or(ERROR)?;
    let mut scopes = BTreeSet::from([workspace.clone(), root.to_string()]);
    scopes.extend(
        grant
            .spec
            .writers
            .iter()
            .map(|writer| writer.namespace.clone()),
    );
    scopes.extend(
        grant
            .spec
            .observation_targets
            .iter()
            .map(|target| format!("kars-{}", target.name)),
    );
    let api = Api::<Namespace>::all(client.clone());
    for name in scopes {
        let Some(namespace) = api.get_opt(&name).await.map_err(|_| ERROR)? else {
            continue;
        };
        let namespace_uid = live(&namespace.metadata)?.0.to_string();
        if name == workspace && namespace_uid != grant.spec.workspace_uid {
            return Err(ERROR.into());
        }
        let fields = BTreeMap::from([
            (format!("{PREFIX}enabled"), "true".to_string()),
            (format!("{PREFIX}state"), "Pending".to_string()),
            (format!("{PREFIX}namespace-uid"), namespace_uid),
            (format!("{PREFIX}root-namespace"), root.to_string()),
            (format!("{PREFIX}root-account"), account.to_string()),
            (format!("{PREFIX}root-user"), user.to_string()),
            (format!("{PREFIX}root-uid"), uid.to_string()),
        ]);
        if namespace
            .metadata
            .annotations
            .as_ref()
            .is_some_and(|a| fields.iter().all(|(key, value)| a.get(key) == Some(value)))
        {
            continue;
        }
        api.patch(&name, &PatchParams::default(), &Patch::Merge(json!({
            "metadata":{"uid":namespace.metadata.uid,"resourceVersion":namespace.metadata.resource_version,"annotations":fields}
        }))).await.map_err(|_| ERROR)?;
    }
    Ok(())
}
