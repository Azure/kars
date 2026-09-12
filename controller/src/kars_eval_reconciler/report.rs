// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded producer/consumer wire validation and a privacy-safe case projection.

use kars_eval_corpus::Corpus;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_REPORT_BYTES: usize = 256 * 1024;
pub const MAX_CASES: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub wire: Value,
    pub version: String,
    pub corpus_digest: String,
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub errored: u32,
}

impl Report {
    pub fn qualified(&self) -> bool {
        self.version == "v2"
    }

    pub fn ready(&self) -> bool {
        self.qualified()
            && self.total > 0
            && self.passed == self.total
            && self.failed == 0
            && self.errored == 0
    }

    pub fn exit_code(&self) -> i32 {
        if self.total == 0 || self.errored > 0 {
            2
        } else if self.failed > 0 {
            1
        } else {
            0
        }
    }
}

fn count(value: &Value, key: &str) -> Result<u32, &'static str> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n <= MAX_CASES as u64)
        .map(|n| n as u32)
        .ok_or("InvalidReportCounts")
}

fn text<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str, &'static str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .ok_or("InvalidReportField")
}

fn decision(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| matches!(*s, "Allowed" | "Blocked" | "RateLimited" | "BudgetExceeded"))
}

fn policy(value: &Value) -> bool {
    value.is_null()
        || value.as_str().is_some_and(|s| {
            matches!(
                s,
                "EgressAllowlist" | "InferencePolicy" | "ToolPolicy" | "KarsMemory" | "McpServer"
            )
        })
}

pub fn parse(
    log: &str,
    corpus: &Corpus,
    digest: &str,
    router: &str,
) -> Result<Report, &'static str> {
    if log.len() > MAX_REPORT_BYTES {
        return Err("ReportTooLarge");
    }
    let mut selected = None;
    let mut candidates = 0;
    let mut offset = 0;
    while let Some(next) = log[offset..].find('{') {
        offset += next;
        candidates += 1;
        if candidates > 32 {
            return Err("TooManyReports");
        }
        let mut stream = serde_json::Deserializer::from_str(&log[offset..]).into_iter::<Value>();
        let Some(Ok(value)) = stream.next() else {
            offset += 1;
            continue;
        };
        offset += stream.byte_offset();
        if value.get("schemaVersion").is_none() {
            continue;
        }
        let parsed = validate(&value, corpus, digest, router)?;
        if selected.as_ref().is_some_and(|old| old != &parsed) {
            return Err("ConflictingReports");
        }
        selected = Some(parsed);
    }
    selected.ok_or("ReportMissingOrMalformed")
}

