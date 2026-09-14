// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in metadata-only verification and network reachability for observations.
//! Network labels select traffic; they never establish credential authority.

use super::*;
use crate::{crd::KarsSandbox, service_observer::Binding};
use kube::{
    api::{DeleteParams, PostParams, Preconditions},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::Value;
use std::collections::BTreeSet;

mod api_egress;

const LABEL: &str = "kars.azure.com/observer-metadata-grant";
const NAMESPACE_UID: &str = "kars.azure.com/observer-namespace-uid";
const GENERATION: &str = "kars.azure.com/observer-grant-generation";

fn policy_prefix(grant: &KarsCredentialGrant, uid: &str) -> Result<String, String> {
    Ok(format!(
        "kars-observer-meta-{}-{}-g{}",
        grant
            .uid()
            .ok_or("Grant UID missing")?
            .chars()
            .take(12)
            .collect::<String>(),
        uid.chars().take(12).collect::<String>(),
        grant.metadata.generation.unwrap_or_default(),
    ))
}

fn same_namespace(live: &Namespace, expected: &Namespace) -> bool {
    live.uid() == expected.uid()
        && live.metadata.deletion_timestamp.is_none()
        && [
            crate::reconciler::namespace_ownership::VERSION,
            crate::reconciler::namespace_ownership::SOURCE_NAMESPACE,
            crate::reconciler::namespace_ownership::SOURCE_NAME,
            crate::reconciler::namespace_ownership::SOURCE_UID,
        ]
        .iter()
        .all(|key| {
            live.metadata
                .annotations
                .as_ref()
                .and_then(|values| values.get(*key))
                == expected
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|values| values.get(*key))
        })
}

fn resource(kind: &str) -> ApiResource {
    if kind == "CiliumNetworkPolicy" {
        return ApiResource::from_gvk(&GroupVersionKind::gvk("cilium.io", "v2", kind));
    }
    let group = if kind == "NetworkPolicy" {
        "networking.k8s.io"
    } else {
        "rbac.authorization.k8s.io"
    };
    ApiResource::from_gvk(&GroupVersionKind::gvk(group, "v1", kind))
}

async fn apply(
    client: &Client,
    grant: &KarsCredentialGrant,
    namespace: Option<&str>,
    kind: &str,
    name: &str,
    data: Value,
) -> Result<(), String> {
    apply_owned(client, grant, namespace, None, kind, name, data).await
}

async fn apply_runtime(
    client: &Client,
    grant: &KarsCredentialGrant,
    namespace: &Namespace,
    kind: &str,
    name: &str,
    data: Value,
) -> Result<(), String> {
    apply_owned(
        client,
        grant,
        Some(&namespace.name_any()),
        Some(namespace),
        kind,
        name,
        data,
    )
    .await
}

