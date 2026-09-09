// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Admission unavailability is not revocation of an already funded execution.

use super::store::{Store, StoreError};
use crate::{
    inference_budget_contract::{AccountPhase, BudgetError, MAX_SESSIONS, ledger::Ledger},
    kars_task::{KarsTask, KarsTaskStatus},
    status::conditions,
};
use kube::{Client, ResourceExt};

const CONDITION: &str = "GovernedInferenceReady";
const PENDING: &str = "AdmissionUnavailable";

pub fn capacity(ledger: &Ledger, task_uid: &str) -> Result<(), BudgetError> {
    ledger.validate()?;
    if ledger.phase != AccountPhase::Active {
        return Err(BudgetError::Closed);
    }
    let exhausted = |limits: crate::inference_budget_contract::Limits,
                     meters: &crate::inference_budget_contract::ledger::Meters|
     -> Result<bool, BudgetError> {
        let used = meters.total()?;
        Ok(limits
            .tokens
            .filter(|limit| *limit > 0)
            .is_some_and(|limit| used.tokens >= limit)
            || limits
                .usd_micros
                .filter(|limit| *limit > 0)
                .is_some_and(|limit| used.usd_micros >= limit))
    };
    if exhausted(ledger.limits, &ledger.meters)? {
        return Err(BudgetError::Exhausted);
    }
    for uid in ledger.ancestors(task_uid)? {
        let node = &ledger.nodes[&uid];
        if !node.active {
            return Err(BudgetError::Closed);
        }
        if exhausted(node.authority.limits, &node.meters)? {
            return Err(BudgetError::Exhausted);
        }
    }
    if ledger.sessions.len() >= MAX_SESSIONS {
        return Err(BudgetError::Capacity);
    }
    Ok(())
}

pub async fn admit_new(client: &Client, task: &KarsTask) -> Result<(), StoreError> {
    let Some(binding) = task
        .status
        .as_ref()
        .and_then(|status| status.inference_budget.as_ref())
    else {
        return Ok(());
    };
    let account = Store::new(client.clone(), &binding.account.namespace)
        .read(&binding.root, &binding.account.uid)
        .await?;
    let ledger = account
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or(StoreError::Missing)?;
    capacity(ledger, &binding.task_uid)?;
    Ok(())
}

pub fn mark_pending(status: &mut KarsTaskStatus, task: &KarsTask, detail: &str) {
    let conditions = status.conditions.get_or_insert_with(Vec::new);
    let prior = task
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .and_then(|conditions| conditions::find(conditions, CONDITION));
    conditions::set(
        conditions,
        conditions::preserve_transition_time(
            prior,
            CONDITION,
            "False",
            PENDING,
            detail,
            task.metadata.generation,
        ),
    );
}

/// Only the explicit budget-admission wait marker can retain a child's
/// execution. A changed UID/spec or failed attenuation remains revocation.
pub fn pending_parent(parent: &KarsTask, child: &KarsTask) -> bool {
    let Some(status) = &parent.status else {
        return false;
    };
    let Some(binding) = &status.inference_budget else {
        return false;
    };
    parent.metadata.deletion_timestamp.is_none()
        && status.observed_generation == parent.metadata.generation
        && parent.uid().as_deref() == Some(binding.task_uid.as_str())
        && parent.envelope_digest() == binding.authorization_digest
        && crate::kars_task::spec_attenuation_violations(&child.spec, &parent.spec).is_empty()
        && child
            .status
            .as_ref()
            .and_then(|status| status.inference_budget.as_ref())
            .is_none_or(|binding| binding.parent_task_uid.as_ref() == parent.metadata.uid.as_ref())
        && status.conditions.iter().flatten().any(|condition| {
            condition.type_ == CONDITION
                && condition.status == "False"
                && condition.reason == PENDING
        })
}

pub fn retain_execution(task: &KarsTask, status: &mut KarsTaskStatus) {
    let prior = task.status.as_ref();
    status.sandbox_ref = prior.and_then(|status| status.sandbox_ref.clone());
    status.execution_phase = prior
        .and_then(|status| status.execution_phase.clone())
        .or_else(|| Some("Pending".into()));
    status.execution_detail = Some(
        "New budget admission is unavailable; existing owned execution and funded work are retained, not re-authorized".into()
    );
}
