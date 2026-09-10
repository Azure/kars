// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Router-only broker authentication. Identity comes from a reviewed,
//! audience-bound Pod token and live UID relationships, never agent headers.

use axum::http::HeaderMap;
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet},
    authentication::v1::{TokenReview, TokenReviewSpec},
    authorization::v1::SubjectAccessReview,
    core::v1::{Namespace, Pod},
};
use kube::{Api, Client, ResourceExt, api::PostParams};
use serde_json::json;

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;

use super::{
    account::KarsBudgetAccount,
    config::{AUDIENCE, PRIVATE_MOUNT, TOKEN_VOLUME},
    store::StoreError,
};
use crate::{
    crd::KarsSandbox,
    inference_budget_contract::{
        BudgetError, ExecutionIdentity, RootKind, RouterBinding, ledger::Ledger,
    },
    kars_task::KarsTask,
};

fn api_error(stage: &'static str, error: kube::Error) -> StoreError {
    StoreError::Api {
        stage,
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

fn denied() -> StoreError {
    BudgetError::Authorization.into()
}

fn bearer(headers: &HeaderMap) -> Result<&str, StoreError> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty() && value.len() <= 16_384)
        .ok_or_else(denied)
}

fn token_extra<'a>(
    extra: &'a Option<std::collections::BTreeMap<String, Vec<String>>>,
    key: &str,
) -> Option<&'a str> {
    let values = extra.as_ref()?.get(key)?;
    if values.len() != 1 {
        return None;
    }
    values.first().map(String::as_str)
}

fn has_router_only_projection(pod: &Pod) -> bool {
    let Some(spec) = &pod.spec else { return false };
    let private = spec
        .volumes
        .iter()
        .flatten()
        .filter(|volume| {
            volume.name == TOKEN_VOLUME
                && volume.projected.as_ref().is_some_and(|projected| {
                    let sources = projected.sources.as_deref().unwrap_or_default();
                    sources.len() == 1
                        && sources[0]
                            .service_account_token
                            .as_ref()
                            .is_some_and(|token| {
                                token.audience.as_deref() == Some(AUDIENCE)
                                    && token.path == "token"
                                    && token
                                        .expiration_seconds
                                        .is_some_and(|seconds| (600..=3600).contains(&seconds))
                            })
                })
        })
        .count()
        == 1;
    let duplicate_audience = spec
        .volumes
        .iter()
        .flatten()
        .filter(|volume| volume.name != TOKEN_VOLUME)
        .any(|volume| {
            volume.projected.as_ref().is_some_and(|projected| {
                projected.sources.iter().flatten().any(|source| {
                    source
                        .service_account_token
                        .as_ref()
                        .is_some_and(|token| token.audience.as_deref() == Some(AUDIENCE))
                })
            })
        });
    let router = spec
        .containers
        .iter()
        .filter(|container| container.name == "inference-router")
        .filter(|container| {
            container.security_context.as_ref().is_some_and(|security| {
                security.run_as_user == Some(1001)
                    && security.allow_privilege_escalation == Some(false)
                    && security.read_only_root_filesystem == Some(true)
            })
        })
        .filter(|container| {
            container.volume_mounts.iter().flatten().any(|mount| {
                mount.name == TOKEN_VOLUME
                    && mount.mount_path == PRIVATE_MOUNT
                    && mount.read_only == Some(true)
            })
        })
        .count()
        == 1;
    let leaked = spec
        .containers
        .iter()
        .filter(|container| container.name != "inference-router")
        .any(|container| {
            container
                .volume_mounts
                .iter()
                .flatten()
                .any(|mount| mount.name == TOKEN_VOLUME)
        })
        || spec.init_containers.iter().flatten().any(|container| {
            container
                .volume_mounts
                .iter()
                .flatten()
                .any(|mount| mount.name == TOKEN_VOLUME)
        })
        || spec.ephemeral_containers.iter().flatten().any(|container| {
            container
                .volume_mounts
                .iter()
                .flatten()
                .any(|mount| mount.name == TOKEN_VOLUME)
        });
    private
        && router
        && !leaked
        && !duplicate_audience
        && spec.host_pid != Some(true)
        && spec.host_network != Some(true)
        && spec.host_ipc != Some(true)
        && spec.share_process_namespace != Some(true)
}

