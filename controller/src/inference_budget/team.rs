// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    binding,
    config::Settings,
    store::{Store, StoreError},
};
use crate::{
    inference_budget_contract::{AccountPhase, BudgetError, RootKind},
    kars_task::KarsTask,
    kars_team::KarsTeam,
};
use kube::{Api, Client, ResourceExt};

/// Cadence must wait for the principal's protected immutable account pin.
/// This checks accounting readiness, not completion of a business operation.
pub async fn ready(client: &Client, team: &KarsTeam, principal: &str) -> Result<(), StoreError> {
    if !binding::has_finite(&team.spec.envelope) {
        return Ok(());
    }
    let limits = binding::limits(&team.spec.envelope)?;
    let settings = Settings::from_env()?.ok_or(BudgetError::Contract)?;
    settings
        .catalog(client, chrono::Utc::now().timestamp())
        .await?;
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
    {
        return Err(BudgetError::Authorization.into());
    }
    let account = Store::new(client.clone(), &settings.accounting_namespace)
        .read(&binding.root, &binding.account.uid)
        .await?;
    let ledger = account
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or(StoreError::Missing)?;
    if ledger.phase != AccountPhase::Active || ledger.limits != limits {
        return Err(BudgetError::Closed.into());
    }
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
