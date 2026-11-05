// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

pub const MAX_TOKEN_TOP_UPS: usize = 64;

/// Operator authorization, separate from the immutable original funding and
/// task snapshots. Receipt IDs are retained for the entire account lifetime.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenTopUp {
    pub id: String,
    pub config_uid: String,
    pub account_uid: String,
    pub root: RootIdentity,
    pub original_limits: Limits,
    pub tokens: u64,
    pub expires_at: String,
    /// Explicit top-level nodes only, pinned to their original authorization.
    pub nodes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding_renewal: Option<FundingRenewal>,
}

/// A separately authorized renewal covers the original built-in grant and
/// exactly the previously applied top-ups, without replacing their authority.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FundingRenewal {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub registration_uid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intent_uid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intent_digest: String,
    pub original_expires_at: String,
    pub previous_top_up_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_reactivation: Option<BridgeReactivation>,
}

/// Explicit, one-use consent to reactivate this exact expired Closed ledger.
/// The controller independently authenticates the still-configured source.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeReactivation {
    pub closed_ledger_digest: String,
}

impl FundingRenewal {
    fn valid_actor(&self, root: &RootIdentity) -> bool {
        if root.kind != RootKind::KarsSandbox {
            return false;
        }
        match &self.bridge_reactivation {
            Some(reactivation) => {
                root.resource.name == "bridge-orchestrator"
                    && self.registration_uid.is_empty()
                    && self.intent_uid.is_empty()
                    && self.intent_digest.is_empty()
                    && valid_digest(&reactivation.closed_ledger_digest)
            }
            None => {
                root.resource.name == "sre"
                    && valid_uid(&self.registration_uid)
                    && valid_uid(&self.intent_uid)
                    && self.intent_digest.len() == 64
                    && self
                        .intent_digest
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            }
        }
    }
}

impl TokenTopUp {
    pub fn validate(&self, now: i64) -> Result<(), BudgetError> {
        self.root.validate()?;
        if !valid_uid(&self.id)
            || !valid_uid(&self.config_uid)
            || !valid_uid(&self.account_uid)
            || !matches!(self.root.kind, RootKind::KarsTeam | RootKind::KarsSandbox)
            || (self.root.kind == RootKind::KarsSandbox
                && !matches!(
                    self.root.resource.name.as_str(),
                    "sre" | "bridge-orchestrator"
                ))
            || self.original_limits != self.original_limits.normalized()
            || self.original_limits.tokens.is_none()
            || self.tokens == 0
            || self.tokens > MAX_LEDGER_INTEGER
            || self.nodes.is_empty()
            || self.nodes.len() > MAX_NODES
            || self
                .nodes
                .iter()
                .any(|(uid, digest)| !valid_uid(uid) || !valid_digest(digest))
        {
            return Err(BudgetError::Authorization);
        }
        let expiry = chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map_err(|_| BudgetError::Authorization)?;
        if let Some(renewal) = &self.funding_renewal {
            let original = chrono::DateTime::parse_from_rfc3339(&renewal.original_expires_at)
                .map_err(|_| BudgetError::Authorization)?;
            if !renewal.valid_actor(&self.root)
                || renewal.previous_top_up_ids.len() >= MAX_TOKEN_TOP_UPS
                || renewal
                    .previous_top_up_ids
                    .iter()
                    .any(|id| !valid_uid(id) || id == &self.id)
                || renewal
                    .previous_top_up_ids
                    .windows(2)
                    .any(|ids| ids[0] >= ids[1])
                || expiry <= original
            {
                return Err(BudgetError::Authorization);
            }
        }
        if expiry.timestamp() <= now {
            return Err(BudgetError::Expired);
        }
        Ok(())
    }
}

fn add_tokens(mut limits: Limits, tokens: u64) -> Result<Limits, BudgetError> {
    let original = limits.tokens.ok_or(BudgetError::Authorization)?;
    limits.tokens = Some(
        Amounts {
            tokens: original,
            usd_micros: 0,
        }
        .checked_add(Amounts {
            tokens,
            usd_micros: 0,
        })?
        .tokens,
    );
    Ok(limits)
}

