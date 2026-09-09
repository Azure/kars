// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Observer transport permissions; UID authorization is checked by the endpoint.

use super::*;
use k8s_openapi::api::rbac::v1::{Role, RoleBinding};
use kube::api::{DeleteParams, PostParams, Preconditions};

const LABEL: &str = "kars.azure.com/credential-operator-grant";

fn owned(meta: &kube::api::ObjectMeta, grant: &KarsCredentialGrant) -> bool {
    meta.labels.as_ref().and_then(|labels| labels.get(LABEL)) == grant.metadata.uid.as_ref()
        && meta.annotations.as_ref().and_then(|a| a.get(GRANT_OWNER)) == grant.metadata.uid.as_ref()
        && identity(meta).is_ok()
}

pub(super) async fn reconcile(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    let workspace = grant
        .namespace()
        .ok_or("Operator grant workspace missing")?;
    let controller = super::writers::controller_uid(client).await?;
    let sandboxes = Api::<crate::crd::KarsSandbox>::namespaced(client.clone(), &workspace)
        .list(&ListParams::default())
        .await
        .map_err(|e| api_error("Read operator grant targets", e))?;
    let mut expected = std::collections::BTreeSet::new();
    for sandbox in sandboxes {
        if !grant.spec.observation_targets.iter().any(|target| {
            target.kind == "KarsSandbox"
                && target.namespace == workspace
                && target.name == sandbox.name_any()
                && Some(target.uid.as_str()) == sandbox.metadata.uid.as_deref()
        }) {
            continue;
        }
        if sandbox.metadata.deletion_timestamp.is_some() {
            continue;
        }
        let namespace = format!("kars-{}", sandbox.name_any());
        let Some(ns) = Api::<Namespace>::all(client.clone())
            .get_opt(&namespace)
            .await
            .map_err(|e| api_error("Read operator target namespace", e))?
        else {
            continue;
        };
        if !crate::reconciler::namespace_ownership::claimed(&ns, &sandbox)
            .map_err(|_| "Operator namespace claim is invalid")?
        {
            continue;
        }
        crate::reconciler::namespace_ownership::recheck(client, &sandbox, &ns)
            .await
            .map_err(|_| "Operator target ownership changed")?;
        expected.insert(namespace.clone());
        let name = format!("kars-credential-operator-{}", identity(&grant.metadata)?.0);
        let metadata = json!({"name":name,"namespace":namespace,"labels":{LABEL:grant.metadata.uid},
            "annotations":{GRANT_OWNER:grant.metadata.uid,"kars.azure.com/credential-reader-controller-uid":controller,
                "kars.azure.com/sandbox-uid":sandbox.metadata.uid,
                "kars.azure.com/namespace-uid":ns.metadata.uid},
            "ownerReferences":[{"apiVersion":"v1","kind":"Namespace","name":namespace,"uid":ns.metadata.uid,
                "controller":true,"blockOwnerDeletion":false}]});
        let role:Role=serde_json::from_value(json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role",
            "metadata":metadata,"rules":[{"apiGroups":[""],"resources":["secrets"],"resourceNames":["router-services-observer"],"verbs":["get"]}]}))
            .map_err(|_|"Operator role serialization failed")?;
        let binding:RoleBinding=serde_json::from_value(json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding",
            "metadata":metadata,"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":name},
            "subjects":grant.spec.writers.iter().map(|writer|json!({"kind":"ServiceAccount","namespace":writer.namespace,"name":writer.name})).collect::<Vec<_>>()}))
            .map_err(|_|"Operator binding serialization failed")?;
        let roles: Api<Role> = Api::namespaced(client.clone(), &namespace);
        if let Some(old) = roles
            .get_opt(&name)
            .await
            .map_err(|e| api_error("Read operator role", e))?
        {
            if !owned(&old.metadata, grant)
                || old.rules != role.rules
                || old
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("kars.azure.com/credential-reader-controller-uid"))
                    != Some(&controller)
                || old
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("kars.azure.com/sandbox-uid"))
                    != sandbox.metadata.uid.as_ref()
                || old
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("kars.azure.com/namespace-uid"))
                    != ns.metadata.uid.as_ref()
            {
                return Err("Operator role target identity changed".into());
            }
        } else {
            roles
                .create(&PostParams::default(), &role)
                .await
                .map_err(|e| api_error("Create exact-name operator role", e))?;
        }
        super::verify(client, grant).await?;
        super::writers::verify(client, grant).await?;
        let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), &namespace);
        if let Some(old) = bindings
            .get_opt(&name)
            .await
            .map_err(|e| api_error("Read operator binding", e))?
        {
            if !owned(&old.metadata, grant)
                || old.role_ref != binding.role_ref
                || old
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("kars.azure.com/credential-reader-controller-uid"))
                    != Some(&controller)
            {
                return Err("Foreign operator binding preserved".into());
            }
            if old.subjects != binding.subjects {
                bindings.patch(&name,&PatchParams::default(),&Patch::Merge(json!({
                    "metadata":{"uid":old.metadata.uid,"resourceVersion":old.metadata.resource_version},"subjects":binding.subjects
                }))).await.map_err(|e|api_error("Update owned operator identities",e))?;
            }
        } else {
            bindings
                .create(&PostParams::default(), &binding)
                .await
                .map_err(|e| api_error("Create owned operator binding", e))?;
        }
    }
    revoke_except(client, grant, &expected).await
}

pub(super) async fn revoke(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    revoke_except(client, grant, &std::collections::BTreeSet::new()).await
}

async fn revoke_except(
    client: &Client,
    grant: &KarsCredentialGrant,
    keep: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let selector = format!(
        "{LABEL}={}",
        grant.uid().ok_or("Operator grant UID missing")?
    );
    let bindings = Api::<RoleBinding>::all(client.clone())
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(|e| api_error("Read owned operator bindings for revocation", e))?;
    for binding in bindings {
        if binding
            .namespace()
            .is_some_and(|namespace| keep.contains(&namespace))
        {
            continue;
        }
        if !owned(&binding.metadata, grant) {
            return Err("Foreign operator binding preserved".into());
        }
        let namespace = binding
            .namespace()
            .ok_or("Operator binding namespace missing")?;
        Api::<RoleBinding>::namespaced(client.clone(), &namespace)
            .delete(
                &binding.name_any(),
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: binding.metadata.uid,
                        resource_version: binding.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Revoke exact-name operator binding", e))?;
    }
    let roles = Api::<Role>::all(client.clone())
        .list(&ListParams::default().labels(&selector))
        .await
        .map_err(|e| api_error("Read owned operator roles for revocation", e))?;
    for role in roles {
        if role
            .namespace()
            .is_some_and(|namespace| keep.contains(&namespace))
        {
            continue;
        }
        if !owned(&role.metadata, grant) {
            return Err("Foreign operator role preserved".into());
        }
        let namespace = role.namespace().ok_or("Operator role namespace missing")?;
        Api::<Role>::namespaced(client.clone(), &namespace)
            .delete(
                &role.name_any(),
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: role.metadata.uid,
                        resource_version: role.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Revoke exact-name operator role", e))?;
    }
    Ok(())
}