pub async fn authenticate(
    client: &Client,
    headers: &HeaderMap,
    account: &KarsBudgetAccount,
    identity: &ExecutionIdentity,
    dispatch: bool,
) -> Result<(), StoreError> {
    identity.validate()?;
    let ledger = account
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or_else(denied)?;
    if dispatch && ledger.phase != crate::inference_budget_contract::AccountPhase::Active {
        return Err(denied());
    }
    let reviews: Api<TokenReview> = Api::all(client.clone());
    let review = reviews
        .create(
            &PostParams::default(),
            &TokenReview {
                spec: TokenReviewSpec {
                    audiences: Some(vec![AUDIENCE.into()]),
                    token: Some(bearer(headers)?.into()),
                },
                ..Default::default()
            },
        )
        .await
        .map_err(|error| api_error("verify budget audience identity", error))?;
    let status = review.status.ok_or_else(denied)?;
    let user = status.user.ok_or_else(denied)?;
    let runtime_namespace = format!("kars-{}", identity.sandbox.name);
    if status.authenticated != Some(true)
        || status.error.is_some()
        || status.audiences.as_deref() != Some([AUDIENCE.to_string()].as_slice())
        || user.username.as_deref()
            != Some(format!("system:serviceaccount:{runtime_namespace}:sandbox").as_str())
        || token_extra(&user.extra, "authentication.kubernetes.io/pod-uid")
            != Some(identity.pod_uid.as_str())
        || token_extra(&user.extra, "authentication.kubernetes.io/pod-name")
            != Some(identity.pod_name.as_str())
    {
        return Err(denied());
    }

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let workspace = namespaces
        .get(&ledger.root.resource.namespace)
        .await
        .map_err(|error| api_error("verify budget workspace incarnation", error))?;
    let cluster = namespaces
        .get("kube-system")
        .await
        .map_err(|error| api_error("verify budget cluster incarnation", error))?;
    if workspace.metadata.uid.as_deref() != Some(ledger.root.workspace_uid.as_str())
        || workspace.metadata.deletion_timestamp.is_some()
        || cluster.metadata.uid.as_deref() != Some(ledger.root.cluster_uid.as_str())
    {
        return Err(denied());
    }
    let namespace = namespaces
        .get(&runtime_namespace)
        .await
        .map_err(|e| api_error("verify budget namespace", e))?;
    if namespace.metadata.uid.as_deref() != Some(identity.runtime_namespace_uid.as_str())
        || namespace
            .labels()
            .get("kars.azure.com/inference-budget")
            .map(String::as_str)
            != Some("v1")
    {
        return Err(denied());
    }
    let sandboxes: Api<KarsSandbox> = Api::namespaced(client.clone(), &identity.sandbox.namespace);
    let sandbox = sandboxes
        .get(&identity.sandbox.name)
        .await
        .map_err(|e| api_error("verify budget Sandbox", e))?;
    if sandbox.metadata.uid.as_deref() != Some(identity.sandbox.uid.as_str())
        || !crate::reconciler::namespace_ownership::claimed(&namespace, &sandbox)
            .map_err(|_| denied())?
        || (dispatch && sandbox.metadata.deletion_timestamp.is_some())
    {
        return Err(denied());
    }
    let pods: Api<Pod> = Api::namespaced(client.clone(), &runtime_namespace);
    let pod = pods
        .get(&identity.pod_name)
        .await
        .map_err(|e| api_error("verify budget Pod", e))?;
    if pod.metadata.uid.as_deref() != Some(identity.pod_uid.as_str())
        || pod
            .spec
            .as_ref()
            .and_then(|spec| spec.service_account_name.as_deref())
            != Some("sandbox")
        || (dispatch && pod.metadata.deletion_timestamp.is_some())
        || !has_router_only_projection(&pod)
    {
        return Err(denied());
    }
    verify_workload_owner(client, &pod, &sandbox, &runtime_namespace, dispatch).await?;

    // The ordinary agent credential must not be able to mint a replacement
    // budget-audience token for its shared Pod service account.
    let access: Api<SubjectAccessReview> = Api::all(client.clone());
    for (resource, subresource, name) in [
        ("serviceaccounts", "token", "sandbox"),
        ("pods", "exec", identity.pod_name.as_str()),
        ("pods", "attach", identity.pod_name.as_str()),
    ] {
        let request: SubjectAccessReview = serde_json::from_value(json!({
            "apiVersion": "authorization.k8s.io/v1", "kind": "SubjectAccessReview",
            "spec": {
                "user": user.username, "groups": user.groups,
                "resourceAttributes": {"namespace": runtime_namespace, "verb": "create",
                    "group": "", "resource": resource, "subresource": subresource, "name": name}
            }
        }))
        .map_err(|_| denied())?;
        let result = access
            .create(&PostParams::default(), &request)
            .await
            .map_err(|e| api_error("verify agent cannot access budget token", e))?;
        let status = result.status.ok_or_else(denied)?;
        if status.allowed
            || status
                .evaluation_error
                .as_ref()
                .is_some_and(|error| !error.is_empty())
        {
            return Err(denied());
        }
    }

    // This is the combined-privacy prerequisite, not an agent-admin-token
    // fallback. It performs real shared Secret GET/LIST/WATCH denial checks and
    // validates the current v2 registration when present.
    let epoch = crate::sre_authority::privacy_epoch(client, &runtime_namespace)
        .await
        .map_err(|_| denied())?;
    let encoded = pod
        .spec
        .as_ref()
        .and_then(|spec| {
            spec.containers
                .iter()
                .find(|container| container.name == "inference-router")
        })
        .and_then(|container| container.env.as_ref())
        .and_then(|env| {
            env.iter()
                .find(|value| value.name == "KARS_INFERENCE_BUDGET_BINDING")
        })
        .and_then(|value| value.value.as_deref())
        .ok_or_else(denied)?;
    let binding: RouterBinding = serde_json::from_str(encoded).map_err(|_| denied())?;
    if binding.privacy_epoch != epoch
        || binding.sandbox != identity.sandbox
        || binding.runtime_namespace != runtime_namespace
        || binding.runtime_namespace_uid != identity.runtime_namespace_uid
        || binding.task.task_uid != identity.task_uid
        || binding.task.authorization_digest != identity.authorization_digest
        || binding.task.account.uid != ledger.account_uid
        || binding.task.root != ledger.root
    {
        return Err(denied());
    }
    verify_task_binding(client, account, ledger, identity, &sandbox, dispatch).await
}

