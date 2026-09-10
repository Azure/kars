// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Live qualification of the generic private capability, not core bootstrap.

use crate::{
    crd::KarsSandbox,
    credential_grant::{KarsCredentialGrant, activation::ControllerProfile},
};
use k8s_openapi::api::{
    admissionregistration::v1::{ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding},
    apps::v1::Deployment,
    core::v1::{Namespace, Pod, ServiceAccount},
};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const PREFIX: &str = "kars.azure.com/private-";
pub(crate) const EPOCH: &str = "kars.azure.com/private-epoch";
pub(crate) const CONTRACT: &str = "kars.azure.com/private-consumption/v1";
const ERROR: &str =
    "Private capability is unqualified; regenerate and apply the reviewed grant activation";

pub(crate) fn bundle() -> Value {
    serde_json::from_str(include_str!(
        "../../deploy/helm/kars/files/private-consumption.json"
    ))
    .expect("embedded private admission bundle is valid JSON")
}

fn live(meta: &kube::api::ObjectMeta) -> Result<(&str, &str), String> {
    crate::credential_grants::identity(meta)
}

fn field(namespace: &Namespace, key: &str) -> Result<String, String> {
    namespace
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(&format!("{PREFIX}{key}")))
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| ERROR.into())
}

fn hash(value: &Value) -> String {
    fn ordered(value: &Value) -> Value {
        match value {
            Value::Object(fields) => serde_json::to_value(
                fields
                    .iter()
                    .map(|(key, value)| (key, ordered(value)))
                    .collect::<BTreeMap<_, _>>(),
            )
            .expect("JSON object serializes"),
            Value::Array(values) => Value::Array(values.iter().map(ordered).collect()),
            _ => value.clone(),
        }
    }
    crate::providers::signing::sha256_hex(
        &serde_json::to_vec(&ordered(value)).expect("JSON serializes"),
    )
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
            inspect_namespace(client, &ns, &epoch).await?;
        }
    }
    if !required.is_subset(&seen) {
        return Err(ERROR.into());
    }
    Ok(())
}

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

pub(crate) fn private_material(pod: &Pod) -> bool {
    let value = serde_json::to_value(pod).expect("Pod serializes");
    let spec = &value["spec"];
    let definition = bundle();
    let protected = |value: &Value| {
        definition["secrets"]
            .as_array()
            .is_some_and(|names| value.is_string() && names.contains(value))
    };
    if spec["volumes"].as_array().is_some_and(|volumes| {
        volumes.iter().any(|volume| {
            protected(&volume["secret"]["secretName"])
                || protected(&volume["csi"]["nodePublishSecretRef"]["name"])
                || volume["projected"]["sources"]
                    .as_array()
                    .is_some_and(|sources| {
                        sources
                            .iter()
                            .any(|source| protected(&source["secret"]["name"]))
                    })
                || [
                    "azureFile",
                    "cephfs",
                    "cinder",
                    "flexVolume",
                    "iscsi",
                    "rbd",
                    "scaleIO",
                    "storageos",
                ]
                .iter()
                .any(|kind| {
                    protected(&volume[*kind]["secretName"])
                        || protected(&volume[*kind]["secretRef"]["name"])
                })
        })
    }) || spec["imagePullSecrets"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| protected(&value["name"])))
    {
        return true;
    }
    ["containers", "initContainers", "ephemeralContainers"]
        .iter()
        .any(|kind| {
            spec[*kind].as_array().is_some_and(|containers| {
                containers.iter().any(|container| {
                    container["envFrom"].as_array().is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| protected(&value["secretRef"]["name"]))
                    }) || container["env"].as_array().is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| protected(&value["valueFrom"]["secretKeyRef"]["name"]))
                    })
                })
            })
        })
}