pub fn validate(
    value: &Value,
    corpus: &Corpus,
    digest: &str,
    router: &str,
) -> Result<Report, &'static str> {
    let version = text(value, "schemaVersion", 8)?;
    if !matches!(version, "v1" | "v2") {
        return Err("UnsupportedReportVersion");
    }
    let v2 = version == "v2";
    let total = count(value, "total")?;
    let passed = count(value, "passed")?;
    let failed = count(value, "failed")?;
    let errored = if v2 { count(value, "errored")? } else { 0 };
    if total != passed + failed + errored {
        return Err("InvalidReportCounts");
    }
    let outcome = if total == 0 || errored > 0 {
        "Inconclusive"
    } else if failed > 0 {
        "PolicyFailure"
    } else {
        "Passed"
    };
    if v2
        && value
            .get("outcome")
            .is_some_and(|value| value.as_str() != Some(outcome))
    {
        return Err("InvalidReportCounts");
    }
    let reported_digest = value
        .get("corpusDigest")
        .and_then(Value::as_str)
        .unwrap_or("");
    if reported_digest.len() > 80 {
        return Err("InvalidCorpusDigest");
    }
    if v2
        && (reported_digest != digest
            || value["corpusName"] != corpus.name
            || value["routerBase"] != router
            || total as usize != corpus.cases.len())
    {
        return Err("ReportAttributionMismatch");
    }
    let cases = value["results"].as_array().ok_or("InvalidReportCases")?;
    if cases.len() != total as usize {
        return Err("InvalidReportCounts");
    }
    let mut ids = BTreeSet::new();
    let mut counts = [0u32; 3];
    let mut safe_cases = Vec::new();
    for case in cases {
        let id = text(case, "caseId", 256)?;
        if !ids.insert(id.to_string()) {
            return Err("DuplicateReportCase");
        }
        let expected = corpus.cases.iter().find(|candidate| candidate.id == id);
        if v2 && expected.is_none() {
            return Err("ReportAttributionMismatch");
        }
        let declared = decision(&case["expected"]["decision"]).ok_or("InvalidExpectedDecision")?;
        if v2 && expected.is_some_and(|expected| expected.expect.decision.as_wire() != declared) {
            return Err("ReportAttributionMismatch");
        }
        let mut safe_expected = json!({"decision": declared});
        if !policy(&case["expected"]["byPolicyKind"]) {
            return Err("InvalidExpectedDecision");
        }
        if !case["expected"]["byPolicyKind"].is_null() {
            safe_expected["byPolicyKind"] = case["expected"]["byPolicyKind"].clone();
        }
        if v2
            && expected.is_some_and(|expected| {
                case["expected"]["byPolicyKind"]
                    != json!(expected.expect.by_policy_kind.map(|kind| kind.as_wire()))
                    || case["expected"]["decisionAtLeastSome"]
                        != json!(
                            expected
                                .expect
                                .decision_at_least_some
                                .map(|decision| decision.as_wire())
                        )
            })
        {
            return Err("ReportAttributionMismatch");
        }
        let result = text(&case["verdict"], "result", 16)?;
        let mut verdict = json!({"result": result});
        let mut actual = Value::Null;
        if result == "Errored" {
            let category = text(&case["verdict"], "category", 32)?;
            if !v2
                || !case["actual"].is_null()
                || !matches!(
                    category,
                    "Transport"
                        | "Timeout"
                        | "Authentication"
                        | "Upstream"
                        | "Protocol"
                        | "BodyRead"
                        | "BodyTooLarge"
                )
            {
                return Err("InvalidInconclusiveCase");
            }
            verdict["category"] = json!(category);
            counts[2] += 1;
        } else {
            let actual_decision =
                decision(&case["actual"]["decision"]).ok_or("InvalidActualDecision")?;
            if !policy(&case["actual"]["byPolicyKind"]) {
                return Err("InvalidActualPolicy");
            }
            actual = json!({"decision": actual_decision});
            if !case["actual"]["byPolicyKind"].is_null() {
                actual["byPolicyKind"] = case["actual"]["byPolicyKind"].clone();
            }
            let expected_decision =
                decision(&case["expected"]["decision"]).ok_or("InvalidExpectedDecision")?;
            if v2
                && expected
                    .is_some_and(|expected| expected.expect.decision.as_wire() != expected_decision)
            {
                return Err("ReportAttributionMismatch");
            }
            let observations = match case["actual"].get("observations") {
                Some(value) => value.as_array().ok_or("InvalidObservations")?.as_slice(),
                None => &[],
            };
            if observations.len() > 4096
                || observations.iter().enumerate().any(|(seq, observation)| {
                    observation["seq"].as_u64() != Some(seq as u64)
                        || decision(&observation["decision"]).is_none()
                })
            {
                return Err("InvalidObservations");
            }
            let at_least_some = expected.and_then(|case| case.expect.decision_at_least_some);
            let has_required = at_least_some.is_none_or(|required| {
                observations
                    .iter()
                    .any(|observation| observation["decision"] == required.as_wire())
            });
            if !observations.is_empty() {
                actual["observations"] = json!(observations.iter().map(|observation|
                    json!({"seq":observation["seq"],"decision":observation["decision"]})).collect::<Vec<_>>());
            }
            match result {
                "Pass" => {
                    if actual_decision != expected_decision {
                        return Err("InvalidPassVerdict");
                    }
                    if v2
                        && expected
                            .and_then(|case| case.expect.by_policy_kind)
                            .is_some_and(|kind| case["actual"]["byPolicyKind"] != kind.as_wire())
                    {
                        return Err("InvalidPassVerdict");
                    }
                    if v2 && !has_required {
                        return Err("InvalidPassVerdict");
                    }
                    counts[0] += 1;
                }
                "Fail" => {
                    let reason = text(&case["verdict"], "reason", 40)?;
                    if !matches!(
                        reason,
                        "DecisionMismatch"
                            | "DecisionAtLeastSomeMissing"
                            | "ByPolicyKindMismatch"
                            | "ReasonContainsMissing"
                    ) {
                        return Err("InvalidFailVerdict");
                    }
                    if (reason == "DecisionMismatch") != (actual_decision != expected_decision) {
                        return Err("InvalidFailVerdict");
                    }
                    if v2
                        && ((reason == "DecisionAtLeastSomeMissing"
                            && (at_least_some.is_none() || has_required))
                            || (reason == "ReasonContainsMissing"
                                && expected
                                    .is_none_or(|case| case.expect.reason_contains.is_none()))
                            || (reason == "ByPolicyKindMismatch"
                                && expected
                                    .and_then(|case| case.expect.by_policy_kind)
                                    .is_none_or(|kind| {
                                        case["actual"]["byPolicyKind"] == kind.as_wire()
                                    })))
                    {
                        return Err("InvalidFailVerdict");
                    }
                    verdict["reason"] = json!(reason);
                    counts[1] += 1;
                }
                _ => return Err("InvalidCaseVerdict"),
            }
        }
        let duration = case["durationMs"].as_u64().ok_or("InvalidCaseDuration")?;
        safe_cases.push(json!({
            "caseId":id, "tags":[], "durationMs":duration, "actual":actual, "expected":safe_expected,
            "verdict":verdict, "pass":match result {"Pass"=>Some(true),"Fail"=>Some(false),_=>None},
            "errored":result=="Errored",
        }));
    }
    if counts != [passed, failed, errored] {
        return Err("InvalidReportCounts");
    }
    for key in ["startedAt", "completedAt"] {
        let stamp = text(value, key, 64)?;
        chrono::DateTime::parse_from_rfc3339(stamp).map_err(|_| "InvalidReportTime")?;
    }
    let started = chrono::DateTime::parse_from_rfc3339(
        value["startedAt"].as_str().ok_or("InvalidReportTime")?,
    )
    .map_err(|_| "InvalidReportTime")?;
    let completed = chrono::DateTime::parse_from_rfc3339(
        value["completedAt"].as_str().ok_or("InvalidReportTime")?,
    )
    .map_err(|_| "InvalidReportTime")?;
    if completed < started {
        return Err("InvalidReportTime");
    }
    let wire = json!({
        "schemaVersion": version, "corpusDigest": reported_digest,
        "corpusName": text(value, "corpusName", 256)?,
        "startedAt": value["startedAt"], "completedAt": value["completedAt"],
        "total": total, "passed": passed, "failed": failed, "errored": errored, "results": safe_cases,
    });
    Ok(Report {
        wire,
        version: version.into(),
        corpus_digest: reported_digest.into(),
        total,
        passed,
        failed,
        errored,
    })
}

#[cfg(test)]
#[path = "report/tests.rs"]
pub(super) mod tests;
