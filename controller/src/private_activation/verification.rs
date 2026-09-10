// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only enforcing-bundle, root, profile and receipt verification.

use super::{CONTRACT, ERROR, PREFIX, bundle, field, hash, inspect_namespace, live};
use crate::{
    crd::KarsSandbox,
    credential_grant::{KarsCredentialGrant, activation::ControllerProfile},
};
use k8s_openapi::api::{
    admissionregistration::v1::{ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding},
    apps::v1::Deployment,
    core::v1::{Namespace, ServiceAccount},
};
use kube::{Api, Client, ResourceExt};
use serde_json::json;
use std::collections::BTreeSet;

fn root_environment<'a>(deployment: &'a Deployment, name: &str) -> Result<Option<&'a str>, String> {
    let containers = &deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .ok_or(ERROR)?
        .containers;
    let controller = containers
        .iter()
        .find(|container| container.name == "controller")
        .ok_or(ERROR)?;
    let values: Vec<_> = controller
        .env
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.name == name)
        .collect();
    if values.len() > 1 || values.iter().any(|entry| entry.value_from.is_some()) {
        return Err(ERROR.into());
    }
    Ok(values.first().and_then(|entry| entry.value.as_deref()))
}

fn budget_namespace(deployment: &Deployment, root: &str) -> Result<String, String> {
    if let Some(value) = root_environment(deployment, "KARS_NAMESPACE")?
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Ok(value.into());
    }
    let containers = &deployment
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .ok_or(ERROR)?
        .containers;
    let controller = containers
        .iter()
        .find(|container| container.name == "controller")
        .ok_or(ERROR)?;
    let entries: Vec<_> = controller
        .env
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.name == "POD_NAMESPACE")
        .collect();
    if entries.len() > 1 {
        return Err(ERROR.into());
    }
    let Some(entry) = entries.first() else {
        return Ok("kars-system".into());
    };
    if let Some(value) = entry.value.as_deref() {
        let value = if value.trim().is_empty() {
            "kars-system"
        } else {
            value.trim()
        };
        return Ok(value.into());
    }
    if entry
        .value_from
        .as_ref()
        .and_then(|source| source.field_ref.as_ref())
        .is_some_and(|field| field.field_path == "metadata.namespace")
    {
        return Ok(root.into());
    }
    Err(ERROR.into())
}

pub(crate) async fn bundle_revision(client: &Client) -> Result<String, String> {
    let mut identities = Vec::new();
    for definition in bundle()["objects"].as_array().ok_or(ERROR)? {
        let name = definition["metadata"]["name"].as_str().ok_or(ERROR)?;
        let kind = definition["kind"].as_str().ok_or(ERROR)?;
        let (meta, spec) = if kind == "ValidatingAdmissionPolicy" {
            let policy = Api::<ValidatingAdmissionPolicy>::all(client.clone())
                .get(name)
                .await
                .map_err(|_| ERROR)?;
            if policy.metadata.generation.is_none()
                || policy.status.as_ref().is_none_or(|status| {
                    status.observed_generation != policy.metadata.generation
                        || status.type_checking.as_ref().is_none_or(|check| {
                            check
                                .expression_warnings
                                .as_ref()
                                .is_some_and(|v| !v.is_empty())
                        })
                })
            {
                return Err(ERROR.into());
            }
            (
                policy.metadata,
                serde_json::to_value(policy.spec).map_err(|_| ERROR)?,
            )
        } else {
            let binding = Api::<ValidatingAdmissionPolicyBinding>::all(client.clone())
                .get(name)
                .await
                .map_err(|_| ERROR)?;
            (
                binding.metadata,
                serde_json::to_value(binding.spec).map_err(|_| ERROR)?,
            )
        };
        let (uid, version) = live(&meta)?;
        if meta.name.as_deref() != Some(name) || spec != definition["spec"] {
            return Err(ERROR.into());
        }
        identities.push(json!({"kind":kind,"name":name,"uid":uid,"resourceVersion":version}));
    }
    Ok(hash(&json!(identities)))
}

