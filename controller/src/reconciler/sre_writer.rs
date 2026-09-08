// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Materialize the permissionless SRE writer identity only in a claimed runtime
//! namespace. Compatible Helm accounts are left byte-for-byte unchanged.

use k8s_openapi::api::core::v1::{Namespace, ServiceAccount};
use kube::{Api, Client, ResourceExt, api::PostParams};
use serde_json::json;

use super::namespace_ownership::{
    self, Error, NAMESPACE_UID, SOURCE_NAME, SOURCE_NAMESPACE, SOURCE_UID,
};
use crate::crd::KarsSandbox;

const NAME: &str = "sre-writer";
const NAMESPACE: &str = "kars-sre";
const MANAGED_BY: &str = "app.kubernetes.io/managed-by";
const HELM_NAME: &str = "meta.helm.sh/release-name";
const HELM_NAMESPACE: &str = "meta.helm.sh/release-namespace";

fn conflict() -> Error {
    Error::Conflict("SRE writer ServiceAccount has conflicting data or unproven ownership".into())
}

fn desired(sandbox: &KarsSandbox, namespace: &Namespace) -> Result<ServiceAccount, Error> {
    Ok(serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ServiceAccount",
        "metadata": {
            "name": NAME,
            "namespace": NAMESPACE,
            "labels": {
                "app.kubernetes.io/name": "kars",
                "app.kubernetes.io/component": "sre",
                MANAGED_BY: "kars-controller",
                "kars.azure.com/role": NAME
            },
            "annotations": {
                "kars.azure.com/no-automount": "true",
                SOURCE_NAME: sandbox.name_any(),
                SOURCE_NAMESPACE: sandbox.namespace(),
                SOURCE_UID: sandbox.metadata.uid,
                NAMESPACE_UID: namespace.metadata.uid
            },
            "ownerReferences": [{
                "apiVersion": "v1", "kind": "Namespace", "name": NAMESPACE,
                "uid": namespace.metadata.uid, "controller": true, "blockOwnerDeletion": false
            }]
        },
        "automountServiceAccountToken": false
    }))?)
}

fn compatible(
    account: &ServiceAccount,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), Error> {
    if account.name_any() != NAME
        || account.namespace().as_deref() != Some(NAMESPACE)
        || account.metadata.uid.as_deref().is_none_or(str::is_empty)
        || account
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || account.metadata.deletion_timestamp.is_some()
        || account.automount_service_account_token != Some(false)
        || account
            .secrets
            .as_ref()
            .is_some_and(|refs| !refs.is_empty())
        || account
            .image_pull_secrets
            .as_ref()
            .is_some_and(|refs| !refs.is_empty())
        || account
            .labels()
            .get("kars.azure.com/role")
            .map(String::as_str)
            != Some(NAME)
        || account
            .annotations()
            .get("kars.azure.com/no-automount")
            .is_some_and(|value| value != "true")
    {
        return Err(conflict());
    }
    let expected = desired(sandbox, namespace)?;
    let annotations = account.annotations();
    for key in [SOURCE_NAME, SOURCE_NAMESPACE, SOURCE_UID, NAMESPACE_UID] {
        if annotations
            .get(key)
            .is_some_and(|value| expected.annotations().get(key) != Some(value))
        {
            return Err(conflict());
        }
    }
    let has_helm_owner = annotations.contains_key(HELM_NAME)
        || annotations.contains_key(HELM_NAMESPACE)
        || account
            .labels()
            .get(MANAGED_BY)
            .is_some_and(|value| value == "Helm");
    if has_helm_owner {
        // CLI-created SRE CRs may coexist with a retained Helm-owned namespace.
        // Either explicit source ownership or that verified namespace must
        // identify the exact release; never infer ownership from the SA alone.
        let owner = if sandbox.annotations().contains_key(HELM_NAME)
            || sandbox.annotations().contains_key(HELM_NAMESPACE)
        {
            sandbox.annotations()
        } else {
            namespace.annotations()
        };
        let release = owner.get(HELM_NAME).filter(|name| !name.is_empty());
        if release.is_none()
            || annotations.get(HELM_NAME) != release
            || annotations.get(HELM_NAMESPACE).map(String::as_str)
                != sandbox.metadata.namespace.as_deref()
            || owner.get(HELM_NAMESPACE).map(String::as_str)
                != sandbox.metadata.namespace.as_deref()
            || account.labels().get(MANAGED_BY).map(String::as_str) != Some("Helm")
            || account
                .metadata
                .owner_references
                .as_ref()
                .is_some_and(|refs| !refs.is_empty())
        {
            return Err(conflict());
        }
    } else if account.labels().get(MANAGED_BY).map(String::as_str) != Some("kars-controller")
        || account.metadata.owner_references != expected.metadata.owner_references
        || [SOURCE_NAME, SOURCE_NAMESPACE, SOURCE_UID, NAMESPACE_UID]
            .iter()
            .any(|key| annotations.get(*key) != expected.annotations().get(*key))
    {
        return Err(conflict());
    }
    Ok(())
}

pub async fn ensure(
    client: &Client,
    sandbox: &KarsSandbox,
    accounts: &Api<ServiceAccount>,
    namespace: Option<&Namespace>,
) -> Result<(), Error> {
    if sandbox.name_any() != "sre"
        || sandbox
            .labels()
            .get("kars.azure.com/role")
            .map(String::as_str)
            != Some("sre")
    {
        return Ok(());
    }
    if sandbox.metadata.deletion_timestamp.is_some()
        || accounts.resource_url() != "/api/v1/namespaces/kars-sre/serviceaccounts"
    {
        return Err(conflict());
    }
    let namespace = namespace.ok_or_else(conflict)?;
    let namespace = namespace_ownership::recheck(client, sandbox, namespace).await?;
    if let Some(existing) = accounts.get_opt(NAME).await? {
        return compatible(&existing, sandbox, &namespace);
    }
    accounts
        .create(
            &PostParams {
                field_manager: Some(crate::field_managers::CLAWSANDBOX.into()),
                ..Default::default()
            },
            &desired(sandbox, &namespace)?,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "sre_writer_tests.rs"]
mod tests;
