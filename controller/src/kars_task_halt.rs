// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Governed halt is a persistent execution gate, not a task/result deletion.
use crate::kars_task::KarsTask;
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, Patch, PatchParams},
};
use serde_json::json;

pub(crate) const HALTED: &str = "kars.azure.com/halted";

pub(crate) fn halted(task: &KarsTask) -> bool {
    task.annotations().contains_key(HALTED)
}

fn owned_child(child: &KarsTask, parent: &KarsTask) -> bool {
    parent.uid().is_some_and(|uid| !uid.is_empty())
        && child.namespace() == parent.namespace()
        && child
            .spec
            .parent_ref
            .as_ref()
            .is_some_and(|r| r.name == parent.name_any())
        && child
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| {
                owners.iter().any(|owner| {
                    owner.api_version == "kars.azure.com/v1alpha1"
                        && owner.kind == "KarsTask"
                        && owner.controller == Some(true)
                        && owner.name == parent.name_any()
                        && Some(&owner.uid) == parent.metadata.uid.as_ref()
                })
            })
}

/// Propagate the existing decision only through live UID-owned Task edges.
/// All writes are fenced; a concurrent activation/replacement retries rather
/// than receiving a stale stop. No task, result, receipt or ledger is removed.
pub(crate) async fn reconcile(client: &Client, task: &KarsTask) -> Result<bool, kube::Error> {
    let Some(decision) = task.annotations().get(HALTED).cloned() else {
        return Ok(false);
    };
    let ns = task.namespace().unwrap_or_else(|| "default".into());
    let api: Api<KarsTask> = Api::namespaced(client.clone(), &ns);
    let children = api.list(&ListParams::default()).await?.items;
    let mut tree = vec![task.clone()];
    let mut index = 0;
    while index < tree.len() {
        let descendants: Vec<_> = children
            .iter()
            .filter(|child| owned_child(child, &tree[index]))
            .filter(|child| !tree.iter().any(|seen| seen.uid() == child.uid()))
            .cloned()
            .collect();
        tree.extend(descendants);
        index += 1;
    }
    // Persist every gate before cleanup, so retries cannot re-admit descendants.
    for member in &mut tree {
        if !halted(member) || member.spec.execution.as_ref().is_some_and(|e| e.launch) {
            let own_decision = member.annotations().get(HALTED).unwrap_or(&decision);
            *member = api.patch(&member.name_any(), &PatchParams::default(), &Patch::Merge(json!({
                "metadata": {"uid": member.uid(), "resourceVersion": member.resource_version(),
                    "annotations": {(HALTED): own_decision}},
                "spec": {"execution": {"launch": false}}
            }))).await?;
        }
    }
    let mut gone = true;
    for member in tree.iter().rev() {
        let mut status = member.status.clone().unwrap_or_default();
        let stopped = super::stop_execution(client, &ns, member, &mut status).await;
        gone &= stopped;
        if stopped {
            status.execution_detail =
                Some("Governed halt; task, results and accounting retained".into());
            if member.uid() == task.uid() && !gone {
                status.execution_phase = Some("Stopping".into());
                status.execution_detail =
                    Some("Governed halt; waiting for UID-owned descendants to stop".into());
            }
        }
        if serde_json::to_value(&member.status).ok() != serde_json::to_value(&status).ok() {
            let mut status_patch = json!(status);
            status_patch["sandboxRef"] = json!(status.sandbox_ref);
            api.patch_status(
                &member.name_any(),
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata": {"uid": member.uid(), "resourceVersion": member.resource_version()},
                    "status": status_patch
                })),
            )
            .await?;
        }
    }
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::Resource;

    #[test]
    fn halt_tree_requires_parent_reference_and_current_controller_uid() {
        let mut parent = KarsTask::new("root", Default::default());
        parent.metadata.namespace = Some("default".into());
        parent.metadata.uid = Some("root-uid".into());
        let mut child = KarsTask::new("phase", Default::default());
        child.metadata.namespace = parent.namespace();
        child.spec.parent_ref = Some(crate::mcp_server::LocalObjectRef {
            name: parent.name_any(),
        });
        child.metadata.owner_references = Some(vec![parent.controller_owner_ref(&()).unwrap()]);
        assert!(owned_child(&child, &parent));
        let valid = child.clone();
        child.metadata.owner_references.as_mut().unwrap()[0].uid = "replaced-root".into();
        assert!(!owned_child(&child, &parent));
        child = valid.clone();
        child.spec.parent_ref = None;
        assert!(!owned_child(&child, &parent));
        child = valid;
        child.metadata.namespace = Some("other".into());
        assert!(!owned_child(&child, &parent));
    }
}
