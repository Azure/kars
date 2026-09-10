// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::probe;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn fixture(mode: &'static str) -> (MockServer, Arc<Mutex<Vec<String>>>) {
    let server = MockServer::start().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let observed = calls.clone();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
        let body: Value = request.body_json().unwrap_or(Value::Null);
        let method = body["method"].as_str().unwrap_or(request.method.as_str());
        observed.lock().unwrap().push(method.into());
        if request.method == "DELETE" {
            assert_eq!(request.headers.get("mcp-session-id").unwrap(), "probe-session");
            return ResponseTemplate::new(204);
        }
        if method == "initialize" {
            if mode == "redirect" {
                return ResponseTemplate::new(302).insert_header("location", "/not-followed");
            }
            return ResponseTemplate::new(200).insert_header("mcp-session-id", "probe-session")
                .set_body_json(if mode == "initialize-error" {
                    json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32603,"message":"PRIVATE_SENTINEL"}})
                } else {
                    json!({"jsonrpc":"2.0","id":body["id"],"result":{
                        "protocolVersion":if mode=="version" {"unknown"} else {"2025-06-18"},"capabilities":{"tools":{}}}})
                });
        }
        if method == "notifications/initialized" {
            return ResponseTemplate::new(202);
        }
        assert_eq!(method, "tools/list");
        assert_eq!(request.headers.get("mcp-session-id").unwrap(), "probe-session");
        assert_eq!(request.headers.get("mcp-protocol-version").unwrap(), "2025-06-18");
        if mode == "list-error" {
            return ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc":"2.0","id":body["id"],"error":{"code":-32000,"message":"PRIVATE_SENTINEL"}}));
        }
        let second = body["params"]["cursor"] == "second";
        let mut result = json!({"tools":[{"name":if second {"second"} else {"first"},"inputSchema":{"type":"object"}}]});
        if mode == "pages" && !second { result["nextCursor"] = "second".into(); }
        if mode == "repeated" { result["nextCursor"] = "second".into(); }
        if mode == "oversize" { result["description"] = "x".repeat(300_000).into(); }
        let response = json!({"jsonrpc":"2.0","id":body["id"],"result":result});
        if mode == "sse" {
            ResponseTemplate::new(200)
                .set_body_raw(format!("event: message\ndata: {response}\n\n"), "text/event-stream")
        } else {
            ResponseTemplate::new(200).set_body_json(response)
        }
    }).mount(&server).await;
    (server, calls)
}

fn client() -> reqwest::Client {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap()
}

#[tokio::test]
async fn real_http_probe_negotiates_pages_and_closes_its_session_without_invoking_tools() {
    let (server, calls) = fixture("pages").await;
    let result = probe::probe(&client(), &server.uri(), &["*".into()])
        .await
        .unwrap();
    assert_eq!(result.names, ["first", "second"]);
    assert!(result.digest.starts_with("sha256:"));
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list",
            "DELETE"
        ]
    );
}

#[tokio::test]
async fn sse_probe_honors_the_allowlist_and_closes_session() {
    let (server, calls) = fixture("sse").await;
    let result = probe::probe(&client(), &server.uri(), &["different".into()])
        .await
        .unwrap();
    assert!(result.names.is_empty());
    assert_eq!(calls.lock().unwrap().last().unwrap(), "DELETE");
}

#[tokio::test]
async fn semantic_failures_invalid_versions_and_size_limits_are_unqualified_and_private() {
    for mode in [
        "initialize-error",
        "list-error",
        "version",
        "repeated",
        "oversize",
    ] {
        let (server, calls) = fixture(mode).await;
        let error = probe::probe(&client(), &server.uri(), &["*".into()])
            .await
            .err()
            .unwrap();
        assert!(!error.contains("PRIVATE_SENTINEL"));
        assert_eq!(calls.lock().unwrap().last().unwrap(), "DELETE", "{mode}");
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|method| method == "tools/call")
        );
    }
}

#[tokio::test]
async fn probe_does_not_follow_redirects() {
    let (server, calls) = fixture("redirect").await;
    assert!(
        probe::probe(&client(), &server.uri(), &["*".into()])
            .await
            .is_err()
    );
    assert_eq!(*calls.lock().unwrap(), ["initialize"]);
}
