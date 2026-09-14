// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};

fn selected(grant: &KarsCredentialGrant, account: &ServiceAccount) -> bool {
    grant.spec.writers.iter().any(|writer| {
        account.metadata.namespace.as_deref() == Some(writer.namespace.as_str())
            && account.metadata.name.as_deref() == Some(writer.name.as_str())
            && account.metadata.uid.as_deref() == Some(writer.uid.as_str())
            && account.metadata.deletion_timestamp.is_none()
    })
}

pub(super) async fn stale(
    client: &Client,
    grant: &KarsCredentialGrant,
    key: &str,
) -> Result<bool, String> {
    let accounts = Api::<ServiceAccount>::all(client.clone())
        .list(&ListParams::default().labels(key))
        .await
        .map_err(|e| api_error("Inventory guarded writer identities", e))?;
    if accounts.iter().any(|account| !selected(grant, account)) {
        return Ok(true);
    }
    let namespaces = Api::<Namespace>::all(client.clone())
        .list(&ListParams::default().labels(key))
        .await
        .map_err(|e| api_error("Inventory guarded writer namespaces", e))?;
    Ok(namespaces.iter().any(|namespace| {
        !grant
            .spec
            .writers
            .iter()
            .any(|writer| writer.namespace == namespace.name_any())
    }))
}

pub(super) async fn stale_readers(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<bool, String> {
    let workspace = grant.namespace().ok_or("Grant workspace missing")?;
    let uid = grant.uid().ok_or("Grant UID missing")?;
    let name = format!("kars-credential-writer-{uid}");
    let mut bindings = Api::<RoleBinding>::all(client.clone())
        .list(
            &ListParams::default()
                .labels(&format!("kars.azure.com/credential-operator-grant={uid}")),
        )
        .await
        .map_err(|e| api_error("Read enrolled observation subjects", e))?
        .items;
    if let Some(binding) = Api::<RoleBinding>::namespaced(client.clone(), &workspace)
        .get_opt(&name)
        .await
        .map_err(|e| api_error("Read enrolled writer subjects", e))?
    {
        bindings.push(binding);
    }
    Ok(bindings.iter().any(|binding| {
        binding.subjects.as_ref().is_some_and(|subjects| {
            subjects.iter().any(|subject| {
                !grant.spec.writers.iter().any(|writer| {
                    subject.kind == "ServiceAccount"
                        && subject.name == writer.name
                        && subject.namespace.as_deref() == Some(writer.namespace.as_str())
                })
            })
        })
    }))
}

fn patch(
    meta: &kube::api::ObjectMeta,
    key: &str,
    controller: Option<&str>,
    ns_uid: &str,
) -> serde_json::Value {
    let mut finalizers = meta.finalizers.clone().unwrap_or_default();
    finalizers.retain(|entry| entry != key);
    if controller.is_some() {
        finalizers.push(key.into());
    }
    json!({"metadata":{"uid":meta.uid,"resourceVersion":meta.resource_version,
        "finalizers":finalizers,"annotations":{key:controller},
        "labels":{key:controller.map(|_|ns_uid)}}})
}

pub(super) async fn protect(
    client: &Client,
    namespace: &Namespace,
    account: &ServiceAccount,
    key: &str,
    controller: &str,
) -> Result<(), String> {
    let uid = identity(&namespace.metadata)?.0;
    if !namespace_held(namespace) {
        return Err(
            "Writer namespace lacks its native finalization hold; no read authority may be issued"
                .into(),
        );
    }
    let namespaces = Api::<Namespace>::all(client.clone());
    if !protected(&namespace.metadata, key, uid) {
        namespaces
            .patch_metadata(
                &namespace.name_any(),
                &PatchParams::default(),
                &Patch::Merge(patch(&namespace.metadata, key, Some(controller), uid)),
            )
            .await
            .map_err(|e| api_error("Protect enrolled writer namespace continuity", e))?;
    }
    if !protected(&account.metadata, key, uid) {
        Api::<ServiceAccount>::namespaced(client.clone(), &namespace.name_any())
            .patch_metadata(
                &account.name_any(),
                &PatchParams::default(),
                &Patch::Merge(patch(&account.metadata, key, Some(controller), uid)),
            )
            .await
            .map_err(|e| api_error("Protect enrolled writer name continuity", e))?;
    }
    Ok(())
}

pub(super) async fn no_read_authority(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<(), String> {
    let workspace = grant.namespace().ok_or("Credential workspace missing")?;
    let uid = grant.uid().ok_or("Credential grant UID missing")?;
    let name = format!("kars-credential-writer-{uid}");
    if Api::<Role>::namespaced(client.clone(), &workspace)
        .get_opt(&name)
        .await
        .map_err(|e| api_error("Verify writer Role retirement", e))?
        .is_some()
        || Api::<RoleBinding>::namespaced(client.clone(), &workspace)
            .get_opt(&name)
            .await
            .map_err(|e| api_error("Verify writer binding retirement", e))?
            .is_some()
    {
        return Err("Writer read authority is still retiring; enrolled names remain held".into());
    }
    let selector =
        ListParams::default().labels(&format!("kars.azure.com/credential-operator-grant={uid}"));
    if !Api::<Role>::all(client.clone())
        .list(&selector)
        .await
        .map_err(|e| api_error("Verify observation Role retirement", e))?
        .items
        .is_empty()
        || !Api::<RoleBinding>::all(client.clone())
            .list(&selector)
            .await
            .map_err(|e| api_error("Verify observation binding retirement", e))?
            .items
            .is_empty()
    {
        return Err(
            "Observation read authority is still retiring; enrolled names remain held".into(),
        );
    }
    Ok(())
}

pub(super) async fn release_stale(
    client: &Client,
    grant: &KarsCredentialGrant,
    key: &str,
) -> Result<(), String> {
    no_read_authority(client, grant).await?;
    let selector = ListParams::default().labels(key);
    let accounts = Api::<ServiceAccount>::all(client.clone())
        .list(&selector)
        .await
        .map_err(|e| api_error("Read guarded identities for retirement", e))?;
    for account in accounts {
        if selected(grant, &account) {
            continue;
        }
        let namespace = account
            .namespace()
            .ok_or("Guarded account namespace missing")?;
        Api::<ServiceAccount>::namespaced(client.clone(), &namespace)
            .patch_metadata(
                &account.name_any(),
                &PatchParams::default(),
                &Patch::Merge(patch(&account.metadata, key, None, "")),
            )
            .await
            .map_err(|e| api_error("Release retired writer name", e))?;
    }
    for namespace in Api::<Namespace>::all(client.clone())
        .list(&selector)
        .await
        .map_err(|e| api_error("Read guarded namespaces for retirement", e))?
    {
        if grant
            .spec
            .writers
            .iter()
            .any(|writer| writer.namespace == namespace.name_any())
        {
            continue;
        }
        no_read_authority(client, grant).await?;
        Api::<Namespace>::all(client.clone())
            .patch_metadata(
                &namespace.name_any(),
                &PatchParams::default(),
                &Patch::Merge(patch(&namespace.metadata, key, None, "")),
            )
            .await
            .map_err(|e| api_error("Release retired writer namespace", e))?;
    }
    Ok(())
}
