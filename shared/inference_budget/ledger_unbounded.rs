// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

/// Operator consent to meter tokens without a ceiling or funding deadline.
/// Original grants and every prior charge remain immutable evidence.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnboundedTokenAllowance {
    pub id: String,
    pub config_uid: String,
    pub account_uid: String,
    pub root: RootIdentity,
    pub original_limits: Limits,
    pub ledger_digest: String,
    pub previous_token_top_up_ids: Vec<String>,
}

impl UnboundedTokenAllowance {
    pub fn validate(&self) -> Result<(), BudgetError> {
        self.root.validate()?;
        if !valid_uid(&self.id)
            || !valid_uid(&self.config_uid)
            || !valid_uid(&self.account_uid)
            || !valid_digest(&self.ledger_digest)
            || self.original_limits != self.original_limits.normalized()
            || self
                .original_limits
                .tokens
                .is_some_and(|n| n > MAX_LEDGER_INTEGER)
            || self
                .original_limits
                .usd_micros
                .is_some_and(|n| n > MAX_LEDGER_INTEGER)
            || (self.root.kind == RootKind::KarsSandbox
                && !matches!(
                    self.root.resource.name.as_str(),
                    "sre" | "bridge-orchestrator"
                ))
            || self.previous_token_top_up_ids.len() > MAX_TOKEN_TOP_UPS
            || self
                .previous_token_top_up_ids
                .iter()
                .any(|id| !valid_uid(id))
            || self
                .previous_token_top_up_ids
                .windows(2)
                .any(|ids| ids[0] >= ids[1])
        {
            return Err(BudgetError::Authorization);
        }
        Ok(())
    }
}

impl Ledger {
    pub fn token_allowance_limits(&self, mut limits: Limits) -> Limits {
        if self.unbounded_tokens.is_some() {
            limits.tokens = None;
        }
        limits
    }

    pub(super) fn validate_unbounded_tokens(&self) -> Result<(), BudgetError> {
        if let Some(grant) = &self.unbounded_tokens {
            grant.validate()?;
            if grant.account_uid != self.account_uid
                || grant.root != self.root
                || grant.original_limits != self.limits
                || grant.previous_token_top_up_ids
                    != self.token_top_ups.keys().cloned().collect::<Vec<_>>()
            {
                return Err(BudgetError::Authorization);
            }
        }
        Ok(())
    }

    /// Called only after authenticating the operator catalog. The caller must
    /// persist the mutation with the existing account UID/resourceVersion CAS.
    pub fn authorize_unbounded_tokens(
        &self,
        grant: &UnboundedTokenAllowance,
        now: i64,
    ) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        grant.validate()?;
        if let Some(existing) = &self.unbounded_tokens {
            self.accepting()?;
            if existing != grant {
                return Err(BudgetError::Authorization);
            }
            return self.mutation(self.clone(), ());
        }
        if grant.account_uid != self.account_uid
            || grant.root != self.root
            || grant.original_limits != self.limits
            || grant.ledger_digest != self.funding_renewal_state_digest()?
            || grant.previous_token_top_up_ids
                != self.token_top_ups.keys().cloned().collect::<Vec<_>>()
            || self.nodes.is_empty()
        {
            return Err(BudgetError::Authorization);
        }
        let reactivate = self.phase == AccountPhase::Closed;
        if reactivate {
            self.verify_bridge_reactivation(
                &BridgeReactivation {
                    closed_ledger_digest: grant.ledger_digest.clone(),
                },
                now,
            )?;
        } else {
            self.accepting()?;
        }
        let mut next = self.clone();
        next.unbounded_tokens = Some(grant.clone());
        if reactivate {
            next.phase = AccountPhase::Active;
            next.nodes
                .get_mut(&self.root.resource.uid)
                .ok_or(BudgetError::Authorization)?
                .active = true;
        }
        self.mutation(next, ())
    }
}