pub(crate) async fn namespace_epoch(
    client: &Client,
    namespace: &Namespace,
) -> Result<Option<String>, String> {
    if namespace
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(&format!("{PREFIX}enabled")))
        .map(String::as_str)
        != Some("true")
    {
        return Ok(None);
    }
    let epoch = field(namespace, "epoch")?;
    if field(namespace, "state")? != "Qualified"
        || field(namespace, "namespace-uid")? != live(&namespace.metadata)?.0
        || epoch.len() != 64
        || !epoch.bytes().all(|b| b.is_ascii_hexdigit())
        || field(namespace, "bundle-revision")? != bundle_revision(client).await?
    {
        return Err(ERROR.into());
    }
    let root_namespace = field(namespace, "root-namespace")?;
    let root_account = field(namespace, "root-account")?;
    let ns = Api::<Namespace>::all(client.clone())
        .get(&root_namespace)
        .await
        .map_err(|_| ERROR)?;
    if live(&ns.metadata)?.0 != field(namespace, "root-namespace-uid")? {
        return Err(ERROR.into());
    }
    let account = Api::<ServiceAccount>::namespaced(client.clone(), &root_namespace)
        .get(&root_account)
        .await
        .map_err(|_| ERROR)?;
    let deployment = Api::<Deployment>::namespaced(client.clone(), &root_namespace)
        .get(&field(namespace, "root-deployment")?)
        .await
        .map_err(|_| ERROR)?;
    if live(&account.metadata)?.0 != field(namespace, "root-uid")?
        || live(&deployment.metadata)?.0 != field(namespace, "root-deployment-uid")?
        || deployment
            .spec
            .as_ref()
            .and_then(|s| s.template.spec.as_ref())
            .and_then(|s| s.service_account_name.as_deref())
            != Some(root_account.as_str())
        || field(namespace, "root-user")?
            != format!("system:serviceaccount:{root_namespace}:{root_account}")
    {
        return Err(ERROR.into());
    }
    match root_environment(&deployment, "KARS_INFERENCE_BUDGET_ENABLED")? {
        Some("true") => {
            let secret_name =
                root_environment(&deployment, "KARS_INFERENCE_BUDGET_TLS_SECRET")?.ok_or(ERROR)?;
            let accounting = budget_namespace(&deployment, &root_namespace)?;
            if field(namespace, "budget-namespace")? != accounting
                || field(namespace, "budget-tls-name")? != secret_name
            {
                return Err(
                    "Enabled budget TLS input lacks the reviewed private activation identity"
                        .into(),
                );
            }
            let accounting_ns = Api::<Namespace>::all(client.clone())
                .get(&accounting)
                .await
                .map_err(|_| ERROR)?;
            let secret =
                Api::<k8s_openapi::api::core::v1::Secret>::namespaced(client.clone(), &accounting)
                    .get_metadata(secret_name)
                    .await
                    .map_err(|_| ERROR)?;
            if live(&accounting_ns.metadata)?.0 != field(namespace, "budget-namespace-uid")?
                || live(&secret.metadata)?.0 != field(namespace, "budget-tls-uid")?
                || live(&secret.metadata)?.1 != field(namespace, "budget-tls-version")?
            {
                return Err("Reviewed budget TLS input changed".into());
            }
        }
        None | Some("") | Some("false") => {}
        _ => return Err(ERROR.into()),
    }
    let caller =
        Api::<k8s_openapi::api::authentication::v1::SelfSubjectReview>::all(client.clone())
            .create(&kube::api::PostParams::default(), &Default::default())
            .await
            .map_err(|_| ERROR)?;
    let caller = serde_json::to_value(caller).map_err(|_| ERROR)?;
    if caller["status"]["userInfo"]["username"] != field(namespace, "root-user")?
        || caller["status"]["userInfo"]["uid"] != field(namespace, "root-uid")?
    {
        return Err("Private capability issuer is not the operator-reviewed root identity".into());
    }
    match field(namespace, "profile")?.as_str() {
        "service-accounts" => {
            for name in bundle()["controllers"].as_array().ok_or(ERROR)? {
                let name = name.as_str().ok_or(ERROR)?;
                let account = Api::<ServiceAccount>::namespaced(client.clone(), "kube-system")
                    .get(name)
                    .await
                    .map_err(|_| ERROR)?;
                if live(&account.metadata)?.0 != field(namespace, &format!("{name}-uid"))? {
                    return Err(ERROR.into());
                }
            }
        }
        "kcm-certificate" => {}
        _ => return Err(ERROR.into()),
    }
    Ok(Some(epoch))
}

