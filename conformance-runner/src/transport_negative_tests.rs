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
async fn provider_bad_request_requires_explicit_inference_denial() {
    let reason = "Azure AI Content Safety blocked: prompt jailbreak detected (content_filter)";
    for status in [400, 403] {
        for decision in ["Blocked", "blocked"] {
            let server = MockServer::start().await;
            Mock::given(wiremock::matchers::any())
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header(DECISION_HEADER, decision)
                        .insert_header(DECISION_BY_HEADER, "InferencePolicy")
                        .insert_header(DECISION_REASON_HEADER, reason)
                        .set_body_json(serde_json::json!({"error": {"code": "content_filter"}})),
                )
                .mount(&server)
                .await;
            let response = reqwest::get(server.uri()).await.unwrap();
            assert_eq!(
                response_to_decision(response, PolicyKindRef::InferencePolicy)
                    .await
                    .unwrap(),
                ActualParts {
                    decision: Decision::Blocked,
                    by_policy_kind: Some(PolicyKindRef::InferencePolicy),
                    reason: Some(reason.into()),
                }
            );
            let response = reqwest::get(server.uri()).await.unwrap();
            if status == 400 {
                assert_eq!(
                    memory_response_to_decision(response).await.unwrap_err(),
                    ReplayError::Protocol
                );
            }
        }
    }
    for (decision, kind, reason, challenge, expected) in [
        (None, None, None, false, ReplayError::Protocol),
        (
            Some("blocked"),
            None,
            Some(reason),
            false,
            ReplayError::Protocol,
        ),
        (
            None,
            Some("InferencePolicy"),
            Some(reason),
            false,
            ReplayError::Protocol,
        ),
        (
            Some("Allowed"),
            Some("InferencePolicy"),
            Some(reason),
            false,
            ReplayError::Protocol,
        ),
        (
            Some("unknown"),
            Some("InferencePolicy"),
            Some(reason),
            false,
            ReplayError::Protocol,
        ),
        (
            Some("blocked"),
            Some("KarsMemory"),
            Some(reason),
            false,
            ReplayError::Protocol,
        ),
        (
            Some("blocked"),
            Some("InferencePolicy"),
            None,
            false,
            ReplayError::Protocol,
        ),
        (
            Some("blocked"),
            Some("InferencePolicy"),
            Some(" "),
            false,
            ReplayError::Protocol,
        ),
        (
            Some("blocked"),
            Some("InferencePolicy"),
            Some(reason),
            true,
            ReplayError::Authentication,
        ),
    ] {
        let server = MockServer::start().await;
        let mut response = ResponseTemplate::new(400)
            .set_body_json(serde_json::json!({"error": {"code": "content_filter"}}));
        for (header, value) in [
            (DECISION_HEADER, decision),
            (DECISION_BY_HEADER, kind),
            (DECISION_REASON_HEADER, reason),
        ] {
            if let Some(value) = value {
                response = response.insert_header(header, value);
            }
        }
        if challenge {
            response = response.insert_header("www-authenticate", "Bearer");
        }
        Mock::given(wiremock::matchers::any())
            .respond_with(response)
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        assert_eq!(
            response_to_decision(response, PolicyKindRef::InferencePolicy)
                .await
                .unwrap_err(),
            expected
        );
    }
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
            mcp_response_to_decision(response).await.unwrap_err(),
            ReplayError::Protocol
        );
    }
}

#[tokio::test]
async fn mcp_access_refusals_do_not_claim_memory_or_tool_enforcement() {
    for (decision, kind) in [
        (None, None),
        (Some("Blocked"), None),
        (None, Some("KarsMemory")),
        (Some("Allowed"), Some("KarsMemory")),
    ] {
        let server = MockServer::start().await;
        let mut template = ResponseTemplate::new(403)
            .set_body_string("Same-router socket peer required for local MCP");
        if let Some(value) = decision {
            template = template.insert_header(DECISION_HEADER, value);
        }
        if let Some(value) = kind {
            template = template.insert_header(DECISION_BY_HEADER, value);
        }
        Mock::given(wiremock::matchers::any())
            .respond_with(template)
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        assert_eq!(
            mcp_response_to_decision(response).await.unwrap_err(),
            ReplayError::Authentication
        );
    }
}

#[tokio::test]
async fn mcp_policy_attribution_requires_explicit_evidence() {
    for (status, decision) in [(403, "Blocked"), (429, "RateLimited")] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header(DECISION_HEADER, decision)
                    .insert_header(DECISION_BY_HEADER, "ToolPolicy")
                    .insert_header(DECISION_REASON_HEADER, "test policy reason"),
            )
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        let parts = mcp_response_to_decision(response).await.unwrap();
        assert_eq!(parts.decision, parse_decision_header(decision).unwrap());
        assert_eq!(parts.by_policy_kind, Some(PolicyKindRef::ToolPolicy));
        assert_eq!(parts.reason.as_deref(), Some("test policy reason"));
    }

    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let response = reqwest::get(server.uri()).await.unwrap();
    let parts = mcp_response_to_decision(response).await.unwrap();
    assert_eq!(parts.decision, Decision::RateLimited);
    assert_eq!(parts.by_policy_kind, None);
}

