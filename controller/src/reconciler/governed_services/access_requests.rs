// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    control_client::{ControlClient, RequestEntry, RequestScope, Snapshot},
    identity_read_only,
};
use crate::providers::signing::sha256_hex;
use crate::{
    crd::KarsSandbox,
    egress_approval::EgressApproval,
    egress_approval_compile::{REQUEST_ID, REQUEST_SCOPE, RequestBinding},
    kars_approval::{
        KarsApproval, approval_authorizes_task, approval_binding_matches_task, request_snapshot,
    },
    kars_task::KarsTask,
};
use k8s_openapi::api::core::v1::Namespace;
use kube::{Api, Client, ResourceExt, api::PostParams};
use serde_json::json;

#[cfg(test)]
#[path = "access_requests_tests.rs"]
mod tests;

const CONTEXT_ANNOTATIONS: [&str; 5] = [
    "kars.azure.com/owner-sub",
    "kars.azure.com/owner-name",
    "kars.azure.com/team",
    "kars.azure.com/milestone",
    "kars.azure.com/req-run",
];

fn request_name(scope: &RequestScope, entry: &RequestEntry) -> String {
    let digest = sha256_hex(
        json!([
            scope.identity,
            scope.id,
            scope.assignment_id,
            entry.request_id
        ])
        .to_string()
        .as_bytes(),
    );
    format!("hitl-{}", &digest[..48])
}

fn approval_document(
    task: &KarsTask,
    scope: &RequestScope,
    entry: &RequestEntry,
) -> Result<KarsApproval, String> {
    let binding = RequestBinding::from_annotations(Some(&std::collections::BTreeMap::from([
        (REQUEST_SCOPE.into(), scope.id.clone()),
        (REQUEST_ID.into(), entry.request_id.clone()),
    ])))?
    .ok_or("Human request binding missing")?;
    if entry.scope_id != binding.scope || entry.reason.len() > 512 || entry.target.len() > 253 {
        return Err("Human request scope or detail invalid".into());
    }
    if entry.kind == "tier"
        && entry.tier.is_none_or(|tier| {
            !(1..=5).contains(&tier) || i32::from(tier) <= task.spec.envelope.tier
        })
    {
        return Err("Tier request must specify an exact higher tier in 1..5".into());
    }
    let kind = match entry.kind.as_str() {
        "tier" => "tierRaise",
        "egress" => "egress",
        "clarification" => "clarification",
        _ => "custom",
    };
    let summary = if entry.kind == "clarification" {
        entry.reason.clone()
    } else if entry.kind == "egress" {
        format!(
            "Allow {}:{} for this assignment",
            entry.target,
            entry.port.ok_or("Egress request port missing")?
        )
    } else {
        format!("{} request: {}", entry.kind, entry.reason)
    };
    let mut detail = json!({"source":"router-access-request","scope":scope.id,"request":entry.request_id,
        "identity":scope.identity,"assignment":scope.assignment_id,"kind":entry.kind,
        "target":entry.target,"port":entry.port,"tier":entry.tier,"reason":entry.reason,
        "expiresAtUnix":entry.expires_at_unix});
    if entry.kind == "tier" {
        detail["authorityEffect"] = "Review of an exact tier only. A consumed execution or controller-owned Task requires promotion of its source package and a fresh authorized run; approval does not widen this running assignment.".into();
    }
    let detail = detail.to_string();
    let sandbox_uid = scope.identity["sandbox"]["uid"]
        .as_str()
        .ok_or("Human request sandbox UID missing")?;
    let annotations: std::collections::BTreeMap<_, _> = CONTEXT_ANNOTATIONS
        .into_iter()
        .filter_map(|key| {
            task.annotations()
                .get(key)
                .or_else(|| task.labels().get(key))
                .map(|value| (key, value))
        })
        .collect();
    serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsApproval",
        "metadata":{"name":request_name(scope, entry),"namespace":task.namespace(),"annotations":annotations,
            "labels":{"app.kubernetes.io/managed-by":"kars-controller","kars.azure.com/access-request":"true"},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":task.name_any(),
                "uid":task.uid(),"controller":true,"blockOwnerDeletion":false}]},
        "spec":{"taskRef":{"name":task.name_any()},"action":{"kind":kind,"summary":summary,"detail":detail,
            "requestedTier":entry.tier},"requestedBy":{"subject":format!("sandbox:{sandbox_uid}"),
                "name":scope.identity["sandbox"]["name"]},"ttl":"PT15M"}
    })).map_err(|_| "Human approval document invalid".into())
}

