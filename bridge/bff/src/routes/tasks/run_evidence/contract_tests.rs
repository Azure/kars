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

fn fetch_reply() -> Value {
    let mut value = reply(4);
    value["reviewedPhase"] = json!({"name":"research",
        "objective":"Read and compare the reviewed public pricing sources.",
        "capabilities":["filesystem-read","filesystem-write","network"],
        "requiredToolCalls":[
            {"name":"http_fetch","arguments":{"method":"GET","url":"https://example.org/pricing"}},
            {"name":"http_fetch","arguments":{"method":"GET","url":"https://example.com/pricing"}}
        ], "minToolCalls":0,"maxToolCalls":4,"freshContext":true});
    let phase = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
    value["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    value["evidence"]["phase"] = json!({"name":"research","minToolCalls":0,"maxToolCalls":4,
        "attemptedToolCalls":4,"successfulToolCalls":4,"completedRequiredToolCalls":2});
    value
}

#[test]
fn native_headroom_is_retained_with_exact_reviewed_evidence_bounds() {
    let mut value = fetch_reply();
    value["reviewedPhase"]["executionMode"] = json!("native");
    value["reviewedPhase"]["maxToolCalls"] = json!(128);
    value["evidence"]["phase"]["maxToolCalls"] = json!(128);
    value["evidence"]["phase"]["attemptedToolCalls"] = json!(128);
    value["evidence"]["phase"]["successfulToolCalls"] = json!(128);
    let phase = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
    value["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    assert!(mission_result(&output(&value)).reviewable);
    value["evidence"]["phase"]["attemptedToolCalls"] = json!(129);
    invalid(&value);
    value["evidence"]["phase"]["attemptedToolCalls"] = json!(128);
    value["reviewedPhase"]
        .as_object_mut()
        .unwrap()
        .remove("executionMode");
    let phase = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
    value["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    invalid(&value);
}

#[test]
fn native_extended_capabilities_retain_producer_usage_and_exact_authority() {
    for capabilities in [
        json!(["memory"]),
        json!(["filesystem-read", "filesystem-write", "memory", "network"]),
        json!(["github-pr"]),
        json!(["filesystem-read", "filesystem-write", "github-pr"]),
        json!([
            "filesystem-read",
            "filesystem-write",
            "network",
            "python-execute"
        ]),
        json!([
            "filesystem-read",
            "filesystem-write",
            "github-pr",
            "memory",
            "network",
            "python-execute"
        ]),
        json!([
            "filesystem-read",
            "filesystem-write",
            "github-pr",
            "memory",
            "network"
        ]),
    ] {
        let mut value = fetch_reply();
        value["reviewedPhase"]["executionMode"] = json!("native");
        value["reviewedPhase"]["capabilities"] = capabilities;
        value["reviewedPhase"]["requiredToolCalls"] = json!([]);
        value["evidence"]["phase"]["completedRequiredToolCalls"] = json!(0);
        let parsed = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
        value["phaseDigest"] = json!(contract::phase_digest(&parsed).unwrap());
        let result = mission_result(&output(&value));
        assert!(result.reviewable && result.usage_known && result.run_evidence.is_some());
        for status in ["failed", "rejected"] {
            let mut changed = value.clone();
            changed["status"] = json!(status);
            changed.as_object_mut().unwrap().remove("artifacts");
            let result = mission_result(&output(&changed));
            assert!(!result.reviewable && result.usage_known && result.run_evidence.is_some());
        }
        for capabilities in [
            json!(["memory", "memory"]),
            json!(["memory", "shell"]),
            json!(["github-pr", "github-pr"]),
            json!(["github-pr", "shell"]),
            json!(["python-execute"]),
            json!(["filesystem-read", "filesystem-write", "python-execute"]),
            json!(["filesystem-read", "network", "python-execute"]),
            json!(["filesystem-write", "network", "python-execute"]),
        ] {
            let mut changed = value.clone();
            changed["reviewedPhase"]["capabilities"] = capabilities;
            let parsed = serde_json::from_value(changed["reviewedPhase"].clone()).unwrap();
            changed["phaseDigest"] = json!(contract::phase_digest(&parsed).unwrap());
            invalid(&changed);
        }
        value["reviewedPhase"]
            .as_object_mut()
            .unwrap()
            .remove("executionMode");
        let parsed = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
        value["phaseDigest"] = json!(contract::phase_digest(&parsed).unwrap());
        invalid(&value);
    }
}

#[test]
fn reviewed_fetch_evidence_is_readable_and_requires_exact_success_counts() {
    let original = fetch_reply();
    let result = mission_result(&output(&original));
    assert_eq!(result.source.as_deref(), Some("durable_agent"));
    assert!(result.reviewable && result.usage_known);
    for count in [
        json!(null),
        json!(true),
        json!("2"),
        json!(-1),
        json!(1.5),
        json!(0),
        json!(1),
        json!(3),
    ] {
        let mut changed = original.clone();
        changed["evidence"]["phase"]["completedRequiredToolCalls"] = count;
        invalid(&changed);
    }
    let mut changed = original.clone();
    changed["evidence"]["phase"]
        .as_object_mut()
        .unwrap()
        .remove("completedRequiredToolCalls");
    invalid(&changed);
    let mut changed = original;
    changed["evidence"]["phase"]["successfulToolCalls"] = json!(1);
    invalid(&changed);
}

#[test]
fn reviewed_fetch_digest_binds_order_arguments_and_rejects_unsupported_calls() {
    let original = fetch_reply();
    // Generated independently by the TypeScript missionPhaseDigest producer.
    assert_eq!(
        original["phaseDigest"],
        json!("sha256:f7eef6fc13f1cf521dee8ff4b001a53009edd32093818ebcdbdff1a8e2b7223b")
    );
    for (pointer, value) in [
        (
            "/requiredToolCalls/0/arguments/url",
            json!("https://example.net/pricing"),
        ),
        ("/requiredToolCalls/0/arguments/method", json!("POST")),
        ("/requiredToolCalls/0/arguments/headers", json!("private")),
        ("/requiredToolCalls/0/name", json!("web_search")),
        (
            "/capabilities",
            json!(["filesystem-read", "filesystem-write"]),
        ),
    ] {
        let mut changed = original.clone();
        let parts: Vec<_> = pointer.trim_start_matches('/').split('/').collect();
        let mut target = &mut changed["reviewedPhase"];
        for part in &parts[..parts.len() - 1] {
            target = if let Ok(index) = part.parse::<usize>() {
                &mut target[index]
            } else {
                &mut target[*part]
            };
        }
        target[parts[parts.len() - 1]] = value;
        invalid(&changed);
        if pointer != "/requiredToolCalls/0/arguments/url" {
            without_evidence_with_fresh_digest(&mut changed);
            invalid(&changed);
        }
    }
    let mut changed = original;
    changed["reviewedPhase"]["requiredToolCalls"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    invalid(&changed);
    let phase = serde_json::from_value(changed["reviewedPhase"].clone()).unwrap();
    changed["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    assert!(mission_result(&output(&changed)).reviewable);
}

#[test]
fn native_phase_digest_and_result_preserve_explicit_authority() {
    let projection: Value = serde_json::from_str(include_str!(
        "../../../../../../controller/src/reviewed_execution/phase-projection-fixture.json"
    ))
    .unwrap();
    let mut phase = projection["reviewedPhase"].clone();
    phase["executionMode"] = json!("native");
    phase["requiredToolCalls"] = json!([]);
    let parsed = serde_json::from_value(phase).unwrap();
    assert_eq!(
        contract::phase_digest(&parsed).unwrap(),
        "sha256:e67d6a318edfa7deed04ba8119bde4ef5b945983c39661e94eacf0f5133f7bff"
    );
    let mut value = fetch_reply();
    value["reviewedPhase"]["executionMode"] = json!("native");
    value["reviewedPhase"]["requiredToolCalls"] = json!([]);
    value["evidence"]["phase"]["completedRequiredToolCalls"] = json!(0);
    let parsed = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
    value["phaseDigest"] = json!(contract::phase_digest(&parsed).unwrap());
    assert!(mission_result(&output(&value)).reviewable);
    for mode in [json!(null), json!("future"), json!(1)] {
        let mut changed = value.clone();
        changed["reviewedPhase"]["executionMode"] = mode;
        invalid(&changed);
    }
    value["reviewedPhase"]
        .as_object_mut()
        .unwrap()
        .remove("executionMode");
    invalid(&value);
}

#[test]
fn required_call_counts_preserve_legacy_replies_and_partial_failures() {
    for version in [2, 3, 4] {
        let mut value = reply(version);
        value["evidence"]["phase"]["completedRequiredToolCalls"] = json!(0);
        assert!(mission_result(&output(&value)).reviewable);
        value["evidence"]["phase"]["completedRequiredToolCalls"] = json!(1);
        invalid(&value);
    }
    for status in ["failed", "rejected"] {
        for count in 0..=2 {
            let mut value = fetch_reply();
            value["status"] = json!(status);
            value.as_object_mut().unwrap().remove("artifacts");
            value["evidence"]["phase"]["completedRequiredToolCalls"] = json!(count);
            let result = mission_result(&output(&value));
            assert!(result.run_evidence.is_some());
            assert_eq!(result.source.as_deref(), Some("durable_agent"));
        }
    }
}

pub(super) fn peer_reply() -> Value {
    let mut value = reply(4);
    value["version"] = json!(5);
    value["reviewedPhase"]["capabilities"] = json!(["filesystem-read", "filesystem-write"]);
    value["reviewedPhase"]["executionMode"] = json!("native");
    value["reviewedPhase"]["requiredToolCalls"] = json!([]);
    let phase = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
    value["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    value["peerContext"] = json!({"rootUid":"root-uid",
        "stateName":format!("kars-reviewed-{}", "a".repeat(40)), "slot":"r0-p0",
        "peers":[{"role":"researcher","phase":"research","slot":"r0-p0",
            "agentName":format!("rp-{}-r0-p0", "a".repeat(20))},
            {"role":"reviewer","phase":"challenge","slot":"r1-p0",
            "agentName":format!("rp-{}-r1-p0", "a".repeat(20))}]});
    value
}

#[test]
fn native_github_v5_keeps_producer_evidence_when_some_tool_calls_fail() {
    let mut value = peer_reply();
    value["reviewedPhase"]["capabilities"] =
        json!(["filesystem-read", "filesystem-write", "github-pr"]);
    value["reviewedPhase"]["minToolCalls"] = json!(0);
    value["reviewedPhase"]["maxToolCalls"] = json!(8);
    let phase = serde_json::from_value(value["reviewedPhase"].clone()).unwrap();
    value["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    value["evidence"]["rounds"] = json!(8);
    value["evidence"]["phase"] = json!({"name":value["reviewedPhase"]["name"],
        "attemptedToolCalls":7,"successfulToolCalls":1,"minToolCalls":0,"maxToolCalls":8});
    let result = mission_result(&output(&value));
    assert_eq!(result.source.as_deref(), Some("durable_agent"));
    assert!(result.usage_known && result.run_evidence.is_some());
    value["reviewedPhase"]["capabilities"] = json!(["filesystem-read", "filesystem-write"]);
    invalid(&value); // Changing reviewed authority still invalidates the bound digest.
}

#[test]
fn peer_context_native_success_and_failure_preserve_recorded_evidence() {
    let original = peer_reply();
    assert!(mission_result(&output(&original)).reviewable);
    for status in ["failed", "rejected"] {
        let mut value = original.clone();
        value["status"] = json!(status);
        value["error"] = json!("Required exports missing");
        value.as_object_mut().unwrap().remove("artifacts");
        value.as_object_mut().unwrap().remove("output");
        let result = mission_result(&output(&value));
        assert!(result.run_evidence.is_some());
        assert!(result.usage_known);
        assert_eq!(result.run_evidence.unwrap().status, status);
    }
    for pointer in ["/peerContext", "/requiredOutputs", "/phaseDigest"] {
        let mut value = original.clone();
        value.as_object_mut().unwrap().remove(&pointer[1..]);
        invalid(&value);
    }
    let mut value = original;
    value["requiredOutputs"] = json!(["missing.md"]);
    invalid(&value);
}

#[test]
fn peer_context_rejects_malformed_cross_role_duplicate_and_legacy_locators() {
    let original = peer_reply();
    for (pointer, value) in [
        ("/peerContext", json!(null)),
        ("/peerContext/rootUid", json!("bad uid")),
        (
            "/peerContext/stateName",
            json!(format!("kars-reviewed-{}", "A".repeat(40))),
        ),
        ("/peerContext/slot", json!("r8-p0")),
        ("/peerContext/slot", json!("r2-p0")),
        ("/peerContext/peers", json!([])),
        ("/peerContext/peers/0/role", json!("-invalid")),
        ("/peerContext/peers/0/phase", json!("Invalid")),
        ("/peerContext/peers/0/slot", json!("r0-p8")),
        ("/peerContext/peers/0/agentName", json!("unrelated-agent")),
        ("/peerContext/peers/1/role", json!("researcher")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        invalid(&changed);
    }
    for version in [1, 2, 3, 4, 6] {
        let mut value = original.clone();
        value["version"] = json!(version);
        invalid(&value);
    }
    for pointer in ["/peerContext", "/peerContext/peers/0"] {
        let mut value = original.clone();
        value.pointer_mut(pointer).unwrap()["extra"] = json!(true);
        invalid(&value);
    }
    let mut value = original.clone();
    value["peerContext"]["peers"][1] = original["peerContext"]["peers"][0].clone();
    invalid(&value);
    value = original;
    value["peerContext"]["slot"] = json!("synthesis");
    value["peerContext"]["peers"] = json!([]);
    assert!(mission_result(&output(&value)).reviewable);
}

#[test]
fn peer_context_accepts_bounded_rosters_and_rejects_role_index_aliases() {
    let mut value = peer_reply();
    let peers: Vec<_> = (0..8)
        .flat_map(|r| {
            (0..8).map(move |p| json!({
        "role":format!("role-{r}"), "phase":format!("phase-{p}"),
        "slot":format!("r{r}-p{p}"), "agentName":format!("rp-{}-r{r}-p{p}", "a".repeat(20))
    }))
        })
        .collect();
    value["peerContext"]["peers"] = json!(peers);
    assert!(mission_result(&output(&value)).reviewable);
    for (field, changed) in [("role", "another-role"), ("phase", "phase-0")] {
        let mut invalid_roster = value.clone();
        invalid_roster["peerContext"]["peers"][1][field] = json!(changed);
        invalid(&invalid_roster);
    }
    value["peerContext"]["peers"]
        .as_array_mut()
        .unwrap()
        .push(peers[0].clone());
    invalid(&value);
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
    reply.as_object_mut().unwrap().remove("artifacts");
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
fn terminal_agent_display_names_match_live_and_dispatcher_contracts() {
    for fixture in fixtures() {
        for name in [
            "coordinator / plan-write",
            "Checklist writer",
            "Review — 日本語",
        ] {
            let mut data = output(&fixture["reply"]);
            data.insert("agentName".into(), name.into());
            let result = mission_result(&data);
            assert!(result.reviewable && result.usage_known, "{name}");
            assert_eq!(result.run_evidence.unwrap().agent_name, name);
        }
    }
    for name in ["", " \t\n", &"x".repeat(513), &"日".repeat(171)] {
        let mut data = output(&reply(4));
        data.insert("agentName".into(), name.into());
        let result = mission_result(&data);
        assert!(!result.reviewable && !result.usage_known);
        assert_eq!(result.source.as_deref(), Some("invalid_evidence"));
    }
    assert!(valid_agent_name(&"x".repeat(512)));
}

#[test]
fn failed_python_receipts_are_diagnostics_with_exact_native_authority() {
    let mut terminal = fetch_reply();
    terminal["reviewedPhase"]["executionMode"] = json!("native");
    terminal["reviewedPhase"]["capabilities"] = json!([
        "filesystem-read",
        "filesystem-write",
        "network",
        "python-execute"
    ]);
    let phase = serde_json::from_value(terminal["reviewedPhase"].clone()).unwrap();
    terminal["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    terminal["status"] = json!("failed");
    terminal["error"] = json!("Application exceeded process limit");
    terminal.as_object_mut().unwrap().remove("output");
    terminal["artifacts"] = json!({"kars-python-executions.json":"{\"version\":1,\"runs\":[]}"});
    let result = mission_result(&output(&terminal));
    assert_eq!(result.source.as_deref(), Some("durable_agent"));
    assert_eq!(result.status.as_deref(), Some("failed"));
    assert!(!result.reviewable && result.usage_known && result.output.is_empty());
    for artifacts in [
        json!({"report.md":"not a completed deliverable"}),
        json!({"kars-python-executions.json":"x".repeat(65537)}),
        json!({"kars-python-executions.json":" "}),
    ] {
        let mut changed = terminal.clone();
        changed["artifacts"] = artifacts;
        invalid(&changed);
    }
    terminal["reviewedPhase"]["capabilities"] =
        json!(["filesystem-read", "filesystem-write", "network"]);
    let phase = serde_json::from_value(terminal["reviewedPhase"].clone()).unwrap();
    terminal["phaseDigest"] = json!(contract::phase_digest(&phase).unwrap());
    invalid(&terminal);
}

#[test]
fn failed_observations_are_diagnostic_only_and_require_negotiated_valid_documents() {
    let content = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../inference-router/tests/fixtures/router-observations-v1.json"
    ));
    for version in [2, 3, 4] {
        let mut terminal = reply(version);
        terminal["status"] = json!("failed");
        terminal["error"] = json!("Write failed");
        terminal.as_object_mut().unwrap().remove("output");
        terminal.as_object_mut().unwrap().remove("artifacts");
        terminal["activityFormat"] = json!("router-observations-v1");
        terminal["routerObservations"] = json!(content);
        let result = mission_result(&output(&terminal));
        assert_eq!(result.source.as_deref(), Some("durable_agent"));
        assert_eq!(result.status.as_deref(), Some("failed"));
        assert!(!result.reviewable && result.output.is_empty());
        for bad in [json!(null), json!("{}"), json!("x".repeat(32 * 1024 + 1))] {
            let mut changed = terminal.clone();
            changed["routerObservations"] = bad;
            invalid(&changed);
        }
        let mut changed = terminal.clone();
        changed.as_object_mut().unwrap().remove("activityFormat");
        invalid(&changed);
        let mut changed = terminal.clone();
        changed["artifacts"] = json!({"briefing.md":"Not delivered"});
        invalid(&changed);
        let mut changed = terminal;
        changed["status"] = json!("rejected");
        invalid(&changed);
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
    for version in [2, 3, 4] {
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
fn required_outputs_are_bounded_unique_names_and_require_successful_exports() {
    for required in [
        Value::Null,
        json!({}),
        json!("briefing.md"),
        json!([1]),
        json!(["briefing.md", "briefing.md"]),
        json!([""]),
        json!(["../briefing.md"]),
        json!(["response.md"]),
        json!(["__proto__"]),
        json!(["constructor"]),
        json!(["prototype"]),
        json!(["résumé.md"]),
        json!(["x".repeat(129)]),
        json!((0..17).map(|i| format!("file-{i}.md")).collect::<Vec<_>>()),
    ] {
        let mut r = reply(4);
        r["requiredOutputs"] = required;
        r["status"] = json!("failed");
        r.as_object_mut().unwrap().remove("artifacts");
        invalid(&r);
    }
    for key in ["requiredOutputs", "artifactFormat", "artifacts"] {
        let mut r = reply(4);
        r.as_object_mut().unwrap().remove(key);
        invalid(&r);
    }
    for artifacts in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"briefing.md":"Partial result"}),
    ] {
        let mut r = reply(4);
        r["artifacts"] = artifacts;
        invalid(&r);
    }
    for content in [
        Value::Null,
        json!(1),
        json!(""),
        json!(" \n\t\u{feff}"),
        json!("é".repeat(65536)),
    ] {
        let mut r = reply(4);
        r["artifacts"]["briefing.md"] = content;
        invalid(&r);
    }
    for name in [
        "response.md",
        "__proto__",
        "constructor",
        "prototype",
        "../extra.md",
    ] {
        let mut r = reply(4);
        r["artifacts"][name] = json!("Unrequested invalid export");
        invalid(&r);
    }
    for format in [Value::Null, json!("text-v2")] {
        let mut r = reply(4);
        r["artifactFormat"] = format;
        invalid(&r);
    }
    let mut r = reply(4);
    for i in 0..15 {
        r["artifacts"][format!("extra-{i}.md")] = json!("Extra export");
    }
    invalid(&r);
    for status in ["failed", "rejected"] {
        let mut r = reply(4);
        r["status"] = json!(status);
        invalid(&r);
    }
}

#[test]
fn activity_reservation_preserves_ordinary_wire_and_artifact_limits() {
    let mut value = reply(4);
    let observations = format!(
        "{}{}",
        json!({"version":1,"source":"runtime-forwarded-router-observations",
        "coverage":"returned-responses-only","responses":1,"rounds":[1],"state":"unavailable",
        "reason":"Router observations unavailable after execution (reset, expiry or read failure)"}),
        " ".repeat(300 * 1024)
    );
    value["activityFormat"] = json!("router-observations-v1");
    value["requiredOutputs"] = json!(["report.md"]);
    value["artifacts"] =
        json!({"report.md":"x".repeat(120 * 1024), "kars-router-observations.json":observations});
    assert!(contract::within_wire_bound(&value));
    assert!(mission_result(&output(&value)).reviewable);
    value["extra"] = json!("x".repeat(192 * 1024));
    assert!(!contract::within_wire_bound(&value));
    invalid(&value);
    value.as_object_mut().unwrap().remove("extra");
    value["artifacts"]["report.md"] = json!("x".repeat(128 * 1024));
    invalid(&value);
    value["artifacts"]["report.md"] = json!("small");
    value["artifacts"]["kars-router-observations.json"] = json!("x".repeat(384 * 1024));
    invalid(&value);
}

#[test]
fn required_output_wire_boundaries_preserve_valid_artifacts() {
    let mut r = reply(4);
    r["requiredOutputs"] = json!(["a"]);
    // JSON envelope for {"a":"..."} is eight bytes; enforce serialized UTF-8 size.
    r["artifacts"] = json!({"a":"é".repeat((128 * 1024 - 8) / 2)});
    assert_eq!(
        serde_json::to_vec(&r["artifacts"]).unwrap().len(),
        128 * 1024
    );
    assert!(mission_result(&output(&r)).reviewable);
    r["artifacts"]["a"] = json!(format!("{}x", r["artifacts"]["a"].as_str().unwrap()));
    invalid(&r);
    let name = "x".repeat(128);
    r["requiredOutputs"] = json!([name]);
    r["artifacts"] = json!({name: "\u{0085}"});
    assert!(mission_result(&output(&r)).reviewable);
    let names = (0..16)
        .rev()
        .map(|i| format!("file-{i}.md"))
        .collect::<Vec<_>>();
    r["requiredOutputs"] = json!(names);
    r["artifacts"] = json!(
        names
            .iter()
            .map(|name| (name, "Valid export"))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(mission_result(&output(&r)).reviewable);
}

#[test]
fn rejects_cross_version_fields_even_when_null() {
    for version in [1, 2, 3] {
        for required in [Value::Null, json!([]), json!(["briefing.md"])] {
            let mut r = reply(version);
            r["requiredOutputs"] = required;
            invalid(&r);
        }
    }
    for version in [1, 2, 3, 4] {
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
    for version in [2, 3, 4] {
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
    for version in [3, 4] {
        for value in [
            Value::Null,
            json!("sha256:short"),
            json!(format!("sha256:{}", "A".repeat(64))),
            json!("a".repeat(64)),
        ] {
            let mut r = reply(version);
            r["inputDigest"] = value;
            invalid(&r);
        }
        let mut r = reply(version);
        r.as_object_mut().unwrap().remove("inputDigest");
        if version == 3 {
            invalid(&r);
        } else {
            assert!(mission_result(&output(&r)).reviewable);
        }
        for capabilities in [json!([]), json!(["filesystem-write"])] {
            let mut r = reply(version);
            r["inputDigest"] = reply(3)["inputDigest"].clone();
            r["reviewedPhase"]["capabilities"] = capabilities;
            r["reviewedPhase"]["minToolCalls"] = json!(0);
            without_evidence_with_fresh_digest(&mut r);
            invalid(&r);
        }
        let mut r = reply(version);
        r["inputDigest"] = reply(3)["inputDigest"].clone();
        r["reviewedPhase"]["minToolCalls"] = json!(0);
        r["reviewedPhase"]["maxToolCalls"] = json!(0);
        without_evidence_with_fresh_digest(&mut r);
        invalid(&r);
    }
}

#[test]
fn phase_counts_must_match_contract_and_successful_minimum() {
    for version in [2, 3, 4] {
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
    for version in [1, 2, 3, 4] {
        for status in ["failed", "rejected"] {
            let mut r = reply(version);
            r["status"] = json!(status);
            r.as_object_mut().unwrap().remove("artifacts");
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
    for version in [2, 3, 4] {
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
