// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The conformance run report — the wire contract between this binary
//! and the slice-6.3 `KarsEval` reconciler.
//!
//! The runner writes one [`RunReport`] per invocation to both
//! `--output <path>` and stdout (one-shot JSON, no trailing newline).
//! The reconciler reads the file by mounting a shared volume; the
//! stdout copy is the fallback path consumed via `kubectl logs`.
//!
//! ### Frozen wire fields
//!
//! - `schemaVersion`: `"v1"`. Bumped only if a field is **removed** or
//!   semantically changed; new optional fields are forward-compatible.
//! - `corpusName`: stable name from the corpus.
//! - `corpusDigest`: `sha256:<hex>` over the raw signed bytes.
//! - `startedAt` / `completedAt`: RFC3339 UTC strings.
//! - `total` / `passed` / `failed`: case counts.
//! - `results[].verdict`: tagged enum mirroring [`kars_eval_corpus::Verdict`].
//!
//! Adding a field: append with `#[serde(skip_serializing_if = "Option::is_none")]`.
//! Removing one: bump `REPORT_SCHEMA_VERSION` and update the reconciler in lockstep.

use kars_eval_corpus::{
    ActualDecision, Case, Expect, ObservedSample, Scenario, Verdict, VerdictFailure,
};
use serde::Serialize;

/// Frozen at `"v1"`. Bump only on incompatible field changes.
pub const REPORT_SCHEMA_VERSION: &str = "v1";

#[derive(Debug, Serialize)]
pub struct RunReport {
    #[serde(rename = "schemaVersion")]
    pub schema_version: &'static str,
    #[serde(rename = "corpusName")]
    pub corpus_name: String,
    #[serde(rename = "corpusDigest")]
    pub corpus_digest: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "completedAt")]
    pub completed_at: String,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
    #[serde(rename = "routerBase")]
    pub router_base: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<CaseReport>,
}

#[derive(Debug, Serialize)]
pub struct CaseReport {
    #[serde(rename = "caseId")]
    pub case_id: String,
    pub tags: Vec<String>,
    pub scenario: ScenarioWire,
    pub expected: ExpectWire,
    pub actual: ActualWire,
    pub verdict: VerdictWire,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
pub enum ScenarioWire {
    EgressConnect {
        host: String,
        port: u16,
    },
    ChatCompletion {
        #[serde(rename = "messageCount")]
        message_count: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    ToolCall {
        tool: String,
        #[serde(rename = "burstCount", skip_serializing_if = "Option::is_none")]
        burst_count: Option<u32>,
    },
    MemoryRead {
        scope: String,
        key: String,
    },
}

#[derive(Debug, Serialize)]
pub struct ExpectWire {
    pub decision: &'static str,
    #[serde(
        rename = "decisionAtLeastSome",
        skip_serializing_if = "Option::is_none"
    )]
    pub decision_at_least_some: Option<&'static str>,
    #[serde(rename = "byPolicyKind", skip_serializing_if = "Option::is_none")]
    pub by_policy_kind: Option<&'static str>,
    #[serde(rename = "reasonContains", skip_serializing_if = "Option::is_none")]
    pub reason_contains: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ActualWire {
    pub decision: &'static str,
    #[serde(rename = "byPolicyKind", skip_serializing_if = "Option::is_none")]
    pub by_policy_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<ObservationWire>,
}

#[derive(Debug, Serialize)]
pub struct ObservationWire {
    pub seq: u32,
    pub decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "result")]
pub enum VerdictWire {
    Pass,
    Fail {
        #[serde(flatten)]
        failure: FailureWire,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "reason")]
pub enum FailureWire {
    DecisionMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    DecisionAtLeastSomeMissing {
        expected: &'static str,
        #[serde(rename = "observedCount")]
        observed_count: usize,
    },
    ByPolicyKindMismatch {
        expected: &'static str,
        actual: Option<&'static str>,
    },
    ReasonContainsMissing {
        needle: String,
        actual: Option<String>,
    },
}

// ─────────────────────────── conversions ───────────────────────────

pub fn scenario_to_wire(s: &Scenario) -> ScenarioWire {
    match s {
        Scenario::EgressConnect { host, port } => ScenarioWire::EgressConnect {
            host: host.clone(),
            port: *port,
        },
        Scenario::ChatCompletion { messages, model } => ScenarioWire::ChatCompletion {
            message_count: messages.len(),
            model: model.clone(),
        },
        Scenario::ToolCall { tool, burst, .. } => ScenarioWire::ToolCall {
            tool: tool.clone(),
            burst_count: burst.as_ref().map(|b| b.count),
        },
        Scenario::MemoryRead { scope, key } => ScenarioWire::MemoryRead {
            scope: scope.clone(),
            key: key.clone(),
        },
    }
}