async fn verify_workload_owner(
    client: &Client,
    pod: &Pod,
    sandbox: &KarsSandbox,
    namespace: &str,
    dispatch: bool,
) -> Result<(), StoreError> {
    let refs = pod.metadata.owner_references.as_deref().unwrap_or_default();
    let replica = refs
        .iter()
        .find(|owner| {
            owner.controller == Some(true)
                && owner.kind == "ReplicaSet"
                && owner.api_version == "apps/v1"
        })
        .ok_or_else(denied)?;
    let replicas: Api<ReplicaSet> = Api::namespaced(client.clone(), namespace);
    let replica_set = replicas
        .get(&replica.name)
        .await
        .map_err(|e| api_error("verify budget ReplicaSet", e))?;
    if replica_set.metadata.uid.as_deref() != Some(replica.uid.as_str()) {
        return Err(denied());
    }
    let refs = replica_set
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default();
    let owner = refs
        .iter()
        .find(|owner| {
            owner.controller == Some(true)
                && owner.kind == "Deployment"
                && owner.api_version == "apps/v1"
                && owner.name == sandbox.name_any()
        })
        .ok_or_else(denied)?;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let deployment = deployments
        .get(&owner.name)
        .await
        .map_err(|e| api_error("verify budget Deployment", e))?;
    if deployment.metadata.uid.as_deref() != Some(owner.uid.as_str())
        || deployment.labels().get("kars.azure.com/sandbox") != sandbox.metadata.name.as_ref()
        || deployment.labels().get("kars.azure.com/parent-namespace")
            != sandbox.metadata.namespace.as_ref()
    {
        return Err(denied());
    }
    let pod_router = pod
        .spec
        .as_ref()
        .and_then(|spec| {
            spec.containers
                .iter()
                .find(|container| container.name == "inference-router")
        })
        .ok_or_else(denied)?;
    let template = if dispatch {
        deployment
            .spec
            .as_ref()
            .and_then(|spec| spec.template.spec.as_ref())
    } else {
        replica_set
            .spec
            .as_ref()
            .and_then(|spec| spec.template.as_ref())
            .and_then(|template| template.spec.as_ref())
    };
    let template_router = template
        .and_then(|spec| {
            spec.containers
                .iter()
                .find(|container| container.name == "inference-router")
        })
        .ok_or_else(denied)?;
    if pod_router.image != template_router.image
        || pod_router.command != template_router.command
        || pod_router.args != template_router.args
        || !router_env_matches(pod_router, template_router)
        || pod_router.env_from != template_router.env_from
        || pod_router.security_context != template_router.security_context
        || !router_mounts_match(pod_router, template_router)
    {
        return Err(denied());
    }
    if dispatch {
        let settings = super::config::Settings::from_env()?.ok_or_else(denied)?;
        if pod_router
            .image
            .as_ref()
            .is_none_or(|image| !image.ends_with(&format!("@{}", settings.router_image_digest)))
        {
            return Err(denied());
        }
    }
    Ok(())
}

