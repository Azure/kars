// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Calendar usage is separate from lifetime reservations and provider contracts.

use super::{BudgetError, ResourceIdentity, valid_name, valid_uid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ACCOUNT_ANNOTATION: &str = "kars.azure.com/calendar-account-uid";
pub const BINDING_ENV: &str = "KARS_CALENDAR_BUDGET_BINDING";
const MAX_WRITERS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub sandbox: ResourceIdentity,
    pub runtime_namespace_uid: String,
    pub account: ResourceIdentity,
    pub privacy_epoch: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub binding: Binding,
    pub pod_name: String,
    pub pod_uid: String,
}

impl Identity {
    pub fn validate(&self) -> Result<(), BudgetError> {
        self.binding.sandbox.validate()?;
        self.binding.account.validate()?;
        if !valid_uid(&self.binding.runtime_namespace_uid)
            || !valid_name(&self.pod_name)
            || !valid_uid(&self.pod_uid)
        {
            return Err(BudgetError::Identity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Usage {
    pub day: u32,
    pub month: u32,
    pub daily: u64,
    pub monthly: u64,
}

impl Usage {
    pub fn current(&self, day: u32, month: u32) -> (u64, u64) {
        (
            if self.day == day { self.daily } else { 0 },
            if self.month == month { self.monthly } else { 0 },
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Commit {
    pub identity: Identity,
    pub writer: String,
    pub sequence: u64,
    pub tokens: u64,
    pub event: Event,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Event {
    Begin,
    Finish,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Read {
    pub identity: Identity,
    pub writer: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    pod_uid: String,
    sequence: u64,
    tokens: u64,
    month: u32,
    event: Event,
    pending: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ledger {
    pub sandbox: ResourceIdentity,
    pub runtime_namespace_uid: String,
    pub usage: Usage,
    writers: BTreeMap<String, Receipt>,
}

impl Ledger {
    pub fn check_reader(&self, writer: &str) -> Result<(), BudgetError> {
        if !valid_uid(writer) {
            return Err(BudgetError::Identity);
        }
        if self
            .writers
            .iter()
            .any(|(id, receipt)| id != writer && receipt.pending > 0)
        {
            return Err(BudgetError::Closed);
        }
        Ok(())
    }

    pub fn new(
        sandbox: ResourceIdentity,
        runtime_namespace_uid: String,
        day: u32,
        month: u32,
    ) -> Self {
        Self {
            sandbox,
            runtime_namespace_uid,
            usage: Usage {
                day,
                month,
                daily: 0,
                monthly: 0,
            },
            writers: BTreeMap::new(),
        }
    }

    pub fn validate(&self, binding: &Binding) -> Result<(), BudgetError> {
        if self.sandbox != binding.sandbox
            || self.runtime_namespace_uid != binding.runtime_namespace_uid
            || self.usage.daily > self.usage.monthly
            || self.writers.len() > MAX_WRITERS
            || self.writers.iter().any(|(writer, receipt)| {
                !valid_uid(writer)
                    || !valid_uid(&receipt.pod_uid)
                    || receipt.sequence == 0
                    || receipt.month > self.usage.month
                    || (receipt.month == self.usage.month && receipt.tokens > self.usage.monthly)
            })
        {
            return Err(BudgetError::Corrupt);
        }
        Ok(())
    }

    pub fn commit(&mut self, command: &Commit, day: u32, month: u32) -> Result<(), BudgetError> {
        command.identity.validate()?;
        self.validate(&command.identity.binding)?;
        if day < self.usage.day || month < self.usage.month {
            return Err(BudgetError::Corrupt);
        }
        if !valid_uid(&command.writer) || command.sequence == 0 {
            return Err(BudgetError::Sequence);
        }
        if let Some(previous) = self.writers.get(&command.writer) {
            if previous.pod_uid != command.identity.pod_uid {
                return Err(BudgetError::Identity);
            }
            if previous.sequence == command.sequence
                && previous.tokens == command.tokens
                && previous.event == command.event
            {
                return Ok(());
            }
            if previous.sequence.checked_add(1) != Some(command.sequence) {
                return Err(BudgetError::Sequence);
            }
        } else if command.sequence != 1 {
            return Err(BudgetError::Sequence);
        }
        let pending = self.writers.get(&command.writer).map_or(0, |r| r.pending);
        let pending = match command.event {
            Event::Begin => {
                self.check_reader(&command.writer)?;
                if command.tokens != 0 {
                    return Err(BudgetError::Corrupt);
                }
                pending.checked_add(1).ok_or(BudgetError::Overflow)?
            }
            Event::Finish => pending.checked_sub(1).ok_or(BudgetError::Sequence)?,
        };
        let mut writers = self.writers.clone();
        if !writers.contains_key(&command.writer) && writers.len() >= MAX_WRITERS {
            return Err(BudgetError::Capacity);
        }
        let (daily, monthly) = self.usage.current(day, month);
        let usage = Usage {
            day,
            month,
            daily: daily
                .checked_add(command.tokens)
                .ok_or(BudgetError::Overflow)?,
            monthly: monthly
                .checked_add(command.tokens)
                .ok_or(BudgetError::Overflow)?,
        };
        writers.insert(
            command.writer.clone(),
            Receipt {
                pod_uid: command.identity.pod_uid.clone(),
                sequence: command.sequence,
                tokens: command.tokens,
                month,
                event: command.event,
                pending,
            },
        );
        self.usage = usage;
        self.writers = writers;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> Commit {
        let resource = ResourceIdentity {
            namespace: "workspace".into(),
            name: "sandbox".into(),
            uid: "sandbox-uid".into(),
        };
        Commit {
            identity: Identity {
                binding: Binding {
                    sandbox: resource,
                    runtime_namespace_uid: "namespace-uid".into(),
                    account: ResourceIdentity {
                        namespace: "system".into(),
                        name: "account".into(),
                        uid: "account-uid".into(),
                    },
                    privacy_epoch: None,
                },
                pod_name: "pod".into(),
                pod_uid: "pod-uid".into(),
            },
            writer: "writer".into(),
            sequence: 2,
            tokens: 39,
            event: Event::Finish,
        }
    }

    fn dispatch(
        ledger: &mut Ledger,
        request: &Commit,
        day: u32,
        month: u32,
    ) -> Result<(), BudgetError> {
        let mut begin = request.clone();
        begin.sequence -= 1;
        begin.tokens = 0;
        begin.event = Event::Begin;
        ledger.commit(&begin, day, month)?;
        ledger.commit(request, day, month)
    }

    #[test]
    fn controller_account_names_preserve_identity_and_existing_usage() {
        let mut request = command();
        request.identity.binding.account.name = format!("calendar-{}", "a".repeat(64));
        assert_eq!(request.identity.binding.account.name.len(), 73);
        request.identity.validate().unwrap();
        assert!(!valid_name(&request.identity.binding.account.name));

        let binding = request.identity.binding.clone();
        let mut ledger = Ledger::new(binding.sandbox.clone(), "namespace-uid".into(), 1, 1);
        dispatch(&mut ledger, &request, 1, 1).unwrap();
        let mut restored: Ledger =
            serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
        request.writer = "replacement-writer".into();
        request.identity.pod_uid = "replacement-pod".into();
        dispatch(&mut restored, &request, 1, 1).unwrap();
        assert_eq!(request.identity.binding, binding);
        assert_eq!(restored.usage.current(1, 1), (78, 78));
    }

    #[test]
    fn resource_name_limits_do_not_relax_namespace_uid_or_dns_validation() {
        let mut identity = command().identity;
        for name in ["a".repeat(253), format!("{}.account", "a".repeat(64))] {
            identity.binding.account.name = name;
            identity.validate().unwrap();
        }
        for name in [
            String::new(),
            "a".repeat(254),
            ".account".into(),
            "account.".into(),
            "account..name".into(),
            "account.-name".into(),
            "account-.name".into(),
            "Account".into(),
            "account/name".into(),
            "account_name".into(),
        ] {
            identity.binding.account.name = name;
            assert!(identity.validate().is_err());
        }
        identity.binding.account.name = format!("calendar-{}", "a".repeat(64));
        identity.binding.account.namespace = "a".repeat(64);
        assert!(identity.validate().is_err());
        identity.binding.account.namespace = "system".into();
        identity.binding.account.uid.clear();
        assert!(identity.validate().is_err());
        assert!(!valid_name(&format!("{}.example.com", "a".repeat(64))));
        assert!(valid_name(&format!("{}.example.com", "a".repeat(63))));
    }

    #[test]
    fn retries_restart_and_utc_rollover_preserve_accounting() {
        let mut request = command();
        let mut ledger = Ledger::new(
            request.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        dispatch(&mut ledger, &request, 1, 1).unwrap();
        ledger.commit(&request, 2, 1).unwrap();
        assert_eq!(ledger.usage.current(1, 1), (39, 39));
        let mut restored: Ledger =
            serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
        request.writer = "replacement-writer".into();
        request.identity.pod_uid = "replacement-pod".into();
        dispatch(&mut restored, &request, 2, 1).unwrap();
        assert_eq!(restored.usage.current(2, 1), (39, 78));
        request.sequence = 4;
        dispatch(&mut restored, &request, 3, 2).unwrap();
        assert_eq!(restored.usage.current(3, 2), (39, 39));
    }

    #[test]
    fn conflicting_retries_identity_and_overflow_do_not_mutate_counters() {
        let mut request = command();
        let mut ledger = Ledger::new(
            request.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        dispatch(&mut ledger, &request, 1, 1).unwrap();
        request.tokens = 40;
        assert!(ledger.commit(&request, 1, 1).is_err());
        request.sequence = 4;
        request.identity.pod_uid = "forged".into();
        assert!(dispatch(&mut ledger, &request, 1, 1).is_err());
        request.identity.pod_uid = "pod-uid".into();
        request.tokens = u64::MAX;
        assert!(matches!(
            dispatch(&mut ledger, &request, 1, 1),
            Err(BudgetError::Overflow)
        ));
        assert_eq!(ledger.usage.current(1, 1), (39, 39));
    }

    #[test]
    fn another_writer_cannot_prune_a_retry_at_month_rollover() {
        let first = command();
        let mut ledger = Ledger::new(
            first.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        dispatch(&mut ledger, &first, 1, 1).unwrap();
        let mut second = first.clone();
        second.writer = "second".into();
        dispatch(&mut ledger, &second, 2, 2).unwrap();
        ledger.commit(&first, 2, 2).unwrap();
        assert_eq!(ledger.usage.current(2, 2), (39, 39));
        second.sequence = 2;
        assert!(ledger.commit(&second, 1, 1).is_err());
    }

    #[test]
    fn writer_capacity_and_corrupt_counters_fail_without_resetting() {
        let mut request = command();
        let mut ledger = Ledger::new(
            request.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        for index in 0..MAX_WRITERS {
            request.writer = format!("writer-{index}");
            dispatch(&mut ledger, &request, 1, 1).unwrap();
        }
        request.writer = "overflow-writer".into();
        let before = serde_json::to_value(&ledger).unwrap();
        assert!(matches!(
            dispatch(&mut ledger, &request, 2, 2),
            Err(BudgetError::Capacity)
        ));
        assert_eq!(before, serde_json::to_value(&ledger).unwrap());
        ledger.usage.daily = ledger.usage.monthly + 1;
        assert!(ledger.validate(&request.identity.binding).is_err());
    }

    #[test]
    fn process_death_keeps_pending_liability_and_concurrent_completions_are_sequenced() {
        let mut request = command();
        let mut ledger = Ledger::new(
            request.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        request.sequence = 1;
        request.tokens = 0;
        request.event = Event::Begin;
        ledger.commit(&request, 1, 1).unwrap();
        ledger.commit(&request, 1, 1).unwrap();
        request.sequence = 2;
        ledger.commit(&request, 1, 1).unwrap();
        let mut restored: Ledger =
            serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
        assert!(restored.check_reader("replacement").is_err());
        assert!(restored.check_reader("writer").is_ok());
        request.event = Event::Finish;
        request.tokens = 39;
        request.sequence = 3;
        restored.commit(&request, 1, 1).unwrap();
        assert!(restored.check_reader("replacement").is_err());
        request.sequence = 4;
        restored.commit(&request, 1, 1).unwrap();
        assert!(restored.check_reader("replacement").is_ok());
        assert_eq!(restored.usage.monthly, 78);
        request.sequence = 5;
        assert!(restored.commit(&request, 1, 1).is_err());
    }
}
