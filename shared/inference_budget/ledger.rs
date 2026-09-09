// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Single-account transitions. A caller must persist `next` with the account
//! UID/resourceVersion CAS before returning a grant or performing a send.

use super::{tariffs::Quote, types::*};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[path = "ledger_lifecycle.rs"]
mod lifecycle;
#[path = "ledger_validation.rs"]
mod validation;

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Meters {
    pub reserved: Amounts,
    pub settled: Amounts,
    pub uncertain: Amounts,
    pub unpriced_attempts: u64,
}

impl Meters {
    pub fn total(&self) -> Result<Amounts, BudgetError> {
        self.reserved
            .checked_add(self.settled)?
            .checked_add(self.uncertain)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Node {
    pub authority: TaskAuthority,
    pub active: bool,
    pub meters: Meters,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Session {
    pub identity: ExecutionIdentity,
    pub closed: bool,
    pub next_sequence: u64,
    /// Terminal attempts through this sequence are never reissued, even after
    /// their detailed rows have been compacted.
    pub closed_through: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attempt {
    pub key: AttemptKey,
    pub identity: ExecutionIdentity,
    pub wire_digest: String,
    pub quote: Quote,
    pub phase: AttemptPhase,
    pub expires_at: i64,
    pub charged: Amounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Decimal JSON text preserves even absurd over-bound provider integers
    /// without coercing them through Kubernetes' signed integer representation.
    pub observed_breach: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ledger {
    pub version: String,
    pub scope: BudgetScope,
    pub account_uid: String,
    pub root: RootIdentity,
    pub limits: Limits,
    pub phase: AccountPhase,
    pub meters: Meters,
    pub nodes: BTreeMap<String, Node>,
    /// Retain closed session identities and high-water marks. Capacity is
    /// explicit: never discard a replay fence to make room for new work.
    pub sessions: BTreeMap<String, Session>,
    pub attempts: BTreeMap<String, Attempt>,
}

#[derive(Clone, Debug)]
pub struct Mutation<T> {
    pub next: Ledger,
    pub value: T,
    pub changed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Reservation {
    pub key: AttemptKey,
    pub maximum: Amounts,
    pub expires_at: i64,
    pub phase: AttemptPhase,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettlementResult {
    pub finalized: bool,
    pub uncertain: bool,
    pub breach: bool,
    /// None means the idempotent terminal row was already compacted. It is not
    /// a zero charge, and never authorizes another refund.
    pub charged: Option<Amounts>,
}

impl Ledger {
    pub fn new(
        account_uid: String,
        root: RootIdentity,
        limits: Limits,
    ) -> Result<Self, BudgetError> {
        root.validate()?;
        if !valid_uid(&account_uid) {
            return Err(BudgetError::Identity);
        }
        Ok(Self {
            version: CONTRACT_VERSION.into(),
            scope: BudgetScope::GovernedInference,
            account_uid,
            root,
            limits: limits.normalized(),
            phase: AccountPhase::Active,
            meters: Meters::default(),
            nodes: BTreeMap::new(),
            sessions: BTreeMap::new(),
            attempts: BTreeMap::new(),
        })
    }

    fn mutation<T>(&self, next: Self, value: T) -> Result<Mutation<T>, BudgetError> {
        next.validate()?;
        let changed = *self != next;
        Ok(Mutation {
            next,
            value,
            changed,
        })
    }

    fn accepting(&self) -> Result<(), BudgetError> {
        if self.phase != AccountPhase::Active {
            return Err(BudgetError::Closed);
        }
        Ok(())
    }

    pub fn ancestors(&self, task_uid: &str) -> Result<Vec<String>, BudgetError> {
        let mut path = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = Some(task_uid);
        while let Some(uid) = current {
            if path.len() >= MAX_NODES || !seen.insert(uid) {
                return Err(BudgetError::Corrupt);
            }
            let node = self.nodes.get(uid).ok_or(BudgetError::Authorization)?;
            path.push(uid.to_string());
            current = node.authority.parent_uid.as_deref();
        }
        Ok(path)
    }

    fn authorized(&self, identity: &ExecutionIdentity) -> Result<Vec<String>, BudgetError> {
        identity.validate()?;
        let path = self.ancestors(&identity.task_uid)?;
        let node = self
            .nodes
            .get(&identity.task_uid)
            .ok_or(BudgetError::Authorization)?;
        if identity.authorization_digest != node.authority.authorization_digest
            || identity.sandbox.namespace != self.root.resource.namespace
            || path.iter().any(|uid| !self.nodes[uid].active)
        {
            return Err(BudgetError::Authorization);
        }
        Ok(path)
    }

    pub fn requires_price(&self, task_uid: &str) -> Result<bool, BudgetError> {
        Ok(self.limits.normalized().usd_micros.is_some()
            || self.ancestors(task_uid)?.iter().any(|uid| {
                self.nodes[uid]
                    .authority
                    .limits
                    .normalized()
                    .usd_micros
                    .is_some()
            }))
    }

    pub fn requires_enforcement(&self, task_uid: &str) -> Result<bool, BudgetError> {
        Ok(self.limits.finite()
            || self
                .ancestors(task_uid)?
                .iter()
                .any(|uid| self.nodes[uid].authority.limits.finite()))
    }

    pub fn register_task(&self, mut authority: TaskAuthority) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        self.accepting()?;
        authority.task.validate()?;
        authority.limits = authority.limits.normalized();
        if authority.task.namespace != self.root.resource.namespace
            || !valid_uid(&authority.root_task_uid)
            || !valid_digest(&authority.authorization_digest)
            || !authority.effective_authorization.is_object()
            || serde_json::to_vec(&authority.effective_authorization)
                .map_err(|_| BudgetError::Corrupt)?
                .len()
                > 65_536
        {
            return Err(BudgetError::Authorization);
        }
        if let Some(existing) = self.nodes.get(&authority.task.uid) {
            if existing.authority != authority || !existing.active {
                return Err(BudgetError::Authorization);
            }
            return self.mutation(self.clone(), ());
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(BudgetError::Capacity);
        }
        if let Some(parent_uid) = &authority.parent_uid {
            let parent = self
                .nodes
                .get(parent_uid)
                .ok_or(BudgetError::Authorization)?;
            if !parent.active
                || parent.authority.root_task_uid != authority.root_task_uid
                || !authority.limits.attenuates(parent.authority.limits)
            {
                return Err(BudgetError::Authorization);
            }
        } else if authority.root_task_uid != authority.task.uid
            || !authority.limits.attenuates(self.limits)
            || (self.root.kind == RootKind::KarsTask && authority.task != self.root.resource)
        {
            return Err(BudgetError::Authorization);
        }
        let mut next = self.clone();
        next.nodes.insert(
            authority.task.uid.clone(),
            Node {
                authority,
                active: true,
                meters: Meters::default(),
            },
        );
        self.mutation(next, ())
    }

    /// Controller-only attenuation. Immutable UID ancestry and prior charges
    /// survive a new full authorization snapshot; old router identities cannot
    /// reserve/begin under the new digest.
    pub fn update_authority(&self, authority: TaskAuthority) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        self.accepting()?;
        let old = self
            .nodes
            .get(&authority.task.uid)
            .ok_or(BudgetError::Authorization)?;
        if authority.task != old.authority.task
            || authority.parent_uid != old.authority.parent_uid
            || authority.root_task_uid != old.authority.root_task_uid
            || !authority.limits.attenuates(old.authority.limits)
            || !authority.limits.allows(old.meters.total()?)
            || !valid_digest(&authority.authorization_digest)
            || !authority.effective_authorization.is_object()
        {
            return Err(BudgetError::Authorization);
        }
        if authority.limits.normalized().usd_micros.is_some()
            && (old.meters.unpriced_attempts > 0
                || self.attempts.values().any(|attempt| {
                    !attempt.phase.terminal()
                        && !attempt.quote.price_covered
                        && self
                            .ancestors(&attempt.identity.task_uid)
                            .is_ok_and(|path| path.contains(&authority.task.uid))
                }))
        {
            return Err(BudgetError::Contract);
        }
        let mut next = self.clone();
        let node = next
            .nodes
            .get_mut(&authority.task.uid)
            .ok_or(BudgetError::Corrupt)?;
        node.authority = authority;
        self.mutation(next, ())
    }

    pub fn register_session(
        &self,
        identity: ExecutionIdentity,
    ) -> Result<Mutation<Session>, BudgetError> {
        self.validate()?;
        self.accepting()?;
        self.authorized(&identity)?;
        if let Some(session) = self.sessions.get(&identity.pod_uid) {
            if session.identity != identity || session.closed {
                return Err(BudgetError::Identity);
            }
            return self.mutation(self.clone(), session.clone());
        }
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(BudgetError::Capacity);
        }
        let session = Session {
            identity: identity.clone(),
            closed: false,
            next_sequence: 1,
            closed_through: 0,
        };
        let mut next = self.clone();
        next.sessions
            .insert(identity.pod_uid.clone(), session.clone());
        self.mutation(next, session)
    }

    pub fn reserve(
        &self,
        request: &ReserveRequest,
        now: i64,
    ) -> Result<Mutation<Reservation>, BudgetError> {
        self.validate()?;
        self.accepting()?;
        if request.account_uid != self.account_uid || !valid_digest(&request.wire_digest) {
            return Err(BudgetError::Identity);
        }
        let path = self.authorized(&request.identity)?;
        let session = self
            .sessions
            .get(&request.identity.pod_uid)
            .ok_or(BudgetError::Identity)?;
        if session.identity != request.identity || session.closed {
            return Err(BudgetError::Identity);
        }
        if request.sequence <= session.closed_through {
            return Err(BudgetError::Sequence);
        }
        let key = AttemptKey {
            pod_uid: request.identity.pod_uid.clone(),
            sequence: request.sequence,
        };
        if let Some(existing) = self.attempts.get(&key.storage_key()) {
            if existing.identity != request.identity
                || existing.wire_digest != request.wire_digest
                || existing.quote != request.quote
            {
                return Err(BudgetError::Identity);
            }
            return self.mutation(
                self.clone(),
                Reservation {
                    key,
                    maximum: existing.quote.maximum,
                    expires_at: existing.expires_at,
                    phase: existing.phase,
                },
            );
        }
        if request.sequence != session.next_sequence {
            return Err(BudgetError::Sequence);
        }
        if self.attempts.len() >= MAX_ATTEMPTS
            || session
                .next_sequence
                .checked_sub(session.closed_through)
                .ok_or(BudgetError::Corrupt)?
                > MAX_REPLAY_WINDOW
        {
            return Err(BudgetError::Capacity);
        }
        request
            .quote
            .validate(now, self.requires_price(&request.identity.task_uid)?)?;
        let maximum = request.quote.maximum;
        if !self
            .limits
            .allows(self.meters.total()?.checked_add(maximum)?)
            || path.iter().any(|uid| {
                let node = &self.nodes[uid];
                node.meters
                    .total()
                    .and_then(|total| total.checked_add(maximum))
                    .map_or(true, |total| !node.authority.limits.allows(total))
            })
        {
            return Err(BudgetError::Exhausted);
        }
        let expires_at = now
            .checked_add(RESERVATION_TTL_SECONDS)
            .ok_or(BudgetError::Overflow)?
            .min(
                chrono::DateTime::parse_from_rfc3339(&request.quote.contract.valid_until)
                    .map_err(|_| BudgetError::Contract)?
                    .timestamp(),
            );
        let mut next = self.clone();
        next.meters.reserved = next.meters.reserved.checked_add(maximum)?;
        for uid in path {
            let node = next.nodes.get_mut(&uid).ok_or(BudgetError::Corrupt)?;
            node.meters.reserved = node.meters.reserved.checked_add(maximum)?;
        }
        next.sessions
            .get_mut(&key.pod_uid)
            .ok_or(BudgetError::Corrupt)?
            .next_sequence = request
            .sequence
            .checked_add(1)
            .ok_or(BudgetError::Overflow)?;
        next.attempts.insert(
            key.storage_key(),
            Attempt {
                key: key.clone(),
                identity: request.identity.clone(),
                wire_digest: request.wire_digest.clone(),
                quote: request.quote.clone(),
                phase: AttemptPhase::Reserved,
                expires_at,
                charged: Amounts::default(),
                observed_breach: None,
            },
        );
        self.mutation(
            next,
            Reservation {
                key,
                maximum,
                expires_at,
                phase: AttemptPhase::Reserved,
            },
        )
    }

    fn attempt(&self, command: &AttemptCommand) -> Result<&Attempt, BudgetError> {
        if command.account_uid != self.account_uid {
            return Err(BudgetError::Identity);
        }
        let attempt = self
            .attempts
            .get(&command.key.storage_key())
            .ok_or(BudgetError::Sequence)?;
        if attempt.key != command.key
            || attempt.identity != command.identity
            || attempt.wire_digest != command.wire_digest
        {
            return Err(BudgetError::Identity);
        }
        Ok(attempt)
    }

    pub fn begin_dispatch(
        &self,
        command: &AttemptCommand,
        now: i64,
    ) -> Result<Mutation<Reservation>, BudgetError> {
        self.validate()?;
        self.accepting()?;
        self.authorized(&command.identity)?;
        let session = self
            .sessions
            .get(&command.key.pod_uid)
            .ok_or(BudgetError::Identity)?;
        if session.closed || session.identity != command.identity {
            return Err(BudgetError::Identity);
        }
        let attempt = self.attempt(command)?;
        if attempt.phase != AttemptPhase::Reserved {
            return Err(BudgetError::AlreadyDispatched);
        }
        if now >= attempt.expires_at {
            return Err(BudgetError::Expired);
        }
        attempt
            .quote
            .validate(now, self.requires_price(&attempt.identity.task_uid)?)?;
        let response = Reservation {
            key: attempt.key.clone(),
            maximum: attempt.quote.maximum,
            expires_at: attempt.expires_at,
            phase: AttemptPhase::InFlight,
        };
        let mut next = self.clone();
        next.attempts
            .get_mut(&command.key.storage_key())
            .ok_or(BudgetError::Corrupt)?
            .phase = AttemptPhase::InFlight;
        self.mutation(next, response)
    }

    fn charge(
        &mut self,
        key: &AttemptKey,
        phase: AttemptPhase,
        charged: Amounts,
    ) -> Result<(), BudgetError> {
        let attempt = self
            .attempts
            .get(&key.storage_key())
            .ok_or(BudgetError::Corrupt)?
            .clone();
        if attempt.phase.terminal() {
            return Err(BudgetError::Corrupt);
        }
        let path = self.ancestors(&attempt.identity.task_uid)?;
        let apply = |meters: &mut Meters| -> Result<(), BudgetError> {
            meters.reserved = meters.reserved.subtract(attempt.quote.maximum)?;
            if phase == AttemptPhase::Uncertain {
                meters.uncertain = meters.uncertain.checked_add(charged)?;
            } else {
                meters.settled = meters.settled.checked_add(charged)?;
            }
            if phase != AttemptPhase::Expired && !attempt.quote.price_covered {
                meters.unpriced_attempts = meters
                    .unpriced_attempts
                    .checked_add(1)
                    .ok_or(BudgetError::Overflow)?;
            }
            Ok(())
        };
        apply(&mut self.meters)?;
        for uid in path {
            apply(&mut self.nodes.get_mut(&uid).ok_or(BudgetError::Corrupt)?.meters)?;
        }
        let attempt = self
            .attempts
            .get_mut(&key.storage_key())
            .ok_or(BudgetError::Corrupt)?;
        attempt.phase = phase;
        attempt.charged = charged;
        Ok(())
    }

    pub fn settle(
        &self,
        settlement: &Settlement,
    ) -> Result<Mutation<SettlementResult>, BudgetError> {
        self.validate()?;
        let command = &settlement.attempt;
        if command.account_uid != self.account_uid {
            return Err(BudgetError::Identity);
        }
        let session = self
            .sessions
            .get(&command.key.pod_uid)
            .ok_or(BudgetError::Identity)?;
        if session.identity != command.identity {
            return Err(BudgetError::Identity);
        }
        if command.key.sequence <= session.closed_through {
            return self.mutation(
                self.clone(),
                SettlementResult {
                    finalized: true,
                    uncertain: false,
                    breach: false,
                    charged: None,
                },
            );
        }
        let attempt = self.attempt(command)?;
        if attempt.phase.terminal() {
            if attempt.phase == AttemptPhase::Uncertain
                && let Some(usage) = &settlement.usage
                && attempt.quote.usage(usage).is_err()
            {
                let mut next = self.clone();
                next.phase = AccountPhase::Frozen;
                next.attempts
                    .get_mut(&command.key.storage_key())
                    .ok_or(BudgetError::Corrupt)?
                    .observed_breach =
                    Some(serde_json::to_string(usage).map_err(|_| BudgetError::Corrupt)?);
                return self.mutation(
                    next,
                    SettlementResult {
                        finalized: true,
                        uncertain: true,
                        breach: true,
                        charged: Some(attempt.charged),
                    },
                );
            }
            return self.mutation(
                self.clone(),
                SettlementResult {
                    finalized: true,
                    uncertain: attempt.phase == AttemptPhase::Uncertain,
                    breach: attempt.observed_breach.is_some(),
                    charged: Some(attempt.charged),
                },
            );
        }
        if attempt.phase != AttemptPhase::InFlight {
            return Err(BudgetError::AlreadyDispatched);
        }
        let (phase, charge, breach) = match &settlement.usage {
            None => (AttemptPhase::Uncertain, attempt.quote.maximum, false),
            Some(usage) => match attempt.quote.usage(usage) {
                Ok(amount) if amount.within(attempt.quote.maximum) => {
                    (AttemptPhase::Settled, amount, false)
                }
                _ => (AttemptPhase::Uncertain, attempt.quote.maximum, true),
            },
        };
        let mut next = self.clone();
        next.charge(&command.key, phase, charge)?;
        if breach {
            next.phase = AccountPhase::Frozen;
            next.attempts
                .get_mut(&command.key.storage_key())
                .ok_or(BudgetError::Corrupt)?
                .observed_breach = settlement
                .usage
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| BudgetError::Corrupt)?;
        }
        next.compact()?;
        self.mutation(
            next,
            SettlementResult {
                finalized: true,
                uncertain: phase == AttemptPhase::Uncertain,
                breach,
                charged: Some(charge),
            },
        )
    }

    /// Only Reserved attempts expire/refund. InFlight is potentially billed,
    /// including when BeginDispatch's acknowledgement was lost.
    pub fn expire_undispatched(&self, now: i64) -> Result<Mutation<usize>, BudgetError> {
        self.validate()?;
        let expired: Vec<_> = self
            .attempts
            .values()
            .filter(|attempt| attempt.phase == AttemptPhase::Reserved && now >= attempt.expires_at)
            .map(|attempt| attempt.key.clone())
            .collect();
        let mut next = self.clone();
        for key in &expired {
            next.charge(key, AttemptPhase::Expired, Amounts::default())?;
        }
        next.compact()?;
        self.mutation(next, expired.len())
    }

    /// Cancellation is an atomic subtree fence. Undispatched work is released;
    /// already authorized attempts are conservatively charged, never refunded.
    pub fn close_subtree(&self, task_uid: &str) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        if !self.nodes.contains_key(task_uid) {
            return Err(BudgetError::Identity);
        }
        let affected: BTreeSet<_> = self
            .nodes
            .keys()
            .filter(|uid| {
                self.ancestors(uid)
                    .is_ok_and(|path| path.iter().any(|parent| parent == task_uid))
            })
            .cloned()
            .collect();
        let mut next = self.clone();
        for uid in &affected {
            next.nodes.get_mut(uid).ok_or(BudgetError::Corrupt)?.active = false;
        }
        for session in next
            .sessions
            .values_mut()
            .filter(|session| affected.contains(&session.identity.task_uid))
        {
            session.closed = true;
        }
        let attempts: Vec<_> = next
            .attempts
            .values()
            .filter(|attempt| {
                affected.contains(&attempt.identity.task_uid) && !attempt.phase.terminal()
            })
            .map(|attempt| (attempt.key.clone(), attempt.phase, attempt.quote.maximum))
            .collect();
        for (key, phase, maximum) in attempts {
            if phase == AttemptPhase::Reserved {
                next.charge(&key, AttemptPhase::Expired, Amounts::default())?;
            } else {
                next.charge(&key, AttemptPhase::Uncertain, maximum)?;
            }
        }
        next.compact()?;
        self.mutation(next, ())
    }

    pub fn close_account(&self) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        let mut next = self.clone();
        next.phase = AccountPhase::Closing;
        let roots: Vec<_> = next
            .nodes
            .values()
            .filter(|node| node.authority.parent_uid.is_none())
            .map(|node| node.authority.task.uid.clone())
            .collect();
        for root in roots {
            next = next.close_subtree(&root)?.next;
        }
        next.phase = AccountPhase::Closed;
        self.mutation(next, ())
    }

    fn compact(&mut self) -> Result<(), BudgetError> {
        for (pod_uid, session) in &mut self.sessions {
            loop {
                let sequence = session
                    .closed_through
                    .checked_add(1)
                    .ok_or(BudgetError::Overflow)?;
                let key = AttemptKey {
                    pod_uid: pod_uid.clone(),
                    sequence,
                }
                .storage_key();
                let Some(attempt) = self.attempts.get(&key) else {
                    break;
                };
                if !attempt.phase.terminal()
                    || attempt.phase == AttemptPhase::Uncertain
                    || attempt.observed_breach.is_some()
                {
                    break;
                }
                self.attempts.remove(&key);
                session.closed_through = sequence;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