fn router_env_matches(
    actual: &k8s_openapi::api::core::v1::Container,
    template: &k8s_openapi::api::core::v1::Container,
) -> bool {
    let actual = actual.env.as_deref().unwrap_or_default();
    let template = template.env.as_deref().unwrap_or_default();
    let mut names = std::collections::BTreeSet::new();
    if actual.iter().any(|entry| !names.insert(&entry.name))
        || template.iter().any(|entry| !actual.contains(entry))
    {
        return false;
    }
    actual
        .iter()
        .filter(|entry| !template.contains(entry))
        .all(|entry| {
            entry.value_from.is_none()
                && entry
                    .value
                    .as_ref()
                    .is_some_and(|value| match entry.name.as_str() {
                        "AZURE_CLIENT_ID" | "AZURE_TENANT_ID" | "AZURE_AUTHORITY_HOST" => {
                            !value.is_empty()
                        }
                        "AZURE_FEDERATED_TOKEN_FILE" => {
                            value == "/var/run/secrets/azure/tokens/azure-identity-token"
                        }
                        _ => false,
                    })
        })
}

fn router_mounts_match(
    actual: &k8s_openapi::api::core::v1::Container,
    template: &k8s_openapi::api::core::v1::Container,
) -> bool {
    let actual = actual.volume_mounts.as_deref().unwrap_or_default();
    let template = template.volume_mounts.as_deref().unwrap_or_default();
    template.iter().all(|mount| actual.contains(mount))
        && actual
            .iter()
            .filter(|mount| !template.contains(mount))
            .all(|mount| {
                mount.read_only == Some(true)
                    && mount.sub_path.is_none()
                    && mount.sub_path_expr.is_none()
                    && mount.mount_propagation.is_none()
                    && ((mount.name.starts_with("kube-api-access-")
                        && mount.mount_path == "/var/run/secrets/kubernetes.io/serviceaccount")
                        || (mount.name == "azure-identity-token"
                            && mount.mount_path == "/var/run/secrets/azure/tokens"))
            })
}

