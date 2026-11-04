// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One-use Task tier transitions using the existing KarsApproval authority binding.
//! A decision never rewrites a consumed run, a Team-owned spec, or a parent's ceiling.

use crate::{
    kars_approval::{KarsApproval, approval_authorizes_task},
    kars_task::KarsTask,
    kars_task_reconciler::task_is_ready,
};
use chrono::{DateTime, Utc};
use kube::{
    Api, Client, Resource, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::json;

pub(crate) const REQUESTED: &str = "kars.azure.com/requested-tier";
pub(crate) const CONSUMED: &str = "kars.azure.com/consumed-promotion";
pub(crate) const RESULT: &str = "kars.azure.com/promotion-result";
pub(crate) const DETAIL: &str = "kars.azure.com/promotion-detail";
const PENDING_RESULT: &str = "kars.azure.com/promotion-pending-result";
const APPLIED_DETAIL: &str = "Task tier updated. Descendant ceiling and other policy gates are unchanged; execution still requires its normal launch authorization.";
pub(crate) const CONTEXT: [&str; 5] = [
    "kars.azure.com/owner-sub",
    "kars.azure.com/owner-name",
    "kars.azure.com/team",
    "kars.azure.com/milestone",
    "kars.azure.com/req-run",
];

pub(crate) fn context(task: &KarsTask) -> std::collections::BTreeMap<String, String> {
    CONTEXT
        .into_iter()
        .filter_map(|key| {
            task.annotations()
                .get(key)
                .or_else(|| task.labels().get(key))
                .map(|value| (key.into(), value.clone()))
        })
        .collect()
}

/// Expiry bounds the first decision, as in the Team transition consumer.
pub(crate) fn timely(approval: &KarsApproval) -> bool {
    let Some(status) = &approval.status else {
        return false;
    };
    let parse =
        |value: Option<&str>| value.and_then(|value| DateTime::parse_from_rfc3339(value).ok());
    matches!((parse(status.requested_at.as_deref()), parse(status.decided_at.as_deref()),
        parse(status.expires_at.as_deref())), (Some(requested), Some(decided), Some(expires))
        if requested <= decided && decided < expires && decided <= Utc::now())
}

pub(crate) fn unavailable(task: &KarsTask) -> Option<&'static str> {
    if task
        .metadata
        .owner_references
        .iter()
        .flatten()
        .any(|owner| owner.controller == Some(true))
    {
        return Some(
            "This Task is controller-owned. Promote its source Team/package; a fresh authorized run is required. The current run's authority is unchanged.",
        );
    }
    if task
        .spec
        .execution
        .as_ref()
        .is_some_and(|execution| execution.launch)
        || task.status.as_ref().is_some_and(|status| {
            status.sandbox_ref.is_some()
                || status
                    .execution_phase
                    .as_deref()
                    .is_some_and(|phase| phase != "Idle")
        })
        || [
            "kars.azure.com/run-requested",
            "kars.azure.com/reviewed-state",
            "kars.azure.com/reviewed-root",
            "kars.azure.com/qualification-run",
            "kars.azure.com/halted",
        ]
        .iter()
        .any(|key| task.annotations().contains_key(*key))
    {
        return Some(
            "This execution identity has already been launched or consumed. A fresh authorized run is required; this approval does not widen the current run.",
        );
    }
    None
}

fn ticket_name(task: &KarsTask, target: i32) -> String {
    let identity = json!([
        task.uid(),
        task.metadata.generation,
        task.envelope_digest(),
        target
    ]);
    format!(
        "task-promote-{}",
        &crate::providers::signing::sha256_hex(identity.to_string().as_bytes())[..48]
    )
}

fn document(task: &KarsTask, target: i32) -> KarsApproval {
    let requester = task.annotations().get("kars.azure.com/promotion-requested-by-sub").map(|subject|
        json!({"subject":subject,"name":task.annotations().get("kars.azure.com/promotion-requested-by").unwrap_or(subject)}));
    serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsApproval",
        "metadata":{"name":ticket_name(task,target),"namespace":task.namespace(),
            "annotations":context(task),"ownerReferences":[task.controller_owner_ref(&()).expect("ready Task UID")]},
        "spec":{"requestedBy":requester,"taskRef":{"name":task.name_any()},"action":{"kind":"tierRaise","requestedTier":target,
            "summary":format!("Promote Mission '{}' from Tier {} to Tier {target}",task.name_any(),task.spec.envelope.tier),
            "detail":unavailable(task).unwrap_or("Raise this Task's autonomy tier only. Descendant authority ceiling, tools, egress, budget and execution gate stay unchanged.")},"ttl":"PT1H"}
    })).expect("typed promotion document")
}

