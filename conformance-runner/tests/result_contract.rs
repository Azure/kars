// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "../../controller/src/kars_eval_reconciler/report.rs"]
mod consumer;
#[path = "../../controller/src/kars_eval_reconciler/runner.rs"]
mod producer;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FORMAT_ENV: &str = "KARS_EVAL_REPORT_FORMAT";
const PRIVATE: &str = "PRIVATE-RESPONSE-HEADER-OR-CREDENTIAL";

async fn invoke(
    router: &str,
    format: Option<&str>,
    empty: bool,
) -> (std::process::Output, Option<Value>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("report.json");
    let mut command = Command::new(env!("CARGO_BIN_EXE_kars-conformance-runner"));
    command
        .args([
            "--corpus",
            "builtin:jailbreak-baseline",
            "--router-base",
            router,
            "--output",
        ])
        .arg(&path)
        .env_remove(FORMAT_ENV)
        .arg("--auth-header")
        .arg(format!("Bearer {PRIVATE}"));
    if let Some(format) = format {
        command.env(FORMAT_ENV, format);
    }
    if empty {
        command.args(["--only-case", "nonexistent-case"]);
    }
    let output = command.output().await.unwrap();
    let report = std::fs::read(path)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap());
    (output, report)
}

fn consume(report: &Value, router: &str) -> consumer::Report {
    let bytes = kars_eval_corpus::builtin_bytes("jailbreak-baseline").unwrap();
    let corpus = kars_eval_corpus::parse(bytes).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
    consumer::parse(&report.to_string(), &corpus, &digest, router).unwrap()
}

async fn server(mixed: bool, allow_all: bool) -> MockServer {
    let server = MockServer::start().await;
    let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
    for (index, case) in corpus.cases.iter().enumerate() {
        let status = if mixed && index == 0 {
            500
        } else if allow_all
            || (mixed && index == 1)
            || case.expect.decision == kars_eval_corpus::Decision::Allowed
        {
            200
        } else {
            403
        };
        Mock::given(method("POST"))
            .and(header("x-kars-eval-case-id", case.id.as_str()))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "reason":case.expect.reason_contains.clone().unwrap_or_default(),
                "untrusted":PRIVATE,
            })))
            .mount(&server)
            .await;
    }
    server
}

#[tokio::test]
async fn actual_builder_negotiates_v2_without_a_new_cli_argument() {
    let server = server(false, false).await;
    let spec = producer::runner_pod_spec_json(
        "eval",
        "corpus",
        "custom-user:latest",
        &server.uri(),
        "builtin",
    );
    let format = spec["containers"][0]["env"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == FORMAT_ENV)
        .unwrap()["value"]
        .as_str()
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let corpus_path = directory.path().join("corpus.json");
    std::fs::write(
        &corpus_path,
        kars_eval_corpus::builtin_bytes("jailbreak-baseline").unwrap(),
    )
    .unwrap();
    // The separate controller_contract test parses the exact mount-path argv.
    // Here the real binary receives that same option shape with its fixture file.
    let mut args: Vec<String> = spec["containers"][0]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().into())
        .collect();
    args[1] = corpus_path.to_string_lossy().into_owned();
    args[5] = directory
        .path()
        .join("report.json")
        .to_string_lossy()
        .into_owned();
    let output = Command::new(env!("CARGO_BIN_EXE_kars-conformance-runner"))
        .args(args)
        .env(FORMAT_ENV, format)
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schemaVersion"], "v2");
    assert!(consume(&report, &server.uri()).ready());
    assert!(
        !spec["containers"][0]["args"]
            .to_string()
            .contains("--report")
    );
    assert!(spec["securityContext"].get("runAsUser").is_none());
}

#[tokio::test]
async fn default_v1_is_faithful_for_conclusive_runs_but_not_fresh_controller_readiness() {
    let server = server(false, false).await;
    let (output, report) = invoke(&server.uri(), None, false).await;
    assert_eq!(output.status.code(), Some(0));
    let report = report.unwrap();
    assert_eq!(report["schemaVersion"], "v1");
    assert!(report.get("errored").is_none());
    assert!(!consume(&report, &server.uri()).ready());
}

#[tokio::test]
async fn actual_unreachable_error_only_mixed_policy_fail_and_empty_runs_have_truthful_exits() {
    let (output, report) = invoke("http://127.0.0.1:1", Some("v2"), false).await;
    assert_eq!(output.status.code(), Some(2));
    let report = report.unwrap();
    assert_eq!(report["failed"], 0);
    assert_eq!(report["errored"], 6);
    assert!(
        report["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|case| case["actual"].is_null() && case["verdict"]["result"] == "Errored")
    );
    assert!(!consume(&report, "http://127.0.0.1:1").ready());
    assert!(!report.to_string().contains(PRIVATE));
    let (old, report) = invoke("http://127.0.0.1:1", None, false).await;
    assert_eq!(old.status.code(), Some(2));
    assert!(report.is_none());
    assert!(old.stdout.is_empty());
    let mixed = server(true, false).await;
    let (output, report) = invoke(&mixed.uri(), Some("v2"), false).await;
    assert_eq!(output.status.code(), Some(2));
    let report = report.unwrap();
    assert_eq!(report["errored"], 1);
    assert_eq!(report["failed"], 1);
    assert!(!consume(&report, &mixed.uri()).ready());
    assert!(!report.to_string().contains(PRIVATE));
    let allowing = server(false, true).await;
    let (output, report) = invoke(&allowing.uri(), Some("v2"), false).await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(consume(&report.unwrap(), &allowing.uri()).failed, 5);
    for format in [None, Some("v2")] {
        let (output, report) = invoke(&allowing.uri(), format, true).await;
        assert_eq!(output.status.code(), Some(2));
        if format.is_none() {
            assert!(report.is_none());
        } else {
            assert_eq!(report.unwrap()["total"], 0);
        }
    }
}
