// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared, read-only Task UID ancestry. This captures live identity, not a
//! durable ancestry registry; consumers must verify and persist their own pins.

use crate::{kars_task::KarsTask, kars_team::KarsTeam};
use k8s_openapi::{api::core::v1::Namespace, apimachinery::pkg::apis::meta::v1::OwnerReference};
use kube::{Api, Client, ResourceExt};
use std::collections::BTreeSet;

const MAX_CHAIN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafReadiness {
    RequireReady,
    /// Identity/bootstrap only. This never declares the leaf governance-Ready.
    AllowPending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectUidRef {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskLineagePin {
    pub task: ObjectUidRef,
    pub parent_task_uid: Option<String>,
    pub root_task_uid: String,
}

#[derive(Clone, Debug)]
pub struct VerifiedTaskNode {
    pub task: KarsTask,
    pub pin: TaskLineagePin,
    pub generation: i64,
    pub resource_version: String,
    pub authorization_digest: String,
    pub effective_authorization: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct VerifiedTaskLineage {
    pub workspace_uid: String,
    /// Root first, requested leaf last.
    pub nodes: Vec<VerifiedTaskNode>,
    pub team: Option<KarsTeam>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Task UID lineage identity is absent, replaced, terminating or inconsistent")]
    Identity,
    #[error("Task UID lineage changed while being read; retry from current authority")]
    Changed,
    #[error("Task UID lineage is not governance-Ready")]
    NotReady,
    #[error("Task UID lineage amplifies its parent's effective authority")]
    Attenuation,
    #[error("Task UID lineage is cyclic or exceeds 64 nodes")]
    Depth,
    #[error("Task UID lineage API {stage} failed (status {code:?})")]
    Api {
        stage: &'static str,
        code: Option<u16>,
    },
}

fn api(stage: &'static str, error: kube::Error) -> Error {
    Error::Api {
        stage,
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
        })
}

fn identity(task: &KarsTask, namespace: &str) -> Result<ObjectUidRef, Error> {
    let uid = task
        .uid()
        .filter(|uid| !uid.is_empty())
        .ok_or(Error::Identity)?;
    if task.metadata.namespace.as_deref() != Some(namespace)
        || !name(&task.name_any())
        || task.metadata.deletion_timestamp.is_some()
        || task
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || task
            .metadata
            .generation
            .is_none_or(|generation| generation <= 0)
    {
        return Err(Error::Identity);
    }
    Ok(ObjectUidRef {
        namespace: namespace.into(),
        name: task.name_any(),
        uid,
    })
}

fn owner(task: &KarsTask) -> Result<Option<&OwnerReference>, Error> {
    let mut owners = task
        .owner_references()
        .iter()
        .filter(|owner| owner.controller == Some(true));
    let first = owners.next();
    if owners.next().is_some() {
        return Err(Error::Identity);
    }
    Ok(first)
}

impl VerifiedTaskLineage {
    /// Empty input is only a live snapshot check, NOT immutable continuity.
    /// Supplied pins must come from the consumer's authoritative persistence.
    pub fn verify_pins(&self, pins: &[TaskLineagePin]) -> Result<(), Error> {
        let mut seen = BTreeSet::new();
        for pin in pins {
            if !seen.insert(&pin.task.uid)
                || self
                    .nodes
                    .iter()
                    .find(|node| node.pin.task.uid == pin.task.uid)
                    .is_none_or(|node| node.pin != *pin)
            {
                return Err(Error::Identity);
            }
        }
        Ok(())
    }
}

/// Capture one stable same-workspace chain, using the canonical readiness,
/// attenuation and full-authorization helpers. A second UID/RV inventory
/// rejects changes across the collected snapshot rather than mixing epochs.
pub async fn resolve(
    client: &Client,
    leaf: &KarsTask,
    readiness: LeafReadiness,
) -> Result<VerifiedTaskLineage, Error> {
    let namespace = leaf
        .namespace()
        .filter(|namespace| name(namespace) && namespace.len() <= 63)
        .ok_or(Error::Identity)?;
    identity(leaf, &namespace)?;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let workspace = namespaces
        .get(&namespace)
        .await
        .map_err(|error| api("read workspace", error))?;
    let workspace_uid = workspace
        .uid()
        .filter(|uid| !uid.is_empty())
        .ok_or(Error::Identity)?;
    if workspace.metadata.deletion_timestamp.is_some() {
        return Err(Error::Identity);
    }
    let tasks: Api<KarsTask> = Api::namespaced(client.clone(), &namespace);
    let mut current = tasks
        .get(&leaf.name_any())
        .await
        .map_err(|error| api("read leaf", error))?;
    if current.metadata.uid != leaf.metadata.uid
        || current.metadata.generation != leaf.metadata.generation
    {
        return Err(Error::Changed);
    }
    let mut path = Vec::new();
    let mut seen = BTreeSet::new();
    loop {
        let id = identity(&current, &namespace)?;
        if path.len() >= MAX_CHAIN || !seen.insert(id.uid) {
            return Err(Error::Depth);
        }
        if (readiness == LeafReadiness::RequireReady || !path.is_empty())
            && !crate::kars_task_reconciler::task_is_ready(&current)
        {
            return Err(Error::NotReady);
        }
        let parent = current
            .spec
            .parent_ref
            .as_ref()
            .map(|reference| reference.name.clone());
        path.push(current.clone());
        let Some(parent) = parent else { break };
        if !name(&parent) {
            return Err(Error::Identity);
        }
        let parent = tasks
            .get(&parent)
            .await
            .map_err(|error| api("read parent", error))?;
        identity(&parent, &namespace)?;
        if !crate::kars_task::spec_attenuation_violations(&current.spec, &parent.spec).is_empty() {
            return Err(Error::Attenuation);
        }
        current = parent;
    }
    path.reverse();
    let root = path.first().ok_or(Error::Identity)?;
    let root_uid = root.uid().ok_or(Error::Identity)?;
    let team_owner = owner(root)?
        .filter(|owner| owner.kind == "KarsTeam" && owner.api_version == "kars.azure.com/v1alpha1");
    let teams: Api<KarsTeam> = Api::namespaced(client.clone(), &namespace);
    let team = if let Some(owner) = team_owner {
        if !name(&owner.name) {
            return Err(Error::Identity);
        }
        let team = teams
            .get(&owner.name)
            .await
            .map_err(|error| api("read Team owner", error))?;
        if team.metadata.namespace.as_deref() != Some(namespace.as_str())
            || team.metadata.uid.as_deref() != Some(owner.uid.as_str())
            || team.metadata.deletion_timestamp.is_some()
            || team
                .metadata
                .resource_version
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(Error::Identity);
        }
        Some(team)
    } else {
        None
    };
    for (index, task) in path.iter().enumerate() {
        if let Some(owner) = owner(task)?
            && owner.api_version == "kars.azure.com/v1alpha1"
        {
            if owner.kind == "KarsTeam"
                && team.as_ref().is_none_or(|team| {
                    team.metadata.uid.as_deref() != Some(owner.uid.as_str())
                        || team.name_any() != owner.name
                })
            {
                return Err(Error::Identity);
            }
            if owner.kind == "KarsTask"
                && index.checked_sub(1).is_none_or(|index| {
                    path[index].metadata.uid.as_deref() != Some(owner.uid.as_str())
                        || path[index].name_any() != owner.name
                })
            {
                return Err(Error::Identity);
            }
        }
    }
    let model = crate::kars_task::blueprint::controller_default_model();
    let nodes: Vec<_> = path
        .iter()
        .enumerate()
        .map(|(index, task)| {
            Ok(VerifiedTaskNode {
                task: task.clone(),
                pin: TaskLineagePin {
                    task: identity(task, &namespace)?,
                    parent_task_uid: index.checked_sub(1).and_then(|index| path[index].uid()),
                    root_task_uid: root_uid.clone(),
                },
                generation: task.metadata.generation.ok_or(Error::Identity)?,
                resource_version: task.resource_version().ok_or(Error::Identity)?,
                authorization_digest: task.spec.authorization_digest_with_model(&model),
                effective_authorization: task.spec.authorization_configuration_with_model(&model),
            })
        })
        .collect::<Result<_, Error>>()?;
    for node in &nodes {
        let fresh = tasks
            .get(&node.pin.task.name)
            .await
            .map_err(|error| api("recheck Task snapshot", error))?;
        if fresh.metadata.uid.as_deref() != Some(node.pin.task.uid.as_str())
            || fresh.metadata.resource_version.as_deref() != Some(node.resource_version.as_str())
            || fresh.metadata.deletion_timestamp.is_some()
        {
            return Err(Error::Changed);
        }
    }
    if let Some(team) = &team {
        let fresh = teams
            .get(&team.name_any())
            .await
            .map_err(|error| api("recheck Team snapshot", error))?;
        if fresh.metadata.uid != team.metadata.uid
            || fresh.metadata.resource_version != team.metadata.resource_version
            || fresh.metadata.deletion_timestamp.is_some()
        {
            return Err(Error::Changed);
        }
    }
    let fresh = namespaces
        .get(&namespace)
        .await
        .map_err(|error| api("recheck workspace", error))?;
    if fresh.metadata.uid.as_deref() != Some(workspace_uid.as_str())
        || fresh.metadata.deletion_timestamp.is_some()
    {
        return Err(Error::Changed);
    }
    Ok(VerifiedTaskLineage {
        workspace_uid,
        nodes,
        team,
    })
}

#[cfg(test)]
#[path = "task_identity_tests.rs"]
mod tests;