fn authorized(task: &KarsTask, approval: &KarsApproval, target: i32) -> bool {
    let owner = task.controller_owner_ref(&());
    task.metadata
        .uid
        .as_ref()
        .is_some_and(|uid| !uid.is_empty())
        && task
            .metadata
            .resource_version
            .as_ref()
            .is_some_and(|rv| !rv.is_empty())
        && task.metadata.generation.is_some()
        && task
            .metadata
            .namespace
            .as_ref()
            .is_some_and(|ns| !ns.is_empty())
        && !task.annotations().contains_key(PENDING_RESULT)
        && task
            .annotations()
            .get(REQUESTED)
            .is_none_or(|requested| requested.parse::<i32>().ok() == Some(target))
        && approval
            .metadata
            .uid
            .as_ref()
            .is_some_and(|uid| !uid.is_empty())
        && approval
            .metadata
            .resource_version
            .as_ref()
            .is_some_and(|rv| !rv.is_empty())
        && approval.metadata.generation.is_some()
        && approval.metadata.namespace == task.metadata.namespace
        && owner.is_some()
        && approval
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|owners| {
                owners.len() == 1
                    && owner.as_ref().is_some_and(|expected| {
                        owners[0].api_version == expected.api_version
                            && owners[0].kind == expected.kind
                            && owners[0].name == expected.name
                            && owners[0].uid == expected.uid
                            && owners[0].controller == Some(true)
                    })
            })
        && CONTEXT
            .iter()
            .all(|key| approval.annotations().get(*key) == context(task).get(*key))
        && approval.spec.task_ref.name == task.name_any()
        && approval.spec.action.kind == "tierRaise"
        && approval.spec.action.requested_tier == Some(target)
        && (1..=5).contains(&target)
        && target > task.spec.envelope.tier
        && task
            .spec
            .requested_tier
            .is_none_or(|requested| requested == target)
        && task.annotations().get(CONSUMED) != approval.metadata.uid.as_ref()
        && approval_authorizes_task(approval, task)
        && timely(approval)
}

