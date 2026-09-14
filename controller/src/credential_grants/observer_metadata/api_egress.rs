// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional installed-Cilium representation of the approved observer API path.

use super::*;
use crate::reconciler::namespace_ownership as claim;

// A durable discovery hint, not authority. It survives partial policy/RBAC
// cleanup and disappears with the namespace; ordinary runtimes never get it.
const INDEX: &str = "kars.azure.com/observer-api-policy";
const KIND: &str = "CiliumNetworkPolicy";

#[derive(Debug)]
pub(super) struct Plan {
    pub rules: Vec<Value>,
    cilium: bool,
}

pub(super) fn approved(
    grant: &KarsCredentialGrant,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), String> {
    identity(&grant.metadata)?;
    identity(&sandbox.metadata)?;
    identity(&namespace.metadata)?;
    if !grant.spec.enabled
        || !grant.spec.observation_targets.iter().any(|target| {
            target.kind == "KarsSandbox"
                && Some(target.namespace.as_str()) == grant.metadata.namespace.as_deref()
                && sandbox.namespace() == grant.namespace()
                && Some(target.name.as_str()) == sandbox.metadata.name.as_deref()
                && Some(target.uid.as_str()) == sandbox.metadata.uid.as_deref()
        })
        || !claim::claimed(namespace, sandbox)
            .map_err(|_| "Observer API namespace claim is invalid")?
    {
        return Err("Observer API path requires an approved current Sandbox and namespace".into());
    }
    Ok(())
}

async fn installed(client: &Client) -> Result<bool, String> {
    let resources = match client.list_api_group_resources("cilium.io/v2").await {
        Ok(resources) => resources,
        Err(kube::Error::Api(error)) if error.code == 404 && error.reason == "NotFound" => {
            return Ok(false);
        }
        Err(error) => return Err(api_error("Discover observer Cilium policy API", error)),
    };
    let candidates: Vec<_> = resources
        .resources
        .iter()
        .filter(|resource| resource.name == "ciliumnetworkpolicies")
        .collect();
    if resources.group_version != "cilium.io/v2"
        || candidates.len() != 1
        || candidates[0].kind != KIND
        || !candidates[0].namespaced
        || !["get", "list", "create", "update", "delete"]
            .iter()
            .all(|verb| {
                candidates[0]
                    .verbs
                    .iter()
                    .any(|value| value.as_str() == *verb)
            })
    {
        return Err("Installed Cilium policy API has an unsupported resource contract".into());
    }
    Ok(true)
}

pub(super) async fn plan(client: &Client, host: &str, port: &str) -> Result<Plan, String> {
    let rules = crate::reconciler::sre_egress::rules(client, host, port).await?;
    Ok(Plan {
        rules,
        cilium: installed(client).await?,
    })
}

fn spec(sandbox: &KarsSandbox, namespace: &Namespace, plan: &Plan) -> Result<Value, String> {
    let ports: BTreeSet<u16> = plan
        .rules
        .iter()
        .map(|rule| {
            rule["ports"][0]["port"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
                .ok_or_else(|| "Canonical observer API port is invalid".to_string())
        })
        .collect::<Result<_, _>>()?;
    if ports.is_empty() {
        return Err("Canonical observer API targets are unavailable".into());
    }
    Ok(json!({
        "endpointSelector":{"matchLabels":{
            "k8s:kars.azure.com/sandbox":sandbox.name_any(),
            "k8s:io.kubernetes.pod.namespace":namespace.name_any()
        }},
        "egress":[{"toEntities":["kube-apiserver"],"toPorts":[{
            "ports":ports.into_iter().map(|port|json!({"port":port.to_string(),"protocol":"TCP"}))
                .collect::<Vec<_>>()
        }]}]
    }))
}

pub(super) async fn ensure(
    client: &Client,
    grant: &KarsCredentialGrant,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    prefix: &str,
    plan: &Plan,
) -> Result<(), String> {
    approved(grant, sandbox, namespace)?;
    if !plan.cilium {
        return Ok(());
    }
    super::super::verify(client, grant).await?;
    let live = Api::<Namespace>::all(client.clone())
        .get(&namespace.name_any())
        .await
        .map_err(|error| api_error("Read observer API namespace index", error))?;
    approved(grant, sandbox, &live)?;
    if live.uid() != namespace.uid() {
        return Err("Observer API namespace was replaced".into());
    }
    match live
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get(INDEX))
        .map(String::as_str)
    {
        Some("v1") => {}
        None => {
            Api::<Namespace>::all(client.clone())
                .patch(
                    &live.name_any(),
                    &PatchParams::default(),
                    &Patch::Merge(json!({"metadata":{"uid":live.metadata.uid,
                    "resourceVersion":live.metadata.resource_version,"labels":{INDEX:"v1"}}})),
                )
                .await
                .map_err(|error| api_error("Index owned observer API policy namespace", error))?;
        }
        Some(_) => return Err("Foreign observer API namespace index preserved".into()),
    }
    apply_runtime(
        client,
        grant,
        namespace,
        KIND,
        &format!("{prefix}-api"),
        json!({"spec":spec(sandbox, namespace, plan)?}),
    )
    .await?;
    super::super::verify(client, grant).await?;
    claim::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| "Observer API namespace changed during policy issuance".to_string())
}

