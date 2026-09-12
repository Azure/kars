// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn auth_upstream_protocol_and_truncated_body_are_inconclusive_not_blocked() {
    for (status, category) in [
        (401, ReplayError::Authentication),
        (407, ReplayError::Authentication),
        (500, ReplayError::Upstream),
        (503, ReplayError::Upstream),
        (404, ReplayError::Protocol),
    ] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header(DECISION_HEADER, "Blocked")
                    .set_body_string("PRIVATE"),
            )
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        assert_eq!(
            response_to_decision(response, PolicyKindRef::InferencePolicy)
                .await
                .unwrap_err(),
            category
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let sender = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut used = 0;
            while !request[..used].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                assert!(
                    used < request.len(),
                    "request headers exceeded the fixture bound"
                );
                let count = stream.read(&mut request[used..]).await.unwrap();
                assert!(
                    count > 0,
                    "client closed before sending complete request headers"
                );
                used += count;
            }
            assert!(request[..used].starts_with(b"GET / HTTP/1.1\r\n"));
        })
        .await
        .unwrap();
        stream
            .write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{}",
            )
            .await
            .unwrap();
    });
    let response = reqwest::get(format!("http://{address}")).await.unwrap();
    assert_eq!(
        response_to_decision(response, PolicyKindRef::InferencePolicy)
            .await
            .unwrap_err(),
        ReplayError::BodyRead
    );
    sender.await.unwrap();
}

#[tokio::test]
async fn malformed_mcp_tool_errors_and_wrong_correlation_never_become_allowed_or_blocked() {
    for body in [
        "",
        "{}",
        "not json",
        r#"{"jsonrpc":"2.0","id":2,"result":{"content":[]}}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"rate limit policy denied"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{"content":[],"isError":true}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text"}]}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"image","data":"?","mimeType":"image/png"}]}}"#,
    ] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        assert_eq!(
            mcp_response_to_decision(response, PolicyKindRef::ToolPolicy)
                .await
                .unwrap_err(),
            ReplayError::Protocol
        );
    }
}

#[tokio::test]
async fn unreachable_or_malformed_forward_proxy_is_not_egress_enforcement() {
    assert!(
        egress_connect_via_proxy(
            "127.0.0.1:1",
            "example.test",
            443,
            "case",
            Duration::from_secs(1)
        )
        .await
        .is_err()
    );
    assert!(parse_http_status_line("GARBAGE 200 Allowed").is_err());
    assert!(parse_http_status_line("HTTP/1.1 999 Allowed").is_err());
}
