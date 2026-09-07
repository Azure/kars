// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One-use, authority-bound human transition tickets. Terminal approvals are
//! never overwritten; consumption and the Team transition share one CAS.

use super::{ReconcileError, namespace, parse_rfc3339, resource_version, tasks};
use crate::kars_approval::{
    ApprovalAction, KarsApproval, KarsApprovalSpec, approval_authorizes_task,
};
use crate::kars_task::KarsTask;
use crate::kars_team::KarsTeam;
use crate::mcp_server::LocalObjectRef;
use crate::providers::signing::sha256_hex;
use chrono::Utc;
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::json;

const ANNOT_CONSUMED: &str = "kars.azure.com/consumed-promotion";
const ANNOT_PRINCIPAL_UID: &str = "kars.azure.com/promotion-principal-uid";
const ANNOT_PRINCIPAL_DIGEST: &str = "kars.azure.com/promotion-principal-digest";

pub(super) fn ticket_name(
    team: &KarsTeam,
    principal: &KarsTask,
    target: i32,
) -> Result<String, ReconcileError> {
    let uid = tasks::owner_ref(team)?.uid;
    let principal_uid = principal
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| ReconcileError::Invalid("promotion principal has no UID".into()))?;
    let digest = principal.envelope_digest();
    if principal
        .status
        .as_ref()
        .and_then(|status| status.envelope_digest.as_deref())
        != Some(digest.as_str())
    {
        return Err(ReconcileError::Invalid(
            "promotion principal has no current full authority digest".into(),
        ));
    }
    let identity =
        serde_json::to_vec(&(uid, team.metadata.generation, principal_uid, digest, target))?;
    let prefix: String = team.name_any().chars().take(180).collect();
    Ok(format!(
        "{prefix}-promote-t{target}-{}",
        &sha256_hex(&identity)[..32]
    ))
}

fn principal_ready(team: &KarsTeam, principal: &KarsTask) -> bool {
    tasks::owned(&principal.metadata, team)
        && principal.metadata.generation.is_some()
        && principal
            .metadata
            .uid
            .as_ref()
            .is_some_and(|uid| !uid.is_empty())
        && principal.name_any() == super::specs::principal_name(team)
        && serde_json::to_value(&principal.spec.envelope).is_ok_and(|live| {
            serde_json::to_value(&team.spec.envelope).is_ok_and(|expected| live == expected)
        })
        && crate::kars_task_reconciler::task_is_ready(principal)
}

pub(super) fn authorized(
    team: &KarsTeam,
    principal: &KarsTask,
    approval: &KarsApproval,
    target: i32,
) -> bool {
    if !principal_ready(team, principal)
        || !approval_authorizes_task(approval, principal)
        || !tasks::owned(&approval.metadata, team)
        || approval.metadata.deletion_timestamp.is_some()
        || approval.metadata.uid.as_ref().is_none_or(String::is_empty)
        || approval.metadata.generation.is_none()
        || ticket_name(team, principal, target).ok().as_deref()
            != Some(approval.name_any().as_str())
        || approval.spec.task_ref.name != principal.name_any()
        || approval.spec.action.kind != "tierRaise"
        || approval.spec.action.requested_tier != Some(target)
        || team.spec.requested_tier != Some(target)
        || target <= team.spec.envelope.tier
        || !(1..=5).contains(&target)
        || team.annotations().get(ANNOT_CONSUMED) == approval.metadata.uid.as_ref()
    {
        return false;
    }
    let Some(status) = &approval.status else {
        return false;
    };
    let digest = principal.envelope_digest();
    if approval.annotations().get(ANNOT_PRINCIPAL_UID) != principal.metadata.uid.as_ref()
        || approval.annotations().get(ANNOT_PRINCIPAL_DIGEST) != Some(&digest)
    {
        return false;
    }
    // TTL limits the first decision, not how long a timely terminal approval
    // may wait for the Team controller to consume it.
    matches!(
        (
            status.requested_at.as_deref().and_then(parse_rfc3339),
            status.decided_at.as_deref().and_then(parse_rfc3339),
            status.expires_at.as_deref().and_then(parse_rfc3339),
        ),
        (Some(requested), Some(decided), Some(expires))
            if requested <= decided && decided < expires && decided <= Utc::now()
    )
}