fn annotation<'a>(metadata: &'a kube::api::ObjectMeta, key: &str) -> Option<&'a str> {
    metadata.annotations.as_ref()?.get(key).map(String::as_str)
}

fn owned(
    object: &DynamicObject,
    namespace: &Namespace,
    grant: &KarsCredentialGrant,
) -> Result<(), String> {
    identity(&object.metadata)?;
    identity(&namespace.metadata)?;
    let owners = object
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default();
    if object.metadata.name.as_deref().is_none_or(str::is_empty)
        || object.namespace() != Some(namespace.name_any())
        || object
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(LABEL))
            != grant.metadata.uid.as_ref()
        || annotation(&object.metadata, GRANT_OWNER) != grant.metadata.uid.as_deref()
        || annotation(&object.metadata, NAMESPACE_UID) != namespace.metadata.uid.as_deref()
        || annotation(&object.metadata, claim::SOURCE_UID)
            != annotation(&namespace.metadata, claim::SOURCE_UID)
        || annotation(&namespace.metadata, claim::SOURCE_UID).is_none_or(str::is_empty)
        || namespace
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|values| !values.is_empty())
        || annotation(&namespace.metadata, claim::VERSION) != Some("v1")
        || annotation(&namespace.metadata, claim::SOURCE_NAMESPACE)
            != grant.metadata.namespace.as_deref()
        || namespace.name_any()
            != format!(
                "kars-{}",
                annotation(&namespace.metadata, claim::SOURCE_NAME).unwrap_or("")
            )
        || owners.len() != 1
        || owners[0].api_version != "v1"
        || owners[0].kind != "Namespace"
        || owners[0].name != namespace.name_any()
        || Some(owners[0].uid.as_str()) != namespace.metadata.uid.as_deref()
        || owners[0].controller != Some(true)
        || owners[0].block_owner_deletion != Some(false)
    {
        return Err("Foreign observer API policy or namespace preserved".into());
    }
    Ok(())
}

pub(super) async fn retire(
    client: &Client,
    grant: &KarsCredentialGrant,
    keep_current: bool,
) -> Result<(), String> {
    let grant_uid = grant
        .uid()
        .filter(|uid| !uid.is_empty())
        .ok_or("Observer grant UID missing")?;
    let workspace = grant
        .namespace()
        .filter(|name| !name.is_empty())
        .ok_or("Observer grant namespace missing")?;
    let namespaces = Api::<Namespace>::all(client.clone())
        .list(&ListParams::default().labels(&format!("{INDEX}=v1")))
        .await
        .map_err(|error| api_error("Read observer API namespace index", error))?;
    if namespaces
        .metadata
        .continue_
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return Err("Observer API namespace index is incomplete".into());
    }
    let namespaces: Vec<_> = namespaces
        .into_iter()
        .filter(|namespace| {
            annotation(&namespace.metadata, claim::SOURCE_NAMESPACE) == Some(workspace.as_str())
        })
        .collect();
    if namespaces.is_empty() || !installed(client).await? {
        return Ok(());
    }
    let resource = resource(KIND);
    for expected in namespaces {
        let namespace = Api::<Namespace>::all(client.clone())
            .get(&expected.name_any())
            .await
            .map_err(|error| api_error("Recheck observer API retirement namespace", error))?;
        identity(&namespace.metadata)?;
        if namespace.uid() != expected.uid()
            || namespace
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(INDEX))
                .map(String::as_str)
                != Some("v1")
        {
            return Err("Observer API retirement namespace changed".into());
        }
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), &namespace.name_any(), &resource);
        let objects = api
            .list(&ListParams::default().labels(&format!("{LABEL}={grant_uid}")))
            .await
            .map_err(|error| {
                api_error(
                    "Read namespaced observer API policies for retirement",
                    error,
                )
            })?;
        if objects
            .metadata
            .continue_
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            return Err("Observer API policy inventory is incomplete".into());
        }
        for object in objects {
            owned(&object, &namespace, grant)?;
            let keep = keep_current
                && grant.spec.enabled
                && annotation(&object.metadata, GENERATION)
                    == Some(
                        grant
                            .metadata
                            .generation
                            .unwrap_or_default()
                            .to_string()
                            .as_str(),
                    )
                && grant.spec.observation_targets.iter().any(|target| {
                    target.kind == "KarsSandbox"
                        && Some(target.namespace.as_str()) == grant.metadata.namespace.as_deref()
                        && Some(target.name.as_str())
                            == annotation(&namespace.metadata, claim::SOURCE_NAME)
                        && Some(target.uid.as_str())
                            == annotation(&object.metadata, claim::SOURCE_UID)
                });
            if keep {
                continue;
            }
            let live = Api::<Namespace>::all(client.clone())
                .get(&namespace.name_any())
                .await
                .map_err(|error| {
                    api_error("Recheck observer API namespace before deletion", error)
                })?;
            if !same_namespace(&live, &namespace) {
                return Err("Observer API namespace changed before deletion".into());
            }
            api.delete(
                &object.name_any(),
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: object.metadata.uid.clone(),
                        resource_version: object.metadata.resource_version.clone(),
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| api_error("Retire owned observer API policy", error))?;
            if api
                .get_opt(&object.name_any())
                .await
                .map_err(|error| api_error("Verify observer API policy retirement", error))?
                .is_some()
            {
                return Err("Observer API policy retirement is pending".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
