// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Calendar usage is separate from lifetime reservations and provider contracts.

use super::{BudgetError, ResourceIdentity, valid_name, valid_uid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ACCOUNT_ANNOTATION: &str = "kars.azure.com/calendar-account-uid";
pub const BINDING_ENV: &str = "KARS_CALENDAR_BUDGET_BINDING";
pub const POLICY_KEY: &str = "calendar-policy";
pub const POLICY_HEADER: &str = "x-kars-calendar-policy";
pub const PENDING_HEADER: &str = "x-kars-calendar-pending";
const MAX_WRITERS: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    #[default]
    Enforce,
    RecordOnly,
}

impl Policy {
    pub fn parse(value: Option<&str>) -> Result<Self, BudgetError> {
        match value {
            None | Some("enforce") => Ok(Self::Enforce),
            Some("record-only") => Ok(Self::RecordOnly),
            _ => Err(BudgetError::Contract),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::RecordOnly => "record-only",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub usage: Usage,
    pub policy: Policy,
    /// Includes in-flight requests and unresolved usage; not a measured token total.
    pub pending_requests: Option<u64>,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    orphan_finalization: Option<OrphanFinalization>,
}

/// Operator-only conservative accounting, never a provider receipt or router Finish.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrphanFinalization {
    version: String,
    account_uid: String,
    account_resource_version: String,
    calendar_resource_version: String,
    lifetime_ledger_sha256: String,
    maximum_tokens: u64,
    day: u32,
    month: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ledger {
    pub sandbox: ResourceIdentity,
    pub runtime_namespace_uid: String,
    pub usage: Usage,
    writers: BTreeMap<String, Receipt>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lifetime_only: bool,
}

impl Ledger {
    pub fn check_reader(&self, writer: &str) -> Result<(), BudgetError> {
        self.check_reader_with_policy(writer, Policy::Enforce)
    }

    pub fn observation(&self, policy: Policy) -> Result<Observation, BudgetError> {
        let pending_requests = self
            .writers
            .values()
            .filter(|receipt| receipt.orphan_finalization.is_none())
            .try_fold(0u64, |total, receipt| total.checked_add(receipt.pending))
            .ok_or(BudgetError::Overflow)?;
        Ok(Observation {
            usage: self.usage.clone(),
            policy,
            pending_requests: Some(pending_requests),
        })
    }

    pub fn check_reader_with_policy(
        &self,
        writer: &str,
        policy: Policy,
    ) -> Result<(), BudgetError> {
        if !valid_uid(writer) {
            return Err(BudgetError::Identity);
        }
        if self.writers.iter().any(|(id, receipt)| {
            if receipt.orphan_finalization.is_some() {
                id == writer
            } else {
                policy == Policy::Enforce && id != writer && receipt.pending > 0
            }
        }) {
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
            lifetime_only: false,
        }
    }

    /// Receipts are never pruned, so an empty writer set proves that no Begin
    /// was acknowledged. Persist this fence with the same CAS as calendar commits.
    pub fn seal_unused(&mut self) -> Result<(), BudgetError> {
        if self.usage.daily != 0 || self.usage.monthly != 0 || !self.writers.is_empty() {
            return Err(BudgetError::Closed);
        }
        self.lifetime_only = true;
        Ok(())
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
                    || receipt
                        .orphan_finalization
                        .as_ref()
                        .is_some_and(|finalization| {
                            let invalid_bound = match finalization.version.as_str() {
                                "calendar-account-bound/v1" => finalization.maximum_tokens == 0,
                                "calendar-never-reserved/v1" => {
                                    finalization.maximum_tokens != 0 || receipt.tokens != 0
                                }
                                _ => true,
                            };
                            invalid_bound
                                || !valid_uid(&finalization.account_uid)
                                || !valid_uid(&finalization.account_resource_version)
                                || !valid_uid(&finalization.calendar_resource_version)
                                || finalization.lifetime_ledger_sha256.len() != 64
                                || !finalization
                                    .lifetime_ledger_sha256
                                    .bytes()
                                    .all(|byte| byte.is_ascii_hexdigit())
                                || finalization.day > self.usage.day
                                || finalization.month != receipt.month
                                || receipt.pending != 1
                                || (receipt.event == Event::Begin && receipt.tokens != 0)
                                || (finalization.month == self.usage.month
                                    && finalization.maximum_tokens > self.usage.monthly)
                                || (finalization.day == self.usage.day
                                    && finalization.maximum_tokens > self.usage.daily)
                        })
            })
        {
            return Err(BudgetError::Corrupt);
        }
        Ok(())
    }

