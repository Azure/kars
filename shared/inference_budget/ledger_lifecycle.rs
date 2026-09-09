// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

impl Ledger {
    /// An interrupted initial registration has no session or reservation for
    /// this UID. Ledger validation proves that a missing node cannot carry
    /// outstanding attempts; do not strand a deleting, never-enrolled Task.
    pub fn close_registered_task(&self, task_uid: &str) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        if !valid_uid(task_uid) {
            return Err(BudgetError::Identity);
        }
        if self.nodes.contains_key(task_uid) {
            return self.close_subtree(task_uid);
        }
        self.mutation(self.clone(), ())
    }

    /// Controller-verified relaunch of the same Task UID keeps all spending.
    /// Closed Pod sessions remain closed; a new runtime Pod UID must enroll.
    pub fn resume_task(&self, task_uid: &str, digest: &str) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        self.accepting()?;
        let node = self.nodes.get(task_uid).ok_or(BudgetError::Authorization)?;
        if node.authority.authorization_digest != digest
            || self
                .ancestors(task_uid)?
                .iter()
                .skip(1)
                .any(|uid| !self.nodes[uid].active)
        {
            return Err(BudgetError::Authorization);
        }
        let mut next = self.clone();
        next.nodes
            .get_mut(task_uid)
            .ok_or(BudgetError::Corrupt)?
            .active = true;
        self.mutation(next, ())
    }

    pub fn close_session(&self, pod_uid: &str) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        if !self.sessions.contains_key(pod_uid) {
            return Err(BudgetError::Identity);
        }
        let mut next = self.clone();
        next.sessions
            .get_mut(pod_uid)
            .ok_or(BudgetError::Corrupt)?
            .closed = true;
        let attempts: Vec<_> = next
            .attempts
            .values()
            .filter(|attempt| attempt.key.pod_uid == pod_uid && !attempt.phase.terminal())
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

    /// A stalled accepted attempt is permanently charged at its full maximum.
    /// Its contract remains as a bounded tombstone so late over-bound evidence
    /// can still freeze the account; capacity pressure never erases uncertainty.
    pub fn commit_uncertain_before(&self, cutoff: i64) -> Result<Mutation<usize>, BudgetError> {
        self.validate()?;
        let attempts: Vec<_> = self
            .attempts
            .values()
            .filter(|attempt| {
                attempt.phase == AttemptPhase::InFlight && attempt.expires_at <= cutoff
            })
            .map(|attempt| (attempt.key.clone(), attempt.quote.maximum))
            .collect();
        let mut next = self.clone();
        for (key, maximum) in &attempts {
            next.charge(key, AttemptPhase::Uncertain, *maximum)?;
        }
        next.compact()?;
        self.mutation(next, attempts.len())
    }

    /// Account grant limits are immutable; the authoritative effective ledger
    /// ceiling may narrow without resetting reservations or previous charges.
    pub fn narrow_root_limits(&self, limits: Limits) -> Result<Mutation<()>, BudgetError> {
        self.validate()?;
        self.accepting()?;
        if !limits.attenuates(self.limits) || !limits.allows(self.meters.total()?) {
            return Err(BudgetError::Authorization);
        }
        if limits.normalized().usd_micros.is_some()
            && (self.meters.unpriced_attempts > 0
                || self
                    .attempts
                    .values()
                    .any(|attempt| !attempt.phase.terminal() && !attempt.quote.price_covered))
        {
            return Err(BudgetError::Contract);
        }
        let mut next = self.clone();
        next.limits = limits.normalized();
        self.mutation(next, ())
    }
}
