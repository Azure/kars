// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn run(
    route: &str,
    pkg: &str,
    accepted: bool,
    tokens: i64,
    wall: i64,
    ttfa: i64,
    fail: i64,
) -> RunMetrics {
    RunMetrics {
        route: route.into(),
        harness: "OpenClaw".into(),
        package: pkg.into(),
        delivered: tokens > 0,
        accepted,
        verified: true,
        total_tokens: Some(tokens),
        prompt_tokens: Some(tokens / 2),
        completion_tokens: Some(tokens / 2),
        rounds: Some(3),
        tool_calls: Some(4),
        tool_fail: Some(fail),
        wall_ms: Some(wall),
        ttfa_ms: Some(ttfa),
        cached_tokens: Some(0),
        fault: String::new(),
    }
}

#[test]
fn derive_from_trace_computes_wall_ttfa_and_fails() {
    let trace = r#"[
          {"kind":"round","ms":800,"ts":"2026-07-02T10:00:00Z","cached_tokens":100},
          {"kind":"tool","ms":0,"ok":true,"ts":"2026-07-02T10:00:00Z"},
          {"kind":"tool","ms":0,"ok":false,"ts":"2026-07-02T10:00:01Z"},
          {"kind":"round","ms":1200,"ts":"2026-07-02T10:00:05Z","cached_tokens":200}
        ]"#;
    let (wall, ttfa, fail, cached) = derive_from_trace(trace);
    assert_eq!(ttfa, 800, "TTFA is the first round latency");
    assert_eq!(fail, 1, "one failed tool");
    // span 0s→5s = 5000ms + last round ms 1200
    assert_eq!(wall, 6200);
    assert_eq!(cached, 300, "cached tokens summed across rounds");
}

#[test]
fn derive_from_trace_is_robust_to_garbage() {
    assert_eq!(derive_from_trace("not json"), (0, 0, 0, 0));
    assert_eq!(derive_from_trace("{}"), (0, 0, 0, 0));
}

#[test]
fn cache_hit_rate_from_cached_tokens() {
    let mut r = run("A", "p1", true, 2000, 5000, 500, 0); // prompt = 1000
    r.cached_tokens = Some(800);
    let dto = aggregate(vec![r], &BTreeMap::new());
    assert!(
        (dto.routes[0].cache_hit_rate.unwrap() - 0.8).abs() < 1e-6,
        "800/1000 cached"
    );
}

#[test]
fn fault_classification_is_deterministic() {
    assert_eq!(classify_fault(true, 5, "length"), "", "accepted → no fault");
    assert_eq!(classify_fault(false, 0, "content_filter"), "policy");
    assert_eq!(classify_fault(false, 0, "length"), "capacity");
    assert_eq!(
        classify_fault(false, 3, "stop"),
        "environment",
        "tool failures → environment"
    );
    assert_eq!(
        classify_fault(false, 0, "stop"),
        "",
        "clean stop but unaccepted → quality miss, no mechanical fault"
    );
    assert_eq!(classify_fault(false, 0, ""), "", "no signal → unattributed");
}

#[test]
fn top_fault_is_the_dominant_one() {
    let mut r1 = run("A", "p1", false, 1000, 100, 50, 2);
    r1.fault = "environment".into();
    let mut r2 = run("A", "p2", false, 1000, 100, 50, 0);
    r2.fault = "environment".into();
    let mut r3 = run("A", "p3", false, 1000, 100, 50, 0);
    r3.fault = "capacity".into();
    let dto = aggregate(vec![r1, r2, r3], &BTreeMap::new());
    assert_eq!(dto.routes[0].top_fault, "environment");
}

#[test]
fn last_finish_reason_picks_final_round() {
    let trace = r#"[
          {"kind":"round","finish_reason":"tool_calls"},
          {"kind":"tool","ok":true},
          {"kind":"round","finish_reason":"length"}
        ]"#;
    assert_eq!(last_finish_reason(trace), "length");
    assert_eq!(last_finish_reason("garbage"), "");
}