#[tokio::test]
async fn memory_rest_non_policy_failures_never_become_memory_denials() {
    for (status, decision, kind, challenge, body, error) in [
        (
            403,
            None,
            None,
            false,
            "memory scope is not authorized",
            ReplayError::Authentication,
        ),
        (
            403,
            Some("Blocked"),
            None,
            false,
            "scope denied",
            ReplayError::Authentication,
        ),
        (
            403,
            None,
            Some("KarsMemory"),
            false,
            "scope denied",
            ReplayError::Authentication,
        ),
        (
            403,
            Some("Allowed"),
            Some("KarsMemory"),
            false,
            "scope denied",
            ReplayError::Authentication,
        ),
        (
            403,
            Some("Blocked"),
            Some("KarsMemory"),
            true,
            "scope denied",
            ReplayError::Authentication,
        ),
        (
            401,
            Some("Blocked"),
            Some("KarsMemory"),
            false,
            "scope denied",
            ReplayError::Authentication,
        ),
        (
            407,
            Some("Blocked"),
            Some("KarsMemory"),
            false,
            "scope denied",
            ReplayError::Authentication,
        ),
        (
            503,
            Some("Blocked"),
            Some("KarsMemory"),
            false,
            "scope denied",
            ReplayError::Upstream,
        ),
        (
            404,
            None,
            None,
            false,
            "scope denied",
            ReplayError::Protocol,
        ),
        (
            403,
            Some("Unknown"),
            Some("KarsMemory"),
            false,
            "scope denied",
            ReplayError::Protocol,
        ),
        (
            403,
            Some("Blocked"),
            Some("Unknown"),
            false,
            "scope denied",
            ReplayError::Protocol,
        ),
        (
            200,
            Some("Blocked"),
            Some("KarsMemory"),
            false,
            "{}",
            ReplayError::Protocol,
        ),
        (
            429,
            Some("Blocked"),
            Some("KarsMemory"),
            false,
            "scope denied",
            ReplayError::Protocol,
        ),
        (200, None, None, true, "{}", ReplayError::Authentication),
        (200, None, None, false, "not json", ReplayError::Protocol),
        (200, None, None, false, "[]", ReplayError::Protocol),
        (
            200,
            None,
            None,
            false,
            r#"{"error":"scope denied"}"#,
            ReplayError::Protocol,
        ),
    ] {
        let server = MockServer::start().await;
        let mut template = ResponseTemplate::new(status).set_body_string(body);
        if let Some(value) = decision {
            template = template.insert_header(DECISION_HEADER, value);
        }
        if let Some(value) = kind {
            template = template.insert_header(DECISION_BY_HEADER, value);
        }
        if challenge {
            template = template.insert_header("www-authenticate", "Bearer");
        }
        Mock::given(wiremock::matchers::any())
            .respond_with(template)
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        assert_eq!(
            memory_response_to_decision(response).await.unwrap_err(),
            error,
            "status={status}, decision={decision:?}, kind={kind:?}, challenge={challenge}"
        );
    }
}

#[tokio::test]
async fn memory_rest_policy_attribution_is_explicit_not_inferred() {
    for (status, decision, kind, expected_kind) in [
        (403, "Blocked", "KarsMemory", PolicyKindRef::KarsMemory),
        (403, "Blocked", "ToolPolicy", PolicyKindRef::ToolPolicy),
        (429, "RateLimited", "KarsMemory", PolicyKindRef::KarsMemory),
    ] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header(DECISION_HEADER, decision)
                    .insert_header(DECISION_BY_HEADER, kind)
                    .insert_header(DECISION_REASON_HEADER, "trusted policy reason")
                    .set_body_json(serde_json::json!({"reason": "untrusted body reason"})),
            )
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        let parts = memory_response_to_decision(response).await.unwrap();
        assert_eq!(parts.decision, parse_decision_header(decision).unwrap());
        assert_eq!(parts.by_policy_kind, Some(expected_kind));
        assert_eq!(parts.reason.as_deref(), Some("trusted policy reason"));
    }
    for (status, decision) in [
        (200, Decision::Allowed),
        (402, Decision::BudgetExceeded),
        (429, Decision::RateLimited),
    ] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(status).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let response = reqwest::get(server.uri()).await.unwrap();
        let parts = memory_response_to_decision(response).await.unwrap();
        assert_eq!(parts.decision, decision);
        assert_eq!(parts.by_policy_kind, None);
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