fn decision(
    approval: &KarsApproval,
    expected: &KarsApproval,
    task: &KarsTask,
    entry: &RequestEntry,
) -> Result<Option<(bool, Option<String>)>, String> {
    if approval.metadata.deletion_timestamp.is_some()
        || request_snapshot(&approval.spec) != request_snapshot(&expected.spec)
        || approval.metadata.owner_references != expected.metadata.owner_references
        || CONTEXT_ANNOTATIONS
            .iter()
            .any(|key| approval.annotations().get(*key) != expected.annotations().get(*key))
    {
        return Err("Human approval request binding changed".into());
    }
    let Some(status) = approval.status.as_ref() else {
        return Ok(None);
    };
    let Some(review) = approval.spec.decision.as_ref() else {
        return Ok(None);
    };
    if !approval_binding_matches_task(approval, task)
        || status.observed_generation != approval.metadata.generation
        || status.decider.as_deref() != Some(review.decider.as_str())
        || review.decider.trim().is_empty()
        || status.decided_at.as_deref().is_none_or(str::is_empty)
    {
        return Ok(None);
    }
    if status.phase.as_deref() == Some("Denied") && review.verdict == "deny" {
        return Ok(Some((false, None)));
    }
    if !approval_authorizes_task(approval, task) {
        return Ok(None);
    }
    let answer = if entry.kind == "clarification" {
        Some(
            review
                .reason
                .as_ref()
                .filter(|answer| !answer.trim().is_empty() && answer.len() <= 8192)
                .ok_or(
                    "Approved clarification requires a nonblank answer of at most 8192 UTF-8 bytes",
                )?
                .clone(),
        )
    } else {
        None
    };
    Ok(Some((true, answer)))
}

async fn ensure_egress_grant(
    client: &Client,
    sandbox: &KarsSandbox,
    approval: &KarsApproval,
    scope: &RequestScope,
    entry: &RequestEntry,
) -> Result<(), String> {
    let name = request_name(scope, entry);
    let grant: EgressApproval = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"EgressApproval",
        "metadata":{"name":name,"namespace":sandbox.namespace(),
            "labels":{"app.kubernetes.io/managed-by":"kars-controller"},
            "annotations":{REQUEST_SCOPE:scope.id,REQUEST_ID:entry.request_id},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsApproval","name":approval.name_any(),
                "uid":approval.uid(),"controller":true,"blockOwnerDeletion":false}]},
        "spec":{"sandbox":sandbox.name_any(),"hosts":[{"host":entry.target,"port":entry.port}],
            "reason":format!("Human-reviewed access request {}", approval.name_any()),"ticket":approval.name_any(),"ttl":"PT15M"}
    })).map_err(|_| "Scoped egress grant document invalid")?;
    let api = Api::<EgressApproval>::namespaced(
        client.clone(),
        &sandbox.namespace().ok_or("Sandbox workspace missing")?,
    );
    let existing = match api.get_opt(&name).await.map_err(super::api_error)? {
        Some(existing) => existing,
        None => match api.create(&PostParams::default(), &grant).await {
            Ok(created) => created,
            Err(kube::Error::Api(error)) if error.code == 409 => {
                api.get(&name).await.map_err(super::api_error)?
            }
            Err(error) => return Err(super::api_error(error)),
        },
    };
    if existing.metadata.deletion_timestamp.is_some()
        || existing.metadata.annotations != grant.metadata.annotations
        || existing.metadata.owner_references != grant.metadata.owner_references
        || serde_json::to_value(&existing.spec).ok() != serde_json::to_value(&grant.spec).ok()
    {
        return Err("Scoped egress grant was replaced or changed".into());
    }
    Ok(())
}