impl Ledger {
    pub fn latest_funding_renewal(&self) -> Result<Option<&TokenTopUp>, BudgetError> {
        let renewals: Vec<_> = self
            .token_top_ups
            .values()
            .filter(|grant| grant.funding_renewal.is_some())
            .collect();
        let Some(latest) = renewals
            .iter()
            .max_by_key(|grant| {
                grant
                    .funding_renewal
                    .as_ref()
                    .unwrap()
                    .previous_top_up_ids
                    .len()
            })
            .copied()
        else {
            return Ok(None);
        };
        let node = self
            .nodes
            .get(&self.root.resource.uid)
            .ok_or(BudgetError::Authorization)?;
        let original: BuiltinFunding =
            serde_json::from_value(node.authority.effective_authorization.clone())
                .map_err(|_| BudgetError::Authorization)?;
        original.validate(i64::MIN)?;
        for grant in &renewals {
            let renewal = grant.funding_renewal.as_ref().unwrap();
            if renewal.original_expires_at != original.expires_at
                || original.root != self.root
                || original.limits.normalized() != self.limits
                || grant.nodes
                    != BTreeMap::from([(
                        self.root.resource.uid.clone(),
                        node.authority.authorization_digest.clone(),
                    )])
                || (*grant != latest
                    && !latest
                        .funding_renewal
                        .as_ref()
                        .unwrap()
                        .previous_top_up_ids
                        .contains(&grant.id))
            {
                return Err(BudgetError::Authorization);
            }
            for id in &renewal.previous_top_up_ids {
                let prior = self
                    .token_top_ups
                    .get(id)
                    .ok_or(BudgetError::Authorization)?;
                if let Some(previous) = &prior.funding_renewal
                    && (previous.registration_uid != renewal.registration_uid
                        || previous.intent_uid != renewal.intent_uid
                        || previous.intent_digest != renewal.intent_digest
                        || previous
                            .previous_top_up_ids
                            .iter()
                            .any(|id| !renewal.previous_top_up_ids.contains(id))
                        || chrono::DateTime::parse_from_rfc3339(&prior.expires_at)
                            .map_err(|_| BudgetError::Authorization)?
                            >= chrono::DateTime::parse_from_rfc3339(&grant.expires_at)
                                .map_err(|_| BudgetError::Authorization)?)
                {
                    return Err(BudgetError::Authorization);
                }
            }
        }
        Ok(Some(latest))
    }

    pub fn validate_funding_expiry(
        &self,
        original: &BuiltinFunding,
        now: i64,
    ) -> Result<(), BudgetError> {
        original.validate(i64::MIN)?;
        if self.unbounded_tokens.is_some() {
            return self.validate_unbounded_tokens();
        }
        if let Some(renewal) = self.latest_funding_renewal()? {
            renewal.validate(now)
        } else {
            original.validate(now)
        }
    }

    pub fn validate_top_up_expiry(&self, grant: &TokenTopUp, now: i64) -> Result<(), BudgetError> {
        if self.unbounded_tokens.as_ref().is_some_and(|allowance| {
            allowance.previous_token_top_up_ids.contains(&grant.id)
                && self.token_top_ups.get(&grant.id) == Some(grant)
        }) {
            return grant.validate(i64::MIN);
        }
        if let Some(latest) = self.latest_funding_renewal()? {
            latest.validate(now)?;
            if latest
                .funding_renewal
                .as_ref()
                .unwrap()
                .previous_top_up_ids
                .contains(&grant.id)
            {
                return grant.validate(i64::MIN);
            }
        }
        grant.validate(now)
    }

    pub fn effective_limits(&self) -> Result<Limits, BudgetError> {
        self.token_top_ups
            .values()
            .try_fold(self.limits, |limits, grant| {
                add_tokens(limits, grant.tokens)
            })
            .map(|limits| self.token_allowance_limits(limits))
    }

    pub fn effective_node_limits(&self, uid: &str) -> Result<Limits, BudgetError> {
        let node = self.nodes.get(uid).ok_or(BudgetError::Authorization)?;
        self.token_top_ups
            .values()
            .try_fold(node.authority.limits, |limits, grant| {
                if node.authority.parent_uid.is_none()
                    && node.authority.limits.tokens == self.limits.tokens
                    && grant.nodes.get(uid) == Some(&node.authority.authorization_digest)
                {
                    add_tokens(limits, grant.tokens)
                } else {
                    Ok(limits)
                }
            })
            .map(|limits| self.token_allowance_limits(limits))
    }