pub fn expect_to_wire(e: &Expect) -> ExpectWire {
    ExpectWire {
        decision: e.decision.as_wire(),
        decision_at_least_some: e.decision_at_least_some.map(|d| d.as_wire()),
        by_policy_kind: e.by_policy_kind.map(|k| k.as_wire()),
        reason_contains: e.reason_contains.clone(),
    }
}

pub fn actual_to_wire(a: &ActualDecision) -> ActualWire {
    ActualWire {
        decision: a.decision.as_wire(),
        by_policy_kind: a.by_policy_kind.map(|k| k.as_wire()),
        reason: a.reason.clone(),
        observations: a.observations.iter().map(observation_to_wire).collect(),
    }
}

fn observation_to_wire(o: &ObservedSample) -> ObservationWire {
    ObservationWire {
        seq: o.seq,
        decision: o.decision.as_wire(),
        reason: o.reason.clone(),
    }
}

pub fn verdict_to_wire(v: &Verdict) -> VerdictWire {
    match v {
        Verdict::Pass => VerdictWire::Pass,
        Verdict::Fail(f) => VerdictWire::Fail {
            failure: failure_to_wire(f),
        },
    }
}

fn failure_to_wire(f: &VerdictFailure) -> FailureWire {
    match f {
        VerdictFailure::DecisionMismatch { expected, actual } => FailureWire::DecisionMismatch {
            expected: expected.as_wire(),
            actual: actual.as_wire(),
        },
        VerdictFailure::DecisionAtLeastSomeMissing {
            expected,
            observed_count,
        } => FailureWire::DecisionAtLeastSomeMissing {
            expected: expected.as_wire(),
            observed_count: *observed_count,
        },
        VerdictFailure::ByPolicyKindMismatch { expected, actual } => {
            FailureWire::ByPolicyKindMismatch {
                expected: expected.as_wire(),
                actual: actual.map(|k| k.as_wire()),
            }
        }
        VerdictFailure::ReasonContainsMissing { needle, actual } => {
            FailureWire::ReasonContainsMissing {
                needle: needle.clone(),
                actual: actual.clone(),
            }
        }
    }
}

