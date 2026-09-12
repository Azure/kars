// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

pub fn fixture() -> (Corpus, Value) {
    let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
    let cases: Vec<_> = corpus.cases.iter().map(|case| json!({
        "caseId":case.id,"durationMs":1,
        "expected":{"decision":case.expect.decision.as_wire(),"byPolicyKind":case.expect.by_policy_kind.map(|kind|kind.as_wire()),
            "decisionAtLeastSome":case.expect.decision_at_least_some.map(|decision|decision.as_wire())},
        "actual":{"decision":case.expect.decision.as_wire(),"byPolicyKind":case.expect.by_policy_kind.map(|p|p.as_wire())},
        "verdict":{"result":"Pass"},
    })).collect();
    let report = json!({
        "schemaVersion":"v2","corpusName":corpus.name,"corpusDigest":"sha256:fixture",
        "routerBase":"http://demo.kars-demo.svc.cluster.local:8443",
        "startedAt":"2026-09-10T20:00:01Z","completedAt":"2026-09-10T20:00:03Z",
        "total":cases.len(),"passed":cases.len(),"failed":0,"errored":0,"results":cases,
    });
    (corpus, report)
}

fn check(value: &Value) -> Result<Report, &'static str> {
    let (corpus, _) = fixture();
    validate(
        value,
        &corpus,
        "sha256:fixture",
        "http://demo.kars-demo.svc.cluster.local:8443",
    )
}

#[test]
fn v1_history_is_readable_but_only_v2_can_qualify() {
    let (_, mut value) = fixture();
    assert!(check(&value).unwrap().ready());
    value["schemaVersion"] = json!("v1");
    value.as_object_mut().unwrap().remove("errored");
    let legacy = check(&value).unwrap();
    assert!(!legacy.qualified());
    assert!(!legacy.ready());
    assert_eq!(legacy.passed, 6);
}

#[test]
fn inconclusive_case_has_no_observed_policy_and_correct_nonzero_exit() {
    let (_, mut value) = fixture();
    value["results"][0]["actual"] = Value::Null;
    value["results"][0]["verdict"] = json!({"result":"Errored","category":"Transport"});
    value["passed"] = json!(5);
    value["errored"] = json!(1);
    let parsed = check(&value).unwrap();
    assert!(!parsed.ready());
    assert_eq!(parsed.exit_code(), 2);
    value["results"][0]["actual"] = json!({"decision":"Blocked"});
    assert!(check(&value).is_err());
    value["results"][0]["actual"] = Value::Null;
    value["results"][0]["verdict"]["category"] = json!("private response body");
    assert!(check(&value).is_err());
}

#[test]
fn malformed_counts_verdicts_and_wrong_current_corpus_are_rejected() {
    let (_, original) = fixture();
    let mut wrong_outcome = original.clone();
    wrong_outcome["outcome"] = json!("Inconclusive");
    assert!(check(&wrong_outcome).is_err());
    for (pointer, replacement) in [
        ("/total", json!(-1)),
        ("/passed", json!(6.5)),
        ("/failed", json!(true)),
        ("/total", json!(513)),
        ("/errored", json!(1)),
        ("/schemaVersion", json!("future")),
        ("/corpusDigest", json!("sha256:other")),
        ("/routerBase", json!("http://other")),
        ("/corpusName", json!("other")),
        ("/results/0/caseId", json!("foreign-case")),
        ("/results/0/verdict/result", json!("Unknown")),
        ("/results/0/actual/decision", json!("Allowed")),
        (
            "/results/0/verdict",
            json!({"result":"Fail","reason":"DecisionMismatch"}),
        ),
    ] {
        let mut candidate = original.clone();
        *candidate.pointer_mut(pointer).unwrap() = replacement;
        assert!(check(&candidate).is_err(), "{pointer}");
    }
}

#[test]
fn projection_excludes_raw_reasons_prompts_headers_and_scenario_payloads() {
    let (_, mut value) = fixture();
    value["results"][0]["actual"]["reason"] = json!("PRIVATE");
    value["results"][0]["scenario"] =
        json!({"kind":"ChatCompletion","messages":[{"content":"PRIVATE"}]});
    value["headers"] = json!({"authorization":"PRIVATE"});
    value["results"][0]["expected"]["reasonContains"] = json!("PRIVATE");
    let projection = check(&value).unwrap().wire.to_string();
    assert!(!projection.contains("PRIVATE"));
    assert!(!projection.contains("headers"));
    assert!(projection.contains("caseId"));
}

#[test]
fn duplicate_stdout_report_is_idempotent_conflicts_and_oversize_fail() {
    let (corpus, value) = fixture();
    let body = serde_json::to_string_pretty(&value).unwrap();
    let parse_body = |body: &str| {
        parse(
            body,
            &corpus,
            "sha256:fixture",
            "http://demo.kars-demo.svc.cluster.local:8443",
        )
    };
    assert!(
        parse_body(&format!("INFO starting\n{body}\n{body}"))
            .unwrap()
            .ready()
    );
    let mut changed = value.clone();
    changed["results"][0]["durationMs"] = json!(20);
    assert!(parse_body(&format!("{body}\n{changed}")).is_err());
    assert!(parse_body(&"x".repeat(MAX_REPORT_BYTES + 1)).is_err());
    assert!(parse_body(&"{".repeat(40)).is_err());
}