#[test]
fn passk_reliability_only_counts_repeated_packages() {
    // route A: package p1 run twice (both accepted) → reliable; p2 once (ignored).
    let runs = vec![
        run("A", "p1", true, 1000, 5000, 500, 0),
        run("A", "p1", true, 1100, 5200, 400, 0),
        run("A", "p2", true, 900, 4000, 300, 0),
    ];
    let dto = aggregate(runs, &BTreeMap::new());
    let a = dto.routes.iter().find(|r| r.route == "A").unwrap();
    assert_eq!(a.reliability_samples, 1, "only p1 repeated");
    assert_eq!(a.reliability_rate, Some(1.0));
    assert_eq!(a.reliability_k, Some(2));
}

#[test]
fn passk_flags_inconsistent_package() {
    // p1 accepted once, rejected once → NOT fully reliable.
    let runs = vec![
        run("A", "p1", true, 1000, 5000, 500, 0),
        run("A", "p1", false, 1100, 5200, 400, 2),
    ];
    let dto = aggregate(runs, &BTreeMap::new());
    let a = dto.routes.iter().find(|r| r.route == "A").unwrap();
    assert_eq!(
        a.reliability_rate,
        Some(0.0),
        "inconsistent package fails pass^k"
    );
    assert_eq!(a.reliability_samples, 1);
}

#[test]
fn usd_per_outcome_only_when_priced() {
    let runs = vec![run("gpt-4o", "p1", true, 2_000_000, 5000, 500, 0)];
    // No prices → None.
    let dto = aggregate(runs.clone(), &BTreeMap::new());
    assert!(dto.routes[0].usd_per_outcome.is_none());
    assert!(!dto.priced);
    // Priced: 1M prompt @ $2.5 + 1M completion @ $10 = $12.5 over 1 outcome.
    let mut prices = BTreeMap::new();
    prices.insert("gpt-4o".to_string(), (2.5, 10.0));
    let dto = aggregate(runs, &prices);
    assert!(dto.priced);
    let usd = dto.routes[0].usd_per_outcome.unwrap();
    assert!((usd - 12.5).abs() < 1e-6, "got {usd}");
}

#[test]
fn tool_fail_rate_computed() {
    let runs = vec![run("A", "p1", true, 1000, 5000, 500, 2)]; // 2 fails of 4 calls
    let dto = aggregate(runs, &BTreeMap::new());
    assert!((dto.routes[0].tool_fail_rate.unwrap() - 0.5).abs() < 1e-6);
}

