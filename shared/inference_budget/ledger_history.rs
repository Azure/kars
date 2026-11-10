// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

/// The immutable history tree is committed by the same account CAS as these
/// counters. It contains no open sessions or reserved/in-flight liability.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct History {
    pub root: String,
    pub nodes: u64,
    pub sessions: u64,
    pub unknown_usage_attempts: u64,
    pub node_unknown: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum HistoryRecord {
    Node {
        node: Node,
        unknown_usage_attempts: u64,
    },
    Session {
        session: Session,
        attempts: BTreeMap<String, Attempt>,
    },
}

impl HistoryRecord {
    pub fn key(&self) -> String {
        match self {
            Self::Node { node, .. } => Self::node_key(&node.authority.task.uid),
            Self::Session { session, .. } => Self::session_key(&session.identity.pod_uid),
        }
    }

    pub fn node_key(uid: &str) -> String {
        format!("node:{uid}")
    }

    pub fn session_key(uid: &str) -> String {
        format!("session:{uid}")
    }

    pub fn unknown(&self) -> u64 {
        match self {
            Self::Node { .. } => 0,
            Self::Session { attempts, .. } => attempts
                .values()
                .filter(|attempt| {
                    attempt.quote.is_record_only() && attempt.phase == AttemptPhase::Uncertain
                })
                .count() as u64,
        }
    }
}

impl Ledger {
    /// Prepare at most one closed session and one inactive leaf for archival.
    /// The caller must write the immutable records and replace history.root
    /// before validating/persisting this intermediate state. Never return it as
    /// a committed ledger or use it to authorize dispatch.
    pub fn archive_closed(
        &mut self,
        protected: &BTreeSet<String>,
    ) -> Result<Vec<HistoryRecord>, BudgetError> {
        let mut records = Vec::new();
        let session_uid = self
            .sessions
            .iter()
            .find(|(uid, session)| {
                session.closed && !protected.contains(&HistoryRecord::session_key(uid))
            })
            .map(|(uid, _)| uid.clone());
        if let Some(uid) = session_uid {
            let session = self.sessions.get(&uid).ok_or(BudgetError::Corrupt)?.clone();
            let path = self.ancestors(&session.identity.task_uid)?;
            let attempts: BTreeMap<_, _> = self
                .attempts
                .iter()
                .filter(|(_, attempt)| attempt.key.pod_uid == uid)
                .map(|(key, attempt)| (key.clone(), attempt.clone()))
                .collect();
            if attempts.values().any(|attempt| !attempt.phase.terminal()) {
                return Err(BudgetError::Corrupt);
            }
            let record = HistoryRecord::Session { session, attempts };
            let unknown = record.unknown();
            let history = self.history.get_or_insert_with(History::default);
            history.sessions = history
                .sessions
                .checked_add(1)
                .ok_or(BudgetError::Overflow)?;
            history.unknown_usage_attempts = history
                .unknown_usage_attempts
                .checked_add(unknown)
                .ok_or(BudgetError::Overflow)?;
            if unknown > 0 {
                for ancestor in path {
                    let count = history.node_unknown.entry(ancestor).or_default();
                    *count = count.checked_add(unknown).ok_or(BudgetError::Overflow)?;
                }
            }
            self.attempts
                .retain(|_, attempt| attempt.key.pod_uid != uid);
            self.sessions.remove(&uid);
            records.push(record);
        }
        let node_uid = self
            .nodes
            .iter()
            .find(|(uid, node)| {
                !node.active
                    && !protected.contains(&HistoryRecord::node_key(uid))
                    && !(self.root.kind != RootKind::KarsTeam && **uid == self.root.resource.uid)
                    && !self
                        .token_top_ups
                        .values()
                        .any(|grant| grant.nodes.contains_key(*uid))
                    && !self
                        .nodes
                        .values()
                        .any(|child| child.authority.parent_uid.as_ref() == Some(*uid))
                    && !self
                        .sessions
                        .values()
                        .any(|session| session.identity.task_uid == **uid)
            })
            .map(|(uid, _)| uid.clone());
        if let Some(uid) = node_uid {
            let node = self.nodes.remove(&uid).ok_or(BudgetError::Corrupt)?;
            if node.meters.reserved != Amounts::default() {
                return Err(BudgetError::Corrupt);
            }
            let history = self.history.get_or_insert_with(History::default);
            history.nodes = history.nodes.checked_add(1).ok_or(BudgetError::Overflow)?;
            let unknown_usage_attempts = history.node_unknown.remove(&uid).unwrap_or_default();
            if unknown_usage_attempts != node.meters.unknown_usage_attempts {
                return Err(BudgetError::Corrupt);
            }
            records.push(HistoryRecord::Node {
                node,
                unknown_usage_attempts,
            });
        }
        Ok(records)
    }

    /// Restore only a verified history record. Restore ancestors before nodes,
    /// and nodes before sessions; update the tree in the account CAS as well.
    pub fn restore_history(&mut self, record: &HistoryRecord) -> Result<(), BudgetError> {
        let history = self.history.as_mut().ok_or(BudgetError::Corrupt)?;
        match record {
            HistoryRecord::Node {
                node,
                unknown_usage_attempts,
            } => {
                let uid = &node.authority.task.uid;
                if node.active
                    || node.meters.reserved != Amounts::default()
                    || *unknown_usage_attempts != node.meters.unknown_usage_attempts
                    || self.nodes.contains_key(uid)
                    || node
                        .authority
                        .parent_uid
                        .as_ref()
                        .is_some_and(|parent| !self.nodes.contains_key(parent))
                {
                    return Err(BudgetError::Corrupt);
                }
                if self.nodes.len() >= MAX_NODES {
                    return Err(BudgetError::Capacity);
                }
                history.nodes = history.nodes.checked_sub(1).ok_or(BudgetError::Corrupt)?;
                if *unknown_usage_attempts > 0 {
                    history
                        .node_unknown
                        .insert(uid.clone(), *unknown_usage_attempts);
                }
                self.nodes.insert(uid.clone(), node.clone());
            }
            HistoryRecord::Session { session, attempts } => {
                if !session.closed
                    || self.sessions.contains_key(&session.identity.pod_uid)
                    || attempts.values().any(|attempt| {
                        !attempt.phase.terminal() || attempt.identity != session.identity
                    })
                    || attempts.keys().any(|key| self.attempts.contains_key(key))
                {
                    return Err(BudgetError::Corrupt);
                }
                if self.sessions.len() >= MAX_SESSIONS
                    || self.attempts.len() + attempts.len() > MAX_ATTEMPTS
                {
                    return Err(BudgetError::Capacity);
                }
                let path = self.ancestors(&session.identity.task_uid)?;
                let history = self.history.as_mut().ok_or(BudgetError::Corrupt)?;
                let unknown = record.unknown();
                history.sessions = history
                    .sessions
                    .checked_sub(1)
                    .ok_or(BudgetError::Corrupt)?;
                history.unknown_usage_attempts = history
                    .unknown_usage_attempts
                    .checked_sub(unknown)
                    .ok_or(BudgetError::Corrupt)?;
                if unknown > 0 {
                    for uid in path {
                        let count = history
                            .node_unknown
                            .get_mut(&uid)
                            .ok_or(BudgetError::Corrupt)?;
                        *count = count.checked_sub(unknown).ok_or(BudgetError::Corrupt)?;
                        if *count == 0 {
                            history.node_unknown.remove(&uid);
                        }
                    }
                }
                self.sessions
                    .insert(session.identity.pod_uid.clone(), session.clone());
                self.attempts.extend(attempts.clone());
            }
        }
        Ok(())
    }
}
