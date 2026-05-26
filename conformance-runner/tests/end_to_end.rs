// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! End-to-end test for the `kars-conformance-runner` binary.
//!
//! Two router surfaces are mocked here:
//!
//! 1. A wiremock HTTP server for `/v1/chat/completions`, `/mcp`,
//!    `/platform/mcp` — used by ChatCompletion / ToolCall / MemoryRead
//!    scenarios.
//! 2. A bare TCP listener that speaks just enough of the forward-proxy
//!    HTTP-CONNECT protocol to return a configurable status line —
//!    used by EgressConnect scenarios.
//!
//! The forward-proxy fake is a multi-accept loop: it answers every
//! incoming connection with the same canned status line until it is
//! shut down via a `oneshot` channel at the end of the test.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::oneshot;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn runner_binary() -> std::path::PathBuf {
    let p = env!("CARGO_BIN_EXE_kars-conformance-runner");
    std::path::PathBuf::from(p)
}

/// Spawn a fake forward proxy that answers every CONNECT with
/// `response` until the returned shutdown sender is dropped. Returns
/// the bound `host:port` and a shutdown handle.
async fn spawn_fake_forward_proxy(response: &'static [u8]) -> (String, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, mut rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    let Ok((mut sock, _)) = accept else { continue; };
                    tokio::spawn(async move {
                        let mut buf = [0u8; 1024];
                        let mut total = 0;
                        while let Ok(n) = sock.read(&mut buf[total..]).await {
                            if n == 0 { break; }
                            total += n;
                            if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") { break; }
                            if total == buf.len() { break; }
                        }
                        let _ = sock.write_all(response).await;
                        let _ = sock.shutdown().await;
                    });
                }
                _ = &mut rx => break,
            }
        }
    });
    (addr, tx)
}

#[tokio::test]
async fn egress_known_bad_all_pass_against_blocking_proxy() {
    let (proxy_addr, _shutdown) =
        spawn_fake_forward_proxy(b"HTTP/1.1 403 host not in allowlist\r\n\r\n").await;

    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp);

    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:egress-known-bad")
        .arg("--router-base")
        .arg("http://127.0.0.1:1")
        .arg("--forward-proxy")
        .arg(&proxy_addr)
        .arg("--output")
        .arg(&output_path)
        .arg("--no-stdout")
        .status()
        .await
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
async fn egress_known_bad_proxy_allowing_yields_all_fail() {
    let (proxy_addr, _shutdown) =
        spawn_fake_forward_proxy(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;

    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp);

    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:egress-known-bad")
        .arg("--router-base")
        .arg("http://127.0.0.1:1")
        .arg("--forward-proxy")
        .arg(&proxy_addr)
        .arg("--output")
        .arg(&output_path)
        .arg("--no-stdout")
        .status()
        .await
        .expect("spawn runner");

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
        .await
        .expect("spawn runner");

    assert_eq!(status.code(), Some(2));
}

#[tokio::test]
async fn only_case_filter_runs_single_case() {
    let (proxy_addr, _shutdown) =
        spawn_fake_forward_proxy(b"HTTP/1.1 403 host not in allowlist\r\n\r\n").await;

    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp);

    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:egress-known-bad")
        .arg("--router-base")
        .arg("http://127.0.0.1:1")
        .arg("--forward-proxy")
        .arg(&proxy_addr)
        .arg("--output")
        .arg(&output_path)
        .arg("--only-case")
        .arg("egress-001-link-local")
        .arg("--no-stdout")
        .status()
        .await
        .expect("spawn runner");

    assert!(status.success());

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output_path).expect("read report"))
            .expect("parse report");
    assert_eq!(report["total"], 1);
    assert_eq!(report["results"][0]["caseId"], "egress-001-link-local");

    let _ = std::fs::remove_file(&output_path);
}

#[tokio::test]
async fn chat_completion_corpus_against_allowing_router() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/chat/completions$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let tmp = tempfile::NamedTempFile::new().expect("tmp file");
    let output_path = tmp.path().to_path_buf();
    drop(tmp);

    // The `jailbreak-baseline` corpus uses ChatCompletion scenarios
    // and expects Blocked decisions — running against a 200-returning
    // router should fail every case.
    let status = Command::new(runner_binary())
        .arg("--corpus")
        .arg("builtin:jailbreak-baseline")
        .arg("--router-base")
        .arg(server.uri())
        .arg("--output")
        .arg(&output_path)
        .arg("--no-stdout")
        .status()
        .await
        .expect("spawn runner");

    assert_eq!(status.code(), Some(1));

    // jailbreak-baseline has 5 jailbreak cases expecting Blocked + 1
    // benign control expecting Allowed. Against a 200-router, the 5
    // jailbreaks fail (DecisionMismatch) and the benign passes.
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output_path).expect("read report"))
            .expect("parse report");
    assert_eq!(report["passed"].as_u64().unwrap(), 1);
    assert_eq!(report["failed"].as_u64().unwrap(), 5);

    let _ = std::fs::remove_file(&output_path);
}