    /// A finalized orphan preserves its last receipt, including a Finish that
    /// left another request pending. Its audit must travel with the ConfigMap;
    /// router RPCs cannot create this representation.
    pub fn validate_orphan_audits(
        &self,
        raw: Option<&str>,
        calendar_uid: &str,
    ) -> Result<(), BudgetError> {
        if self
            .writers
            .values()
            .all(|receipt| receipt.orphan_finalization.is_none())
        {
            return Ok(());
        }
        let audits: serde_json::Value = serde_json::from_str(raw.ok_or(BudgetError::Corrupt)?)
            .map_err(|_| BudgetError::Corrupt)?;
        for (writer, receipt) in &self.writers {
            let Some(finalization) = &receipt.orphan_finalization else {
                continue;
            };
            let audit = &audits[format!("{writer}:{}", receipt.sequence)];
            let after = serde_json::to_value(receipt).map_err(|_| BudgetError::Corrupt)?;
            let mut before = after.clone();
            before
                .as_object_mut()
                .ok_or(BudgetError::Corrupt)?
                .remove("orphanFinalization");
            if audit["version"] != finalization.version
                || audit["calendarUid"] != calendar_uid
                || audit["beforeResourceVersion"] != finalization.calendar_resource_version
                || audit["account"]["uid"] != finalization.account_uid
                || audit["accountResourceVersion"] != finalization.account_resource_version
                || audit["lifetimeLedgerSha256"] != finalization.lifetime_ledger_sha256
                || audit["actualUsage"] != "unknown"
                || audit["lifetimeChargeChanged"] != false
                || audit["newFunding"] != false
                || audit["writerBefore"] != before
                || audit["writerAfter"] != after
            {
                return Err(BudgetError::Corrupt);
            }
            if finalization.version == "calendar-never-reserved/v1" {
                let proof = &audit["aggregateProof"];
                let session = &proof["retiredSession"];
                let identity = &session["identity"];
                let sandbox =
                    serde_json::to_value(&self.sandbox).map_err(|_| BudgetError::Corrupt)?;
                if finalization.maximum_tokens != 0
                    || receipt.tokens != 0
                    || proof["retiredSessionNeverReserved"] != true
                    || proof["calendarCharge"] != 0
                    || proof["allSessionsClosed"] != true
                    || proof["allSessionPodsAbsent"] != true
                    || session["closed"] != true
                    || session["nextSequence"] != 1
                    || session["closedThrough"] != 0
                    || identity["podUid"] != receipt.pod_uid
                    || identity["sandbox"] != sandbox
                    || identity["runtimeNamespaceUid"] != self.runtime_namespace_uid
                    || identity["taskUid"] != self.sandbox.uid
                {
                    return Err(BudgetError::Corrupt);
                }
            }
            let usage_before: Usage = serde_json::from_value(audit["usageBefore"].clone())
                .map_err(|_| BudgetError::Corrupt)?;
            let usage_after: Usage = serde_json::from_value(audit["usageAfter"].clone())
                .map_err(|_| BudgetError::Corrupt)?;
            if usage_before.day != finalization.day
                || usage_before.month != finalization.month
                || usage_after.day != finalization.day
                || usage_after.month != finalization.month
                || usage_before.daily > usage_before.monthly
                || receipt.tokens > usage_before.monthly
                || usage_before.daily.checked_add(finalization.maximum_tokens)
                    != Some(usage_after.daily)
                || usage_before
                    .monthly
                    .checked_add(finalization.maximum_tokens)
                    != Some(usage_after.monthly)
                || (self.usage.day == finalization.day && self.usage.daily < usage_after.daily)
                || (self.usage.month == finalization.month
                    && self.usage.monthly < usage_after.monthly)
            {
                return Err(BudgetError::Corrupt);
            }
        }
        Ok(())
    }

