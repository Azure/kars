// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    binding,
    config::Settings,
    store::{Store, StoreError},
};
use crate::{
    inference_budget_contract::{AccountPhase, BudgetError, BudgetScope, RootKind, ledger::Ledger},
    kars_task::KarsTask,
    kars_team::KarsTeam,
};
use kube::{Api, Client, ResourceExt};

pub const LIFETIME_SCOPE: &str = "GovernedInferenceLifetime";
pub const SYNTHESIS_SCOPE: &str = "RetainedRootSynthesis";

pub fn governed(team: &KarsTeam) -> bool {
    team.spec
        .envelope
        .budget
        .as_ref()
        .is_some_and(|budget| budget.scope == Some(BudgetScope::GovernedInference))
        || team.status.as_ref().is_some_and(|status| {
            status.inference_budget_account.is_some()
                || status.tokens_spent_scope.as_deref() == Some(LIFETIME_SCOPE)
        })
}

/// Authenticated settlement is observable even when no further admission is possible.
/// Never sum node meters: ancestor meters already include their descendants.
pub struct Observation {
    pub settled_tokens: i64,
    pub budget: crate::inference_budget_contract::TeamBudgetObservation,
    pub admission: Result<(), StoreError>,
}

/// Cadence must still satisfy every admission check after authenticating the account.
pub async fn ready(client: &Client, team: &KarsTeam, principal: &str) -> Result<(), StoreError> {
    if !binding::has_finite(&team.spec.envelope) {
        return Ok(());
    }
    observe(client, team, principal).await?.admission
}

pub async fn observe(
    client: &Client,
    team: &KarsTeam,
    principal: &str,
) -> Result<Observation, StoreError> {
    let settings = Settings::from_env()?.ok_or(BudgetError::Contract)?;
    observe_with_settings(client, team, principal, &settings).await
}

async fn observe_with_settings(
    client: &Client,
    team: &KarsTeam,
    principal: &str,
    settings: &Settings,
) -> Result<Observation, StoreError> {
    settings
        .catalog(client, chrono::Utc::now().timestamp())
        .await?;
    read_observation(client, team, principal, settings).await
}

async fn read_observation(
    client: &Client,
    team: &KarsTeam,
    principal: &str,
    settings: &Settings,
) -> Result<Observation, StoreError> {
    let limits = binding::limits(&team.spec.envelope)?;
    let tasks: Api<KarsTask> = Api::namespaced(
        client.clone(),
        &team.namespace().ok_or(BudgetError::Identity)?,
    );
    let principal = tasks
        .get(principal)
        .await
        .map_err(|error| StoreError::Api {
            stage: "read finite Team principal",
            code: match error {
                kube::Error::Api(status) => Some(status.code),
                _ => None,
            },
        })?;
    let binding = principal
        .status
        .as_ref()
        .and_then(|status| status.inference_budget.as_ref())
        .ok_or(StoreError::Missing)?;
    if !crate::kars_task_reconciler::task_is_ready(&principal)
        || binding.root.kind != RootKind::KarsTeam
        || team.metadata.uid.as_deref() != Some(binding.root.resource.uid.as_str())
        || team.metadata.namespace.as_deref() != Some(binding.root.resource.namespace.as_str())
        || team.metadata.name.as_deref() != Some(binding.root.resource.name.as_str())
        || binding.account.namespace != settings.accounting_namespace
        || binding.account.name != super::account::name_for_root(&binding.root)
        || principal.metadata.uid.as_deref() != Some(binding.task_uid.as_str())
        || team
            .status
            .as_ref()
            .and_then(|status| status.inference_budget_account.as_ref())
            != Some(&binding.account)
    {
        return Err(BudgetError::Authorization.into());
    }
    let account = Store::new(client.clone(), &settings.accounting_namespace)
        .for_task(&binding.task_uid)
        .read(&binding.root, &binding.account.uid)
        .await?;
    let ledger = Store::ledger(&account)?;
    if ledger.limits != limits {
        return Err(BudgetError::Closed.into());
    }
    Ok(Observation {
        settled_tokens: i64::try_from(ledger.meters.settled.tokens)
            .map_err(|_| BudgetError::Overflow)?,
        budget: budget_observation(&binding.account, ledger)?,
        admission: super::topups::authorize_read(client, settings, ledger)
            .await
            .and_then(|()| admission(ledger)),
    })
}

fn budget_observation(
    account: &crate::inference_budget_contract::AccountReference,
    ledger: &Ledger,
) -> Result<crate::inference_budget_contract::TeamBudgetObservation, StoreError> {
    use crate::inference_budget_contract::{AttemptPhase, TeamBudgetObservation};
    let pending_unpriced = ledger.attempts.values().any(|attempt| {
        matches!(
            attempt.phase,
            AttemptPhase::Reserved | AttemptPhase::InFlight
        ) && !attempt.quote.price_covered()
    });
    Ok(TeamBudgetObservation {
        account: account.clone(),
        team: ledger.root.resource.clone(),
        original_limits: ledger.limits,
        effective_limits: ledger.effective_limits()?,
        phase: ledger.phase,
        unbounded_tokens: ledger.unbounded_tokens.is_some(),
        settled: ledger.meters.settled,
        reserved: ledger.meters.reserved,
        uncertain: ledger.meters.uncertain,
        price_coverage_complete: ledger.meters.unpriced_attempts == 0 && !pending_unpriced,
    })
}

fn admission(ledger: &Ledger) -> Result<(), StoreError> {
    if ledger.phase != AccountPhase::Active {
        return Err(BudgetError::Closed.into());
    }
    let limits = ledger.effective_limits()?;
    let used = ledger.meters.total()?;
    if limits.tokens.is_some_and(|limit| used.tokens >= limit)
        || limits
            .usd_micros
            .is_some_and(|limit| used.usd_micros >= limit)
    {
        return Err(BudgetError::Exhausted.into());
    }
    if ledger.nodes.len() >= crate::inference_budget_contract::MAX_NODES
        || ledger.sessions.len() >= crate::inference_budget_contract::MAX_SESSIONS
    {
        return Err(BudgetError::Capacity.into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "team_tests.rs"]
mod tests;
