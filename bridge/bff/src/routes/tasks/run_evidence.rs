// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::models::{MissionResultDto, MissionTelemetryDto};
use super::presentation::{classify_blocked, deliverable_text, is_real_deliverable};

mod contract;
#[cfg(test)]
mod contract_tests;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Recorded producer identity, not the identity of today's running sandbox.
#[derive(Debug, Serialize)]
pub struct MissionRunEvidenceDto {
    pub task_uid: String,
    pub run_nonce: String,
    pub assignment_id: String,
    pub agent_name: String,
    pub agent_did: String,
    pub dispatcher_did: String,
    pub sandbox_uid: String,
    pub pod_uid: String,
    pub runtime_boot_id: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: String,
    pub rounds: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    #[serde(rename = "type")]
    kind: String,
    version: u8,
    task_name: String,
    task_uid: String,
    run_nonce: String,
    assignment_id: String,
    agent_did: String,
    dispatcher_did: String,
    sandbox_uid: String,
    pod_uid: String,
    boot_id: String,
    status: String,
    output: Option<String>,
    #[serde(default, deserialize_with = "present_evidence")]
    evidence: Option<Evidence>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

fn present_evidence<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Evidence>, D::Error> {
    Evidence::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct Evidence {
    model: String,
    rounds: i64,
    #[serde(deserialize_with = "Option::<Usage>::deserialize")]
    usage: Option<Usage>,
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Usage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

fn safe_integer(value: i64) -> bool {
    (0..=MAX_SAFE_INTEGER).contains(&value)
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

fn did(value: &str) -> bool {
    value.strip_prefix("did:mesh:").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn terminal(data: &BTreeMap<String, String>) -> Option<(MissionRunEvidenceDto, Option<Evidence>)> {
    let raw = data.get("evidence.json")?;
    if raw.len() > 192 * 1024 {
        return None;
    }
    let reply: Reply = serde_json::from_str(raw).ok()?;
    let expected_status = match reply.status.as_str() {
        "succeeded" => "ok",
        "failed" => "failed",
        "rejected" => "rejected",
        _ => return None,
    };
    if reply.kind != "mission:reply"
        || contract::valid(&reply).is_none()
        || data.get("status").map(String::as_str) != Some(expected_status)
        || !did(&reply.agent_did)
        || !did(&reply.dispatcher_did)
    {
        return None;
    }
    for (key, value) in [
        ("taskName", &reply.task_name),
        ("taskUid", &reply.task_uid),
        ("assignmentNonce", &reply.run_nonce),
        ("assignmentId", &reply.assignment_id),
        ("agentDid", &reply.agent_did),
        ("dispatcherDid", &reply.dispatcher_did),
        ("sandboxUid", &reply.sandbox_uid),
        ("podUid", &reply.pod_uid),
        ("runtimeBootId", &reply.boot_id),
    ] {
        if !identifier(value) || data.get(key) != Some(value) {
            return None;
        }
    }
    let agent_name = data.get("agentName")?.clone();
    if !identifier(&agent_name) {
        return None;
    }
    let started_at = data.get("startedAt")?.clone();
    let finished_at = data.get("finishedAt")?.clone();
    if chrono::DateTime::parse_from_rfc3339(&started_at).ok()?
        > chrono::DateTime::parse_from_rfc3339(&finished_at).ok()?
    {
        return None;
    }
    let usage = reply.evidence.as_ref().and_then(|e| e.usage.as_ref());
    if data.get("usageKnown").map(String::as_str)
        != Some(if usage.is_some() { "true" } else { "false" })
    {
        return None;
    }
    if let Some(evidence) = &reply.evidence
        && (evidence.model.trim().is_empty()
            || evidence.model.len() > 253
            || !safe_integer(evidence.rounds)
            || data.get("model") != Some(&evidence.model))
    {
        return None;
    }
    if let Some(usage) = usage
        && (!safe_integer(usage.prompt_tokens)
            || !safe_integer(usage.completion_tokens)
            || !safe_integer(usage.total_tokens)
            || usage.prompt_tokens.checked_add(usage.completion_tokens) != Some(usage.total_tokens)
            || data.get("totalTokens").and_then(|s| s.parse::<i64>().ok())
                != Some(usage.total_tokens))
    {
        return None;
    }
    if reply.status == "succeeded"
        && (reply.output.as_ref() != data.get("output")
            || reply.output.as_ref().is_none_or(|s| s.trim().is_empty())
            || usage.is_none_or(|u| u.total_tokens == 0)
            || reply.evidence.as_ref().is_none_or(|e| e.rounds == 0))
    {
        return None;
    }
    Some((
        MissionRunEvidenceDto {
            task_uid: reply.task_uid,
            run_nonce: reply.run_nonce,
            assignment_id: reply.assignment_id,
            agent_name,
            agent_did: reply.agent_did,
            dispatcher_did: reply.dispatcher_did,
            sandbox_uid: reply.sandbox_uid,
            pod_uid: reply.pod_uid,
            runtime_boot_id: reply.boot_id,
            status: reply.status,
            started_at,
            finished_at,
            rounds: reply.evidence.as_ref().map(|e| e.rounds),
        },
        reply.evidence,
    ))
}

/// Input must already pass the current Task UID/run/owner ConfigMap fence.
pub(crate) fn mission_result(data: &BTreeMap<String, String>) -> MissionResultDto {
    let output = deliverable_text(data.get("output").map(String::as_str).unwrap_or(""));
    let status = data.get("status").cloned();
    let blocked = classify_blocked(status.as_deref(), &output);
    let number = |key: &str| {
        data.get(key)
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| safe_integer(*n))
    };
    let mut result = MissionResultDto {
        reviewable: status.as_deref() == Some("ok")
            && is_real_deliverable(status.as_deref(), &output),
        output,
        status,
        blocked,
        model: data.get("model").cloned(),
        total_tokens: number("totalTokens"),
        prompt_tokens: number("promptTokens"),
        completion_tokens: number("completionTokens"),
        finished_at: data.get("finishedAt").cloned(),
        assignment_nonce: data.get("assignmentNonce").cloned(),
        source: data.get("source").cloned(),
        artifact_persistence: data.get("artifactPersistence").cloned(),
        artifact_count: number("artifactCount"),
        declared_artifact_count: number("declaredArtifactCount"),
        usage_known: false,
        run_evidence: None,
    };
    if data.contains_key("evidence.json") {
        result.total_tokens = None;
        result.prompt_tokens = None;
        result.completion_tokens = None;
        match terminal(data) {
            Some((run, evidence)) => {
                result.source = Some("durable_agent".into());
                if let Some(evidence) = evidence {
                    result.model = Some(evidence.model);
                    if let Some(usage) = evidence.usage {
                        result.usage_known = true;
                        result.total_tokens = Some(usage.total_tokens);
                        result.prompt_tokens = Some(usage.prompt_tokens);
                        result.completion_tokens = Some(usage.completion_tokens);
                    }
                }
                result.run_evidence = Some(run);
            }
            None => {
                result.source = Some("invalid_evidence".into());
                result.reviewable = false;
            }
        }
    } else if data.get("usageKnown").map(String::as_str) == Some("false") {
        result.total_tokens = None;
        result.prompt_tokens = None;
        result.completion_tokens = None;
    } else {
        result.usage_known = result.total_tokens.is_some();
    }
    result
}

/// Whole-run counts come from the committed terminal, not partial trace records.
pub(super) fn run_telemetry(result: Option<&MissionResultDto>) -> Option<MissionTelemetryDto> {
    let run = result?.run_evidence.as_ref()?;
    Some(MissionTelemetryDto {
        rounds: run.rounds,
        tool_calls: None,
    })
}

/// Unbound router/trace records are not evidence for a particular revision.
pub(crate) fn scope_activity(activity: Vec<Value>, nonce: Option<&str>) -> Vec<Value> {
    let Some(nonce) = nonce.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    activity
        .into_iter()
        .filter(|event| {
            let bindings: Vec<_> = ["runNonce", "assignmentNonce", "assignment_nonce"]
                .iter()
                .filter_map(|key| event.get(key))
                .collect();
            !bindings.is_empty() && bindings.iter().all(|value| value.as_str() == Some(nonce))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output() -> BTreeMap<String, String> {
        let reply = json!({"type":"mission:reply", "version":1,
            "taskName":"briefing", "taskUid":"task-uid", "runNonce":"rev-2",
            "assignmentId":"assignment-2", "agentDid":"did:mesh:11111111111111111111111111111111",
            "dispatcherDid":"did:mesh:22222222222222222222222222222222",
            "sandboxUid":"sandbox-uid", "podUid":"old-pod", "bootId":"old-boot",
            "status":"succeeded", "output":"Useful briefing",
            "evidence":{"model":"recorded-model", "rounds":2,
                "usage":{"promptTokens":6940,"completionTokens":485,"totalTokens":7425}}});
        let mut data = BTreeMap::new();
        for (key, value) in reply.as_object().unwrap() {
            if let Some(value) = value.as_str() {
                data.insert(key.clone(), value.into());
            }
        }
        data.insert("assignmentNonce".into(), "rev-2".into());
        data.insert("runtimeBootId".into(), "old-boot".into());
        for (key, value) in [
            ("agentName", "briefing"),
            ("status", "ok"),
            ("model", "recorded-model"),
            ("totalTokens", "7425"),
            ("usageKnown", "true"),
            ("startedAt", "2026-10-03T06:47:40Z"),
            ("finishedAt", "2026-10-03T06:47:45Z"),
        ] {
            data.insert(key.into(), value.into());
        }
        data.insert("evidence.json".into(), reply.to_string());
        data
    }

    #[test]
    fn committed_terminal_reports_producer_and_exact_whole_usage() {
        let result = mission_result(&output());
        assert!(result.reviewable && result.usage_known);
        assert_eq!(
            (
                result.prompt_tokens,
                result.completion_tokens,
                result.total_tokens
            ),
            (Some(6940), Some(485), Some(7425))
        );
        let telemetry = run_telemetry(Some(&result)).unwrap();
        assert_eq!(telemetry.rounds, Some(2));
        assert_eq!(telemetry.tool_calls, None);
        let run = result.run_evidence.unwrap();
        assert_eq!(run.pod_uid, "old-pod");
        assert_eq!(run.assignment_id, "assignment-2");
        assert_eq!(run.rounds, Some(2));
    }

    #[test]
    fn mismatched_or_malformed_terminal_is_not_approval_or_usage_evidence() {
        for (key, value) in [
            ("taskUid", "other"),
            ("assignmentNonce", "other"),
            ("podUid", "new-pod"),
            ("assignmentId", "other"),
            ("agentDid", "other"),
            ("runtimeBootId", "new-boot"),
            ("sandboxUid", "other"),
            ("dispatcherDid", "other"),
            ("status", "failed"),
            ("output", "Different result"),
            ("totalTokens", "99999"),
            ("usageKnown", "false"),
            ("evidence.json", "{}"),
            ("finishedAt", "bad-date"),
        ] {
            let mut data = output();
            data.insert(key.into(), value.into());
            let result = mission_result(&data);
            assert!(!result.reviewable && !result.usage_known, "{key}");
            assert!(
                result.run_evidence.is_none() && result.total_tokens.is_none(),
                "{key}"
            );
            assert!(run_telemetry(Some(&result)).is_none());
        }
    }

    #[test]
    fn failed_terminal_with_unknown_usage_keeps_failure_and_producer_not_partial_total() {
        let mut data = output();
        let mut reply: Value = serde_json::from_str(&data["evidence.json"]).unwrap();
        reply["status"] = json!("failed");
        reply["evidence"]["usage"] = Value::Null;
        data.insert("evidence.json".into(), reply.to_string());
        data.insert("status".into(), "failed".into());
        data.insert("usageKnown".into(), "false".into());
        let result = mission_result(&data);
        assert!(result.run_evidence.is_some());
        assert!(!result.reviewable && !result.usage_known);
        assert!(result.total_tokens.is_none());
    }

    #[test]
    fn invalid_usage_and_nonterminal_status_are_rejected() {
        for (field, value) in [
            ("rounds", json!(-1)),
            ("rounds", json!(0)),
            (
                "usage",
                json!({"promptTokens":1,"completionTokens":1,"totalTokens":7425}),
            ),
            (
                "usage",
                json!({"promptTokens":-1,"completionTokens":7426,"totalTokens":7425}),
            ),
            (
                "usage",
                json!({"promptTokens":9007199254740992_i64,"completionTokens":0,"totalTokens":9007199254740992_i64}),
            ),
            ("usage", Value::Null),
        ] {
            let mut data = output();
            let mut reply: Value = serde_json::from_str(&data["evidence.json"]).unwrap();
            reply["evidence"][field] = value;
            data.insert("evidence.json".into(), reply.to_string());
            assert!(!mission_result(&data).reviewable);
        }
        for status in ["accepted", "running", "ready"] {
            let mut data = output();
            let mut reply: Value = serde_json::from_str(&data["evidence.json"]).unwrap();
            reply["status"] = json!(status);
            data.insert("evidence.json".into(), reply.to_string());
            assert!(mission_result(&data).run_evidence.is_none());
        }
    }

    #[test]
    fn legacy_results_require_explicit_success_and_useful_output_for_approval() {
        let mut data = output();
        data.remove("evidence.json");
        assert!(mission_result(&data).reviewable);
        for status in ["failed", "rejected", "error", "", "unknown"] {
            data.insert("status".into(), status.into());
            assert!(!mission_result(&data).reviewable);
        }
        data.remove("status");
        assert!(!mission_result(&data).reviewable);
        data.insert("status".into(), "ok".into());
        data.insert("output".into(), "  ".into());
        assert!(!mission_result(&data).reviewable);
        data.insert("usageKnown".into(), "false".into());
        assert!(mission_result(&data).total_tokens.is_none());
    }

    #[test]
    fn trace_requires_exact_revision_and_never_supplies_whole_run_usage() {
        let events = vec![
            json!({"kind":"round","total_tokens":99999}),
            json!({"runNonce":"rev-1"}),
            json!({"runNonce":"rev-2","assignmentNonce":"rev-1"}),
            json!({"runNonce":"rev-2","kind":"tool"}),
        ];
        assert!(scope_activity(events.clone(), None).is_empty());
        assert_eq!(
            scope_activity(events, Some("rev-2")),
            vec![json!({"runNonce":"rev-2","kind":"tool"})]
        );
        assert_eq!(mission_result(&output()).total_tokens, Some(7425));
    }
}