pub(super) async fn process_promotion(
    client: &Client,
    team: &KarsTeam,
    principal_name: &str,
) -> Result<bool, ReconcileError> {
    let Some(target) = team
        .spec
        .requested_tier
        .filter(|target| *target > team.spec.envelope.tier)
    else {
        return Ok(false);
    };
    if !(1..=5).contains(&target) {
        return Err(ReconcileError::Invalid(
            "requestedTier must be in 1..5".into(),
        ));
    }
    let ns = namespace(team)?;
    let tasks_api = Api::<KarsTask>::namespaced(client.clone(), ns);
    let Some(principal) = tasks_api.get_opt(principal_name).await? else {
        return Ok(false);
    };
    if !principal_ready(team, &principal) {
        return Ok(false);
    }
    let approvals = Api::<KarsApproval>::namespaced(client.clone(), ns);
    let name = ticket_name(team, &principal, target)?;
    match approvals.get_opt(&name).await? {
        Some(approval) => {
            if !tasks::owned(&approval.metadata, team) {
                return Err(ReconcileError::Invalid(format!(
                    "refusing foreign promotion ticket '{name}'"
                )));
            }
            if !authorized(team, &principal, &approval, target) {
                return Ok(false);
            }
            // Re-read after examining the ticket so a concurrent principal
            // replacement/update cannot silently qualify via the old read.
            let current = tasks_api.get(principal_name).await?;
            if current.metadata.resource_version != principal.metadata.resource_version
                || !authorized(team, &current, &approval, target)
            {
                return Ok(false);
            }
            Api::<KarsTeam>::namespaced(client.clone(), ns)
                .patch(
                    &team.name_any(),
                    &PatchParams::default(),
                    &Patch::Merge(json!({
                        "metadata": {
                            "uid": team.metadata.uid,
                            "resourceVersion": resource_version(team)?,
                            "annotations": { ANNOT_CONSUMED: approval.metadata.uid },
                        },
                        "spec": {
                            "envelope": { "tier": target, "authorityCeiling": target },
                            "requestedTier": null,
                        },
                    })),
                )
                .await?;
            Ok(true)
        }
        None => {
            let mut approval = KarsApproval::new(
                &name,
                KarsApprovalSpec {
                    task_ref: LocalObjectRef {
                        name: principal_name.into(),
                    },
                    action: ApprovalAction {
                        kind: "tierRaise".into(),
                        summary: format!(
                            "Promote team '{}' from Tier {} to Tier {target}",
                            team.name_any(),
                            team.spec.envelope.tier
                        ),
                        detail: Some(
                            "Grant this Team a higher authority tier and descendant ceiling."
                                .into(),
                        ),
                        requested_tier: Some(target),
                    },
                    ttl: Some("PT1H".into()),
                    decision: None,
                },
            );
            approval.metadata.namespace = team.metadata.namespace.clone();
            approval.metadata.owner_references = Some(vec![tasks::owner_ref(team)?]);
            let annotations = approval
                .metadata
                .annotations
                .get_or_insert_with(Default::default);
            annotations.insert(
                ANNOT_PRINCIPAL_UID.into(),
                principal.metadata.uid.clone().expect("ready principal UID"),
            );
            annotations.insert(
                ANNOT_PRINCIPAL_DIGEST.into(),
                principal
                    .status
                    .as_ref()
                    .and_then(|status| status.envelope_digest.clone())
                    .expect("ready principal digest"),
            );
            // A racing create returns Conflict and retries. Never force-apply
            // over an existing pending decision or an immutable terminal ticket.
            approvals.create(&PostParams::default(), &approval).await?;
            Ok(false)
        }
    }
}
