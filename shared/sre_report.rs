// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Latest-only, advisory SRE reports. These records confer no remediation authority.

use crate::providers::signing::sha256_hex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const NAME: &str = "sre-health-report";
pub const NAMESPACE: &str = "kars-sre";
pub const PATH: &str = "/reports/latest";
pub const KEY: &str = "report.json";
pub const MAX_TEXT: usize = 8192;
pub const MAX_RECORD: usize = 65536;
pub const OWNER: &str = "kars.azure.com/sre-registration-uid";
pub const SOURCE: &str = "kars.azure.com/sre-report-source";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub registration_uid: String,
    pub source_namespace: String,
    pub source_uid: String,
    pub namespace_uid: String,
}

impl Identity {
    pub fn from_registration(reg: &Value) -> Option<Self> {
        let string = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        if reg["metadata"]["name"] != "canonical"
            || reg["spec"]["sandbox"]["name"] != "sre"
            || reg["spec"]["runtimeNamespace"]["name"] != NAMESPACE
        {
            return None;
        }
        Some(Self {
            registration_uid: string(&reg["metadata"]["uid"])?,
            source_namespace: string(&reg["spec"]["sandbox"]["namespace"])?,
            source_uid: string(&reg["spec"]["sandbox"]["uid"])?,
            namespace_uid: string(&reg["spec"]["runtimeNamespace"]["uid"])?,
        })
    }

    pub fn target(&self) -> Value {
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{
            "name":NAME,"namespace":NAMESPACE,
            "annotations":{OWNER:self.registration_uid,SOURCE:serde_json::to_string(self).expect("identity serializes")},
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
                "name":"canonical","uid":self.registration_uid,"controller":true}]},"data":{}})
    }

    pub fn owns_target(&self, target: &Value) -> bool {
        target["metadata"]["name"] == NAME
            && target["metadata"]["namespace"] == NAMESPACE
            && target["metadata"]["deletionTimestamp"].is_null()
            && target["metadata"]["annotations"][OWNER] == self.registration_uid
            && target["metadata"]["ownerReferences"] == self.target()["metadata"]["ownerReferences"]
            && target["metadata"]["uid"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            && target["metadata"]["resourceVersion"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            && target["immutable"] != true
            && target["binaryData"].is_null()
    }

    pub fn matches_target(&self, target: &Value) -> bool {
        self.owns_target(target)
            && target["metadata"]["annotations"][SOURCE]
                == self.target()["metadata"]["annotations"][SOURCE]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub sweep_id: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub turn_status: String,
    pub text: String,
    pub truncated: bool,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
}

impl Input {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.sweep_id.len() != 32
            || !self.sweep_id.bytes().all(|b| b.is_ascii_hexdigit())
            || !["finished", "failed", "blocked", "timeout"].contains(&self.turn_status.as_str())
            || self.text.trim().is_empty()
            || self.text.len() > MAX_TEXT
            || self
                .text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
            || self.started_at_ms == 0
            || self.finished_at_ms < self.started_at_ms
            || self.finished_at_ms - self.started_at_ms > 1_800_000
            || !(60..=86400).contains(&self.interval_seconds)
            || !(10..=900).contains(&self.timeout_seconds)
        {
            return Err("Invalid or oversized SRE report");
        }
        Ok(())
    }

    pub fn digest(&self) -> String {
        let serialized = serde_json::to_vec(self).expect("report serializes");
        sha256_hex(serialized.as_slice())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub version: u8,
    pub identity: Identity,
    pub privacy_epoch: String,
    pub accepted_at_ms: u64,
    pub digest: String,
    pub report: Input,
}

impl Report {
    pub fn decode(value: &str) -> Result<Self, &'static str> {
        if value.len() > MAX_RECORD {
            return Err("Oversized stored report");
        }
        let report: Self = serde_json::from_str(value).map_err(|_| "Malformed stored report")?;
        report.report.validate()?;
        if report.version != 1
            || report.digest != report.report.digest()
            || report.privacy_epoch.is_empty()
            || report.accepted_at_ms.saturating_add(30_000) < report.report.finished_at_ms
            || report.accepted_at_ms > report.report.finished_at_ms.saturating_add(300_000)
        {
            return Err("Invalid stored report");
        }
        Ok(report)
    }

    pub fn stale(&self, now: u64) -> bool {
        self.report.finished_at_ms > now.saturating_add(30_000)
            || now
                > self.report.finished_at_ms.saturating_add(
                    (self.report.interval_seconds + self.report.timeout_seconds + 60) * 1000,
                )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input {
            sweep_id: "a".repeat(32),
            started_at_ms: 1000,
            finished_at_ms: 2000,
            turn_status: "blocked".into(),
            text: "Diagnostic coverage unavailable".into(),
            truncated: false,
            interval_seconds: 900,
            timeout_seconds: 180,
        }
    }

    #[test]
    fn report_digest_preserves_serialized_wire_bytes() {
        assert_eq!(
            input().digest(),
            "8f0c8d31e360d95fcf8943c0b60311eb5b030e0529987f2f783a9f1078c13737"
        );
    }

    #[test]
    fn report_bounds_integrity_and_staleness_preserve_blocked_state() {
        let mut report = Report {
            version: 1,
            identity: Identity {
                registration_uid: "reg".into(),
                source_namespace: "kars-system".into(),
                source_uid: "source".into(),
                namespace_uid: "ns".into(),
            },
            privacy_epoch: "epoch".into(),
            accepted_at_ms: 2000,
            digest: input().digest(),
            report: input(),
        };
        assert!(Report::decode(&serde_json::to_string(&report).unwrap()).is_ok());
        assert!(!report.stale(2000));
        assert!(report.stale(1_200_000));
        assert_eq!(report.report.turn_status, "blocked");
        report.report.text = "changed".into();
        assert!(Report::decode(&serde_json::to_string(&report).unwrap()).is_err());
        for text in ["x".repeat(MAX_TEXT + 1), "\u{1b}hidden".into(), " ".into()] {
            let mut bad = input();
            bad.text = text;
            assert!(bad.validate().is_err());
        }
    }

    #[test]
    fn owned_target_requires_registration_and_exact_source_not_a_name_match() {
        let identity = Identity {
            registration_uid: "reg".into(),
            source_namespace: "system".into(),
            source_uid: "source".into(),
            namespace_uid: "ns".into(),
        };
        let mut target = identity.target();
        target["metadata"]["uid"] = "cm-uid".into();
        target["metadata"]["resourceVersion"] = "1".into();
        assert!(identity.matches_target(&target));
        let mut changed = identity.clone();
        changed.source_uid = "replacement".into();
        assert!(changed.owns_target(&target));
        assert!(!changed.matches_target(&target));
        changed.registration_uid = "new-registration".into();
        assert!(!changed.owns_target(&target));
        target["metadata"]["ownerReferences"] = json!([]);
        assert!(!identity.matches_target(&target));
    }
}