    pub fn commit(&mut self, command: &Commit, day: u32, month: u32) -> Result<(), BudgetError> {
        self.commit_authorized(command, day, month, false)
    }

    pub fn commit_authorized(
        &mut self,
        command: &Commit,
        day: u32,
        month: u32,
        lifetime_bound: bool,
    ) -> Result<(), BudgetError> {
        self.commit_with_policy(command, day, month, lifetime_bound, Policy::Enforce)
    }

    pub fn commit_with_policy(
        &mut self,
        command: &Commit,
        day: u32,
        month: u32,
        lifetime_bound: bool,
        policy: Policy,
    ) -> Result<(), BudgetError> {
        if self.lifetime_only && !lifetime_bound {
            return Err(BudgetError::Authorization);
        }
        command.identity.validate()?;
        self.validate(&command.identity.binding)?;
        if day < self.usage.day || month < self.usage.month {
            return Err(BudgetError::Corrupt);
        }
        if !valid_uid(&command.writer) || command.sequence == 0 {
            return Err(BudgetError::Sequence);
        }
        if let Some(previous) = self.writers.get(&command.writer) {
            if previous.orphan_finalization.is_some() {
                return Err(BudgetError::Closed);
            }
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
                self.check_reader_with_policy(&command.writer, policy)?;
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
                orphan_finalization: None,
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
    fn record_only_preserves_pending_liability_and_actual_usage_with_strict_reversion() {
        let mut begin = command();
        begin.sequence = 1;
        begin.tokens = 0;
        begin.event = Event::Begin;
        let mut ledger = Ledger::new(
            begin.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        ledger.commit(&begin, 1, 1).unwrap();
        let retained = serde_json::to_value(&ledger).unwrap()["writers"][&begin.writer].clone();
        let mut replacement = begin.clone();
        replacement.writer = "replacement".into();
        replacement.identity.pod_uid = "replacement-pod".into();
        assert!(ledger.commit(&replacement, 1, 1).is_err());
        ledger
            .commit_with_policy(&replacement, 1, 1, false, Policy::RecordOnly)
            .unwrap();
        replacement.sequence = 2;
        replacement.event = Event::Finish;
        replacement.tokens = 39;
        ledger
            .commit_with_policy(&replacement, 1, 1, false, Policy::RecordOnly)
            .unwrap();
        let observed = ledger.observation(Policy::RecordOnly).unwrap();
        assert_eq!(observed.usage.current(1, 1), (39, 39));
        assert_eq!(observed.pending_requests, Some(1));
        assert_eq!(
            serde_json::to_value(&ledger).unwrap()["writers"][&begin.writer],
            retained
        );
        assert!(ledger.check_reader("replacement").is_err());
        assert!(
            ledger
                .check_reader_with_policy("replacement", Policy::RecordOnly)
                .is_ok()
        );
        let mut restored: Ledger =
            serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
        restored.validate(&begin.identity.binding).unwrap();
        assert_eq!(
            restored
                .observation(Policy::Enforce)
                .unwrap()
                .pending_requests,
            Some(1)
        );
        replacement.sequence = 3;
        replacement.event = Event::Begin;
        replacement.tokens = 0;
        assert!(restored.commit(&replacement, 1, 1).is_err());
    }

    #[test]
    fn record_only_does_not_relax_identity_sequence_or_lifetime_fences() {
        let mut begin = command();
        begin.sequence = 1;
        begin.tokens = 0;
        begin.event = Event::Begin;
        let mut ledger = Ledger::new(
            begin.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        ledger.seal_unused().unwrap();
        assert!(
            ledger
                .commit_with_policy(&begin, 1, 1, false, Policy::RecordOnly)
                .is_err()
        );
        ledger
            .commit_with_policy(&begin, 1, 1, true, Policy::RecordOnly)
            .unwrap();
        let before = serde_json::to_value(&ledger).unwrap();
        for (sequence, pod, namespace) in [
            (3, "pod-uid", "namespace-uid"),
            (2, "another-pod", "namespace-uid"),
            (2, "pod-uid", "wrong-namespace"),
        ] {
            let mut invalid = begin.clone();
            invalid.sequence = sequence;
            invalid.identity.pod_uid = pod.into();
            invalid.identity.binding.runtime_namespace_uid = namespace.into();
            assert!(
                ledger
                    .commit_with_policy(&invalid, 1, 1, true, Policy::RecordOnly)
                    .is_err()
            );
            assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
        }
        assert!(
            ledger
                .check_reader_with_policy("", Policy::RecordOnly)
                .is_err()
        );
        assert_eq!(Policy::parse(None).unwrap(), Policy::Enforce);
        assert_eq!(
            Policy::parse(Some("record-only")).unwrap(),
            Policy::RecordOnly
        );
        for invalid in ["", "false", "RECORD-ONLY", "record-only "] {
            assert!(Policy::parse(Some(invalid)).is_err());
        }
    }

    #[test]
    fn unused_enrollment_fence_rejects_every_legacy_writer_and_retains_identity() {
        let mut request = command();
        request.sequence = 1;
        request.tokens = 0;
        request.event = Event::Begin;
        let binding = request.identity.binding.clone();
        let mut ledger = Ledger::new(binding.sandbox.clone(), "namespace-uid".into(), 1, 1);
        let original = serde_json::to_value(&ledger).unwrap();
        assert!(original.get("lifetimeOnly").is_none());
        ledger.seal_unused().unwrap();
        ledger.seal_unused().unwrap();
        let mut restored: Ledger =
            serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
        restored.validate(&binding).unwrap();
        assert!(restored.lifetime_only);
        assert_eq!(restored.sandbox, binding.sandbox);
        assert!(restored.commit(&request, 2, 2).is_err());
        restored.commit_authorized(&request, 2, 2, true).unwrap();
        assert!(restored.seal_unused().is_err());
        request.sequence = 2;
        request.tokens = 39;
        request.event = Event::Finish;
        assert!(restored.commit(&request, 2, 2).is_err());
        restored.commit_authorized(&request, 2, 2, true).unwrap();
        assert_eq!(restored.usage.monthly, 39);
        assert!(restored.seal_unused().is_err());
        let old: Ledger = serde_json::from_value(original).unwrap();
        assert!(!old.lifetime_only);
    }

    #[test]
    fn zero_counters_never_replace_no_attempt_history() {
        let mut request = command();
        request.tokens = 0;
        let mut ledger = Ledger::new(
            request.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        dispatch(&mut ledger, &request, 1, 1).unwrap();
        assert_eq!(ledger.usage.monthly, 0);
        assert!(ledger.seal_unused().is_err());
        request.sequence = 3;
        request.event = Event::Begin;
        ledger.commit(&request, 2, 2).unwrap();
        assert!(ledger.seal_unused().is_err());
        for (daily, monthly) in [(1, 1), (0, 1)] {
            let mut empty = Ledger::new(
                request.identity.binding.sandbox.clone(),
                "namespace-uid".into(),
                1,
                1,
            );
            empty.usage.daily = daily;
            empty.usage.monthly = monthly;
            assert!(empty.seal_unused().is_err());
        }
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

    fn finalized_orphan() -> (Ledger, Commit, serde_json::Value) {
        finalized_orphan_receipt(None)
    }

    fn finalized_orphan_receipt(tokens: Option<u64>) -> (Ledger, Commit, serde_json::Value) {
        let mut request = command();
        request.sequence = 1;
        request.tokens = 0;
        request.event = Event::Begin;
        let mut ledger = Ledger::new(
            request.identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            1,
            1,
        );
        ledger.commit(&request, 1, 1).unwrap();
        if let Some(tokens) = tokens {
            request.sequence = 2;
            ledger.commit(&request, 1, 1).unwrap();
            request.sequence = 3;
            request.event = Event::Finish;
            request.tokens = tokens;
            ledger.commit(&request, 1, 1).unwrap();
        }
        assert!(ledger.check_reader("replacement").is_err());
        let before = serde_json::to_value(&ledger.writers["writer"]).unwrap();
        let usage_before = ledger.usage.clone();
        ledger
            .writers
            .get_mut("writer")
            .unwrap()
            .orphan_finalization = Some(OrphanFinalization {
            version: "calendar-account-bound/v1".into(),
            account_uid: "lifetime-uid".into(),
            account_resource_version: "10".into(),
            calendar_resource_version: "20".into(),
            lifetime_ledger_sha256: "a".repeat(64),
            maximum_tokens: 17,
            day: 1,
            month: 1,
        });
        ledger.usage.daily += 17;
        ledger.usage.monthly += 17;
        let audits = serde_json::json!({ (format!("writer:{}", request.sequence)): {
            "version": "calendar-account-bound/v1", "calendarUid": "account-uid", "beforeResourceVersion": "20",
            "account": { "uid": "lifetime-uid" }, "accountResourceVersion": "10", "lifetimeLedgerSha256": "a".repeat(64),
            "actualUsage": "unknown", "lifetimeChargeChanged": false, "newFunding": false,
            "writerBefore": before, "writerAfter": ledger.writers["writer"],
            "usageBefore": usage_before, "usageAfter": ledger.usage,
        }});
        (ledger, request, audits)
    }

    fn never_reserved_fixture() -> (Ledger, Commit, serde_json::Value) {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("calendar-never-reserved.fixture.json")).unwrap();
        let ledger: Ledger = serde_json::from_value(fixture["ledger"].clone()).unwrap();
        let mut request = command();
        request.identity.binding.sandbox = ledger.sandbox.clone();
        request.identity.binding.runtime_namespace_uid = ledger.runtime_namespace_uid.clone();
        request.identity.binding.account.uid = fixture["calendarUid"].as_str().unwrap().into();
        let receipt = &ledger.writers["writer"];
        request.identity.pod_uid = receipt.pod_uid.clone();
        request.sequence = receipt.sequence;
        request.tokens = receipt.tokens;
        request.event = receipt.event.clone();
        (ledger, request, fixture["audits"].clone())
    }

    #[test]
    fn cli_never_reserved_fixture_preserves_receipts_and_fences_all_retired_writes() {
        let (mut ledger, mut request, audits) = never_reserved_fixture();
        ledger.validate(&request.identity.binding).unwrap();
        ledger
            .validate_orphan_audits(Some(&audits.to_string()), "calendar-uid")
            .unwrap();
        assert_eq!(
            serde_json::to_value(&ledger.usage).unwrap(),
            audits["writer:45"]["usageBefore"]
        );
        assert_eq!(
            audits["writer:45"]["usageBefore"],
            audits["writer:45"]["usageAfter"]
        );
        assert!(ledger.check_reader("replacement").is_ok());
        assert!(ledger.check_reader("writer").is_err());
        let before = serde_json::to_value(&ledger).unwrap();
        for sequence in [request.sequence, request.sequence + 1] {
            for event in [Event::Begin, Event::Finish] {
                request.sequence = sequence;
                request.event = event;
                assert!(
                    ledger
                        .commit(&request, ledger.usage.day, ledger.usage.month)
                        .is_err()
                );
            }
        }
        assert_eq!(before, serde_json::to_value(&ledger).unwrap());
        assert!(ledger.seal_unused().is_err());
    }

    #[test]
    fn never_reserved_begin_and_finish_markers_survive_normal_period_rollover() {
        for event in [Event::Begin, Event::Finish] {
            let (mut ledger, mut request, mut audits) = never_reserved_fixture();
            ledger.writers.get_mut("writer").unwrap().event = event.clone();
            let encoded = serde_json::to_value(event).unwrap();
            audits["writer:45"]["writerBefore"]["event"] = encoded.clone();
            audits["writer:45"]["writerAfter"]["event"] = encoded;
            let receipt_before = serde_json::to_value(&ledger.writers["writer"]).unwrap();
            let day = ledger.usage.day;
            let month = ledger.usage.month;
            let monthly = ledger.usage.monthly;
            ledger.validate(&request.identity.binding).unwrap();
            ledger
                .validate_orphan_audits(Some(&audits.to_string()), "calendar-uid")
                .unwrap();
            request.writer = "replacement".into();
            request.identity.pod_uid = "replacement-pod".into();
            request.sequence = 2;
            request.event = Event::Finish;
            request.tokens = 7;
            dispatch(&mut ledger, &request, day + 1, month).unwrap();
            assert_eq!(ledger.usage.daily, 7);
            assert_eq!(ledger.usage.monthly, monthly + 7);
            ledger
                .validate_orphan_audits(Some(&audits.to_string()), "calendar-uid")
                .unwrap();
            request.sequence = 4;
            dispatch(&mut ledger, &request, day + 32, month + 1).unwrap();
            assert_eq!(ledger.usage.daily, 7);
            assert_eq!(ledger.usage.monthly, 7);
            ledger.validate(&request.identity.binding).unwrap();
            ledger
                .validate_orphan_audits(Some(&audits.to_string()), "calendar-uid")
                .unwrap();
            assert_eq!(
                serde_json::to_value(&ledger.writers["writer"]).unwrap(),
                receipt_before
            );
            assert!(ledger.check_reader("writer").is_err());
        }
    }

    #[test]
    fn never_reserved_audit_requires_exact_closed_session_and_identity_proof() {
        let (ledger, _, audits) = never_reserved_fixture();
        for (path, value) in [
            ("retiredSessionNeverReserved", serde_json::json!(false)),
            ("calendarCharge", serde_json::json!(1)),
            ("allSessionsClosed", serde_json::json!(false)),
            ("allSessionPodsAbsent", serde_json::json!(false)),
            ("retiredSession", serde_json::Value::Null),
            ("retiredSession/closed", serde_json::json!(false)),
            ("retiredSession/nextSequence", serde_json::json!(2)),
            ("retiredSession/closedThrough", serde_json::json!(1)),
            (
                "retiredSession/identity/podUid",
                serde_json::json!("other-pod"),
            ),
            (
                "retiredSession/identity/sandbox/uid",
                serde_json::json!("other-sandbox"),
            ),
            (
                "retiredSession/identity/runtimeNamespaceUid",
                serde_json::json!("other-runtime"),
            ),
            (
                "retiredSession/identity/taskUid",
                serde_json::json!("other-task"),
            ),
        ] {
            let mut changed = audits.clone();
            *changed
                .pointer_mut(&format!("/writer:45/aggregateProof/{path}"))
                .unwrap() = value;
            assert!(
                ledger
                    .validate_orphan_audits(Some(&changed.to_string()), "calendar-uid")
                    .is_err(),
                "{path}"
            );
        }
        for field in [
            "aggregateProof",
            "writerBefore",
            "usageBefore",
            "usageAfter",
        ] {
            let mut changed = audits.clone();
            changed["writer:45"].as_object_mut().unwrap().remove(field);
            assert!(
                ledger
                    .validate_orphan_audits(Some(&changed.to_string()), "calendar-uid")
                    .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn never_reserved_markers_cannot_claim_charges_or_hide_new_pending_writers() {
        let (ledger, request, audits) = never_reserved_fixture();
        for path in ["tokens", "orphanFinalization/maximumTokens"] {
            let mut encoded = serde_json::to_value(&ledger).unwrap();
            *encoded
                .pointer_mut(&format!("/writers/writer/{path}"))
                .unwrap() = serde_json::json!(1);
            let changed: Ledger = serde_json::from_value(encoded).unwrap();
            assert!(changed.validate(&request.identity.binding).is_err());
        }
        let mut changed = audits.clone();
        changed["writer:45"]["usageAfter"]["daily"] = serde_json::json!(529505);
        assert!(
            ledger
                .validate_orphan_audits(Some(&changed.to_string()), "calendar-uid")
                .is_err()
        );
        let mut pending = ledger.clone();
        let mut replacement = request.clone();
        replacement.writer = "replacement".into();
        replacement.identity.pod_uid = "replacement-pod".into();
        replacement.sequence = 1;
        replacement.event = Event::Begin;
        pending
            .commit(&replacement, ledger.usage.day, ledger.usage.month)
            .unwrap();
        assert!(pending.check_reader("third-writer").is_err());
        assert!(pending.check_reader("replacement").is_ok());
        assert!(pending.check_reader("writer").is_err());
    }

    #[test]
    fn never_reserved_marker_coexists_with_historical_positive_bound_markers() {
        let (mut ledger, request, mut audits) = never_reserved_fixture();
        let (legacy, _, legacy_audits) = finalized_orphan();
        ledger
            .writers
            .insert("legacy".into(), legacy.writers["writer"].clone());
        audits["legacy:1"] = legacy_audits["writer:1"].clone();
        audits["legacy:1"]["calendarUid"] = serde_json::json!("calendar-uid");
        ledger.validate(&request.identity.binding).unwrap();
        ledger
            .validate_orphan_audits(Some(&audits.to_string()), "calendar-uid")
            .unwrap();
        assert!(ledger.check_reader("replacement").is_ok());
        assert!(ledger.check_reader("legacy").is_err());
        assert!(ledger.check_reader("writer").is_err());
    }

    #[test]
    fn audited_orphan_retains_begin_and_rejects_every_late_old_writer_commit() {
        let (ledger, mut request, audits) = finalized_orphan();
        let mut restored: Ledger =
            serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
        let audit = audits.to_string();
        restored.validate(&request.identity.binding).unwrap();
        restored
            .validate_orphan_audits(Some(&audit), "account-uid")
            .unwrap();
        assert!(restored.check_reader("replacement").is_ok());
        assert!(restored.check_reader("writer").is_err());
        let before = serde_json::to_value(&restored).unwrap();
        assert!(
            restored.commit(&request, 1, 1).is_err(),
            "even exact old Begin replay is fenced"
        );
        request.sequence = 2;
        request.event = Event::Finish;
        request.tokens = 9;
        assert!(
            restored.commit(&request, 1, 1).is_err(),
            "late Finish cannot refund or replace the tombstone"
        );
        assert_eq!(serde_json::to_value(&restored).unwrap(), before);
        request.writer = "replacement".into();
        request.identity.pod_uid = "replacement-pod".into();
        dispatch(&mut restored, &request, 1, 1).unwrap();
        assert_eq!(restored.usage.monthly, 26);
        restored
            .validate_orphan_audits(Some(&audit), "account-uid")
            .unwrap();
        request.sequence = 4;
        dispatch(&mut restored, &request, 2, 2).unwrap();
        restored.validate(&request.identity.binding).unwrap();
        restored
            .validate_orphan_audits(Some(&audit), "account-uid")
            .unwrap();
        assert_eq!(restored.usage.monthly, 9);
        assert_eq!(restored.writers["writer"].pending, 1);
        assert_eq!(restored.writers["writer"].event, Event::Begin);
        assert!(restored.seal_unused().is_err());
    }

    #[test]
    fn audited_finish_orphan_preserves_prior_charge_and_fences_the_retired_writer() {
        for tokens in [0, 39] {
            let (ledger, mut request, audits) = finalized_orphan_receipt(Some(tokens));
            let mut restored: Ledger =
                serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
            let audit = audits.to_string();
            restored.validate(&request.identity.binding).unwrap();
            restored
                .validate_orphan_audits(Some(&audit), "account-uid")
                .unwrap();
            assert_eq!(restored.usage.monthly, tokens + 17);
            assert_eq!(restored.writers["writer"].event, Event::Finish);
            assert_eq!(restored.writers["writer"].pending, 1);
            assert_eq!(restored.writers["writer"].tokens, tokens);
            assert!(restored.check_reader("replacement").is_ok());
            assert!(restored.check_reader("writer").is_err());
            let before = serde_json::to_value(&restored).unwrap();
            assert!(restored.commit(&request, 1, 1).is_err());
            request.sequence += 1;
            assert!(restored.commit(&request, 1, 1).is_err());
            request.event = Event::Begin;
            request.tokens = 0;
            assert!(restored.commit(&request, 1, 1).is_err());
            assert_eq!(serde_json::to_value(&restored).unwrap(), before);
            let receipt = serde_json::to_value(&restored.writers["writer"]).unwrap();
            request.writer = "replacement".into();
            request.identity.pod_uid = "replacement-pod".into();
            request.sequence = 1;
            restored.commit(&request, 1, 1).unwrap();
            assert!(restored.check_reader("third-writer").is_err());
            request.sequence = 2;
            request.event = Event::Finish;
            request.tokens = 5;
            restored.commit(&request, 1, 1).unwrap();
            assert_eq!(restored.usage.monthly, tokens + 22);
            request.sequence = 4;
            dispatch(&mut restored, &request, 2, 2).unwrap();
            restored.validate(&request.identity.binding).unwrap();
            restored
                .validate_orphan_audits(Some(&audit), "account-uid")
                .unwrap();
            assert_eq!(
                serde_json::to_value(&restored.writers["writer"]).unwrap(),
                receipt
            );
        }
    }

    #[test]
    fn finish_orphan_audit_cannot_cover_an_original_charge_with_the_new_bound() {
        let (mut ledger, request, mut audits) = finalized_orphan_receipt(Some(39));
        ledger.writers.get_mut("writer").unwrap().tokens = 40;
        audits["writer:3"]["writerBefore"]["tokens"] = serde_json::json!(40);
        audits["writer:3"]["writerAfter"]["tokens"] = serde_json::json!(40);
        ledger.validate(&request.identity.binding).unwrap();
        assert!(
            ledger
                .validate_orphan_audits(Some(&audits.to_string()), "account-uid")
                .is_err()
        );
    }

    #[test]
    fn malformed_orphan_markers_and_missing_or_changed_audits_fail_closed() {
        let (ledger, request, audits) = finalized_orphan();
        assert!(ledger.validate_orphan_audits(None, "account-uid").is_err());
        assert!(
            ledger
                .validate_orphan_audits(Some("{}"), "account-uid")
                .is_err()
        );
        assert!(
            ledger
                .validate_orphan_audits(Some(&audits.to_string()), "other-uid")
                .is_err()
        );
        for (field, value) in [
            ("version", serde_json::json!("unknown")),
            ("actualUsage", serde_json::json!("measured")),
            ("lifetimeChargeChanged", serde_json::json!(true)),
            ("newFunding", serde_json::json!(true)),
            ("writerBefore", serde_json::json!({})),
            ("writerAfter", serde_json::json!({})),
            (
                "usageAfter",
                serde_json::json!({"day":1,"month":1,"daily":0,"monthly":0}),
            ),
        ] {
            let mut changed = audits.clone();
            changed["writer:1"][field] = value;
            assert!(
                ledger
                    .validate_orphan_audits(Some(&changed.to_string()), "account-uid")
                    .is_err()
            );
        }
        for (field, value) in [
            ("version", serde_json::json!("unknown")),
            ("maximumTokens", serde_json::json!(0)),
            ("maximumTokens", serde_json::json!(18)),
            ("day", serde_json::json!(2)),
            ("month", serde_json::json!(2)),
            ("lifetimeLedgerSha256", serde_json::json!("not-a-digest")),
        ] {
            let mut encoded = serde_json::to_value(&ledger).unwrap();
            encoded["writers"]["writer"]["orphanFinalization"][field] = value;
            let changed: Ledger = serde_json::from_value(encoded).unwrap();
            assert!(changed.validate(&request.identity.binding).is_err());
        }
        let mut changed = ledger.clone();
        changed.writers.get_mut("writer").unwrap().event = Event::Finish;
        assert!(
            changed
                .validate_orphan_audits(Some(&audits.to_string()), "account-uid")
                .is_err()
        );
        for (pending, tokens) in [(0, 0), (2, 0), (1, 1)] {
            let mut changed = ledger.clone();
            let receipt = changed.writers.get_mut("writer").unwrap();
            receipt.pending = pending;
            receipt.tokens = tokens;
            assert!(changed.validate(&request.identity.binding).is_err());
        }
        let mut wire = serde_json::to_value(&request).unwrap();
        wire["orphanFinalization"] = serde_json::json!({});
        assert!(
            serde_json::from_value::<Commit>(wire).is_err(),
            "router cannot submit operator markers"
        );
    }

    #[test]
    fn finalized_orphan_does_not_ignore_a_new_writers_pending_liability() {
        let (mut ledger, mut request, _) = finalized_orphan();
        request.writer = "replacement".into();
        request.identity.pod_uid = "replacement-pod".into();
        ledger.commit(&request, 1, 1).unwrap();
        assert!(ledger.check_reader("third-writer").is_err());
        assert!(ledger.check_reader("replacement").is_ok());
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