    pub(super) fn validate_top_ups(&self) -> Result<(), BudgetError> {
        if self.token_top_ups.len() > MAX_TOKEN_TOP_UPS {
            return Err(BudgetError::Capacity);
        }
        for (id, grant) in &self.token_top_ups {
            // Expired/revoked receipts remain liability evidence. Live authority
            // is independently re-read before new reservations and dispatch.
            grant.validate(i64::MIN)?;
            if id != &grant.id
                || grant.account_uid != self.account_uid
                || grant.root != self.root
                || grant.original_limits != self.limits
                || grant.nodes.keys().any(|uid| !self.nodes.contains_key(uid))
            {
                return Err(BudgetError::Corrupt);
            }
        }
        self.effective_limits()?;
        self.latest_funding_renewal()?;
        Ok(())
    }

    /// SHA-256 of compact, recursively key-sorted serialized ledger JSON. The
    /// complete state, including historical receipts and replay fences, is bound.
    pub fn funding_renewal_state_digest(&self) -> Result<String, BudgetError> {
        use sha2::{Digest, Sha256};
        let mut value = serde_json::to_value(self).map_err(|_| BudgetError::Corrupt)?;
        value.sort_all_objects();
        let bytes = serde_json::to_vec(&value).map_err(|_| BudgetError::Corrupt)?;
        Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
    }

    pub(super) fn verify_bridge_reactivation(
        &self,
        pin: &BridgeReactivation,
        now: i64,
    ) -> Result<(), BudgetError> {
        if self.root.kind != RootKind::KarsSandbox
            || self.root.resource.name != "bridge-orchestrator"
            || self.phase != AccountPhase::Closed
            || self.nodes.len() != 1
            || self.nodes.values().any(|node| node.active)
            || self.sessions.values().any(|session| !session.closed)
            || self.meters.reserved != Amounts::default()
            || self.attempts.values().any(|attempt| {
                matches!(
                    attempt.phase,
                    AttemptPhase::Reserved | AttemptPhase::InFlight
                ) || attempt.observed_breach.is_some()
            })
            || !matches!(self.funding_at(now), Err(BudgetError::Expired))
            || self.funding_renewal_state_digest()? != pin.closed_ledger_digest
        {
            return Err(BudgetError::Authorization);
        }
        Ok(())
    }

    /// The caller authenticates the operator ConfigMap, then CAS-persistently
    /// applies this transition. This is not a workload-provided grant API.
    pub fn apply_token_top_up(
        &self,
        grant: &TokenTopUp,
        now: i64,
    ) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        let reactivation = grant
            .funding_renewal
            .as_ref()
            .and_then(|renewal| renewal.bridge_reactivation.as_ref())
            .filter(|_| !self.token_top_ups.contains_key(&grant.id));
        if let Some(pin) = reactivation {
            self.verify_bridge_reactivation(pin, now)?;
        } else {
            self.accepting()?;
        }
        if grant.root != self.root
            || grant.account_uid != self.account_uid
            || grant.original_limits != self.limits
        {
            return Err(BudgetError::Authorization);
        }
        if let Some(existing) = self.token_top_ups.get(&grant.id) {
            if existing != grant {
                return Err(BudgetError::Authorization);
            }
            self.funding_at(now)?;
            self.validate_top_up_expiry(grant, now)?;
            return self.mutation(self.clone(), ());
        }
        if self.unbounded_tokens.is_some() {
            return Err(BudgetError::Authorization);
        }
        grant.validate(now)?;
        if let Some(renewal) = &grant.funding_renewal {
            if renewal.previous_top_up_ids != self.token_top_ups.keys().cloned().collect::<Vec<_>>()
            {
                return Err(BudgetError::Authorization);
            }
        } else {
            self.funding_at(now)?;
        }
        if self.token_top_ups.len() >= MAX_TOKEN_TOP_UPS {
            return Err(BudgetError::Capacity);
        }
        for (uid, digest) in &grant.nodes {
            let node = self.nodes.get(uid).ok_or(BudgetError::Authorization)?;
            if (reactivation.is_none() && !node.active)
                || node.authority.parent_uid.is_some()
                || node.authority.root_task_uid != *uid
                || node.authority.authorization_digest != *digest
                || node.authority.limits.tokens != self.limits.tokens
            {
                return Err(BudgetError::Authorization);
            }
        }
        let mut next = self.clone();
        if reactivation.is_some() {
            next.phase = AccountPhase::Active;
            next.nodes
                .get_mut(&self.root.resource.uid)
                .ok_or(BudgetError::Authorization)?
                .active = true;
        }
        next.token_top_ups.insert(grant.id.clone(), grant.clone());
        self.mutation(next, ())
    }
}
