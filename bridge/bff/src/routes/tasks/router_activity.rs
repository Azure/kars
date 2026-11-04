// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};

use super::models::{MissionArtifactDto, MissionResultDto};
use crate::kars::task::KarsTask;

pub(crate) const ARTIFACT: &str = "kars-router-observations.json";
pub(crate) const MAX_ACTIVITY_BYTES: usize = 384 * 1024;
pub(crate) const MAX_MISSION_BYTES: usize = 192 * 1024 + MAX_ACTIVITY_BYTES;
const MAX_OBSERVATION_ROUNDS: usize = 130;
const MAX_OBSERVATION_EVENTS: usize = 1024;
const SOURCE: &str = "runtime-forwarded-router-observations";
const COVERAGE: &str = "returned-responses-only";
const NATIVE_COVERAGE: &str = "native-tool-correlated-only";
const NATIVE_UNAVAILABLE: &str =
    "Native tool-correlated observations unavailable (reset, expiry, truncation or read failure)";
const MAX_SAFE: u64 = 9_007_199_254_740_991;

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct RouterActivity {
    version: u8,
    source: String,
    coverage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    responses: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    correlated_tools: Option<u64>,
    scope_id: Option<String>,
    rounds: Vec<u64>,
    #[serde(flatten)]
    observation: Observation,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Observation {
    Observed { trace: Trace },
    Unavailable { reason: String },
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct Trace {
    scope_id: String,
    rounds: Vec<u64>,
    coverage: String,
    durable: bool,
    missing_rounds: Vec<u64>,
    dropped_events: u64,
    truncated: bool,
    events: Vec<Event>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct Event {
    scope_id: String,
    round: u64,
    seq: u64,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_at: Option<String>,
    #[serde(flatten)]
    detail: EventDetail,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EventDetail {
    RouterOperation {
        call_id: String,
        name: String,
        operation: String,
        flag: String,
        allowed: Option<bool>,
        http_status: Option<u16>,
        complete: Option<bool>,
        destination_host: Option<String>,
        destination_port: Option<u16>,
        ms: u64,
    },
    Round {
        provider: String,
        model: Option<String>,
        http_status: Option<u16>,
        accepted: Option<bool>,
        outcome: String,
        usage: Option<Usage>,
        usage_state: String,
        finish_reason: Option<String>,
        partial_observation: bool,
        ms: u64,
        tool_calls_observed: u64,
    },
    ToolProposed {
        call_id: String,
        name: String,
        ok: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument_preview: Option<ToolPreview>,
    },
    ToolResult {
        call_id: String,
        name: String,
        ok: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reported_in_round: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_summary: Option<ToolSummary>,
    },
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolPreview {
    text: String,
    truncated: bool,
    redacted: bool,
}

impl ToolPreview {
    fn valid(&self) -> bool {
        !self.text.is_empty()
            && self.text.len() <= 2048
            && !self
                .text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToolSummary {
    FileRead {
        bytes: u64,
        returned_bytes: u64,
        truncated: bool,
    },
    FileWrite {
        bytes: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_name: Option<String>,
    },
    PythonExecution {
        bytes: u64,
        receipt_sha256: String,
        #[serde(deserialize_with = "Option::deserialize")]
        exit_code: Option<i64>,
        reason: String,
    },
    Failure {
        reason: String,
    },
}

impl ToolSummary {
    fn valid(&self, name: &str, ok: Option<bool>) -> bool {
        match self {
            Self::FileRead {
                bytes,
                returned_bytes,
                truncated,
            } => {
                ok == Some(true)
                    && matches!(
                        name,
                        "file_read" | "read_file" | "kars_mission_read" | "kars_mission_file_read"
                    )
                    && *bytes <= MAX_SAFE
                    && *returned_bytes <= 16 * 1024 * 1024
                    && returned_bytes <= bytes
                    && *truncated == (returned_bytes < bytes)
            }
            Self::FileWrite {
                bytes,
                artifact_name,
            } => {
                ok == Some(true)
                    && matches!(
                        name,
                        "file_write"
                            | "write_file"
                            | "kars_mission_write"
                            | "kars_mission_file_write"
                    )
                    && *bytes <= 16 * 1024 * 1024
                    && artifact_name.as_ref().is_none_or(|name| {
                        !name.is_empty()
                            && name.len() <= 128
                            && !name.starts_with('.')
                            && name
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                    })
            }
            Self::PythonExecution {
                bytes,
                receipt_sha256,
                exit_code,
                reason,
            } => {
                matches!(name, "python_execute" | "kars_mission_python_execute")
                    && (1..=65536).contains(bytes)
                    && receipt_sha256
                        .strip_prefix("sha256:")
                        .is_some_and(|digest| {
                            digest.len() == 64
                                && digest
                                    .bytes()
                                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                        })
                    && exit_code.is_none_or(|code| (-2_147_483_647..=2_147_483_647).contains(&code))
                    && [
                        "exited",
                        "timeout",
                        "cancelled",
                        "output_limit",
                        "process_limit",
                        "launch_failed",
                        "cleanup_failed",
                    ]
                    .contains(&reason.as_str())
                    && ok == Some(reason == "exited" && *exit_code == Some(0))
            }
            Self::Failure { reason } => {
                ok == Some(false)
                    && [
                        "filesystem_operation_failed",
                        "invalid_arguments",
                        "policy_denied",
                        "reviewed_phase_denied",
                        "tool_execution_failed",
                    ]
                    .contains(&reason.as_str())
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl RouterActivity {
    pub(super) fn observation_count(&self) -> u64 {
        if self.coverage == NATIVE_COVERAGE {
            self.correlated_tools.unwrap_or(0)
        } else {
            self.responses.unwrap_or(0)
        }
    }

    pub(super) fn coverage(&self) -> &str {
        &self.coverage
    }

    pub(super) fn scope_id(&self) -> Option<&str> {
        self.scope_id.as_deref()
    }

    /// Validated, partial display observations; never whole-run accounting.
    pub(super) fn observed_events(&self) -> Option<Vec<serde_json::Value>> {
        match &self.observation {
            Observation::Observed { trace } => Some(
                trace
                    .events
                    .iter()
                    .filter_map(|event| serde_json::to_value(event).ok())
                    .collect(),
            ),
            Observation::Unavailable { .. } => None,
        }
    }
}

fn text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn scope(value: &str) -> bool {
    text(value, 128)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}
fn ordered(ids: &[u64]) -> bool {
    ids.len() <= MAX_OBSERVATION_ROUNDS
        && ids.iter().all(|n| *n > 0 && *n <= MAX_SAFE)
        && ids.windows(2).all(|pair| pair[0] < pair[1])
}

impl Event {
    fn valid(&self, selection: &RouterActivity) -> bool {
        if Some(&self.scope_id) != selection.scope_id.as_ref()
            || !selection.rounds.contains(&self.round)
            || self.seq == 0
            || self.seq > MAX_SAFE
            || self.observed_at.as_ref().is_some_and(|value| {
                value.len() != 24
                    || chrono::DateTime::parse_from_rfc3339(value).map_or(true, |time| {
                        time.with_timezone(&chrono::Utc)
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                            != *value
                    })
            })
        {
            return false;
        }
        match &self.detail {
            EventDetail::RouterOperation {
                call_id,
                name,
                operation,
                flag,
                allowed,
                http_status,
                complete,
                destination_host,
                destination_port,
                ms,
            } => {
                scope(call_id)
                    && scope(name)
                    && *ms <= 3_600_000
                    && destination_host.is_some() == destination_port.is_some()
                    && destination_host.as_ref().is_none_or(|host| {
                        !host.is_empty()
                            && host.len() <= 253
                            && host
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || b".-:[]".contains(&c))
                    })
                    && destination_port.is_none_or(|port| port > 0)
                    && match operation.as_str() {
                        "tool_policy" => {
                            self.source == "router-policy"
                                && allowed.is_some()
                                && http_status.is_none()
                                && complete.is_none()
                                && destination_host.is_none()
                                && (flag == "governance"
                                    || (flag == "rate_limit" && *allowed == Some(false)))
                        }
                        "egress_policy" => {
                            self.source == "router-policy"
                                && allowed.is_some()
                                && http_status.is_none()
                                && complete.is_none()
                                && (flag == "egress_check"
                                    || (["private_target", "dispatch_recheck"]
                                        .contains(&flag.as_str())
                                        && *allowed == Some(false)))
                        }
                        "egress_fetch" => {
                            self.source == "router"
                                && allowed.is_none()
                                && match flag.as_str() {
                                    "response" => {
                                        complete.is_some()
                                            && http_status.is_some_and(|s| (100..=599).contains(&s))
                                    }
                                    "body_error" => {
                                        *complete == Some(false)
                                            && http_status.is_some_and(|s| (100..=599).contains(&s))
                                    }
                                    "transport_error" => {
                                        *complete == Some(false) && http_status.is_none()
                                    }
                                    _ => false,
                                }
                        }
                        _ => false,
                    }
            }
            EventDetail::Round {
                provider,
                model,
                http_status,
                accepted: _,
                outcome,
                usage,
                usage_state,
                finish_reason,
                partial_observation: _,
                ms,
                tool_calls_observed,
            } => {
                self.source == "router-upstream"
                    && text(provider, 256)
                    && model.as_ref().is_none_or(|v| text(v, 256))
                    && http_status.is_none_or(|v| (100..=599).contains(&v))
                    && text(outcome, 128)
                    && ["present", "partial", "missing"].contains(&usage_state.as_str())
                    && finish_reason.as_ref().is_none_or(|v| text(v, 128))
                    && *ms <= MAX_SAFE
                    && *tool_calls_observed <= MAX_SAFE
                    && usage.as_ref().is_none_or(|v| {
                        [v.prompt_tokens, v.completion_tokens, v.total_tokens]
                            .into_iter()
                            .flatten()
                            .all(|n| n <= MAX_SAFE)
                    })
            }
            EventDetail::ToolProposed {
                call_id,
                name,
                ok,
                argument_preview,
            } => {
                self.source == "model-proposed"
                    && text(call_id, 256)
                    && text(name, 256)
                    && ok.is_none()
                    && argument_preview.as_ref().is_none_or(ToolPreview::valid)
            }
            EventDetail::ToolResult {
                call_id,
                name,
                ok,
                reported_in_round,
                ms,
                result_summary,
            } => {
                self.source == "harness-reported"
                    && text(call_id, 256)
                    && text(name, 256)
                    && match reported_in_round {
                        Some(round) => *round > self.round && selection.rounds.contains(round),
                        None => ok.is_some(),
                    }
                    && ms.is_none_or(|value| value <= 3_600_000)
                    && result_summary
                        .as_ref()
                        .is_none_or(|value| value.valid(name, *ok))
            }
        }
    }
}

pub(super) fn parse(content: &str) -> Option<RouterActivity> {
    if content.len() > MAX_ACTIVITY_BYTES {
        return None;
    }
    let raw: serde_json::Value = serde_json::from_str(content).ok()?;
    let native = raw.get("coverage")?.as_str()? == NATIVE_COVERAGE;
    if native && (raw.get("responses").is_some() || raw.get("scope_id").is_none()) {
        return None;
    }
    // Null is not an absent reporting round. Immediate reports omit this field.
    if raw
        .pointer("/trace/events")
        .and_then(|v| v.as_array())
        .is_some_and(|events| {
            events.iter().any(|event| {
                event.get("kind").and_then(|v| v.as_str()) == Some("tool_result")
                    && event.get("reported_in_round").is_some_and(|v| v.is_null())
            })
        })
    {
        return None;
    }
    let value: RouterActivity = serde_json::from_value(raw).ok()?;
    if value.version != 1
        || value.source != SOURCE
        || ![COVERAGE, NATIVE_COVERAGE].contains(&value.coverage.as_str())
        || value.observation_count() == 0
        || value.observation_count()
            > if native && matches!(&value.observation, Observation::Observed { .. }) {
                256
            } else {
                MAX_SAFE
            }
        || (!native && value.correlated_tools.is_some())
        || !ordered(&value.rounds)
        || value.rounds.len() as u64 > value.observation_count()
        || value.scope_id.as_ref().is_some_and(|s| !scope(s))
    {
        return None;
    }
    match &value.observation {
        Observation::Unavailable { reason } => {
            let valid_reason = if native {
                reason == NATIVE_UNAVAILABLE
            } else {
                [
                    "Response correlation changed or was invalid",
                    "Router selection did not match returned response IDs",
                    "Router selection exceeded the observation artifact limit",
                    "Router observations unavailable after execution (reset, expiry or read failure)",
                ]
                .contains(&reason.as_str())
            };
            if !valid_reason {
                return None;
            }
        }
        Observation::Observed { trace } => {
            if value.rounds.is_empty()
                || Some(&trace.scope_id) != value.scope_id.as_ref()
                || trace.rounds != value.rounds
                || trace.coverage != value.coverage
                || trace.durable
                || !ordered(&trace.missing_rounds)
                || trace
                    .missing_rounds
                    .iter()
                    .any(|n| !value.rounds.contains(n))
                || trace.dropped_events > MAX_SAFE
                || trace.events.len() > MAX_OBSERVATION_EVENTS
                || trace
                    .events
                    .windows(2)
                    .any(|pair| pair[0].seq >= pair[1].seq)
                || trace.events.iter().any(|event| !event.valid(&value))
                || trace.events.iter().any(|event| match &event.detail {
                    EventDetail::RouterOperation { call_id, name, .. }
                    | EventDetail::ToolResult { call_id, name, reported_in_round: None, .. } => !trace.events.iter().any(|proposal|
                        proposal.seq < event.seq && proposal.round == event.round
                        && matches!(&proposal.detail, EventDetail::ToolProposed { call_id: id, name: tool, .. } if id == call_id && tool == name)),
                    _ => false,
                })
                || (native && !valid_native_trace(&value, trace))
            {
                return None;
            }
        }
    }
    Some(value)
}

fn valid_native_trace(selection: &RouterActivity, trace: &Trace) -> bool {
    use std::collections::{BTreeSet, HashMap, HashSet};
    if !trace.missing_rounds.is_empty() || trace.dropped_events != 0 || trace.truncated {
        return false;
    }
    let mut proposals = HashMap::new();
    let mut completed = HashSet::new();
    let mut proposing_rounds = BTreeSet::new();
    let mut accepted_rounds = BTreeSet::new();
    for event in &trace.events {
        match &event.detail {
            EventDetail::Round { accepted, .. } => {
                if *accepted != Some(true) || !accepted_rounds.insert(event.round) {
                    return false;
                }
            }
            EventDetail::ToolProposed { call_id, name, .. } => {
                if proposals.insert(call_id, (name, event.round)).is_some() {
                    return false;
                }
                proposing_rounds.insert(event.round);
            }
            EventDetail::RouterOperation { call_id, name, .. } => {
                if proposals.get(call_id) != Some(&(name, event.round)) {
                    return false;
                }
            }
            EventDetail::ToolResult { call_id, name, .. } => {
                if proposals.get(call_id) != Some(&(name, event.round))
                    || !completed.insert(call_id)
                {
                    return false;
                }
            }
        }
    }
    proposals.len() as u64 == selection.observation_count()
        && proposing_rounds
            .iter()
            .copied()
            .eq(selection.rounds.iter().copied())
        && accepted_rounds == proposing_rounds
}

/// These files already passed the query's immutable current-run artifact binding.
/// Response or tool correlations select observations; neither independently attests execution.
pub(super) fn from_artifacts(
    task: &KarsTask,
    result: Option<&MissionResultDto>,
    artifacts: &[MissionArtifactDto],
) -> Option<RouterActivity> {
    let result = result?;
    let evidence = result.run_evidence.as_ref()?;
    let nonce = task
        .metadata
        .annotations
        .as_ref()?
        .get("kars.azure.com/run-requested")?;
    if nonce.trim().is_empty()
        || result.source.as_deref() != Some("durable_agent")
        || !matches!(
            (result.status.as_deref(), evidence.status.as_str()),
            (Some("ok"), "succeeded") | (Some("failed"), "failed")
        )
        || task.metadata.uid.as_deref() != Some(evidence.task_uid.as_str())
        || nonce != &evidence.run_nonce
        || result.assignment_nonce.as_ref() != Some(nonce)
    {
        return None;
    }
    let mut selected = artifacts.iter().filter(|a| a.name == ARTIFACT);
    let artifact = selected.next()?;
    if selected.next().is_some() {
        return None;
    }
    parse(artifact.full_content.as_deref()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn native_fixture() -> Value {
        let mut value = fixture();
        value.as_object_mut().unwrap().remove("responses");
        value["coverage"] = json!(NATIVE_COVERAGE);
        value["correlated_tools"] = json!(1);
        value["rounds"] = json!([1]);
        value["trace"]["rounds"] = json!([1]);
        value["trace"]["coverage"] = json!(NATIVE_COVERAGE);
        value["trace"]["events"][0]["tool_calls_observed"] = json!(3);
        value["trace"]["events"].as_array_mut().unwrap().extend([
            json!({"kind":"tool_proposed","scope_id":"scope-a","round":1,"seq":2,
                "source":"model-proposed","call_id":"write-1","name":"kars_mission_file_write","ok":null}),
            json!({"kind":"tool_result","scope_id":"scope-a","round":1,"seq":5,
                "source":"harness-reported","call_id":"write-1","name":"kars_mission_file_write","ok":true,
                "result_summary":{"kind":"file_write","bytes":17,"artifact_name":"briefing.md"}}),
        ]);
        value
    }

    #[test]
    fn ordinary_128_tool_activity_preserves_both_runtime_coverages_and_rejects_late_corruption() {
        let cycle: Vec<Value> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../inference-router/tests/fixtures/router-observation-tool-cycle.json"
        )))
        .unwrap();
        for native in [true, false] {
            let coverage = if native { NATIVE_COVERAGE } else { COVERAGE };
            let rounds: Vec<u64> = (1..=if native { 128 } else { 129 }).collect();
            let mut events = Vec::new();
            for round in 1..=128 {
                for template in &cycle {
                    if template["kind"] == "tool_result"
                        && template.get("reported_in_round").is_some() == native
                    {
                        continue;
                    }
                    let mut event = template.clone();
                    event["round"] = json!(round);
                    event["seq"] = json!((round - 1) * 7 + template["seq"].as_u64().unwrap());
                    if event.get("call_id").is_some() {
                        event["call_id"] = json!(format!("call-{round}"));
                    }
                    if event.get("reported_in_round").is_some() {
                        event["reported_in_round"] = json!(round + 1);
                    }
                    events.push(event);
                }
            }
            if !native {
                let mut final_round = cycle[0].clone();
                final_round["round"] = json!(129);
                final_round["seq"] = json!(897);
                final_round["tool_calls_observed"] = json!(0);
                final_round["finish_reason"] = json!("stop");
                events.push(final_round);
            }
            let mut value = json!({"version":1,"source":"runtime-forwarded-router-observations",
                "coverage":coverage,"scope_id":cycle[0]["scope_id"],"rounds":rounds,"state":"observed",
                "trace":{"scope_id":cycle[0]["scope_id"],"rounds":rounds,"coverage":coverage,"durable":false,
                    "missing_rounds":[],"dropped_events":0,"truncated":false,"events":events}});
            value[if native {
                "correlated_tools"
            } else {
                "responses"
            }] = json!(if native { 128 } else { 129 });
            let serialized = value.to_string();
            assert!(serialized.len() > 128 * 1024 && serialized.len() < MAX_ACTIVITY_BYTES);
            let parsed = parse(&serialized).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
            for (pointer, invalid) in [
                ("/trace/events/260/scope_id", json!("reset-scope")),
                ("/trace/events/260/seq", json!(1)),
                ("/trace/events/259/name", json!("mismatched-tool")),
                ("/trace/missing_rounds", json!([64])),
            ] {
                let mut changed = value.clone();
                *changed.pointer_mut(pointer).unwrap() = invalid;
                assert_eq!(
                    parse(&changed.to_string()).is_none(),
                    native || pointer != "/trace/missing_rounds",
                    "{pointer}, native={native}"
                );
            }
        }
    }

    #[test]
    fn native_observations_preserve_actual_rounds_and_immediate_reports_without_response_ids() {
        let mut value = native_fixture();
        let parsed = parse(&value.to_string()).unwrap();
        assert_eq!(parsed.observation_count(), 1);
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        let mut file = artifact();
        file.full_content = Some(value.to_string());
        assert!(from_artifacts(&task(), Some(&result()), &[file]).is_some());
        value["trace"]["events"].as_array_mut().unwrap().pop();
        assert!(
            parse(&value.to_string()).is_some(),
            "a proposal need not yet be completed"
        );
        for name in [
            "kars_mission_file_read",
            "kars_mission_read",
            "file_read",
            "read_file",
        ] {
            let mut value = native_fixture();
            value["trace"]["events"][1]["name"] = json!(name);
            value["trace"]["events"][2]["name"] = json!(name);
            value["trace"]["events"][2]["result_summary"] =
                json!({"kind":"file_read","bytes":17,"returned_bytes":8,"truncated":true});
            assert!(parse(&value.to_string()).is_some(), "{name}");
        }
    }

    #[test]
    fn python_execution_receipts_preserve_both_runtime_names_and_failures() {
        for name in ["python_execute", "kars_mission_python_execute"] {
            for (reason, code) in [
                ("exited", Some(0)),
                ("exited", Some(3)),
                ("exited", Some(-15)),
                ("timeout", None),
                ("cancelled", None),
                ("output_limit", None),
                ("process_limit", None),
                ("launch_failed", None),
                ("cleanup_failed", None),
            ] {
                let mut value = native_fixture();
                value["trace"]["events"][1]["name"] = json!(name);
                value["trace"]["events"][2]["name"] = json!(name);
                let ok = reason == "exited" && code == Some(0);
                value["trace"]["events"][2]["ok"] = json!(ok);
                value["trace"]["events"][2]["result_summary"] = json!({
                    "kind":"python_execution", "bytes":512,
                    "receipt_sha256":format!("sha256:{}", "a".repeat(64)),
                    "exit_code":code, "reason":reason
                });
                assert_eq!(
                    serde_json::to_value(parse(&value.to_string()).unwrap()).unwrap(),
                    value
                );
                value["trace"]["events"][2]["ok"] = json!(!ok);
                assert!(parse(&value.to_string()).is_none());
            }
        }
    }

    #[test]
    fn python_execution_summary_rejects_unbound_and_malformed_receipts() {
        let summary = json!({"kind":"python_execution", "bytes":512,
            "receipt_sha256":format!("sha256:{}", "a".repeat(64)), "exit_code":0, "reason":"exited"});
        for (key, invalid) in [
            ("bytes", json!(0)),
            ("bytes", json!(65537)),
            (
                "receipt_sha256",
                json!(format!("sha256:{}", "A".repeat(64))),
            ),
            ("exit_code", json!(2147483648_i64)),
            ("exit_code", json!(0.5)),
            ("reason", json!("invented")),
            ("stdout", json!("unvalidated output")),
        ] {
            let mut changed = summary.clone();
            changed[key] = invalid;
            assert!(
                serde_json::from_value::<ToolSummary>(changed).map_or(true, |v| !v
                    .valid("kars_mission_python_execute", Some(true)))
            );
        }
        let parsed: ToolSummary = serde_json::from_value(summary.clone()).unwrap();
        assert!(!parsed.valid("file_write", Some(true)));
        let mut missing = summary;
        missing.as_object_mut().unwrap().remove("exit_code");
        assert!(serde_json::from_value::<ToolSummary>(missing).is_err());
    }

    #[test]
    fn bounded_argument_previews_survive_projection_for_both_coverage_modes() {
        for native in [true, false] {
            let mut value = native_fixture();
            if !native {
                value.as_object_mut().unwrap().remove("correlated_tools");
                value["responses"] = json!(2);
                value["coverage"] = json!(COVERAGE);
                value["trace"]["coverage"] = json!(COVERAGE);
                value["rounds"] = json!([1, 2]);
                value["trace"]["rounds"] = json!([1, 2]);
                value["trace"]["events"][2]["reported_in_round"] = json!(2);
            }
            value["trace"]["events"][1]["argument_preview"] = json!({"text":"{\n  \"path\": \"/sandbox/report.md\"\n}", "truncated":false, "redacted":true});
            assert_eq!(
                serde_json::to_value(parse(&value.to_string()).unwrap()).unwrap(),
                value
            );
            for invalid in [
                json!({"text":"x".repeat(2049), "truncated":true, "redacted":false}),
                json!({"text":"🦀".repeat(513), "truncated":false, "redacted":false}),
                json!({"text":"x", "truncated":false, "redacted":"yes"}),
                json!({"text":"x", "truncated":false, "redacted":true, "raw":"not permitted"}),
                json!({"text":"\0", "truncated":false, "redacted":false}),
            ] {
                value["trace"]["events"][1]["argument_preview"] = invalid;
                assert!(parse(&value.to_string()).is_none());
            }
        }
    }

    #[test]
    fn retained_hermes_native_trace_survives_typed_projection() {
        let content = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../inference-router/tests/fixtures/router-observations-hermes-native.json"
        ));
        let projected = parse(content).unwrap();
        assert_eq!(projected.observation_count(), 7);
        assert_eq!(projected.coverage(), COVERAGE);
        assert_eq!(
            serde_json::to_value(projected).unwrap(),
            serde_json::from_str::<Value>(content).unwrap()
        );
    }

    #[test]
    fn returned_responses_keep_proposal_correlated_immediate_results() {
        let mut value = native_fixture();
        value.as_object_mut().unwrap().remove("correlated_tools");
        value["responses"] = json!(1);
        value["coverage"] = json!(COVERAGE);
        value["trace"]["coverage"] = json!(COVERAGE);
        assert!(parse(&value.to_string()).is_some());
        for (field, replacement) in [
            ("call_id", json!("unrelated")),
            ("name", json!("unrelated")),
            ("round", json!(2)),
            ("reported_in_round", Value::Null),
            ("ok", Value::Null),
        ] {
            let mut invalid = value.clone();
            invalid["trace"]["events"][2][field] = replacement;
            assert!(parse(&invalid.to_string()).is_none(), "{field}");
        }
        value["trace"]["events"].as_array_mut().unwrap().remove(1);
        assert!(parse(&value.to_string()).is_none());
    }

    #[test]
    fn hermes_write_summary_preserves_returned_response_coverage() {
        let mut value = native_fixture();
        value.as_object_mut().unwrap().remove("correlated_tools");
        value["responses"] = json!(2);
        value["coverage"] = json!(COVERAGE);
        value["trace"]["coverage"] = json!(COVERAGE);
        value["rounds"] = json!([1, 2]);
        value["trace"]["rounds"] = json!([1, 2]);
        value["trace"]["events"][1]["name"] = json!("write_file");
        value["trace"]["events"][2]["name"] = json!("write_file");
        value["trace"]["events"][2]["reported_in_round"] = json!(2);
        assert!(parse(&value.to_string()).is_some());
        value["trace"]["events"][2]["result_summary"]["content"] = json!("raw file contents");
        assert!(parse(&value.to_string()).is_none());
    }

    #[test]
    fn native_observations_reject_count_coverage_and_correlation_changes() {
        for (path, replacement) in [
            ("/correlated_tools", json!(0)),
            ("/correlated_tools", json!(257)),
            ("/correlated_tools", json!(2)),
            ("/rounds", json!([2, 1])),
            ("/trace/events/0/accepted", json!(false)),
            ("/trace/events/0/accepted", Value::Null),
            ("/trace/events/2/call_id", json!("unrelated")),
            ("/trace/events/2/name", json!("unrelated")),
            ("/trace/events/2/round", json!(2)),
            ("/trace/missing_rounds", json!([1])),
            ("/trace/truncated", json!(true)),
            ("/trace/dropped_events", json!(1)),
            ("/trace/coverage", json!(COVERAGE)),
            (
                "/trace/events/2/result_summary/artifact_name",
                json!("../secret"),
            ),
        ] {
            let mut value = native_fixture();
            *value.pointer_mut(path).unwrap() = replacement;
            assert!(parse(&value.to_string()).is_none(), "{path}");
        }
        for response in [Value::Null, json!(1)] {
            let mut value = native_fixture();
            value["responses"] = response;
            assert!(parse(&value.to_string()).is_none());
        }
        for reporting_round in [Value::Null, json!(1), json!(2)] {
            let mut value = native_fixture();
            value["trace"]["events"][2]["reported_in_round"] = reporting_round;
            assert!(parse(&value.to_string()).is_none());
        }
        for index in 0..3 {
            let mut value = native_fixture();
            let mut duplicate = value["trace"]["events"][index].clone();
            duplicate["seq"] = json!(6);
            value["trace"]["events"]
                .as_array_mut()
                .unwrap()
                .push(duplicate);
            assert!(
                parse(&value.to_string()).is_none(),
                "duplicate event {index}"
            );
        }
        let mut value = native_fixture();
        let events = value["trace"]["events"].as_array_mut().unwrap();
        events.swap(1, 2);
        events[1]["seq"] = json!(2);
        events[2]["seq"] = json!(5);
        assert!(
            parse(&value.to_string()).is_none(),
            "result must follow its proposal"
        );
        let mut value = native_fixture();
        value["trace"]["events"].as_array_mut().unwrap().remove(0);
        assert!(
            parse(&value.to_string()).is_none(),
            "accepted proposing round required"
        );
        let mut value = native_fixture();
        value["correlated_tools"] = json!(2);
        value["rounds"] = json!([1, 2]);
        value["trace"]["rounds"] = json!([1, 2]);
        assert!(
            parse(&value.to_string()).is_none(),
            "no model-only selected rounds"
        );
    }

    #[test]
    fn native_unavailable_is_fixed_and_returned_results_allow_correlated_immediate_reports() {
        let mut value = native_fixture();
        value.as_object_mut().unwrap().remove("trace");
        value["state"] = json!("unavailable");
        value["reason"] = json!(NATIVE_UNAVAILABLE);
        assert!(parse(&value.to_string()).is_some());
        value["scope_id"] = Value::Null;
        value["rounds"] = json!([]);
        assert!(parse(&value.to_string()).is_some());
        value["correlated_tools"] = json!(1024);
        assert!(parse(&value.to_string()).is_some());
        value["reason"] = json!("Response correlation changed or was invalid");
        assert!(parse(&value.to_string()).is_none());
        let mut value = native_fixture();
        value.as_object_mut().unwrap().remove("correlated_tools");
        value["coverage"] = json!(COVERAGE);
        value["trace"]["coverage"] = json!(COVERAGE);
        value["responses"] = json!(2);
        assert!(parse(&value.to_string()).is_some());
        value["trace"]["events"][2]["reported_in_round"] = Value::Null;
        assert!(parse(&value.to_string()).is_none());
        value["rounds"] = json!([1, 2]);
        value["trace"]["rounds"] = json!([1, 2]);
        value["trace"]["events"][2]["reported_in_round"] = json!(2);
        assert!(parse(&value.to_string()).is_some());
    }

    #[test]
    fn retains_valid_observed_times_and_safe_completion_details_only() {
        let mut value = fixture();
        value["trace"]["events"][0]["observed_at"] = json!("2026-10-08T20:00:00.123Z");
        value["trace"]["events"].as_array_mut().unwrap().push(json!({
            "kind":"tool_result","scope_id":"scope-a","round":1,"seq":2,"source":"harness-reported",
            "name":"file_write","call_id":"write-1","ok":true,"reported_in_round":2,
            "observed_at":"2026-10-08T20:00:00.456Z","ms":12,
            "result_summary":{"kind":"file_write","bytes":17,"artifact_name":"briefing.md"}
        }));
        assert_eq!(
            serde_json::to_value(parse(&value.to_string()).unwrap()).unwrap(),
            value
        );
        for (field, bad) in [
            ("observed_at", json!("2026-10-08T20:00:00")),
            ("observed_at", json!("2026-02-30T20:00:00.000Z")),
            ("observed_at", json!(13)),
            ("ms", json!(-1)),
            ("ms", json!(3_600_001)),
            ("ok", json!(false)),
            ("ok", Value::Null),
            (
                "result_summary",
                json!({"kind":"file_write","bytes":17,"content":"secret"}),
            ),
            (
                "result_summary",
                json!({"kind":"file_write","bytes":17,"artifact_name":"../secret"}),
            ),
            (
                "result_summary",
                json!({"kind":"file_read","bytes":17,"returned_bytes":17,"truncated":false}),
            ),
        ] {
            let mut bad_value = value.clone();
            bad_value["trace"]["events"][1][field] = bad;
            assert!(
                parse(&bad_value.to_string()).is_none(),
                "{field}: {bad_value}"
            );
        }
    }

    #[test]
    fn accepts_the_actual_router_and_runtime_contract_with_current_run_binding() {
        let content = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../inference-router/tests/fixtures/router-observations-v1.json"
        ));
        let expected: Value = serde_json::from_str(content).unwrap();
        let mut file = artifact();
        file.full_content = Some(content.into());
        let projected = from_artifacts(&task(), Some(&result()), &[file]).unwrap();
        assert_eq!(serde_json::to_value(projected).unwrap(), expected);
    }

    fn fixture() -> Value {
        json!({"version":1,"source":SOURCE,"coverage":COVERAGE,"responses":2,"scope_id":"scope-a","rounds":[1,2],"state":"observed",
            "trace":{"scope_id":"scope-a","rounds":[1,2],"coverage":COVERAGE,"durable":false,"missing_rounds":[],"dropped_events":0,"truncated":false,
            "events":[{"kind":"round","scope_id":"scope-a","round":1,"seq":1,"source":"router-upstream","provider":"foundry","model":"model","http_status":200,"accepted":true,"outcome":"complete","usage":{"prompt_tokens":null,"completion_tokens":null,"total_tokens":null},"usage_state":"missing","finish_reason":"stop","partial_observation":false,"ms":2,"tool_calls_observed":0}]}})
    }
    fn task() -> KarsTask {
        serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
            "metadata":{"name":"briefing","uid":"task-uid",
                "annotations":{"kars.azure.com/run-requested":"revision-2"}},
            "spec":{"objective":"Write a useful briefing", "envelope":{"tier":1,"authorityCeiling":1}}
        })).unwrap()
    }

    fn result() -> MissionResultDto {
        use super::super::run_evidence::MissionRunEvidenceDto;
        MissionResultDto {
            output: "Attached briefing.md".into(),
            error: None,
            reviewable: true,
            usage_known: true,
            status: Some("ok".into()),
            model: None,
            total_tokens: None,
            prompt_tokens: None,
            completion_tokens: None,
            finished_at: None,
            assignment_nonce: Some("revision-2".into()),
            source: Some("durable_agent".into()),
            blocked: None,
            artifact_persistence: None,
            artifact_count: None,
            declared_artifact_count: None,
            run_evidence: Some(MissionRunEvidenceDto {
                task_uid: "task-uid".into(),
                run_nonce: "revision-2".into(),
                assignment_id: "assignment-2".into(),
                agent_name: "writer".into(),
                agent_did: "did:mesh:11111111111111111111111111111111".into(),
                dispatcher_did: "did:mesh:22222222222222222222222222222222".into(),
                sandbox_uid: "sandbox-uid".into(),
                pod_uid: "pod-uid".into(),
                runtime_boot_id: "boot-id".into(),
                runtime: None,
                status: "succeeded".into(),
                started_at: "2026-10-03T06:47:40Z".into(),
                finished_at: "2026-10-03T06:47:45Z".into(),
                rounds: Some(2),
            }),
        }
    }

    fn artifact() -> MissionArtifactDto {
        MissionArtifactDto {
            name: ARTIFACT.into(),
            size_bytes: None,
            content: Some("truncated preview".into()),
            content_bytes: None,
            content_truncated: true,
            source_agent: None,
            source_path: None,
            digest: None,
            full_content: Some(fixture().to_string()),
        }
    }

    #[test]
    fn retains_activity_for_current_failed_runs_without_making_them_reviewable() {
        let mut failed = result();
        failed.status = Some("failed".into());
        failed.run_evidence.as_mut().unwrap().status = "failed".into();
        failed.reviewable = false;
        failed.output.clear();
        failed.error = Some("Write failed".into());
        assert!(from_artifacts(&task(), Some(&failed), &[artifact()]).is_some());
        assert!(!failed.reviewable);
        assert!(failed.output.is_empty());
        assert!(from_artifacts(&task(), Some(&failed), &[]).is_none());
        failed.assignment_nonce = Some("other-run".into());
        assert!(from_artifacts(&task(), Some(&failed), &[artifact()]).is_none());
    }

    #[test]
    fn binds_only_full_unique_artifact_to_current_successful_terminal() {
        assert!(from_artifacts(&task(), Some(&result()), &[artifact()]).is_some());
        assert!(from_artifacts(&task(), None, &[artifact()]).is_none());
        assert!(from_artifacts(&task(), Some(&result()), &[artifact(), artifact()]).is_none());
        assert!(from_artifacts(&task(), Some(&result()), &[]).is_none());
        let mut preview_only = artifact();
        preview_only.content = preview_only.full_content.take();
        assert!(from_artifacts(&task(), Some(&result()), &[preview_only]).is_none());
        let mut wrong_name = artifact();
        wrong_name.name = "user-document.json".into();
        assert!(from_artifacts(&task(), Some(&result()), &[wrong_name]).is_none());
        for mismatch in 0..7 {
            let mut r = result();
            match mismatch {
                0 => r.run_evidence = None,
                1 => r.source = Some("single_turn".into()),
                2 => r.status = Some("failed".into()),
                3 => r.assignment_nonce = Some("revision-1".into()),
                4 => r.run_evidence.as_mut().unwrap().task_uid = "recreated-task".into(),
                5 => r.run_evidence.as_mut().unwrap().run_nonce = "revision-1".into(),
                _ => r.run_evidence.as_mut().unwrap().status = "failed".into(),
            }
            assert!(
                from_artifacts(&task(), Some(&r), &[artifact()]).is_none(),
                "{mismatch}"
            );
        }
        for nonce in [None, Some(""), Some("revision-3")] {
            let mut t = task();
            t.metadata.annotations =
                nonce.map(|nonce| [("kars.azure.com/run-requested".into(), nonce.into())].into());
            assert!(from_artifacts(&t, Some(&result()), &[artifact()]).is_none());
        }
    }

    #[test]
    fn accepts_bounded_invalid_correlation_envelopes_from_collector() {
        for (responses, scope_id, rounds) in [
            (1, Value::Null, vec![]),
            (33, json!("scope-a"), (1..=32).collect::<Vec<u64>>()),
        ] {
            let v = json!({"version":1,"source":SOURCE,"coverage":COVERAGE,
                "responses":responses,"scope_id":scope_id,"rounds":rounds,"state":"unavailable",
                "reason":"Response correlation changed or was invalid"});
            assert!(parse(&v.to_string()).is_some());
        }
    }

    #[test]
    fn preserves_nullable_rounds_and_distinct_tool_reporting_rounds() {
        let mut v = fixture();
        for field in ["model", "http_status", "accepted", "usage", "finish_reason"] {
            v["trace"]["events"][0][field] = Value::Null;
        }
        let events = v["trace"]["events"].as_array_mut().unwrap();
        events.push(
            json!({"kind":"tool_proposed","scope_id":"scope-a","round":1,"seq":2,
            "source":"model-proposed","call_id":"call-1","name":"file_write","ok":null}),
        );
        events.push(json!({"kind":"tool_result","scope_id":"scope-a","round":1,"seq":3,
            "source":"harness-reported","call_id":"call-1","name":"file_write","ok":false,"reported_in_round":2}));
        let output = serde_json::to_value(parse(&v.to_string()).unwrap()).unwrap();
        assert!(output["trace"]["events"][0]["http_status"].is_null());
        assert_eq!(output["trace"]["events"][2]["ok"], false);
        for reporting_round in [1, 3] {
            v["trace"]["events"][2]["reported_in_round"] = json!(reporting_round);
            assert!(parse(&v.to_string()).is_none());
        }
    }

    #[test]
    fn projects_only_typed_fields_without_inventing_usage() {
        let mut v = fixture();
        v["secret"] = json!("root-secret");
        v["trace"]["events"][0]["arguments"] = json!("argument-secret");
        let output = serde_json::to_value(parse(&v.to_string()).unwrap()).unwrap();
        assert!(output["trace"]["events"][0]["usage"]["total_tokens"].is_null());
        assert!(!output.to_string().contains("secret"));
    }
    #[test]
    fn rejects_malformed_or_cross_selection_records() {
        for (path, replacement) in [
            ("/version", json!(2)),
            ("/responses", json!(1)),
            ("/rounds", json!([2, 1])),
            ("/trace/durable", json!(true)),
            ("/trace/scope_id", json!("other")),
            ("/trace/missing_rounds", json!([3])),
            ("/trace/events/0/round", json!(3)),
            ("/trace/events/0/model", json!({"secret":"nested"})),
            ("/trace/events/0/usage/total_tokens", json!(-1)),
            ("/trace/events/0/seq", json!(0)),
            ("/trace/events/0/source", json!("harness-reported")),
        ] {
            let mut v = fixture();
            *v.pointer_mut(path).unwrap() = replacement;
            assert!(parse(&v.to_string()).is_none(), "{path}");
        }
        assert!(parse(&" ".repeat(MAX_ACTIVITY_BYTES + 1)).is_none());
    }
    #[test]
    fn preserves_gaps_and_accepts_only_fixed_unavailable_reasons() {
        let mut v = fixture();
        v["trace"]["events"] = json!([]);
        v["trace"]["missing_rounds"] = json!([1, 2]);
        v["trace"]["truncated"] = json!(true);
        v["trace"]["dropped_events"] = json!(12);
        assert!(parse(&v.to_string()).is_some());
        v["state"] = json!("unavailable");
        v["reason"] = json!("Response correlation changed or was invalid");
        assert!(parse(&v.to_string()).is_some());
        v["reason"] = json!("arbitrary provider text");
        assert!(parse(&v.to_string()).is_none());
    }
}
