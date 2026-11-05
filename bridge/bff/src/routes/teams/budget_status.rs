// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::inference_budget_contract::{
    AccountPhase, Amounts, Limits, TeamBudgetObservation, valid_name, valid_uid,
};
use crate::kars::team::KarsTeam;
use anyhow::{Result, ensure};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TeamBudgetStatusDto {
    Unavailable {
        reason: &'static str,
    },
    Available {
        observed_at: String,
        account_phase: AccountPhase,
        unbounded_tokens: bool,
        effective_tokens: Option<String>,
        effective_usd_micros: Option<String>,
        settled: BudgetAmountsDto,
        reserved: BudgetAmountsDto,
        uncertain: Box<BudgetAmountsDto>,
        price_coverage_complete: bool,
    },
}

#[derive(Debug, Serialize)]
pub struct BudgetAmountsDto {
    tokens: String,
    usd_micros: String,
}

impl From<Amounts> for BudgetAmountsDto {
    fn from(value: Amounts) -> Self {
        Self {
            tokens: value.tokens.to_string(),
            usd_micros: value.usd_micros.to_string(),
        }
    }
}

// Called only after require_owned_team. The controller already reads and verifies
// the exact account; consume its protected Team status without budget-account RBAC.
pub(super) fn read(team: &KarsTeam, now: chrono::DateTime<chrono::Utc>) -> TeamBudgetStatusDto {
    project(team, now).unwrap_or(TeamBudgetStatusDto::Unavailable {
        reason: "A fresh controller-verified lifetime account observation is unavailable.",
    })
}

fn project(team: &KarsTeam, now: chrono::DateTime<chrono::Utc>) -> Result<TeamBudgetStatusDto> {
    let status = team
        .status
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing status"))?;
    let reference = status
        .inference_budget_account
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing binding"))?;
    let observed_at = status
        .tokens_observed_at
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing observation time"))?;
    let age = now.signed_duration_since(chrono::DateTime::parse_from_rfc3339(observed_at)?);
    ensure!(
        team.metadata.deletion_timestamp.is_none()
            && team
                .metadata
                .generation
                .is_some_and(|generation| generation > 0)
            && status.observed_generation == team.metadata.generation
            && status.tokens_spent_scope.as_deref() == Some("GovernedInferenceLifetime")
            && (chrono::Duration::zero()..=chrono::Duration::seconds(300)).contains(&age),
        "Stale or unscoped observation"
    );
    let observation: TeamBudgetObservation = serde_json::from_value(
        status
            .inference_budget_observation
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Missing observation"))?,
    )?;
    observation.team.validate()?;
    ensure!(
        valid_name(&reference.namespace)
            && valid_name(&reference.name)
            && valid_uid(&reference.uid)
            && observation.account.namespace == reference.namespace
            && observation.account.name == reference.name
            && observation.account.uid == reference.uid
            && Some(&observation.team.uid) == team.metadata.uid.as_ref()
            && Some(&observation.team.name) == team.metadata.name.as_ref()
            && Some(&observation.team.namespace) == team.metadata.namespace.as_ref(),
        "Observation identity differs from Team binding"
    );
    let original = team
        .spec
        .envelope
        .budget
        .as_ref()
        .filter(|b| {
            matches!(
                b.scope,
                Some(crate::kars::task::BudgetScope::GovernedInference)
            )
        })
        .ok_or_else(|| anyhow::anyhow!("Team is not explicitly enrolled"))?;
    let original_limits = Limits {
        tokens: original.tokens.map(u64::try_from).transpose()?,
        usd_micros: original.usd_micros.map(u64::try_from).transpose()?,
    }
    .normalized();
    let limits = observation.effective_limits.normalized();
    ensure!(
        observation.original_limits == original_limits
            && limits == observation.effective_limits
            && limits.usd_micros == original_limits.usd_micros
            && (!observation.unbounded_tokens || limits.tokens.is_none())
            && (observation.unbounded_tokens
                || match original_limits.tokens {
                    Some(original) => limits.tokens.is_some_and(|effective| effective >= original),
                    None => limits.tokens.is_none(),
                })
            && status.tokens_spent_total == Some(i64::try_from(observation.settled.tokens)?),
        "Observation differs from original grant or lifetime settlement"
    );
    observation
        .settled
        .checked_add(observation.reserved)?
        .checked_add(observation.uncertain)?;
    Ok(TeamBudgetStatusDto::Available {
        observed_at: observed_at.clone(),
        account_phase: observation.phase,
        unbounded_tokens: observation.unbounded_tokens,
        effective_tokens: limits.tokens.map(|n| n.to_string()),
        effective_usd_micros: limits.usd_micros.map(|n| n.to_string()),
        settled: observation.settled.into(),
        reserved: observation.reserved.into(),
        uncertain: Box::new(observation.uncertain.into()),
        price_coverage_complete: observation.price_coverage_complete,
    })
}

#[cfg(test)]
#[path = "budget_status_tests.rs"]
mod tests;