fn terminal_fixture(
    runtime: &str,
) -> (
    BTreeMap<String, String>,
    k8s_openapi::api::core::v1::ConfigMap,
) {
    use serde_json::json;
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../tasks/run_evidence/protocol-fixtures.json")).unwrap();
    let reply = fixtures
        .iter()
        .find(|f| f["name"] == "required-outputs")
        .unwrap()["reply"]
        .clone();
    let mut data = BTreeMap::new();
    for key in [
        "taskName",
        "taskUid",
        "assignmentId",
        "agentDid",
        "dispatcherDid",
        "sandboxUid",
        "podUid",
        "output",
    ] {
        data.insert(key.into(), reply[key].as_str().unwrap().into());
    }
    for (key, value) in [
        ("assignmentNonce", "rev-2"),
        ("runtimeBootId", "old-boot"),
        ("agentName", "briefing"),
        ("status", "ok"),
        ("error", ""),
        ("model", "recorded-model"),
        ("totalTokens", "7425"),
        ("usageKnown", "true"),
        ("artifactCount", "3"),
        ("startedAt", "2026-10-03T06:47:40Z"),
        ("finishedAt", "2026-10-03T06:47:45Z"),
    ] {
        data.insert(key.into(), value.into());
    }
    data.insert("evidence.json".into(), reply.to_string());
    let content = "Write a useful briefing from the reviewed inputs.";
    let mut assignment = reply.clone();
    for key in ["status", "output", "artifacts", "evidence"] {
        assignment.as_object_mut().unwrap().remove(key);
    }
    assignment["type"] = json!("mission:assign");
    assignment["content"] = json!(content);
    let mut candidate = assignment.clone();
    candidate["namespace"] = json!("kars-system");
    candidate["agentName"] = json!("briefing");
    candidate["runtime"] = json!(runtime);
    let attempt = json!({
        "version": 1, "candidate": candidate, "assignment": assignment, "reply": reply,
        "phase": "succeeded", "ownerSession": "dispatcher-session",
        "contentDigest": format!("sha256:{}", crate::providers::signing::sha256_hex(content)),
        "startedAt": data["startedAt"], "updatedAt": data["finishedAt"],
        "events": [{"at": data["finishedAt"], "status": "succeeded", "evidence": reply["evidence"]}]
    });
    let cm = serde_json::from_value(json!({
        "apiVersion":"v1", "kind":"ConfigMap",
        "metadata": {"name": crate::kars::cluster::Cluster::mission_attempt_name("task-uid", "rev-2"),
            "namespace":"kars-system", "uid":"attempt-uid", "resourceVersion":"3",
            "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"briefing","uid":"task-uid","controller":true}]},
        "data":{"attempt.json":attempt.to_string()}
    })).unwrap();
    (data, cm)
}

fn approved() -> BTreeMap<String, String> {
    [
        ("taskUid", "task-uid"),
        ("assignmentNonce", "rev-2"),
        ("status", "approved"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}

#[test]
fn modern_producer_projection_recovers_terminal_measurements_and_harness() {
    for runtime in ["OpenClaw", "Hermes"] {
        let (mut data, cm) = terminal_fixture(runtime);
        // Flat fields and current Task presentation are not the modern authority.
        data.insert("rounds".into(), "999".into());
        data.insert("harness".into(), "wrong".into());
        data.insert(
            "objective".into(),
            "Today's changed private objective".into(),
        );
        let r = normalize_run(
            &data,
            &approved(),
            Some(&cm),
            Some("[{\"kind\":\"round\",\"ms\":999}]"),
        );
        assert!(r.verified && r.delivered && r.accepted);
        assert_eq!(r.harness, runtime);
        assert_eq!(
            (r.total_tokens, r.prompt_tokens, r.completion_tokens),
            (Some(7425), Some(6940), Some(485))
        );
        assert_eq!(
            (r.rounds, r.tool_calls, r.tool_fail, r.wall_ms),
            (Some(2), Some(1), Some(0), Some(5000))
        );
        assert_eq!((r.ttfa_ms, r.cached_tokens), (None, None));
        assert_eq!(r.package.len(), 64);
        let dto = aggregate(vec![r], &BTreeMap::new());
        assert_eq!(dto.recommended_harness.as_deref(), Some(runtime));
        assert!(dto.recommended_low_confidence);
        assert!(
            !serde_json::to_string(&dto)
                .unwrap()
                .contains("private objective")
        );
    }
}

#[test]
fn missing_or_mismatched_terminal_never_falls_back_to_flattened_telemetry() {
    let (data, cm) = terminal_fixture("Hermes");
    for (key, replacement) in [
        ("evidence.json", None),
        ("evidence.json", Some("{}")),
        ("assignmentNonce", Some("other")),
        ("taskUid", Some("other")),
        ("totalTokens", Some("1")),
        ("usageKnown", Some("false")),
    ] {
        let mut changed = data.clone();
        if let Some(value) = replacement {
            changed.insert(key.into(), value.into());
        } else {
            changed.remove(key);
        }
        let run = normalize_run(&changed, &approved(), Some(&cm), None);
        assert!(!run.verified && !run.delivered && !run.accepted, "{key}");
        assert!(run.total_tokens.is_none() && run.rounds.is_none(), "{key}");
        assert!(aggregate(vec![run], &BTreeMap::new()).recommended.is_none());
    }
}

#[test]
fn missing_or_mismatched_attempt_cannot_infer_harness_or_repeated_package() {
    let (data, cm) = terminal_fixture("Hermes");
    let unbound = normalize_run(&data, &approved(), None, None);
    assert!(unbound.harness.is_empty() && unbound.package.is_empty());
    assert!(unbound.total_tokens.is_some());
    for (pointer, value) in [
        ("/candidate/runtime", serde_json::json!("not-a-runtime")),
        (
            "/candidate/content",
            serde_json::json!("Different private input"),
        ),
        ("/candidate/podUid", serde_json::json!("other-pod")),
        (
            "/assignment/assignmentId",
            serde_json::json!("other-assignment"),
        ),
        ("/events/0/evidence/rounds", serde_json::json!(1)),
        ("/events/0/status", serde_json::json!("running")),
        ("/events/0/at", serde_json::json!("2025-01-01T00:00:00Z")),
        ("/reply/output", serde_json::json!("Different deliverable")),
    ] {
        let mut changed = cm.clone();
        let raw = changed
            .data
            .as_mut()
            .unwrap()
            .get_mut("attempt.json")
            .unwrap();
        let mut attempt: Value = serde_json::from_str(raw).unwrap();
        *attempt.pointer_mut(pointer).unwrap() = value;
        *raw = attempt.to_string();
        let r = normalize_run(&data, &approved(), Some(&changed), None);
        assert!(r.harness.is_empty() && r.package.is_empty(), "{pointer}");
        assert!(r.tool_calls.is_none(), "{pointer}");
        assert!(aggregate(vec![r], &BTreeMap::new()).recommended.is_none());
    }
    let mut changed = cm;
    changed.metadata.owner_references.as_mut().unwrap()[0].uid = "different-task".into();
    assert!(
        normalize_run(&data, &approved(), Some(&changed), None)
            .harness
            .is_empty()
    );
}

#[test]
fn approval_requires_the_exact_reviewed_task_and_nonce_not_a_parent_or_prior_run() {
    let (data, cm) = terminal_fixture("OpenClaw");
    assert!(normalize_run(&data, &approved(), Some(&cm), None).accepted);
    for (key, value) in [
        ("taskUid", "parent-uid"),
        ("assignmentNonce", "rev-1"),
        ("status", "changes_requested"),
    ] {
        let mut review = approved();
        review.insert(key.into(), value.into());
        let r = normalize_run(&data, &review, Some(&cm), None);
        assert!(!r.accepted && r.delivered);
    }
}

#[test]
fn unknown_measurements_are_null_and_do_not_dilute_cost_or_win_recommendations() {
    let known = run("known", "a", true, 1000, 50, 10, 0);
    let missing = RunMetrics {
        route: "cheap-looking".into(),
        harness: "Hermes".into(),
        delivered: true,
        accepted: true,
        verified: true,
        ..Default::default()
    };
    let dto = aggregate(vec![missing.clone(), known.clone()], &BTreeMap::new());
    let r = dto
        .routes
        .iter()
        .find(|r| r.route == "cheap-looking")
        .unwrap();
    assert!(r.avg_tokens.is_none() && r.avg_rounds.is_none() && r.avg_tool_calls.is_none());
    assert!(r.avg_wall_ms.is_none() && r.cache_hit_rate.is_none() && r.tool_fail_rate.is_none());
    assert_eq!(dto.recommended.as_deref(), Some("known"));
    let dto = aggregate(
        vec![
            RunMetrics {
                route: known.route.clone(),
                harness: known.harness.clone(),
                ..missing
            },
            known,
        ],
        &BTreeMap::new(),
    );
    assert_eq!(dto.routes[0].tokens_per_outcome, Some(1000));
    assert_eq!(dto.routes[0].measured_tokens, Some(1000));
    assert_eq!(dto.routes[0].measured_token_runs, 1);
    assert!(
        dto.recommended.is_none(),
        "incomplete cost coverage is not recommendation authority"
    );
}

#[test]
fn legacy_history_is_visible_but_never_verified_or_recommended() {
    let data = [
        ("model", "old-model"),
        ("harness", "OpenClaw"),
        ("output", "A useful historical briefing"),
        ("status", "ok"),
        ("totalTokens", "100"),
        ("objective", "Original task"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let r = normalize_run(&data, &BTreeMap::new(), None, None);
    assert!(r.delivered && !r.verified && !r.accepted);
    assert_eq!(r.total_tokens, Some(100));
    assert!(r.prompt_tokens.is_none() && r.rounds.is_none());
    let mut prices = BTreeMap::new();
    prices.insert("old".into(), (1.0, 2.0));
    let dto = aggregate(vec![r], &prices);
    assert_eq!(dto.routes[0].verified_runs, 0);
    assert!(dto.routes[0].usd_per_outcome.is_none() && dto.recommended.is_none());
}

#[test]
fn recommendation_basis_is_for_exact_winning_harness_and_factual_ranking() {
    let mut other = run("same-model", "a", false, 1, 50, 10, 0);
    other.harness = "Hermes".into();
    let winner = run("same-model", "b", true, 10_000, 50, 10, 0);
    let dto = aggregate(vec![other, winner], &BTreeMap::new());
    assert_eq!(dto.routes.len(), 2);
    assert_eq!(dto.recommended_harness.as_deref(), Some("OpenClaw"));
    let basis = dto.recommended_basis.unwrap();
    assert!(basis.contains("same-model on OpenClaw") && basis.contains("1/1 accepted"));
    assert!(!basis.contains("Highest delivery success"));
    assert_ne!(
        package_key(&format!("{}A", "x".repeat(200))),
        package_key(&format!("{}B", "x".repeat(200)))
    );
    assert_ne!(package_key("Case"), package_key("case"));
}

#[test]
fn retained_review_history_counts_only_attested_exact_nonce_decisions_of_the_same_task() {
    use serde_json::json;
    let (data, _) = terminal_fixture("Hermes");
    let mut review = approved();
    review.insert("assignmentNonce".into(), "rev-3".into());
    review.insert("history".into(), json!([
        {"decision":"approve","reviewer":"user","decided_at":"2026-10-03T07:00:00Z","revision":1,"attested":true,"assignment_nonce":"rev-2"},
        {"decision":"request_changes","reviewer":"user","decided_at":"2026-10-03T06:00:00Z","revision":1,"attested":true,"assignment_nonce":"rev-2"}
    ]).to_string());
    assert!(accepted_revision(&data, &review));
    review.insert("taskUid".into(), "parent".into());
    assert!(!accepted_revision(&data, &review));
    review.insert("taskUid".into(), "task-uid".into());
    for (field, value) in [
        ("attested", json!(false)),
        ("assignment_nonce", json!("different")),
        ("decision", json!("request_changes")),
    ] {
        let mut changed = review.clone();
        let mut history: Value = serde_json::from_str(&review["history"]).unwrap();
        history[0][field] = value;
        changed.insert("history".into(), history.to_string());
        assert!(!accepted_revision(&data, &changed), "{field}");
    }
}

#[test]
fn failed_modern_terminal_preserves_unknown_usage_and_does_not_become_a_free_delivery() {
    use serde_json::json;
    let (mut data, _) = terminal_fixture("Hermes");
    let mut reply: Value = serde_json::from_str(&data["evidence.json"]).unwrap();
    reply["status"] = json!("failed");
    reply["output"] = json!("");
    reply["evidence"]["usage"] = Value::Null;
    reply.as_object_mut().unwrap().remove("artifacts");
    data.insert("evidence.json".into(), reply.to_string());
    data.insert("status".into(), "failed".into());
    data.insert("output".into(), "".into());
    data.insert("usageKnown".into(), "false".into());
    let r = normalize_run(&data, &approved(), None, None);
    assert!(r.verified && !r.accepted && !r.delivered);
    assert!(r.total_tokens.is_none());
    assert_eq!(r.rounds, Some(2));
    let dto = aggregate(vec![r], &BTreeMap::new());
    assert!(dto.recommended.is_none() && dto.routes[0].avg_tokens.is_none());
}
