// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Historical decision evidence is independent of current task authorization.

use chrono::DateTime;
use serde::Serialize;

use super::PredicateApproval;
use crate::kars_approval::{
    KarsApproval, PHASE_APPROVED, PHASE_DENIED, VERDICT_APPROVE, VERDICT_DENY, request_snapshot,
};
use crate::kars_task::KarsTask;

/// A recorded decision, not a current grant or evidence of a consumed transition.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PredicateHistoricalApproval {
    pub decision: PredicateApproval,
    pub approval_uid: String,
    pub task_uid: String,
    pub task_name: String,
    pub task_namespace: String,
    /// Original D0 binding, even when the receipt's subject now describes D1.
    pub bound_envelope_digest: String,
    pub bound_request: String,
    pub requested_at: String,
    pub expires_at: String,
    pub evidence_scope: &'static str,
    /// This historical evidence record never confers current authority.
    pub authorizes_current_task: bool,
    /// No consumption or causal relationship to the task's current state is attested.
    pub consumption_attested: bool,
}

fn decision_fact(approval: &KarsApproval) -> Option<PredicateApproval> {
    let status = approval.status.as_ref()?;
    let decision = approval.spec.decision.as_ref()?;
    let verdict = match status.phase.as_deref()? {
        PHASE_APPROVED => VERDICT_APPROVE,
        PHASE_DENIED => VERDICT_DENY,
        _ => return None,
    };
    let generation = approval
        .metadata
        .generation
        .filter(|generation| *generation > 0)?;
    let requested_at = DateTime::parse_from_rfc3339(status.requested_at.as_deref()?).ok()?;
    let decided_at = DateTime::parse_from_rfc3339(status.decided_at.as_deref()?).ok()?;
    let expires_at = DateTime::parse_from_rfc3339(status.expires_at.as_deref()?).ok()?;
    if status.observed_generation != Some(generation)
        || decision.verdict != verdict
        || decision.decider.trim().is_empty()
        || status.decider.as_deref() != Some(decision.decider.as_str())
        || status.bound_request.as_deref() != Some(request_snapshot(&approval.spec).as_str())
        || approval.spec.action.kind.trim().is_empty()
        || decided_at < requested_at
        || decided_at >= expires_at
    {
        return None;
    }
    Some(PredicateApproval {
        name: approval
            .metadata
            .name
            .clone()
            .filter(|name| !name.is_empty())?,
        action_kind: approval.spec.action.kind.clone(),
        summary: approval.spec.action.summary.clone(),
        verdict: verdict.into(),
        decider: decision.decider.clone(),
        decided_at: status.decided_at.clone()?,
        requested_tier: approval.spec.action.requested_tier,
    })
}

pub fn decision_facts(approvals: &[KarsApproval]) -> Vec<PredicateApproval> {
    let mut facts: Vec<_> = approvals.iter().filter_map(decision_fact).collect();
    facts.sort_by(|a, b| a.name.cmp(&b.name));
    facts
}

pub fn historical_approval_facts(
    task: &KarsTask,
    approvals: &[KarsApproval],
) -> Vec<PredicateHistoricalApproval> {
    let Some(uid) = task.metadata.uid.as_deref().filter(|uid| !uid.is_empty()) else {
        return Vec::new();
    };
    let Some(name) = task
        .metadata
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
    else {
        return Vec::new();
    };
    let namespace = task.metadata.namespace.as_deref().unwrap_or("default");
    let mut facts: Vec<_> = approvals
        .iter()
        .filter_map(|approval| {
            let decision = decision_fact(approval)?;
            let status = approval.status.as_ref()?;
            if namespace.is_empty()
                || approval.metadata.namespace.as_deref().unwrap_or("default") != namespace
                || approval.spec.task_ref.name != name
                || status.bound_task_uid.as_deref() != Some(uid)
            {
                return None;
            }
            let bound = status.bound_envelope_digest.as_deref()?;
            let hash = bound.strip_prefix("sha256:")?;
            if !matches!(hash.len(), 32 | 64) || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return None;
            }
            Some(PredicateHistoricalApproval {
                decision,
                approval_uid: approval
                    .metadata
                    .uid
                    .clone()
                    .filter(|uid| !uid.is_empty())?,
                task_uid: uid.into(),
                task_name: name.into(),
                task_namespace: namespace.into(),
                bound_envelope_digest: bound.into(),
                bound_request: status.bound_request.clone()?,
                requested_at: status.requested_at.clone()?,
                expires_at: status.expires_at.clone()?,
                evidence_scope: "historicalDecision",
                authorizes_current_task: false,
                consumption_attested: false,
            })
        })
        .collect();
    facts.sort_by(|a, b| {
        (&a.decision.name, &a.approval_uid).cmp(&(&b.decision.name, &b.approval_uid))
    });
    facts
}

#[cfg(test)]
#[path = "kars_receipt_approvals_tests.rs"]
mod tests;
