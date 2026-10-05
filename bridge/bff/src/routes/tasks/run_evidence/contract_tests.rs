// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars::task::KarsTask;
use crate::routes::tasks::{MissionArtifactDto, router_activity};
use serde_json::json;

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("protocol-fixtures.json")).unwrap()
}

fn reply(version: u8) -> Value {
    fixtures()
        .into_iter()
        .find(|f| {
            f["reply"]["version"] == version && (version != 2 || f["name"] == "sorted-capabilities")
        })
        .unwrap()["reply"]
        .take()
}

fn output(reply: &Value) -> BTreeMap<String, String> {
    let mut data: BTreeMap<String, String> = reply
        .as_object()
        .unwrap()
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.into())))
        .collect();
    data.insert(
        "assignmentNonce".into(),
        reply["runNonce"].as_str().unwrap().into(),
    );
    data.insert(
        "runtimeBootId".into(),
        reply["bootId"].as_str().unwrap().into(),
    );
    let usage = &reply["evidence"]["usage"];
    for (key, value) in [
        ("agentName", "briefing"),
        (
            "status",
            if reply["status"] == "succeeded" {
                "ok"
            } else {
                reply["status"].as_str().unwrap()
            },
        ),
        ("model", "recorded-model"),
        (
            "usageKnown",
            if usage.is_object() { "true" } else { "false" },
        ),
        ("startedAt", "2026-10-03T06:47:40Z"),
        ("finishedAt", "2026-10-03T06:47:45Z"),
    ] {
        data.insert(key.into(), value.into());
    }
    if let Some(total) = usage["totalTokens"].as_i64() {
        data.insert("totalTokens".into(), total.to_string());
    }
    data.insert("evidence.json".into(), reply.to_string());
    data
}

fn without_evidence_with_fresh_digest(reply: &mut Value) {
    reply["status"] = json!("failed");
    reply.as_object_mut().unwrap().remove("evidence");
    if let Ok(mut phase) =
        serde_json::from_value::<crate::kars::task::ExecutionPhase>(reply["reviewedPhase"].clone())
    {
        phase.capabilities.sort();
        reply["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    }
}

fn invalid(reply: &Value) {
    let result = mission_result(&output(reply));
    assert_eq!(
        result.source.as_deref(),
        Some("invalid_evidence"),
        "{reply}"
    );
    assert!(!result.reviewable && !result.usage_known);
    assert!(result.run_evidence.is_none() && result.total_tokens.is_none());
    assert!(run_telemetry(Some(&result)).is_none());
}

#[test]
fn typescript_terminal_fixtures_preserve_producer_usage_and_rounds() {
    for fixture in fixtures() {
        let result = mission_result(&output(&fixture["reply"]));
        assert_eq!(
            result.source.as_deref(),
            Some("durable_agent"),
            "{}",
            fixture["name"]
        );
        assert!(result.reviewable && result.usage_known);
        assert_eq!(result.total_tokens, Some(7425));
        let run = result.run_evidence.as_ref().unwrap();
        assert_eq!(run.agent_name, "briefing");
        assert_eq!(run.agent_did, "did:mesh:11111111111111111111111111111111");
        assert_eq!(run.pod_uid, "old-pod");
        assert_eq!(run.assignment_id, "assignment-2");
        assert_eq!(run_telemetry(Some(&result)).unwrap().rounds, Some(2));
    }
}

#[test]
fn newer_terminals_feed_existing_activity_without_weakening_current_run_fence() {
    let mut task: KarsTask = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
        "metadata":{"name":"briefing","uid":"task-uid",
            "annotations":{"kars.azure.com/run-requested":"rev-2"}},
        "spec":{"objective":"Write a useful briefing", "envelope":{"tier":1,"authorityCeiling":1}}
    }))
    .unwrap();
    let content = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../inference-router/tests/fixtures/router-observations-v1.json"
    ));
    let artifact = MissionArtifactDto {
        name: "kars-router-observations.json".into(),
        size_bytes: None,
        content: None,
        content_bytes: None,
        content_truncated: true,
        source_agent: None,
        source_path: None,
        digest: None,
        full_content: Some(content.into()),
    };
    for version in [2, 3] {
        let result = mission_result(&output(&reply(version)));
        assert!(
            router_activity::from_artifacts(&task, Some(&result), std::slice::from_ref(&artifact))
                .is_some()
        );
        task.metadata.uid = Some("recreated-task".into());
        assert!(
            router_activity::from_artifacts(&task, Some(&result), std::slice::from_ref(&artifact))
                .is_none()
        );
        task.metadata.uid = Some("task-uid".into());
        task.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/run-requested".into(), "later-run".into());
        assert!(
            router_activity::from_artifacts(&task, Some(&result), std::slice::from_ref(&artifact))
                .is_none()
        );
        task.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("kars.azure.com/run-requested".into(), "rev-2".into());
    }
}

#[test]
fn rejects_cross_version_fields_even_when_null() {
    for version in [1, 2, 3] {
        let mut r = reply(version);
        r["inputArtifacts"] = Value::Null;
        invalid(&r);
    }
    for key in ["reviewedPhase", "phaseDigest", "inputDigest"] {
        let mut r = reply(1);
        r[key] = Value::Null;
        invalid(&r);
    }
    let mut r = reply(1);
    r["evidence"]["phase"] = Value::Null;
    invalid(&r);
    let mut r = reply(2);
    r["inputDigest"] = Value::Null;
    invalid(&r);
    for version in [0, 4, 255] {
        let mut r = reply(1);
        r["version"] = json!(version);
        invalid(&r);
    }
}

