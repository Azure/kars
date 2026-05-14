// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! End-to-end test for the `azureclaw-conformance-runner` binary.
//!
//! Spins up a wiremock-mocked router that returns a fixed response
//! shape for every request, invokes the runner binary against it,
//! and asserts the JSON `RunReport` written to `--output` matches
//! expectations.

use std::process::Command;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn runner_binary() -> std::path::PathBuf {
    // `target/debug/azureclaw-conformance-runner` relative to crate
    // root. The cargo integration-test layout puts the binary at
    // `CARGO_BIN_EXE_<name>` for binary crates.
    let p = env!("CARGO_BIN_EXE_azureclaw-conformance-runner");
    std::path::PathBuf::from(p)
}

#[tokio::test]
async fn egress_known_bad_all_pass_against_blocking_router() {
    let server = MockServer::start().await;

    // Every egress request returns 403 with the "not in allowlist"
    // reason — matches all 6 cases in the egress-known-bad corpus.
    Mock::given(method("POST"))
        .and(path_regex(r"^/internal/egress/connect$"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-azureclaw-decision", "Blocked")
                .insert_header("x-azureclaw-decision-by", "EgressAllowlist")
                .set_body_json(serde_json::json!({
                    "reason": "host not in allowlist"
                })),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp); // remove the file; runner will create it

    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:egress-known-bad")
        .arg("--router-base")
        .arg(server.uri())
        .arg("--output")
        .arg(&output_path)
        .arg("--no-stdout")
        .status()
        .expect("spawn runner");

    assert!(status.success(), "runner failed: {status:?}");

    let report_bytes = std::fs::read(&output_path).expect("read report");
    let report: serde_json::Value = serde_json::from_slice(&report_bytes).expect("parse report");

    assert_eq!(report["schemaVersion"], "v1");
    assert_eq!(report["corpusName"], "egress-known-bad");
    assert_eq!(report["total"], 6);
    assert_eq!(report["passed"], 6);
    assert_eq!(report["failed"], 0);

    let results = report["results"].as_array().expect("results array");
    assert_eq!(results.len(), 6);
    for r in results {
        assert_eq!(r["verdict"]["result"], "Pass");
    }

    let _ = std::fs::remove_file(&output_path);
}

#[tokio::test]
async fn egress_known_bad_router_allowing_yields_all_fail() {
    let server = MockServer::start().await;

    // Router returns 200 — every case expects Blocked, so every
    // case must fail with DecisionMismatch.
    Mock::given(method("POST"))
        .and(path_regex(r"^/internal/egress/connect$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp);

    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:egress-known-bad")
        .arg("--router-base")
        .arg(server.uri())
        .arg("--output")
        .arg(&output_path)
        .arg("--no-stdout")
        .status()
        .expect("spawn runner");

    // Any failure → exit code 1.
    assert_eq!(status.code(), Some(1), "expected exit 1 on all-fail");

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output_path).expect("read report"))
            .expect("parse report");
    assert_eq!(report["total"], 6);
    assert_eq!(report["passed"], 0);
    assert_eq!(report["failed"], 6);

    for r in report["results"].as_array().unwrap() {
        assert_eq!(r["verdict"]["result"], "Fail");
        assert_eq!(r["verdict"]["reason"], "DecisionMismatch");
    }

    let _ = std::fs::remove_file(&output_path);
}

#[tokio::test]
async fn unknown_builtin_corpus_exits_with_2() {
    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp);

    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:does-not-exist")
        .arg("--router-base")
        .arg("http://127.0.0.1:1")
        .arg("--output")
        .arg(&output_path)
        .arg("--no-stdout")
        .status()
        .expect("spawn runner");

    assert_eq!(status.code(), Some(2));
}

#[tokio::test]
async fn only_case_filter_runs_single_case() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/internal/egress/connect$"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-azureclaw-decision", "Blocked")
                .insert_header("x-azureclaw-decision-by", "EgressAllowlist")
                .set_body_json(serde_json::json!({
                    "reason": "host not in allowlist"
                })),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp);

    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:egress-known-bad")
        .arg("--router-base")
        .arg(server.uri())
        .arg("--output")
        .arg(&output_path)
        .arg("--only-case")
        .arg("egress-001-link-local")
        .arg("--no-stdout")
        .status()
        .expect("spawn runner");

    assert!(status.success());

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output_path).expect("read report"))
            .expect("parse report");
    assert_eq!(report["total"], 1);
    assert_eq!(report["results"][0]["caseId"], "egress-001-link-local");

    let _ = std::fs::remove_file(&output_path);
}