pub(crate) async fn retired_material_consumers(
    client: &Client,
    namespace: &str,
) -> Result<bool, String> {
    let pods = Api::<Pod>::namespaced(client.clone(), namespace)
        .list(&ListParams::default())
        .await
        .map_err(|_| ERROR)?;
    if pods
        .metadata
        .continue_
        .as_ref()
        .is_some_and(|v| !v.is_empty())
        || pods.items.iter().any(|pod| {
            pod.spec.is_none()
                || pod.metadata.uid.as_deref().is_none_or(str::is_empty)
                || pod
                    .metadata
                    .resource_version
                    .as_deref()
                    .is_none_or(str::is_empty)
        })
    {
        return Err(ERROR.into());
    }

    pub(crate) async fn inspect_namespace(
        client: &Client,
        namespace: &Namespace,
        epoch: &str,
    ) -> Result<(), String> {
        let pods = Api::<Pod>::namespaced(client.clone(), &namespace.name_any())
            .list(&ListParams::default())
            .await
            .map_err(|_| ERROR)?;
        if pods
            .metadata
            .continue_
            .as_ref()
            .is_some_and(|v| !v.is_empty())
        {
            return Err(ERROR.into());
        }
        let annotations = namespace.metadata.annotations.as_ref().ok_or(ERROR)?;
        for pod in pods {
            let uid = pod
                .metadata
                .uid
                .as_deref()
                .filter(|v| !v.is_empty())
                .ok_or(ERROR)?;
            let spec = pod.spec.as_ref().ok_or(ERROR)?;
            let raw = serde_json::to_value(spec).map_err(|_| ERROR)?;
            let sa = spec.service_account_name.as_deref().unwrap_or("default");
            let private_identity = (namespace.name_any() == field(namespace, "root-namespace")?
                && sa == field(namespace, "root-account")?)
                || (namespace.name_any() == "kars-sre" && sa == "sre-api-router")
                || (namespace.name_any() == "kube-system"
                    && bundle()["controllers"]
                        .as_array()
                        .is_some_and(|names| names.contains(&json!(sa))));
            let projected_token = raw["volumes"].as_array().is_some_and(|volumes| {
                volumes.iter().any(|volume| {
                    volume["projected"]["sources"]
                        .as_array()
                        .is_some_and(|sources| {
                            sources
                                .iter()
                                .any(|source| source.get("serviceAccountToken").is_some())
                        })
                })
            });
            let dangerous = ["hostPID", "hostIPC", "hostNetwork"]
                .iter()
                .any(|key| raw[*key] == true)
                || raw["volumes"].as_array().is_some_and(|volumes| {
                    volumes
                        .iter()
                        .any(|volume| volume.get("hostPath").is_some())
                })
                || ["containers", "initContainers", "ephemeralContainers"]
                    .iter()
                    .any(|key| {
                        raw[*key].as_array().is_some_and(|containers| {
                            containers.iter().any(|container| {
                                container["securityContext"]["privileged"] == true
                                    || container["securityContext"]["capabilities"]["add"]
                                        .as_array()
                                        .is_some_and(|caps| {
                                            caps.iter().any(|cap| {
                                                [
                                                    "ALL",
                                                    "SYS_ADMIN",
                                                    "SYS_PTRACE",
                                                    "SYS_MODULE",
                                                    "SYS_RAWIO",
                                                    "BPF",
                                                    "PERFMON",
                                                    "CHECKPOINT_RESTORE",
                                                    "DAC_READ_SEARCH",
                                                ]
                                                .iter()
                                                .any(|name| cap.as_str() == Some(*name))
                                            })
                                        })
                            })
                        })
                    });
            let material = private_material(&pod);
            let marked = pod.metadata.annotations.as_ref().and_then(|a| a.get(EPOCH));
            if !material
                && !dangerous
                && !(private_identity
                    && (spec.automount_service_account_token != Some(false) || projected_token))
                && marked.is_none()
            {
                continue;
            }
            // Current-epoch consumers were admitted under this exact enforcing
            // bundle. The policy requires authenticated actor authority as well.
            if marked.map(String::as_str) == Some(epoch) {
                use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
                let owners: Vec<_> = pod
                    .metadata
                    .owner_references
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .filter(|owner| owner.controller == Some(true))
                    .collect();
                if owners.len() == 1 {
                    let owner = owners[0];
                    let group = match (owner.api_version.as_str(), owner.kind.as_str()) {
                        ("apps/v1", "ReplicaSet" | "Deployment" | "StatefulSet" | "DaemonSet") => {
                            "apps"
                        }
                        ("batch/v1", "Job" | "CronJob") => "batch",
                        ("v1", "ReplicationController") => "",
                        _ => return Err(ERROR.into()),
                    };
                    let resource =
                        ApiResource::from_gvk(&GroupVersionKind::gvk(group, "v1", &owner.kind));
                    let parent = Api::<DynamicObject>::namespaced_with(
                        client.clone(),
                        &namespace.name_any(),
                        &resource,
                    )
                    .get(&owner.name)
                    .await
                    .map_err(|_| ERROR)?;
                    if live(&parent.metadata)?.0 != owner.uid {
                        return Err(ERROR.into());
                    }
                    let template = if owner.kind == "CronJob" {
                        &parent.data["spec"]["jobTemplate"]["spec"]["template"]
                    } else {
                        &parent.data["spec"]["template"]
                    };
                    if template["metadata"]["annotations"][EPOCH] == epoch
                        || annotations
                            .get(&format!("{PREFIX}parent-{}", owner.uid))
                            .map(String::as_str)
                            == Some(epoch)
                    {
                        continue;
                    }
                }
            }
            if material
                || annotations
                    .get(&format!("{PREFIX}pod-{uid}"))
                    .map(String::as_str)
                    != Some(epoch)
                || annotations.get(&format!("{PREFIX}pod-spec-{uid}")) != Some(&hash(&raw))
            {
                return Err("Unexplained or prior-epoch private consumer preserved; operator qualification is required".into());
            }
        }
        Ok(())
    }
    Ok(!pods.items.iter().any(private_material))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn pod_spec_digest(value: &Value) -> String {
        hash(value)
    }

    pub(crate) fn install(
        objects: &mut BTreeMap<String, Value>,
        root: &str,
        root_uid: &str,
        account_uid: &str,
        scopes: &[(&str, &str)],
    ) -> Value {
        let mut ids = Vec::new();
        for (index, definition) in bundle()["objects"].as_array().unwrap().iter().enumerate() {
            let mut value = definition.clone();
            let kind = value["kind"].as_str().unwrap().to_string();
            let name = value["metadata"]["name"].as_str().unwrap().to_string();
            let uid = format!("private-admission-{index}");
            value["metadata"]["uid"] = uid.clone().into();
            value["metadata"]["resourceVersion"] = "1".into();
            value["metadata"]["generation"] = 1.into();
            if kind == "ValidatingAdmissionPolicy" {
                value["status"] = json!({"observedGeneration":1,"typeChecking":{}});
            }
            ids.push(json!({"kind":kind,"name":name,"uid":uid,"resourceVersion":"1"}));
            let plural = if kind == "ValidatingAdmissionPolicy" {
                "validatingadmissionpolicies"
            } else {
                "validatingadmissionpolicybindings"
            };
            objects.insert(
                format!("/apis/admissionregistration.k8s.io/v1/{plural}/{name}"),
                value,
            );
        }
        let revision = hash(&json!(ids));
        let epoch = "a".repeat(64);
        let mut scope_list = BTreeMap::from([(root, root_uid)]);
        scope_list.extend(scopes.iter().copied());
        let mut namespaces = Vec::new();
        for (name, uid) in scope_list {
            let namespace = objects.entry(format!("/api/v1/namespaces/{name}")).or_insert_with(|| {
                json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":name,"uid":uid,"resourceVersion":"1"},
                    "spec":{"finalizers":["kubernetes"]}})
            });
            for (key, value) in [
                ("enabled", "true"),
                ("state", "Qualified"),
                ("epoch", epoch.as_str()),
                ("namespace-uid", uid),
                ("root-namespace", root),
                ("root-namespace-uid", root_uid),
                ("root-account", "kars-controller"),
                ("root-uid", account_uid),
                ("root-deployment", "kars-controller"),
                ("root-deployment-uid", "controller-deploy"),
                ("bundle-revision", revision.as_str()),
                ("profile", "kcm-certificate"),
            ] {
                namespace["metadata"]["annotations"][format!("{PREFIX}{key}")] = value.into();
            }
            namespace["metadata"]["annotations"][format!("{PREFIX}root-user")] =
                format!("system:serviceaccount:{root}:kars-controller").into();
            namespace["metadata"]["annotations"][format!("{PREFIX}root-template-digest")] =
                "b".repeat(64).into();
            namespaces.push(
                json!({"namespace":{"name":name,"uid":uid,"resourceVersion":"1"},
                "consumers":[],"epoch":epoch}),
            );
        }
        objects.insert(format!("/api/v1/namespaces/{root}/serviceaccounts/kars-controller"), json!({
            "apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":"kars-controller","namespace":root,
                "uid":account_uid,"resourceVersion":"1"}
        }));
        objects.insert(format!("/apis/apps/v1/namespaces/{root}/deployments/kars-controller"), json!({
            "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"kars-controller","namespace":root,
                "uid":"controller-deploy","resourceVersion":"1"},
            "spec":{"template":{"metadata":{},"spec":{"serviceAccountName":"kars-controller",
                "containers":[{"name":"controller","image":"fixture"}]}}}
        }));
        json!({"contract":CONTRACT,"phase":"qualified","bundleRevision":revision,
            "root":{"namespace":{"name":root,"uid":root_uid,"resourceVersion":"1"},
                "account":{"name":"kars-controller","uid":account_uid,"resourceVersion":"1"},
                "deployment":{"name":"kars-controller","uid":"controller-deploy","resourceVersion":"1"},
                "templateDigest":"b".repeat(64)},
            "profile":"kcm-certificate","controllerUids":{},"namespaces":namespaces})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn private_activation_checks_exact_policy_binding_epoch_and_root_incarnations() {
        let server = MockServer::start().await;
        let mut objects = BTreeMap::new();
        let activation = test_support::install(
            &mut objects,
            "core",
            "core-uid",
            "controller",
            &[("work", "work-uid"), ("bridge", "bridge-uid")],
        );
        let grant: KarsCredentialGrant = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"work","uid":"grant","resourceVersion":"1"},
            "spec":{"workspaceUid":"work-uid","writers":[{"namespace":"bridge","name":"bff","uid":"writer"}],
                "privateActivation":activation}
        })).unwrap();
        let baseline = objects.clone();
        let objects = Arc::new(Mutex::new(objects));
        let captured = objects.clone();
        Mock::given(|_: &wiremock::Request| true)
            .respond_with(move |r: &wiremock::Request| {
                if r.method == "POST" && r.url.path().ends_with("/selfsubjectreviews") {
                    return ResponseTemplate::new(201).set_body_json(json!({
                        "apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                        "status":{"userInfo":{"username":"system:serviceaccount:core:kars-controller","uid":"controller"}}
                    }));
                }
                assert_eq!(r.method, "GET");
                if r.url.path().ends_with("/pods") {
                    return ResponseTemplate::new(200).set_body_json(json!({
                        "apiVersion":"v1","kind":"PodList","metadata":{},"items":[]
                    }));
                }
                captured.lock().unwrap().get(r.url.path()).map_or_else(
                    || ResponseTemplate::new(404),
                    |value| ResponseTemplate::new(200).set_body_json(value),
                )
            })
            .mount(&server)
            .await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        verify(&client, &grant).await.unwrap();
        for (path, pointer, value) in [
            (
                "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/kars-private-consumption",
                "/spec/failurePolicy",
                json!("Ignore"),
            ),
            (
                "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/kars-private-consumption",
                "/spec/validations/0/expression",
                json!("true"),
            ),
            (
                "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies/kars-private-consumption",
                "/status/observedGeneration",
                json!(0),
            ),
            (
                "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/kars-private-consumption",
                "/spec/validationActions",
                json!(["Audit"]),
            ),
            (
                "/api/v1/namespaces/work",
                "/metadata/uid",
                json!("replacement"),
            ),
            (
                "/api/v1/namespaces/core/serviceaccounts/kars-controller",
                "/metadata/uid",
                json!("replacement"),
            ),
            (
                "/apis/apps/v1/namespaces/core/deployments/kars-controller",
                "/metadata/uid",
                json!("replacement"),
            ),
        ] {
            *objects.lock().unwrap() = baseline.clone();
            *objects
                .lock()
                .unwrap()
                .get_mut(path)
                .unwrap()
                .pointer_mut(pointer)
                .unwrap() = value;
            assert!(verify(&client, &grant).await.is_err(), "{path} {pointer}");
        }
        *objects.lock().unwrap() = baseline.clone();
        objects
            .lock()
            .unwrap()
            .get_mut("/api/v1/namespaces/work")
            .unwrap()["metadata"]["annotations"][EPOCH] = "unqualified".into();
        assert!(verify(&client, &grant).await.is_err());
        *objects.lock().unwrap() = baseline;
        objects.lock().unwrap().remove("/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings/kars-private-consumption");
        assert!(verify(&client, &grant).await.is_err());
        let mut retired = grant.clone();
        retired.spec.writers.clear();
        verify(&client, &retired).await.unwrap();
    }

    #[test]
    fn private_activation_material_inventory_includes_unlabelled_and_terminating_consumers_not_legacy_agent_tokens()
     {
        for container in ["containers", "initContainers", "ephemeralContainers"] {
            let mut pod = json!({"metadata":{"deletionTimestamp":"2026-01-01T00:00:00Z"},
                "spec":{"containers":[{"name":"agent","image":"fixture"}]}});
            pod["spec"][container] = json!([{"name":"reader","image":"fixture",
                "envFrom":[{"secretRef":{"name":"router-services-observer-identity"}}]}]);
            let pod: Pod = serde_json::from_value(pod).unwrap();
            assert!(private_material(&pod));
        }
        for secret in bundle()["secrets"].as_array().unwrap() {
            let pod: Pod = serde_json::from_value(json!({"metadata":{},"spec":{
                "containers":[{"name":"agent","image":"fixture"}],
                "volumes":[{"name":"private","projected":{"sources":[{"secret":{"name":secret}}]}}]
            }}))
            .unwrap();
            assert!(private_material(&pod));
        }
        let legacy: Pod = serde_json::from_value(json!({"metadata":{},"spec":{
            "containers":[{"name":"agent","image":"fixture"}],
            "volumes":[{"name":"agent","secret":{"secretName":"router-admin-token"}}]
        }}))
        .unwrap();
        assert!(!private_material(&legacy));
    }

    #[test]
    fn private_activation_rsa_rotation_compares_keys_not_pem_encoding() {
        use rsa::{RsaPrivateKey, pkcs1::EncodeRsaPrivateKey, pkcs8::EncodePrivateKey};
        let first = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
        let second = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
        let one = first.to_pkcs1_pem(Default::default()).unwrap();
        let same = first.to_pkcs8_pem(Default::default()).unwrap();
        let other = second.to_pkcs8_pem(Default::default()).unwrap();
        assert!(!different_rsa_keys(&one, &same).unwrap());
        assert!(different_rsa_keys(&one, &other).unwrap());
    }

    #[tokio::test]
    async fn private_activation_absence_does_not_require_a_bundle_for_ordinary_namespaces() {
        let server = MockServer::start().await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let namespace: Namespace = serde_json::from_value(json!({
            "metadata":{"name":"ordinary","uid":"ordinary-uid","resourceVersion":"1"}
        }))
        .unwrap();
        assert!(
            namespace_epoch(&client, &namespace)
                .await
                .unwrap()
                .is_none()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