pub(crate) async fn current(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Option<Snapshot>, String> {
    match ControlClient::connect(client, sandbox, namespace).await? {
        Some(control) => control.inspect().await.map(Some),
        None => Ok(None),
    }
}

pub(crate) fn authorizes_grant(snapshot: &Snapshot, approval: &EgressApproval) -> bool {
    let Ok(Some(binding)) =
        RequestBinding::from_annotations(approval.metadata.annotations.as_ref())
    else {
        return false;
    };
    let Some(entry) = snapshot.active_entry(&binding.scope, &binding.request) else {
        return false;
    };
    entry.kind == "egress"
        && matches!(entry.status.as_str(), "approved" | "dispatch_claimed")
        && approval.spec.hosts.len() == 1
        && approval.spec.hosts[0].host == entry.target
        && approval.spec.hosts[0].port == entry.port
        && entry.port.is_some()
}

async fn finish_reviewed_tier(
    client: &Client,
    control: &ControlClient,
    task: &KarsTask,
    approval: &KarsApproval,
    scope: &RequestScope,
    entry: &RequestEntry,
) -> Result<(), String> {
    use crate::kars_task_reconciler::promotion::{ReviewEffect, resolve_approved};
    let tier = i32::from(entry.tier.ok_or("Tier request missing exact tier")?);
    if let ReviewEffect::RequiresFreshRun(detail) = resolve_approved(client, task, approval, tier)
        .await
        .map_err(super::api_error)?
    {
        // The human approval remains Approved and bound to this Task. Cancel only
        // this queue request after its non-grant outcome has been persisted.
        let explanation = format!(
            "requires-fresh-run: Human review approved Tier {tier}, but no authority was granted to this assignment. {detail}"
        );
        control
            .cancel_reviewed_tier(&scope.id, &entry.request_id, &explanation)
            .await?;
    }
    // An applied authority change invalidates the old queue's identity; a stale
    // ticket has no effect. Neither is an approval ACK for the old assignment.
    Ok(())
}

pub(crate) async fn reconcile(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), String> {
    let Some(control) = ControlClient::connect(client, sandbox, namespace).await? else {
        return Ok(());
    };
    let snapshot = control.inspect().await?;
    let Some(task_name) = snapshot.scope.identity["task"]["name"].as_str() else {
        return Ok(());
    };
    let workspace = sandbox.namespace().ok_or("Sandbox workspace missing")?;
    let tasks = Api::<KarsTask>::namespaced(client.clone(), &workspace);
    let approvals = Api::<KarsApproval>::namespaced(client.clone(), &workspace);
    for entry in &snapshot.entries {
        if entry.status != "pending"
            || snapshot
                .active_entry(&snapshot.scope.id, &entry.request_id)
                .is_none()
        {
            continue;
        }
        if identity_read_only(client, sandbox, namespace).await? != snapshot.scope.identity {
            return Err("Human request task authority changed".into());
        }
        let task = tasks.get(task_name).await.map_err(super::api_error)?;
        if task.metadata.uid.as_deref() != snapshot.scope.identity["task"]["uid"].as_str()
            || task.metadata.generation != snapshot.scope.identity["task_generation"].as_i64()
        {
            return Err("Human request task incarnation changed".into());
        }
        let expected = approval_document(&task, &snapshot.scope, entry)?;
        let name = expected.name_any();
        let approval = match approvals.get_opt(&name).await.map_err(super::api_error)? {
            Some(existing) => existing,
            None => match approvals.create(&PostParams::default(), &expected).await {
                Ok(created) => created,
                Err(kube::Error::Api(error)) if error.code == 409 => {
                    approvals.get(&name).await.map_err(super::api_error)?
                }
                Err(error) => return Err(super::api_error(error)),
            },
        };
        if let Some((approve, answer)) = decision(&approval, &expected, &task, entry)? {
            if approve && entry.kind == "tier" {
                finish_reviewed_tier(client, &control, &task, &approval, &snapshot.scope, entry)
                    .await?;
                continue;
            }
            if approve && entry.kind == "egress" {
                ensure_egress_grant(client, sandbox, &approval, &snapshot.scope, entry).await?;
            }
            // Queue approval is not a policy grant. Egress waits for the scoped mount;
            // other capability decisions remain subject to their existing enforcement.
            control
                .decide(
                    &snapshot.scope.id,
                    &entry.request_id,
                    approve,
                    answer.as_deref(),
                )
                .await?;
        }
    }
    Ok(())
}