async fn apply_owned(
    client: &Client,
    grant: &KarsCredentialGrant,
    namespace: Option<&str>,
    expected_namespace: Option<&Namespace>,
    kind: &str,
    name: &str,
    data: Value,
) -> Result<(), String> {
    let resource = resource(kind);
    let api = if let Some(namespace) = namespace {
        Api::<DynamicObject>::namespaced_with(client.clone(), namespace, &resource)
    } else {
        Api::<DynamicObject>::all_with(client.clone(), &resource)
    };
    let mut definition = json!({"apiVersion":resource.api_version,"kind":kind,"metadata":{"name":name,
        "labels":{LABEL:grant.metadata.uid},"annotations":{GRANT_OWNER:grant.metadata.uid,
            "kars.azure.com/observer-grant-generation":grant.metadata.generation.unwrap_or_default().to_string()}}});
    if let Some(namespace) = namespace {
        let ns = Api::<Namespace>::all(client.clone())
            .get(namespace)
            .await
            .map_err(|e| api_error("Verify observer metadata namespace", e))?;
        identity(&ns.metadata)?;
        if expected_namespace.is_some_and(|expected| !same_namespace(&ns, expected)) {
            return Err("Observer metadata namespace was replaced".into());
        }
        definition["metadata"]["namespace"] = namespace.into();
        definition["metadata"]["annotations"]["kars.azure.com/observer-namespace-uid"] =
            json!(ns.metadata.uid);
        definition["metadata"]["ownerReferences"] = json!([{"apiVersion":"v1","kind":"Namespace",
            "name":namespace,"uid":ns.metadata.uid,"controller":true,"blockOwnerDeletion":false}]);
        if kind == "CiliumNetworkPolicy" {
            let target = ns
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(crate::reconciler::namespace_ownership::SOURCE_UID))
                .ok_or("Observer API namespace target UID missing")?;
            definition["metadata"]["annotations"]
                [crate::reconciler::namespace_ownership::SOURCE_UID] = json!(target);
        }
    }
    for (key, value) in data
        .as_object()
        .ok_or("Observer metadata definition invalid")?
    {
        definition[key] = value.clone();
    }
    let current = api
        .get_opt(name)
        .await
        .map_err(|e| api_error("Read observer metadata resource", e))?;
    if let Some(expected) = expected_namespace {
        let live = Api::<Namespace>::all(client.clone())
            .get(&expected.name_any())
            .await
            .map_err(|error| api_error("Recheck observer policy namespace before write", error))?;
        if !same_namespace(&live, expected) {
            return Err("Observer policy namespace changed before write".into());
        }
    }
    if let Some(current) = current {
        identity(&current.metadata)?;
        if current
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(GRANT_OWNER))
            != grant.metadata.uid.as_ref()
            || current.metadata.deletion_timestamp.is_some()
            || current
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/observer-namespace-uid"))
                .map(String::as_str)
                != definition["metadata"]["annotations"]["kars.azure.com/observer-namespace-uid"]
                    .as_str()
        {
            return Err("Foreign observer metadata resource preserved".into());
        }
        let policy = matches!(kind, "NetworkPolicy" | "CiliumNetworkPolicy");
        if kind == "CiliumNetworkPolicy"
            && current
                .metadata
                .annotations
                .as_ref()
                .and_then(|values| values.get(crate::reconciler::namespace_ownership::SOURCE_UID))
                .map(String::as_str)
                != definition["metadata"]["annotations"]
                    [crate::reconciler::namespace_ownership::SOURCE_UID]
                    .as_str()
        {
            return Err("Foreign observer API target policy preserved".into());
        }
        if policy
            && (current.metadata.labels.as_ref().and_then(|a| a.get(LABEL))
                != grant.metadata.uid.as_ref()
                || serde_json::to_value(&current.metadata.owner_references).ok()
                    != Some(definition["metadata"]["ownerReferences"].clone())
                || current
                    .metadata
                    .finalizers
                    .as_ref()
                    .is_some_and(|values| !values.is_empty()))
        {
            return Err("Foreign observer network policy preserved".into());
        }
        let fields_match = data
            .as_object()
            .unwrap()
            .iter()
            .all(|(key, value)| current.data.get(key) == Some(value));
        let exact_policy = !policy
            || (current.data.as_object().is_some_and(|fields| {
                fields
                    .keys()
                    .all(|key| key == "status" || data.get(key).is_some())
            }) && serde_json::to_value(&current.metadata.annotations).ok()
                == Some(definition["metadata"]["annotations"].clone())
                && serde_json::to_value(&current.metadata.labels).ok()
                    == Some(definition["metadata"]["labels"].clone()));
        if fields_match && exact_policy {
            return Ok(());
        }
        definition["metadata"]["uid"] = json!(current.metadata.uid);
        definition["metadata"]["resourceVersion"] = json!(current.metadata.resource_version);
        if policy {
            let value: DynamicObject = serde_json::from_value(definition)
                .map_err(|_| "Observer network policy serialization failed")?;
            api.replace(name, &PostParams::default(), &value)
                .await
                .map_err(|e| api_error("Replace owned observer network policy", e))?;
        } else {
            api.patch(name, &PatchParams::default(), &Patch::Merge(definition))
                .await
                .map_err(|e| api_error("Update owned observer metadata resource", e))?;
        }
    } else {
        let value: DynamicObject = serde_json::from_value(definition)
            .map_err(|_| "Observer metadata serialization failed")?;
        api.create(&PostParams::default(), &value)
            .await
            .map_err(|e| api_error("Create observer metadata resource", e))?;
    }
    Ok(())
}

