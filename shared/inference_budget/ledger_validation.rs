// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn signed_integer_range(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .is_none_or(|number| number <= MAX_LEDGER_INTEGER),
        serde_json::Value::Array(values) => values.iter().all(signed_integer_range),
        serde_json::Value::Object(values) => values.values().all(signed_integer_range),
        _ => true,
    }
}

impl Ledger {
    pub fn validate(&self) -> Result<(), BudgetError> {
        self.root.validate()?;
        if self.version != CONTRACT_VERSION
            || !valid_uid(&self.account_uid)
            || self.nodes.len() > MAX_NODES
            || self.sessions.len() > MAX_SESSIONS
            || self.attempts.len() > MAX_ATTEMPTS
        {
            return Err(BudgetError::Corrupt);
        }
        let serialized = serde_json::to_value(self).map_err(|_| BudgetError::Corrupt)?;
        if !signed_integer_range(&serialized) {
            return Err(BudgetError::Overflow);
        }
        if serde_json::to_vec(self)
            .map_err(|_| BudgetError::Corrupt)?
            .len()
            > MAX_LEDGER_BYTES
        {
            return Err(BudgetError::Capacity);
        }
        if !self.limits.allows(self.meters.total()?)
            || (self.limits.normalized().usd_micros.is_some() && self.meters.unpriced_attempts > 0)
        {
            return Err(BudgetError::Corrupt);
        }
        let mut root_reserved = Amounts::default();
        let mut node_reserved = BTreeMap::<String, Amounts>::new();
        let mut children_used = BTreeMap::<String, Amounts>::new();
        let mut root_nodes_used = Amounts::default();
        for (uid, node) in &self.nodes {
            node.authority.task.validate()?;
            if uid != &node.authority.task.uid
                || node.authority.task.namespace != self.root.resource.namespace
                || !valid_digest(&node.authority.authorization_digest)
                || !node.authority.effective_authorization.is_object()
                || serde_json::to_vec(&node.authority.effective_authorization)
                    .map_err(|_| BudgetError::Corrupt)?
                    .len()
                    > 65_536
                || !node.authority.limits.allows(node.meters.total()?)
                || (node.authority.limits.normalized().usd_micros.is_some()
                    && node.meters.unpriced_attempts > 0)
            {
                return Err(BudgetError::Corrupt);
            }
            let path = self.ancestors(uid)?;
            let root_uid = path.last().ok_or(BudgetError::Corrupt)?;
            if *root_uid != node.authority.root_task_uid {
                return Err(BudgetError::Corrupt);
            }
            let used = node.meters.settled.checked_add(node.meters.uncertain)?;
            if let Some(parent) = &node.authority.parent_uid {
                let prior = children_used.get(parent).copied().unwrap_or_default();
                children_used.insert(parent.clone(), prior.checked_add(used)?);
            } else {
                if self.root.kind == RootKind::KarsTask && node.authority.task != self.root.resource
                {
                    return Err(BudgetError::Corrupt);
                }
                root_nodes_used = root_nodes_used.checked_add(used)?;
            }
        }
        if !root_nodes_used.within(self.meters.settled.checked_add(self.meters.uncertain)?) {
            return Err(BudgetError::Corrupt);
        }
        for (uid, used) in children_used {
            let node = self.nodes.get(&uid).ok_or(BudgetError::Corrupt)?;
            if !used.within(node.meters.settled.checked_add(node.meters.uncertain)?) {
                return Err(BudgetError::Corrupt);
            }
        }
        for (uid, session) in &self.sessions {
            session.identity.validate()?;
            if uid != &session.identity.pod_uid
                || session.next_sequence == 0
                || session.closed_through >= session.next_sequence
                || session.next_sequence - session.closed_through > MAX_REPLAY_WINDOW + 1
                || !self.nodes.contains_key(&session.identity.task_uid)
            {
                return Err(BudgetError::Corrupt);
            }
            for sequence in session.closed_through + 1..session.next_sequence {
                if !self.attempts.contains_key(
                    &AttemptKey {
                        pod_uid: uid.clone(),
                        sequence,
                    }
                    .storage_key(),
                ) {
                    return Err(BudgetError::Corrupt);
                }
            }
        }
        for (key, attempt) in &self.attempts {
            let session = self
                .sessions
                .get(&attempt.key.pod_uid)
                .ok_or(BudgetError::Corrupt)?;
            if key != &attempt.key.storage_key()
                || attempt.identity != session.identity
                || attempt.key.sequence <= session.closed_through
                || attempt.key.sequence >= session.next_sequence
                || !valid_digest(&attempt.wire_digest)
                || !attempt.charged.within(attempt.quote.maximum)
                || attempt.observed_breach.as_ref().is_some_and(|observation| {
                    serde_json::from_str::<Usage>(observation).is_err()
                        || self.phase == AccountPhase::Active
                })
            {
                return Err(BudgetError::Corrupt);
            }

            // Expired contracts remain accounting evidence; validate their
            // shape and arithmetic without making old charges disappear.
            attempt
                .quote
                .validate(i64::MIN, attempt.quote.price_covered)?;
            if !attempt.phase.terminal() {
                if attempt.charged != Amounts::default() {
                    return Err(BudgetError::Corrupt);
                }
                root_reserved = root_reserved.checked_add(attempt.quote.maximum)?;
                for uid in self.ancestors(&attempt.identity.task_uid)? {
                    let previous = node_reserved.get(&uid).copied().unwrap_or_default();
                    node_reserved.insert(uid, previous.checked_add(attempt.quote.maximum)?);
                }
            }
        }
        if self.meters.reserved != root_reserved {
            return Err(BudgetError::Corrupt);
        }
        for (uid, node) in &self.nodes {
            if node.meters.reserved != node_reserved.get(uid).copied().unwrap_or_default() {
                return Err(BudgetError::Corrupt);
            }
        }
        Ok(())
    }
}
