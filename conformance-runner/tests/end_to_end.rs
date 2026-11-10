// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! End-to-end test for the `kars-conformance-runner` binary.
//!
//! Two router surfaces are mocked here:
//!
//! 1. A wiremock HTTP server for `/v1/chat/completions`, `/mcp`,
//!    `/memory_stores/{store}:search_memories` — used by ChatCompletion / ToolCall / MemoryRead
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
async fn inference_safety_diagnostics_are_case_bound_private_and_do_not_change_verdicts() {
    for detected in [false, true] {
        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "prompt_filter_results": [{"content_filter_results": {
                    "jailbreak": {"detected": detected, "filtered": false}
                }}],
                "choices": [{"finish_reason": "stop", "message": {
                    "content": "private-response-marker", "refusal": "private-refusal-marker"
                }}]
            })))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let report_path = dir.path().join("report.json");
        let output = Command::new(runner_binary())
            .env("KARS_EVAL_REPORT_FORMAT", "v2")
            .env("RUST_LOG", "info")
            .args([
                "--corpus",
                "builtin:jailbreak-baseline",
                "--only-case",
                "jb-003-roleplay-bypass",
                "--router-base",
                &server.uri(),
                "--no-stdout",
                "--output",
            ])
            .arg(&report_path)
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        let diagnostic = stderr
            .lines()
            .find(|line| line.contains("Inference safety observation"))
            .unwrap();
        assert!(diagnostic.contains("jb-003-roleplay-bypass"));
        assert!(diagnostic.contains("complete_shield_results: 1"));
        assert!(diagnostic.contains(&format!("detected_attacks: {}", usize::from(detected))));
        assert!(diagnostic.contains("structured_refusal_choices: 1"));
        let report_bytes = std::fs::read(report_path).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
        assert_eq!(report["total"], 1);
        assert_eq!(report["failed"], 1);
        assert_eq!(report["errored"], 0);
        assert_eq!(report["results"][0]["actual"]["decision"], "Allowed");
        assert_eq!(
            report["results"][0]["verdict"]["reason"],
            "DecisionMismatch"
        );
        for private in ["private-response-marker", "private-refusal-marker"] {
            assert!(!stderr.contains(private));
            assert!(!String::from_utf8_lossy(&report_bytes).contains(private));
        }
    }
}

#[tokio::test]
async fn governed_tunnel_corpus_requires_the_real_boundary_refusal() {
    const REFUSAL: &str =
        "Opaque tunnels are unsupported for governed inference; use mediated router APIs";
    assert!(include_str!("../../inference-router/src/forward_proxy.rs").contains(REFUSAL));
    let governed = "HTTP/1.1 403 Forbidden — Opaque tunnels are unsupported for governed inference; use mediated router APIs\r\nContent-Length: 0\r\n\r\n".as_bytes();
    for (corpus, response, exit, passed, errored) in [
        ("governed-egress-tunnels", governed, 0, 6, 0),
        ("egress-known-bad", governed, 1, 0, 0),
        (
            "governed-egress-tunnels",
            b"HTTP/1.1 403 host not in allowlist\r\n\r\n".as_slice(),
            1,
            0,
            0,
        ),
        (
            "governed-egress-tunnels",
            b"HTTP/1.1 403 Forbidden\r\n\r\n".as_slice(),
            1,
            0,
            0,
        ),
        (
            "governed-egress-tunnels",
            b"HTTP/1.1 502 Bad Gateway\r\n\r\n".as_slice(),
            2,
            0,
            6,
        ),
    ] {
        let (proxy_addr, _shutdown) = spawn_fake_forward_proxy(response).await;
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("report.json");
        let status = Command::new(runner_binary())
            .env("KARS_EVAL_REPORT_FORMAT", "v2")
            .args([
                "--corpus",
                &format!("builtin:{corpus}"),
                "--router-base",
                "http://127.0.0.1:1",
                "--forward-proxy",
                &proxy_addr,
                "--output",
            ])
            .arg(&output)
            .arg("--no-stdout")
            .status()
            .await
            .unwrap();
        assert_eq!(status.code(), Some(exit), "{corpus}");
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        assert_eq!(report["total"], 6);
        assert_eq!(report["passed"], passed);
        assert_eq!(report["errored"], errored);
        if response == governed {
            for case in report["results"].as_array().unwrap() {
                assert_eq!(case["actual"]["decision"], "Blocked");
                assert!(case["actual"].get("byPolicyKind").is_none());
                assert!(case["actual"].get("reason").is_none());
            }
        }
    }
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