pub(super) async fn ensure(
    client: &Client,
    grant: &KarsCredentialGrant,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    binding: &Binding,
) -> Result<(), String> {
    api_egress::approved(grant, sandbox, namespace)?;
    if Some(binding.grant.uid.as_str()) != grant.metadata.uid.as_deref()
        || Some(binding.grant.namespace.as_str()) != grant.metadata.namespace.as_deref()
        || binding.grant.generation != grant.metadata.generation.unwrap_or_default()
        || binding.workspace_uid != grant.spec.workspace_uid
    {
        return Err("Observer metadata binding differs from its approved grant".into());
    }
    let recipients = &binding.recipients;
    let verifier = binding
        .verifier
        .as_ref()
        .ok_or("Privacy verifier capability missing")?;
    super::observation_network::rpc_baseline(client, sandbox, namespace, verifier).await?;
    let service_host = std::env::var("KUBERNETES_SERVICE_HOST")
        .map_err(|_| "Observer API Service host is unavailable")?;
    let service_port = std::env::var("KUBERNETES_SERVICE_PORT_HTTPS")
        .or_else(|_| std::env::var("KUBERNETES_SERVICE_PORT"))
        .map_err(|_| "Observer API HTTPS port is unavailable")?;
    let api_path = api_egress::plan(client, &service_host, &service_port).await?;
    let uid = sandbox.uid().ok_or("Observer source UID missing")?;
    let prefix = policy_prefix(grant, &uid)?;
    let runtime = namespace.name_any();
    let workspace = sandbox
        .namespace()
        .ok_or("Observer source workspace missing")?;
    let subject = json!([{"kind":"ServiceAccount","name":"sandbox","namespace":runtime}]);
    let mut namespaces = BTreeSet::from([runtime.clone(), workspace.clone()]);
    namespaces.insert(verifier.namespace.clone());
    namespaces.extend(
        recipients
            .iter()
            .map(|recipient| recipient.namespace.clone()),
    );
    apply(client,grant,None,"ClusterRole",&prefix,json!({"rules":[
        {"apiGroups":[""],"resources":["namespaces"],"resourceNames":namespaces,"verbs":["get"]},
        {"apiGroups":["kars.azure.com"],"resources":["karssreregistrations"],"resourceNames":["canonical"],"verbs":["get"]},
        {"apiGroups":["authorization.k8s.io"],"resources":["subjectaccessreviews"],"verbs":["create"]},
    ]})).await?;
    apply(
        client,
        grant,
        None,
        "ClusterRoleBinding",
        &prefix,
        json!({"roleRef":{"apiGroup":"rbac.authorization.k8s.io",
        "kind":"ClusterRole","name":prefix},"subjects":subject}),
    )
    .await?;
    apply(client,grant,Some(&workspace),"Role",&prefix,json!({"rules":[
        {"apiGroups":["kars.azure.com"],"resources":["karssandboxes"],"resourceNames":[sandbox.name_any()],"verbs":["get"]},
        {"apiGroups":["kars.azure.com"],"resources":["karscredentialgrants"],"resourceNames":[NAME],"verbs":["get"]},
    ]})).await?;
    apply(
        client,
        grant,
        Some(&workspace),
        "RoleBinding",
        &prefix,
        json!({"roleRef":{"apiGroup":"rbac.authorization.k8s.io",
        "kind":"Role","name":prefix},"subjects":subject}),
    )
    .await?;
    let mut receiver_namespaces = BTreeSet::new();
    for recipient in recipients {
        receiver_namespaces.insert(recipient.namespace.clone());
    }
    for receiver_namespace in receiver_namespaces {
        let names = recipients
            .iter()
            .filter(|recipient| recipient.namespace == receiver_namespace)
            .map(|recipient| recipient.name.clone())
            .collect::<Vec<_>>();
        let role = format!("{prefix}-sa");
        apply(client,grant,Some(&receiver_namespace),"Role",&role,json!({"rules":[
            {"apiGroups":[""],"resources":["serviceaccounts"],"resourceNames":names,"verbs":["get"]},
        ]})).await?;
        apply(client,grant,Some(&receiver_namespace),"RoleBinding",&role,json!({"roleRef":{"apiGroup":"rbac.authorization.k8s.io",
            "kind":"Role","name":role},"subjects":std::iter::once(json!({"kind":"ServiceAccount","name":"sandbox","namespace":runtime}))
                .chain(recipients.iter().filter(|recipient|recipient.namespace==receiver_namespace)
                    .map(|recipient|json!({"kind":"ServiceAccount","name":recipient.name,"namespace":recipient.namespace})))
                .collect::<Vec<_>>()})).await?;
    }
    let rpc_role = format!("{prefix}-rpc");
    apply(client,grant,Some(&verifier.namespace),"Role",&rpc_role,json!({"rules":[
        {"apiGroups":[""],"resources":["configmaps"],"resourceNames":[crate::observation_privacy::DESCRIPTOR],"verbs":["get"]},
        {"apiGroups":[""],"resources":["services"],"resourceNames":[crate::observation_privacy::SERVICE],"verbs":["get"]},
        {"apiGroups":[""],"resources":["serviceaccounts"],"resourceNames":["kars-controller"],"verbs":["get"]},
    ]})).await?;
    apply(client,grant,Some(&verifier.namespace),"RoleBinding",&rpc_role,json!({
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":rpc_role},"subjects":subject
    })).await?;
    let runtime_peer = json!({"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":runtime}},
        "podSelector":{"matchLabels":{"kars.azure.com/sandbox":sandbox.name_any()}}});
    let controller_peer = json!({"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":verifier.namespace}},
        "podSelector":{"matchLabels":{"app.kubernetes.io/name":"kars","app.kubernetes.io/component":"controller",
            crate::observation_privacy::REVISION_LABEL:verifier.revision()}}});
    super::verify(client, grant).await?;
    crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| "Observer API runtime namespace changed")?;
    let mut runtime_egress = api_path.rules.clone();
    runtime_egress.push(json!({"to":[controller_peer],"ports":[{"protocol":"TCP","port":crate::observation_privacy::PORT}]}));
    apply_runtime(client,grant,namespace,"NetworkPolicy",&format!("{prefix}-rpc"),json!({"spec":{
        "podSelector":{"matchLabels":{"kars.azure.com/sandbox":sandbox.name_any()}},"policyTypes":["Ingress","Egress"],
        "egress":runtime_egress,
        "ingress":[{"from":[controller_peer],"ports":[{"protocol":"TCP","port":crate::service_observer::PORT}]}],
    }})).await?;
    api_egress::ensure(client, grant, sandbox, namespace, &prefix, &api_path).await?;
    apply(client,grant,Some(&verifier.namespace),"NetworkPolicy",&format!("{prefix}-rpc"),json!({"spec":{
        "podSelector":{"matchLabels":{"app.kubernetes.io/name":"kars","app.kubernetes.io/component":"controller"}},
        "policyTypes":["Ingress","Egress"],
        "ingress":[{"from":[runtime_peer],"ports":[{"protocol":"TCP","port":crate::observation_privacy::PORT}]}],
        "egress":[{"to":[runtime_peer],"ports":[{"protocol":"TCP","port":crate::service_observer::PORT}]}],
    }})).await?;
    apply(client,grant,Some(&runtime),"NetworkPolicy",&prefix,json!({"spec":{
        "podSelector":{"matchLabels":{"kars.azure.com/sandbox":sandbox.name_any()}},"policyTypes":["Ingress"],
        "ingress":recipients.iter().map(|recipient|json!({"from":[{"namespaceSelector":{"matchLabels":{
            "kubernetes.io/metadata.name":recipient.namespace}},"podSelector":{"matchLabels":{
                "app.kubernetes.io/name":"kars-bridge","app.kubernetes.io/component":"bff"}}}],
            "ports":[{"protocol":"TCP","port":crate::service_observer::PORT}]})).collect::<Vec<_>>(),
    }})).await
}