pub(crate) async fn verify(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    if !grant.spec.enabled || grant.spec.writers.is_empty() {
        return Ok(());
    }
    let activation = grant.spec.private_activation.as_ref().ok_or(ERROR)?;
    if activation.contract != CONTRACT
        || activation.phase != "qualified"
        || activation.bundle_revision != bundle_revision(client).await?
        || activation.namespaces.is_empty()
        || activation.namespaces.len() > 64
    {
        return Err(ERROR.into());
    }
    let expected: BTreeSet<String> = match activation.profile {
        ControllerProfile::ServiceAccounts => bundle()["controllers"]
            .as_array()
            .ok_or(ERROR)?
            .iter()
            .map(|value| value.as_str().ok_or(ERROR).map(String::from))
            .collect::<Result<_, _>>()?,
        ControllerProfile::KcmCertificate => BTreeSet::new(),
    };
    if activation
        .controller_uids
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected
    {
        return Err(ERROR.into());
    }
    if activation.root.template_digest.len() != 64
        || !activation
            .root
            .template_digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(ERROR.into());
    }
    let workspace = grant.namespace().ok_or(ERROR)?;
    let mut required = BTreeSet::from([workspace.clone(), activation.root.namespace.name.clone()]);
    if let Some(budget) = &activation.root.budget_tls {
        required.insert(budget.namespace.name.clone());
    }
    required.extend(
        grant
            .spec
            .writers
            .iter()
            .map(|writer| writer.namespace.clone()),
    );
    required.extend(
        grant
            .spec
            .observation_targets
            .iter()
            .map(|target| format!("kars-{}", target.name)),
    );
    let mut seen = BTreeSet::new();
    for scope in &activation.namespaces {
        if !seen.insert(scope.namespace.name.clone()) {
            return Err(ERROR.into());
        }
        let ns = Api::<Namespace>::all(client.clone())
            .get(&scope.namespace.name)
            .await
            .map_err(|_| ERROR)?;
        if !required.contains(&scope.namespace.name) {
            let annotations = ns.metadata.annotations.as_ref().ok_or(ERROR)?;
            if annotations
                .get("kars.azure.com/sandbox-namespace")
                .map(String::as_str)
                != Some(workspace.as_str())
            {
                return Err(ERROR.into());
            }
            let name = annotations
                .get("kars.azure.com/sandbox-name")
                .ok_or(ERROR)?;
            let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), &workspace)
                .get(name)
                .await
                .map_err(|_| ERROR)?;
            crate::reconciler::namespace_ownership::recheck(client, &sandbox, &ns)
                .await
                .map_err(|_| ERROR)?;
        }
        let epoch = namespace_epoch(client, &ns).await?.ok_or(ERROR)?;
        if live(&ns.metadata)?.0 != scope.namespace.uid
            || scope.epoch.as_deref() != Some(epoch.as_str())
            || field(&ns, "root-namespace")? != activation.root.namespace.name
            || field(&ns, "root-namespace-uid")? != activation.root.namespace.uid
            || field(&ns, "root-uid")? != activation.root.account.uid
            || field(&ns, "root-deployment-uid")? != activation.root.deployment.uid
            || field(&ns, "root-template-digest")? != activation.root.template_digest
            || (scope.namespace.name == workspace
                && scope.namespace.uid != grant.spec.workspace_uid)
            || field(&ns, "profile")?
                != match activation.profile {
                    ControllerProfile::ServiceAccounts => "service-accounts",
                    ControllerProfile::KcmCertificate => "kcm-certificate",
                }
        {
            return Err(ERROR.into());
        }
        for (name, uid) in &activation.controller_uids {
            if field(&ns, &format!("{name}-uid"))? != *uid {
                return Err(ERROR.into());
            }
        }
        if let Some(budget) = &activation.root.budget_tls {
            if field(&ns, "budget-namespace-uid")? != budget.namespace.uid
                || field(&ns, "budget-tls-uid")? != budget.secret.uid
                || field(&ns, "budget-tls-version")? != budget.secret.resource_version
                || field(&ns, "budget-key")? != budget.key_digest
            {
                return Err(ERROR.into());
            }
        }
        inspect_namespace(client, &ns, &epoch).await?;
    }
    if !required.is_subset(&seen) {
        return Err(ERROR.into());
    }
    Ok(())
}
