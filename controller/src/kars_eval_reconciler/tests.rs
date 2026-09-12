// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[test]
fn sha256_hex_known_vector() {
    assert_eq!(
        sha256_hex(b""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn short_hash_is_deterministic_and_compact() {
    let a = short_hash("12345");
    let b = short_hash("12345");
    let c = short_hash("12346");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 10);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn run_now_job_name_differs_per_resource_version() {
    let n1 = run_now_job_name("e1", Some("100"));
    let n2 = run_now_job_name("e1", Some("101"));
    let n3 = run_now_job_name("e1", None);
    assert_ne!(n1, n2);
    assert!(n1.starts_with("karseval-e1-runnow-"));
    assert!(n3.ends_with("now"));
}

#[test]
fn cron_job_name_stable() {
    assert_eq!(cron_job_name("my-eval"), "karseval-my-eval");
}

#[test]
fn sandbox_router_url_format() {
    assert_eq!(
        sandbox_router_url("agent-001"),
        "http://agent-001.kars-agent-001.svc.cluster.local:8443"
    );
}

#[test]
fn karseval_owner_refs_emits_controller_blocking_reference() {
    let refs = karseval_owner_refs("nightly-regression", "8f2a3b4c-1111-2222-3333-444455556666");
    let arr = refs.as_array().expect("owner refs must be a JSON array");
    assert_eq!(
        arr.len(),
        1,
        "exactly one owner reference per spawned child"
    );
    let r = &arr[0];
    assert_eq!(r["apiVersion"], "kars.azure.com/v1alpha1");
    assert_eq!(r["kind"], "KarsEval");
    assert_eq!(r["name"], "nightly-regression");
    assert_eq!(r["uid"], "8f2a3b4c-1111-2222-3333-444455556666");
    assert_eq!(
        r["controller"], true,
        "must mark controller=true so Jobs/CronJobs are GC'd with the parent",
    );
    assert_eq!(
        r["blockOwnerDeletion"], true,
        "finalizer correctness: parent delete waits for child cleanup",
    );
}

#[test]
fn default_runner_image_env_override() {
    // Save / restore the env var around the call to avoid bleeding
    // into other tests (cargo runs tests in parallel; this is
    // tolerated only because the assertion runs strictly inside
    // the unsafe-set/unsafe-remove brackets).
    let prior = std::env::var(RUNNER_IMAGE_ENV).ok();
    // SAFETY: tests are single-threaded within this function and the
    // env-var roundtrip is restored before exit. Other parallel
    // tests do not touch `RUNNER_IMAGE_ENV`.
    unsafe {
        std::env::set_var(RUNNER_IMAGE_ENV, "myrepo/runner:1.2.3");
    }
    assert_eq!(default_runner_image(), "myrepo/runner:1.2.3");
    unsafe {
        std::env::remove_var(RUNNER_IMAGE_ENV);
    }
    assert_eq!(default_runner_image(), DEFAULT_RUNNER_IMAGE);
    if let Some(prior) = prior {
        unsafe {
            std::env::set_var(RUNNER_IMAGE_ENV, prior);
        }
    }
}

#[test]
fn parse_report_happy_path() {
    let log = r#"
        2026-05-14T10:00:00Z INFO starting runner
        {"schemaVersion":"v1","corpusName":"builtin:jailbreak-baseline","corpusDigest":"sha256:abc","startedAt":"2026-05-14T10:00:00Z","completedAt":"2026-05-14T10:00:05Z","durationMs":5000,"routerBase":"http://x:8443","total":3,"passed":2,"failed":1,"results":[
          {"caseId":"c1","tags":[],"scenario":{"kind":"ChatCompletion","messageCount":1},"expected":{"decision":"Allowed"},"actual":{"decision":"Allowed"},"verdict":{"result":"Pass"},"durationMs":100},
          {"caseId":"c2","tags":[],"scenario":{"kind":"ChatCompletion","messageCount":1},"expected":{"decision":"Allowed"},"actual":{"decision":"Allowed"},"verdict":{"result":"Pass"},"durationMs":100},
          {"caseId":"c3","tags":[],"scenario":{"kind":"ChatCompletion","messageCount":1},"expected":{"decision":"Blocked"},"actual":{"decision":"Allowed"},"verdict":{"result":"Fail","reason":"DecisionMismatch","expected":"Blocked","actual":"Allowed"},"durationMs":100}
        ]}
        "#;
    let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
    let parsed = report::parse(log, &corpus, "unused", "unused").expect("parses readable legacy");
    assert_eq!(parsed.version, "v1");
    assert_eq!(parsed.total, 3);
    assert_eq!(parsed.passed, 2);
    assert_eq!(parsed.failed, 1);
    assert!(!parsed.qualified());
    assert_eq!(parsed.wire["results"][0]["verdict"]["result"], "Pass");
    assert_eq!(parsed.wire["results"][2]["verdict"]["result"], "Fail");
}

#[test]
fn parse_report_ignores_non_json_lines() {
    let log = "INFO booting\nERROR oh no\nnot json at all\n";
    let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
    assert!(report::parse(log, &corpus, "unused", "unused").is_err());
}

#[test]
fn malformed_count_only_reports_are_not_current_evidence() {
    let log = r#"{"schemaVersion":"v1","total":1,"passed":1,"failed":0,"results":[]}
{"schemaVersion":"v1","total":5,"passed":4,"failed":1,"results":[]}"#;
    let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
    assert!(report::parse(log, &corpus, "unused", "unused").is_err());
}

#[tokio::test]
async fn resolve_corpus_builtin_roundtrip() {
    let src = CorpusSource {
        builtin: Some("jailbreak-baseline".into()),
        bundle_ref: None,
    };
    let resolved = resolve_corpus(&src).await.expect("builtin loads");
    assert!(resolved.label.starts_with("builtin:"));
    assert!(resolved.digest.starts_with("sha256:"));
    assert!(!resolved.bytes.is_empty());
    // Bytes must round-trip through the eval-corpus parser.
    let _ = kars_eval_corpus::parse(&resolved.bytes).expect("re-parses");
}

#[tokio::test]
async fn resolve_corpus_unknown_builtin_errors() {
    let src = CorpusSource {
        builtin: Some("does-not-exist".into()),
        bundle_ref: None,
    };
    let (why, _msg) = resolve_corpus(&src).await.unwrap_err();
    assert_eq!(why, reason::CORPUS_BUILTIN_MISSING);
}

#[tokio::test]
async fn resolve_corpus_neither_set_errors() {
    let src = CorpusSource {
        builtin: None,
        bundle_ref: None,
    };
    let (why, _msg) = resolve_corpus(&src).await.unwrap_err();
    assert_eq!(why, reason::SPEC_INVALID);
}

#[test]
fn build_conditions_first_reconcile_has_no_result() {
    let resolved = ResolvedCorpus {
        bytes: vec![],
        digest: "sha256:deadbeef".into(),
        label: "builtin:jailbreak-baseline".into(),
    };
    let conds = build_conditions(
        &[],
        Some(1),
        &resolved,
        None,
        Some("karseval-x"),
        None,
        ("Pending", false),
    );
    let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
    assert_eq!(ready.status, "False");
    assert_eq!(ready.reason, "Scheduled");
    let drift = conds
        .iter()
        .find(|c| c.type_ == TYPE_CONFORMANCE_DRIFT)
        .unwrap();
    assert_eq!(drift.status, "False");
    assert_eq!(drift.reason, "NotEvaluated");
}

#[test]
fn build_conditions_drift_branches_ready_false_and_drift_true() {
    let resolved = ResolvedCorpus {
        bytes: vec![],
        digest: "sha256:abc".into(),
        label: "builtin:jailbreak-baseline".into(),
    };
    let r = EvalResult {
        schema_version: "v2".into(),
        corpus_digest: "sha256:abc".into(),
        total: 10,
        passed: 7,
        failed: 3,
        errored: 0,
        corpus_label: "builtin:jailbreak-baseline".into(),
        job_name: "karseval-x-runnow-aa".into(),
        first_failing_cases: vec!["c-1".into()],
    };
    let conds = build_conditions(
        &[],
        Some(2),
        &resolved,
        Some("job-x"),
        None,
        Some(&r),
        ("DriftDetected", true),
    );
    let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
    let degraded = conds.iter().find(|c| c.type_ == "Degraded").unwrap();
    let drift = conds
        .iter()
        .find(|c| c.type_ == TYPE_CONFORMANCE_DRIFT)
        .unwrap();
    assert_eq!(ready.status, "False");
    assert_eq!(ready.reason, "DriftDetected");
    assert_eq!(degraded.status, "True");
    assert_eq!(drift.status, "True");
}

#[test]
fn build_conditions_all_pass_branches_ready_true() {
    let resolved = ResolvedCorpus {
        bytes: vec![],
        digest: "sha256:abc".into(),
        label: "builtin:jailbreak-baseline".into(),
    };
    let r = EvalResult {
        schema_version: "v2".into(),
        corpus_digest: "sha256:abc".into(),
        total: 10,
        passed: 10,
        failed: 0,
        errored: 0,
        corpus_label: "builtin:jailbreak-baseline".into(),
        job_name: "karseval-x-runnow-bb".into(),
        first_failing_cases: vec![],
    };
    let conds = build_conditions(
        &[],
        Some(3),
        &resolved,
        None,
        Some("cj"),
        Some(&r),
        ("AllPassed", false),
    );
    let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
    assert_eq!(ready.status, "True");
    assert_eq!(ready.reason, "AllPassed");
}

#[test]
fn pod_spec_renders_corpus_mount_and_args() {
    let spec = runner_pod_spec_json(
        "my-eval",
        "karseval-my-eval-corpus",
        "myrepo/runner:1",
        "http://agent-1.kars-agent-1.svc.cluster.local:8443",
        "builtin:jailbreak-baseline",
    );
    let args = spec["containers"][0]["args"].as_array().unwrap();
    let args: Vec<&str> = args.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(args.contains(&"--corpus"));
    assert!(args.contains(&"/etc/kars/eval-corpus/corpus.json"));
    assert!(args.contains(&"--router-base"));
    assert!(args.contains(&"http://agent-1.kars-agent-1.svc.cluster.local:8443"));
    assert!(!args.contains(&"--corpus-label"));
    assert!(!args.contains(&"builtin:jailbreak-baseline"));
    let vol = &spec["volumes"][0];
    assert_eq!(vol["name"], "corpus");
    assert_eq!(vol["configMap"]["name"], "karseval-my-eval-corpus");
}
