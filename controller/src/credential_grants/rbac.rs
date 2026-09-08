// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube::api::{DeleteParams, PostParams, Preconditions};

fn name(grant: &KarsCredentialGrant) -> Result<String, String> {
    Ok(format!(
        "kars-credential-writer-{}",
        grant.uid().ok_or("Credential grant UID missing")?
    ))
}
fn owned(meta: &kube::api::ObjectMeta, grant: &KarsCredentialGrant) -> bool {
    meta.annotations.as_ref().is_some_and(|a| {
        a.get(GRANT_OWNER) == grant.metadata.uid.as_ref()
            && a.get("kars.azure.com/credential-workspace-uid") == Some(&grant.spec.workspace_uid)
    }) && meta.namespace == grant.metadata.namespace
        && identity(meta).is_ok()
}

pub(super) async fn apply(
    client: &Client,
    grant: &KarsCredentialGrant,
    sources: &[SourceMetadata],
) -> Result<(), String> {
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let stores: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    for store in &grant.spec.integration_stores {
        let current = stores
            .get_metadata(&store.secret.name)
            .await
            .map_err(|e| api_error("Read enrolled store marker", e))?;
        if identity(&current.metadata)?.0 != store.secret.uid {
            return Err("Enrolled store was replaced".into());
        }
        if current
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("kars.azure.com/credential-store-grant-uid"))
            != grant.metadata.uid.as_ref()
        {
            stores.patch_metadata(&store.secret.name,&PatchParams::default(),&Patch::Merge(json!({
                "metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version,
                    "annotations":{"kars.azure.com/credential-store-grant-uid":grant.metadata.uid}}
            }))).await.map_err(|e|api_error("Mark explicitly enrolled operator store",e))?;
        }
    }
    let name = name(grant)?;
    let mut names = sources
        .iter()
        .filter(|s| s.phase == "Ready" || s.phase == "Unbound")
        .map(|s| s.name.clone())
        .collect::<Vec<_>>();
    names.extend(
        grant
            .spec
            .integration_stores
            .iter()
            .map(|s| s.secret.name.clone()),
    );
    names.sort();
    names.dedup();
    let mut writable = sources.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    writable.extend(
        grant
            .spec
            .integration_stores
            .iter()
            .map(|s| s.secret.name.clone()),
    );
    writable.sort();
    writable.dedup();
    let mut rules = vec![
        json!({"apiGroups":[""],"resources":["secrets"],"verbs":["create"]}),
        json!({"apiGroups":["kars.azure.com"],"resources":["karscredentialgrants"],"resourceNames":[NAME],"verbs":["use-agent-credentials"]}),
    ];
    if !names.is_empty() {
        rules.push(
            json!({"apiGroups":[""],"resources":["secrets"],"resourceNames":names,"verbs":["get"]}),
        );
    }
    if !writable.is_empty() {
        rules.push(json!({"apiGroups":[""],"resources":["secrets"],"resourceNames":writable,"verbs":["patch","delete"]}));
    }
    let role:Role=serde_json::from_value(json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role",
        "metadata":{"name":name,"namespace":namespace,"annotations":{GRANT_OWNER:grant.metadata.uid,
            "kars.azure.com/credential-workspace-uid":grant.spec.workspace_uid},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant","name":NAME,
                "uid":grant.metadata.uid,"controller":true,"blockOwnerDeletion":false}]},
        "rules":rules})).map_err(|_|"Credential role serialization failed")?;
    let binding:RoleBinding=serde_json::from_value(json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding",
        "metadata":{"name":name,"namespace":namespace,"annotations":{GRANT_OWNER:grant.metadata.uid,
            "kars.azure.com/credential-workspace-uid":grant.spec.workspace_uid},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant","name":NAME,
                "uid":grant.metadata.uid,"controller":true,"blockOwnerDeletion":false}]},
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":name},
        "subjects":grant.spec.writers.iter().map(|w|json!({"kind":"ServiceAccount","namespace":w.namespace,"name":w.name})).collect::<Vec<_>>(),
    })).map_err(|_|"Credential binding serialization failed")?;
    let roles: Api<Role> = Api::namespaced(client.clone(), &namespace);
    match roles
        .get_opt(&name)
        .await
        .map_err(|e| api_error("Inspect credential writer role", e))?
    {
        None => {
            roles
                .create(&PostParams::default(), &role)
                .await
                .map_err(|e| api_error("Create credential writer role", e))?;
        }
        Some(old) => {
            if !owned(&old.metadata, grant) {
                return Err("Credential writer role belongs to another identity".into());
            }
            if old.rules != role.rules {
                roles
                    .patch(
                        &name,
                        &PatchParams::default(),
                        &Patch::Merge(json!({"metadata":{"uid":old.metadata.uid,
                    "resourceVersion":old.metadata.resource_version},"rules":role.rules})),
                    )
                    .await
                    .map_err(|e| api_error("Update owned credential writer role", e))?;
            }
        }
    }
    super::verify(client, grant).await?;
    let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), &namespace);
    match bindings
        .get_opt(&name)
        .await
        .map_err(|e| api_error("Inspect credential writer binding", e))?
    {
        None => {
            bindings
                .create(&PostParams::default(), &binding)
                .await
                .map_err(|e| api_error("Create credential writer binding", e))?;
        }
        Some(old) => {
            if !owned(&old.metadata, grant) || old.role_ref != binding.role_ref {
                return Err("Credential writer binding belongs to another authority".into());
            }
            if old.subjects != binding.subjects {
                bindings
                    .patch(
                        &name,
                        &PatchParams::default(),
                        &Patch::Merge(json!({"metadata":{"uid":old.metadata.uid,
                    "resourceVersion":old.metadata.resource_version},"subjects":binding.subjects})),
                    )
                    .await
                    .map_err(|e| api_error("Update credential writer subjects", e))?;
            }
        }
    }
    Ok(())
}

pub(super) async fn revoke(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    let namespace = grant.namespace().ok_or("Grant workspace missing")?;
    let name = name(grant)?;
    let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), &namespace);
    if let Some(binding) = bindings
        .get_opt(&name)
        .await
        .map_err(|e| api_error("Inspect revoked credential binding", e))?
    {
        if !owned(&binding.metadata, grant) {
            return Err("Foreign credential binding preserved".into());
        }
        bindings
            .delete(
                &name,
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: binding.metadata.uid,
                        resource_version: binding.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Revoke credential writer binding", e))?;
    }
    let roles: Api<Role> = Api::namespaced(client.clone(), &namespace);
    if let Some(role) = roles
        .get_opt(&name)
        .await
        .map_err(|e| api_error("Inspect revoked credential role", e))?
    {
        if !owned(&role.metadata, grant) {
            return Err("Foreign credential role preserved".into());
        }
        roles
            .delete(
                &name,
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: role.metadata.uid,
                        resource_version: role.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Revoke credential writer role", e))?;
    }
    Ok(())
}