#[test]
fn rejects_unsupported_or_altered_phase_contracts() {
    for version in [2, 3] {
        for key in ["reviewedPhase", "phaseDigest"] {
            let mut r = reply(version);
            r.as_object_mut().unwrap().remove(key);
            invalid(&r);
        }
        for (key, value) in [
            ("name", json!("Draft")),
            ("name", json!("-draft")),
            ("name", json!("draft-")),
            ("name", json!("x".repeat(49))),
            ("objective", json!("\u{feff}".repeat(20))),
            ("objective", json!("é".repeat(601))),
            ("capabilities", json!(["shell"])),
            (
                "capabilities",
                json!(["filesystem-read", "filesystem-read"]),
            ),
            (
                "requiredToolCalls",
                json!([{"name":"shell","arguments":{}}]),
            ),
            ("minToolCalls", json!(-1)),
            ("minToolCalls", json!(4)),
            ("maxToolCalls", json!(33)),
            ("maxToolCalls", json!(1.5)),
            ("freshContext", Value::Null),
            ("extra", json!(true)),
        ] {
            let mut r = reply(version);
            r["reviewedPhase"][key] = value;
            without_evidence_with_fresh_digest(&mut r);
            invalid(&r);
        }
        let mut r = reply(version);
        r["phaseDigest"] = json!(format!("sha256:{}", "0".repeat(64)));
        invalid(&r);
    }
}

#[test]
fn inputs_require_digest_read_capability_and_positive_ceiling() {
    for value in [
        Value::Null,
        json!("sha256:short"),
        json!(format!("sha256:{}", "A".repeat(64))),
        json!("a".repeat(64)),
    ] {
        let mut r = reply(3);
        r["inputDigest"] = value;
        invalid(&r);
    }
    let mut r = reply(3);
    r.as_object_mut().unwrap().remove("inputDigest");
    invalid(&r);
    for capabilities in [json!([]), json!(["filesystem-write"])] {
        let mut r = reply(3);
        r["reviewedPhase"]["capabilities"] = capabilities;
        r["reviewedPhase"]["minToolCalls"] = json!(0);
        without_evidence_with_fresh_digest(&mut r);
        invalid(&r);
    }
    let mut r = reply(3);
    r["reviewedPhase"]["minToolCalls"] = json!(0);
    r["reviewedPhase"]["maxToolCalls"] = json!(0);
    without_evidence_with_fresh_digest(&mut r);
    invalid(&r);
}

#[test]
fn phase_counts_must_match_contract_and_successful_minimum() {
    for version in [2, 3] {
        for (key, value) in [
            ("name", json!("different")),
            ("minToolCalls", json!(0)),
            ("maxToolCalls", json!(4)),
            ("attemptedToolCalls", json!(-1)),
            ("attemptedToolCalls", json!(4)),
            ("attemptedToolCalls", json!(0)),
            ("successfulToolCalls", json!(-1)),
            ("successfulToolCalls", json!(0)),
            ("successfulToolCalls", json!(2)),
            ("successfulToolCalls", json!(1.5)),
            ("extra", json!(true)),
        ] {
            let mut r = reply(version);
            r["evidence"]["phase"][key] = value;
            invalid(&r);
        }
        for key in [
            "name",
            "minToolCalls",
            "maxToolCalls",
            "attemptedToolCalls",
            "successfulToolCalls",
        ] {
            let mut r = reply(version);
            r["evidence"]["phase"].as_object_mut().unwrap().remove(key);
            invalid(&r);
        }
        let mut r = reply(version);
        r["evidence"].as_object_mut().unwrap().remove("phase");
        invalid(&r);
    }
}

#[test]
fn failures_can_preserve_unknown_usage_but_not_malformed_evidence() {
    for version in [1, 2, 3] {
        for status in ["failed", "rejected"] {
            let mut r = reply(version);
            r["status"] = json!(status);
            r["evidence"]["usage"] = Value::Null;
            if version != 1 {
                r["evidence"]["phase"]["successfulToolCalls"] = json!(0);
            }
            let result = mission_result(&output(&r));
            assert!(result.run_evidence.is_some());
            assert!(!result.reviewable && !result.usage_known);
            r["evidence"].as_object_mut().unwrap().remove("usage");
            invalid(&r);
            r.as_object_mut().unwrap().remove("evidence");
            assert!(mission_result(&output(&r)).run_evidence.is_some());
            r["evidence"] = Value::Null;
            invalid(&r);
        }
        let mut r = reply(version);
        r["evidence"]["extra"] = json!(true);
        invalid(&r);
        let mut r = reply(version);
        r["evidence"]["usage"]["extra"] = json!(1);
        invalid(&r);
        let mut r = reply(version);
        r["evidence"]["model"] = json!("m".repeat(254));
        invalid(&r);
    }
}

#[test]
fn newer_contracts_do_not_relax_durable_identity_status_or_usage_binding() {
    for version in [2, 3] {
        for (key, value) in [
            ("taskUid", "other"),
            ("assignmentNonce", "other"),
            ("assignmentId", "other"),
            ("agentDid", "other"),
            ("dispatcherDid", "other"),
            ("sandboxUid", "other"),
            ("podUid", "other"),
            ("runtimeBootId", "other"),
            ("status", "failed"),
            ("output", "Different briefing"),
            ("model", "other"),
            ("totalTokens", "9999"),
            ("usageKnown", "false"),
            ("finishedAt", "2026-10-03T06:47:39Z"),
        ] {
            let mut data = output(&reply(version));
            data.insert(key.into(), value.into());
            let result = mission_result(&data);
            assert!(
                result.run_evidence.is_none() && !result.reviewable && !result.usage_known,
                "{version}: {key}"
            );
        }
    }
}