pub(super) async fn revoke(client: &Client, grant: &KarsCredentialGrant) -> Result<(), String> {
    retire(client, grant, false).await
}

pub(super) async fn revoke_stale(
    client: &Client,
    grant: &KarsCredentialGrant,
) -> Result<(), String> {
    retire(client, grant, true).await
}

async fn retire(
    client: &Client,
    grant: &KarsCredentialGrant,
    keep_current: bool,
) -> Result<(), String> {
    // CNP cleanup has its own namespace index: partial RBAC/KNP cleanup must not
    // hide a remaining API allowance, or prevent other revocation attempts.
    let api = api_egress::retire(client, grant, keep_current).await;
    let metadata = retire_metadata(client, grant, keep_current).await;
    api.and(metadata)
}

async fn retire_metadata(
    client: &Client,
    grant: &KarsCredentialGrant,
    keep_current: bool,
) -> Result<(), String> {
    let selector = format!("{LABEL}={}", grant.uid().ok_or("Grant UID missing")?);
    let prefixes = grant
        .spec
        .observation_targets
        .iter()
        .filter(|target| {
            target.kind == "KarsSandbox"
                && !target.uid.is_empty()
                && Some(target.namespace.as_str()) == grant.metadata.namespace.as_deref()
        })
        .map(|target| policy_prefix(grant, &target.uid))
        .collect::<Result<Vec<_>, _>>()?;
    for kind in [
        "RoleBinding",
        "Role",
        "NetworkPolicy",
        "ClusterRoleBinding",
        "ClusterRole",
    ] {
        let resource = resource(kind);
        let all: Api<DynamicObject> = Api::all_with(client.clone(), &resource);
        for object in all
            .list(&ListParams::default().labels(&selector))
            .await
            .map_err(|e| api_error("Read observer metadata for retirement", e))?
        {
            identity(&object.metadata)?;
            if object
                .metadata
                .annotations
                .as_ref()
                .and_then(|values| values.get(GRANT_OWNER))
                != grant.metadata.uid.as_ref()
                || object
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|values| values.get(LABEL))
                    != grant.metadata.uid.as_ref()
                || object.metadata.name.as_deref().is_none_or(str::is_empty)
            {
                return Err("Foreign observer metadata resource preserved".into());
            }
            if keep_current
                && grant.spec.enabled
                && prefixes.iter().any(|prefix| {
                    let name = object.name_any();
                    name == *prefix
                        || name == format!("{prefix}-sa")
                        || name == format!("{prefix}-rpc")
                })
                && object
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|a| a.get("kars.azure.com/observer-grant-generation"))
                    == Some(&grant.metadata.generation.unwrap_or_default().to_string())
            {
                continue;
            }
            let api = if let Some(namespace) = object.namespace() {
                if kind == "NetworkPolicy" {
                    let live = Api::<Namespace>::all(client.clone())
                        .get(&namespace)
                        .await
                        .map_err(|error| {
                            api_error("Verify observer policy retirement namespace", error)
                        })?;
                    identity(&live.metadata)?;
                    if object
                        .metadata
                        .annotations
                        .as_ref()
                        .and_then(|values| values.get(NAMESPACE_UID))
                        != live.metadata.uid.as_ref()
                        || serde_json::to_value(&object.metadata.owner_references).ok()
                            != Some(
                                json!([{"apiVersion":"v1","kind":"Namespace","name":namespace,
                                "uid":live.metadata.uid,"controller":true,"blockOwnerDeletion":false}]),
                            )
                    {
                        return Err("Foreign observer policy namespace preserved".into());
                    }
                }
                Api::namespaced_with(client.clone(), &namespace, &resource)
            } else {
                all.clone()
            };
            api.delete(
                &object.name_any(),
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: object.metadata.uid,
                        resource_version: object.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Retire observer metadata resource", e))?;
        }
    }
    Ok(())
}