pub(crate) async fn record_result(
    client: &Client,
    approval: &KarsApproval,
    result: &str,
    detail: &str,
) -> Result<(), kube::Error> {
    if approval.annotations().get(RESULT).map(String::as_str) == Some(result)
        && approval.annotations().get(DETAIL).map(String::as_str) == Some(detail)
    {
        return Ok(());
    }
    Api::<KarsApproval>::namespaced(client.clone(), &approval.namespace().unwrap_or_default())
        .patch(
            &approval.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{
            "uid":approval.uid(),"resourceVersion":approval.resource_version(),
            "annotations":{RESULT:result,DETAIL:detail}}})),
        )
        .await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReviewEffect {
    Applied,
    RequiresFreshRun(&'static str),
    NotApplied,
}

/// Returns true only after persisting the authority transition. Native callers
/// use the explicit outcome below to distinguish a non-grant from a stale ticket.
pub(crate) async fn apply_approved(
    client: &Client,
    task: &KarsTask,
    approval: &KarsApproval,
    target: i32,
) -> Result<bool, kube::Error> {
    Ok(resolve_approved(client, task, approval, target).await? == ReviewEffect::Applied)
}

pub(crate) async fn resolve_approved(
    client: &Client,
    task: &KarsTask,
    approval: &KarsApproval,
    target: i32,
) -> Result<ReviewEffect, kube::Error> {
    if !authorized(task, approval, target) {
        return Ok(ReviewEffect::NotApplied);
    }
    if let Some(detail) = unavailable(task) {
        record_result(client, approval, "requires-fresh-run", detail).await?;
        return Ok(ReviewEffect::RequiresFreshRun(detail));
    }
    let api = Api::<KarsTask>::namespaced(client.clone(), &task.namespace().unwrap_or_default());
    let parent = if let Some(reference) = &task.spec.parent_ref {
        let parent = api.get(&reference.name).await?;
        let mut promoted = task.spec.clone();
        promoted.envelope.tier = target;
        if !task_is_ready(&parent)
            || !crate::kars_task::spec_attenuation_violations(&promoted, &parent.spec).is_empty()
        {
            record_result(client, approval, "parent-ceiling", "Requested tier exceeds current delegated authority. Promote the source authority first; the Task is unchanged.").await?;
            return Ok(ReviewEffect::NotApplied);
        }
        Some(parent)
    } else {
        None
    };
    let current = api.get(&task.name_any()).await?;
    if current.metadata.resource_version != task.metadata.resource_version
        || !authorized(&current, approval, target)
        || unavailable(&current).is_some()
    {
        return Ok(ReviewEffect::NotApplied);
    }
    if let Some(parent) = parent {
        let fresh = api.get(&parent.name_any()).await?;
        if fresh.metadata.uid != parent.metadata.uid
            || fresh.metadata.resource_version != parent.metadata.resource_version
            || !task_is_ready(&fresh)
        {
            return Ok(ReviewEffect::NotApplied);
        }
    }
    let updated = api.patch(&task.name_any(), &PatchParams::default(), &Patch::Merge(json!({
        "metadata":{"uid":task.uid(),"resourceVersion":task.resource_version(),"annotations":{
            CONSUMED:approval.uid(),REQUESTED:null,PENDING_RESULT:approval.name_any()}},
        "spec":{"envelope":{"tier":target},"requestedTier":null}
    }))).await?;
    record_result(client, approval, "applied", APPLIED_DETAIL).await?;
    clear_pending_result(client, &updated).await?;
    Ok(ReviewEffect::Applied)
}

async fn clear_pending_result(client: &Client, task: &KarsTask) -> Result<(), kube::Error> {
    Api::<KarsTask>::namespaced(client.clone(), &task.namespace().unwrap_or_default())
        .patch(&task.name_any(), &PatchParams::default(), &Patch::Merge(json!({"metadata":{
            "uid":task.uid(),"resourceVersion":task.resource_version(),"annotations":{PENDING_RESULT:null}
        }}))).await?;
    Ok(())
}

pub(crate) async fn process(client: &Client, task: &KarsTask) -> Result<bool, kube::Error> {
    if task.metadata.uid.as_ref().is_none_or(String::is_empty)
        || task
            .metadata
            .resource_version
            .as_ref()
            .is_none_or(String::is_empty)
        || task
            .metadata
            .namespace
            .as_ref()
            .is_none_or(String::is_empty)
    {
        return Ok(false);
    }
    // The authority CAS is already durable. Recover only its presentation after
    // an interrupted approval annotation write; never apply the ticket twice.
    if let Some(name) = task.annotations().get(PENDING_RESULT) {
        let api =
            Api::<KarsApproval>::namespaced(client.clone(), &task.namespace().unwrap_or_default());
        if let Some(approval) = api.get_opt(name).await?
            && approval
                .uid()
                .as_ref()
                .is_some_and(|uid| task.annotations().get(CONSUMED) == Some(uid))
            && approval
                .status
                .as_ref()
                .and_then(|status| status.bound_task_uid.as_ref())
                == task.metadata.uid.as_ref()
            && task
                .metadata
                .uid
                .as_ref()
                .is_some_and(|uid| !uid.is_empty())
            && approval.spec.task_ref.name == task.name_any()
            && approval.spec.action.kind == "tierRaise"
            && approval
                .status
                .as_ref()
                .and_then(|status| status.phase.as_deref())
                == Some("Approved")
            && timely(&approval)
        {
            record_result(client, &approval, "applied", APPLIED_DETAIL).await?;
            clear_pending_result(client, task).await?;
            return Ok(true);
        }
        return Ok(false);
    }
    let metadata_target = task
        .annotations()
        .get(REQUESTED)
        .and_then(|value| value.parse::<i32>().ok());
    if task.spec.requested_tier.is_some()
        && metadata_target.is_some()
        && task.spec.requested_tier != metadata_target
    {
        return Ok(false);
    }
    let Some(target) = task
        .spec
        .requested_tier
        .or(metadata_target)
        .filter(|target| (1..=5).contains(target) && *target > task.spec.envelope.tier)
    else {
        return Ok(false);
    };
    if !task_is_ready(task)
        || task.metadata.uid.as_ref().is_none_or(String::is_empty)
        || task.metadata.generation.is_none()
    {
        return Ok(false);
    }
    let expected = document(task, target);
    let api =
        Api::<KarsApproval>::namespaced(client.clone(), &task.namespace().unwrap_or_default());
    let approval = match api.get_opt(&expected.name_any()).await? {
        Some(approval) => approval,
        None => {
            api.create(&PostParams::default(), &expected).await?;
            return Ok(false);
        }
    };
    if crate::kars_approval::request_snapshot(&approval.spec)
        != crate::kars_approval::request_snapshot(&expected.spec)
    {
        return Ok(false);
    }
    apply_approved(client, task, &approval, target).await
}

#[cfg(test)]
#[path = "kars_task_promotion_tests.rs"]
mod tests;