async fn verify_task_binding(
    client: &Client,
    account: &KarsBudgetAccount,
    ledger: &Ledger,
    identity: &ExecutionIdentity,
    sandbox: &KarsSandbox,
    dispatch: bool,
) -> Result<(), StoreError> {
    let node = ledger.nodes.get(&identity.task_uid).ok_or_else(denied)?;
    let refs = sandbox
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default();
    if !refs.iter().any(|owner| {
        owner.controller == Some(true)
            && owner.kind == "KarsTask"
            && owner.api_version == "kars.azure.com/v1alpha1"
            && owner.uid == identity.task_uid
            && owner.name == node.authority.task.name
    }) {
        return Err(denied());
    }
    if !dispatch {
        let session = ledger.sessions.get(&identity.pod_uid).ok_or_else(denied)?;
        return if session.identity == *identity {
            Ok(())
        } else {
            Err(denied())
        };
    }
    let tasks: Api<KarsTask> = Api::namespaced(client.clone(), &ledger.root.resource.namespace);
    let leaf = tasks
        .get(&node.authority.task.name)
        .await
        .map_err(|error| api_error("verify current task authority", error))?;
    let lineage = crate::task_identity::resolve(
        client,
        &leaf,
        crate::task_identity::LeafReadiness::RequireReady,
    )
    .await?;
    if lineage.workspace_uid != ledger.root.workspace_uid
        || lineage.nodes.len() != ledger.ancestors(&identity.task_uid)?.len()
    {
        return Err(denied());
    }
    let mut pins = Vec::new();
    for current in &lineage.nodes {
        let uid = &current.pin.task.uid;
        let node = ledger.nodes.get(uid).ok_or_else(denied)?;
        let task = &current.task;
        let status = task.status.as_ref().ok_or_else(denied)?;
        if task.metadata.uid.as_deref() != Some(uid.as_str())
            || current.authorization_digest != node.authority.authorization_digest
            || (uid == &identity.task_uid
                && !task
                    .spec
                    .execution
                    .as_ref()
                    .is_some_and(|execution| execution.launch))
        {
            return Err(denied());
        }
        let binding = status.inference_budget.as_ref().ok_or_else(denied)?;
        if binding.account.uid != ledger.account_uid
            || binding.account.name != account.name_any()
            || Some(binding.account.namespace.as_str()) != account.metadata.namespace.as_deref()
            || binding.root != ledger.root
            || binding.task_uid != *uid
            || binding.parent_task_uid != node.authority.parent_uid
            || binding.root_task_uid != node.authority.root_task_uid
            || binding.authorization_digest != node.authority.authorization_digest
        {
            return Err(denied());
        }
        pins.push(crate::task_identity::TaskLineagePin {
            task: crate::task_identity::ObjectUidRef {
                namespace: node.authority.task.namespace.clone(),
                name: node.authority.task.name.clone(),
                uid: node.authority.task.uid.clone(),
            },
            parent_task_uid: node.authority.parent_uid.clone(),
            root_task_uid: node.authority.root_task_uid.clone(),
        });
    }
    lineage.verify_pins(&pins)?;
    if ledger.root.kind == RootKind::KarsTeam {
        let team = lineage.team.as_ref().ok_or_else(denied)?;
        if team.metadata.uid.as_deref() != Some(ledger.root.resource.uid.as_str())
            || team.name_any() != ledger.root.resource.name
            || team.metadata.deletion_timestamp.is_some()
            || team.spec.paused
            || super::binding::limits(&team.spec.envelope)? != ledger.limits
            || team
                .status
                .as_ref()
                .and_then(|status| status.inference_budget_account.as_ref())
                .is_none_or(|reference| {
                    reference.uid != ledger.account_uid
                        || reference.name != account.name_any()
                        || Some(reference.namespace.as_str())
                            != account.metadata.namespace.as_deref()
                })
        {
            return Err(denied());
        }
    } else if lineage.team.is_some()
        || lineage.nodes.first().is_none_or(|node| {
            node.pin.task.uid != ledger.root.resource.uid
                || node.pin.task.name != ledger.root.resource.name
        })
    {
        return Err(denied());
    }
    Ok(())
}