/// Build a [`CaseReport`] from the case + replay outputs.
pub fn build_case_report(
    case: &Case,
    actual: &ActualDecision,
    verdict: &Verdict,
    duration_ms: u64,
) -> CaseReport {
    CaseReport {
        case_id: case.id.clone(),
        tags: case.tags.clone(),
        scenario: scenario_to_wire(&case.scenario),
        expected: expect_to_wire(&case.expect),
        actual: actual_to_wire(actual),
        verdict: verdict_to_wire(verdict),
        duration_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kars_eval_corpus::{Burst, ChatMessage, Decision, PolicyKindRef};
    use serde_json::Value;

    fn dec(d: Decision) -> &'static str {
        d.as_wire()
    }

    #[test]
    fn schema_version_is_v1() {
        assert_eq!(REPORT_SCHEMA_VERSION, "v1");
    }

    #[test]
    fn scenario_egress_connect_round_trip() {
        let s = Scenario::EgressConnect {
            host: "evil.example.com".into(),
            port: 443,
        };
        let w = scenario_to_wire(&s);
        let j = serde_json::to_value(&w).unwrap();
        assert_eq!(j["kind"], "EgressConnect");
        assert_eq!(j["host"], "evil.example.com");
        assert_eq!(j["port"], 443);
    }

    #[test]
    fn scenario_chat_completion_records_message_count_not_content() {
        let s = Scenario::ChatCompletion {
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "secret jailbreak payload".into(),
            }],
            model: Some("gpt-4o".into()),
        };
        let w = scenario_to_wire(&s);
        let j = serde_json::to_value(&w).unwrap();
        assert_eq!(j["kind"], "ChatCompletion");
        assert_eq!(j["messageCount"], 1);
        assert_eq!(j["model"], "gpt-4o");
        // Privacy: content must not appear in the wire report.
        assert!(!j.to_string().contains("secret jailbreak payload"));
    }

    #[test]
    fn scenario_chat_completion_omits_model_when_none() {
        let s = Scenario::ChatCompletion {
            messages: vec![],
            model: None,
        };
        let w = scenario_to_wire(&s);
        let j = serde_json::to_value(&w).unwrap();
        assert!(!j.as_object().unwrap().contains_key("model"));
    }

    #[test]
    fn scenario_tool_call_with_burst_records_count() {
        let s = Scenario::ToolCall {
            tool: "search".into(),
            args: None,
            burst: Some(Burst {
                count: 50,
                window_ms: 1000,
            }),
        };
        let w = scenario_to_wire(&s);
        let j = serde_json::to_value(&w).unwrap();
        assert_eq!(j["tool"], "search");
        assert_eq!(j["burstCount"], 50);
    }

    #[test]
    fn scenario_tool_call_omits_burst_count_when_absent() {
        let s = Scenario::ToolCall {
            tool: "search".into(),
            args: None,
            burst: None,
        };
        let j = serde_json::to_value(scenario_to_wire(&s)).unwrap();
        assert!(!j.as_object().unwrap().contains_key("burstCount"));
    }

    #[test]
    fn expect_serializes_optional_fields_only_when_set() {
        let e = Expect {
            decision: Decision::Blocked,
            decision_at_least_some: None,
            by_policy_kind: Some(PolicyKindRef::EgressAllowlist),
            reason_contains: Some("not in allowlist".into()),
        };
        let j = serde_json::to_value(expect_to_wire(&e)).unwrap();
        assert_eq!(j["decision"], "Blocked");
        assert!(!j.as_object().unwrap().contains_key("decisionAtLeastSome"));
        assert_eq!(j["byPolicyKind"], "EgressAllowlist");
        assert_eq!(j["reasonContains"], "not in allowlist");
    }

    #[test]
    fn actual_serializes_observations_only_when_non_empty() {
        let a = ActualDecision {
            decision: Decision::Allowed,
            by_policy_kind: None,
            reason: None,
            observations: vec![],
        };
        let j = serde_json::to_value(actual_to_wire(&a)).unwrap();
        assert!(!j.as_object().unwrap().contains_key("observations"));
    }

    #[test]
    fn verdict_pass_serializes_as_tagged_pass() {
        let v = Verdict::Pass;
        let j = serde_json::to_value(verdict_to_wire(&v)).unwrap();
        assert_eq!(j["result"], "Pass");
    }

    #[test]
    fn verdict_fail_decision_mismatch_serializes_with_reason_tag() {
        let v = Verdict::Fail(VerdictFailure::DecisionMismatch {
            expected: Decision::Blocked,
            actual: Decision::Allowed,
        });
        let j = serde_json::to_value(verdict_to_wire(&v)).unwrap();
        assert_eq!(j["result"], "Fail");
        assert_eq!(j["reason"], "DecisionMismatch");
        assert_eq!(j["expected"], "Blocked");
        assert_eq!(j["actual"], "Allowed");
    }

    #[test]
    fn verdict_fail_by_policy_kind_null_actual_is_explicit() {
        let v = Verdict::Fail(VerdictFailure::ByPolicyKindMismatch {
            expected: PolicyKindRef::ToolPolicy,
            actual: None,
        });
        let j = serde_json::to_value(verdict_to_wire(&v)).unwrap();
        assert_eq!(j["reason"], "ByPolicyKindMismatch");
        assert_eq!(j["actual"], Value::Null);
    }

    #[test]
    fn verdict_fail_reason_contains_missing_carries_needle() {
        let v = Verdict::Fail(VerdictFailure::ReasonContainsMissing {
            needle: "expected substring".into(),
            actual: Some("something else".into()),
        });
        let j = serde_json::to_value(verdict_to_wire(&v)).unwrap();
        assert_eq!(j["needle"], "expected substring");
        assert_eq!(j["actual"], "something else");
    }

    #[test]
    fn build_case_report_carries_all_inputs() {
        let case = Case {
            id: "c1".into(),
            tags: vec!["egress".into()],
            scenario: Scenario::EgressConnect {
                host: "evil.example.com".into(),
                port: 443,
            },
            expect: Expect {
                decision: Decision::Blocked,
                decision_at_least_some: None,
                by_policy_kind: Some(PolicyKindRef::EgressAllowlist),
                reason_contains: None,
            },
        };
        let actual = ActualDecision {
            decision: Decision::Blocked,
            by_policy_kind: Some(PolicyKindRef::EgressAllowlist),
            reason: None,
            observations: vec![],
        };
        let v = Verdict::Pass;
        let r = build_case_report(&case, &actual, &v, 17);
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["caseId"], "c1");
        assert_eq!(j["tags"][0], "egress");
        assert_eq!(j["scenario"]["kind"], "EgressConnect");
        assert_eq!(j["expected"]["decision"], dec(Decision::Blocked));
        assert_eq!(j["actual"]["decision"], dec(Decision::Blocked));
        assert_eq!(j["verdict"]["result"], "Pass");
        assert_eq!(j["durationMs"], 17);
    }
}
