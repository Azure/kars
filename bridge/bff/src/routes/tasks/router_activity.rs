// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};

use super::models::{MissionArtifactDto, MissionResultDto};
use crate::kars::task::KarsTask;

const ARTIFACT: &str = "kars-router-observations.json";
const SOURCE: &str = "runtime-forwarded-router-observations";
const COVERAGE: &str = "returned-responses-only";
const MAX_SAFE: u64 = 9_007_199_254_740_991;

#[derive(Debug, Deserialize, Serialize)]
pub struct RouterActivity {
    version: u8,
    source: String,
    coverage: String,
    responses: u64,
    scope_id: Option<String>,
    rounds: Vec<u64>,
    #[serde(flatten)]
    observation: Observation,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Observation {
    Observed { trace: Trace },
    Unavailable { reason: String },
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
struct Event {
    scope_id: String,
    round: u64,
    seq: u64,
    source: String,
    #[serde(flatten)]
    detail: EventDetail,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EventDetail {
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
    },
    ToolResult {
        call_id: String,
        name: String,
        ok: Option<bool>,
        reported_in_round: u64,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
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
    ids.len() <= 32
        && ids.iter().all(|n| *n > 0 && *n <= MAX_SAFE)
        && ids.windows(2).all(|pair| pair[0] < pair[1])
}

impl Event {
    fn valid(&self, selection: &RouterActivity) -> bool {
        if Some(&self.scope_id) != selection.scope_id.as_ref()
            || !selection.rounds.contains(&self.round)
            || self.seq == 0
            || self.seq > MAX_SAFE
        {
            return false;
        }
        match &self.detail {
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
            EventDetail::ToolProposed { call_id, name, ok } => {
                self.source == "model-proposed"
                    && text(call_id, 256)
                    && text(name, 256)
                    && ok.is_none()
            }
            EventDetail::ToolResult {
                call_id,
                name,
                ok: _,
                reported_in_round,
            } => {
                self.source == "harness-reported"
                    && text(call_id, 256)
                    && text(name, 256)
                    && *reported_in_round > self.round
                    && selection.rounds.contains(reported_in_round)
            }
        }
    }
}

fn parse(content: &str) -> Option<RouterActivity> {
    if content.len() > 32 * 1024 {
        return None;
    }
    let value: RouterActivity = serde_json::from_str(content).ok()?;
    if value.version != 1
        || value.source != SOURCE
        || value.coverage != COVERAGE
        || value.responses == 0
        || value.responses > MAX_SAFE
        || !ordered(&value.rounds)
        || value.rounds.len() as u64 > value.responses
        || value.scope_id.as_ref().is_some_and(|s| !scope(s))
    {
        return None;
    }
    match &value.observation {
        Observation::Unavailable { reason } => {
            if ![
                "Response correlation changed or was invalid",
                "Router selection did not match returned response IDs",
                "Router selection exceeded the observation artifact limit",
                "Router observations unavailable after execution (reset, expiry or read failure)",
            ]
            .contains(&reason.as_str())
            {
                return None;
            }
        }
        Observation::Observed { trace } => {
            if value.rounds.is_empty()
                || Some(&trace.scope_id) != value.scope_id.as_ref()
                || trace.rounds != value.rounds
                || trace.coverage != COVERAGE
                || trace.durable
                || !ordered(&trace.missing_rounds)
                || trace
                    .missing_rounds
                    .iter()
                    .any(|n| !value.rounds.contains(n))
                || trace.dropped_events > MAX_SAFE
                || trace.events.len() > 256
                || trace
                    .events
                    .windows(2)
                    .any(|pair| pair[0].seq >= pair[1].seq)
                || trace.events.iter().any(|event| !event.valid(&value))
            {
                return None;
            }
        }
    }
    Some(value)
}

/// These files already passed the query's immutable current-run artifact binding.
/// Response IDs select observations; they do not independently attest execution.
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
        || result.status.as_deref() != Some("ok")
        || evidence.status != "succeeded"
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
        assert!(parse(&" ".repeat(32 * 1024 + 1)).is_none());
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
