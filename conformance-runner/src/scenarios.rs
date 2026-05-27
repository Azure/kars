// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Per-scenario replay logic.
//!
//! One entry point: [`replay`] takes a [`Scenario`] + [`Transport`] and
//! returns a fully-built [`ActualDecision`] ready for [`kars_eval_corpus::judge`].
//!
//! Endpoint conventions (matches `inference-router` routes — verified
//! against `inference-router/src/routes/{inference,mcp}.rs` and
//! `inference-router/src/forward_proxy.rs` for slice 6.5):
//!
//! | Scenario kind     | Wire                                                              | Default `PolicyKindRef` |
//! |-------------------|-------------------------------------------------------------------|-------------------------|
//! | `EgressConnect`   | HTTP `CONNECT host:port` through the forward proxy on `:8444`     | `EgressAllowlist`       |
//! | `ChatCompletion`  | `POST /v1/chat/completions`                                       | `InferencePolicy`       |
//! | `ToolCall`        | `POST /mcp` JSON-RPC `tools/call` (name = scenario.tool, args)    | `ToolPolicy`            |
//! | `MemoryRead`      | `POST /platform/mcp` JSON-RPC `tools/call` `foundry.memory:search`| `KarsMemory`            |
//!
//! `ToolCall` with a `burst` repeats the call up to `count` times within
//! `window_ms`, recording each attempt's status/decision as an
//! [`ObservedSample`]. The aggregate `decision` field of the resulting
//! [`ActualDecision`] is the **first** observation; the judge handles
//! `decisionAtLeastSome` against the observations vector.
//!
//! The runner does NOT send the case ID in the body (it tags responses
//! via [`crate::transport::CASE_ID_HEADER`] so the router's audit log
//! can correlate without polluting bodies).

use anyhow::{Context, Result};
use kars_eval_corpus::{
    ActualDecision, Burst, ChatMessage, Decision, ObservedSample, PolicyKindRef, Scenario,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use crate::transport::{
    CASE_ID_HEADER, Transport, egress_connect_via_proxy, mcp_response_to_decision,
    response_to_decision,
};

/// Replay one [`Scenario`] against the router. `case_id` is echoed via
/// header on every request so the router can correlate.
pub async fn replay(
    transport: &Transport,
    scenario: &Scenario,
    case_id: &str,
    auth_header: Option<&str>,
) -> Result<ActualDecision> {
    match scenario {
        Scenario::EgressConnect { host, port } => {
            let proxy_addr = transport.forward_proxy_addr().ok_or_else(|| {
                anyhow::anyhow!(
                    "EgressConnect scenario {case_id} requires --forward-proxy <host:port>; \
                     the inference router exposes no HTTP egress-decision endpoint, only \
                     the forward proxy on :8444 (run with --forward-proxy router-svc:8444)"
                )
            })?;
            let parts =
                egress_connect_via_proxy(proxy_addr, host, *port, case_id, transport.timeout())
                    .await;
            Ok(ActualDecision {
                decision: parts.decision,
                by_policy_kind: parts.by_policy_kind,
                reason: parts.reason,
                observations: Vec::new(),
            })
        }
        Scenario::ChatCompletion { messages, model } => {
            // Omit `model` when the corpus didn't specify one — sending
            // `"model": null` causes Azure OpenAI to route to a default
            // deployment that may strip prompt_filter_results
            // annotations, which then fail Prompt-Shields-required
            // policies and produce a false-positive Block on benign
            // controls. The corpus author can pin a model when needed.
            let body = match model {
                Some(m) => json!({
                    "messages": messages_to_json(messages),
                    "model": m,
                }),
                None => json!({
                    "messages": messages_to_json(messages),
                }),
            };
            single_call(
                transport,
                "/v1/chat/completions",
                body,
                PolicyKindRef::InferencePolicy,
                case_id,
                auth_header,
                CallStyle::HttpStatus,
            )
            .await
        }
        Scenario::ToolCall { tool, args, burst } => {
            let body = mcp_envelope(
                "tools/call",
                json!({
                    "name": tool,
                    "arguments": args.clone().unwrap_or(json!({})),
                }),
                1,
            );
            match burst {
                Some(b) => {
                    burst_call(
                        transport,
                        "/mcp",
                        body,
                        PolicyKindRef::ToolPolicy,
                        b,
                        case_id,
                        auth_header,
                        CallStyle::McpJsonRpc,
                    )
                    .await
                }
                None => {
                    single_call(
                        transport,
                        "/mcp",
                        body,
                        PolicyKindRef::ToolPolicy,
                        case_id,
                        auth_header,
                        CallStyle::McpJsonRpc,
                    )
                    .await
                }
            }
        }
        Scenario::MemoryRead { scope, key } => {
            // Foundry's memory tool exposes a single `foundry.memory`
            // tool with `operation` discriminator. `scope` is encoded
            // in the search text so the audit log captures which
            // sandbox-scope the eval asked for; the upstream Foundry
            // search treats it as plain text.
            let text = if scope.is_empty() {
                key.clone()
            } else {
                format!("[scope={scope}] {key}")
            };
            let body = mcp_envelope(
                "tools/call",
                json!({
                    "name": "foundry.memory",
                    "arguments": {
                        "operation": "search",
                        "text": text,
                    },
                }),
                1,
            );
            single_call(
                transport,
                "/platform/mcp",
                body,
                PolicyKindRef::KarsMemory,
                case_id,
                auth_header,
                CallStyle::McpJsonRpc,
            )
            .await
        }
    }
}

/// Style of response decoding for an HTTP call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallStyle {
    /// Decode the decision purely from HTTP status + decision headers
    /// (the legacy inference-API surface).
    HttpStatus,
    /// Decode from the JSON-RPC envelope in the response body
    /// (MCP `tools/call`).
    McpJsonRpc,
}

fn messages_to_json(msgs: &[ChatMessage]) -> Vec<Value> {
    msgs.iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect()
}

fn mcp_envelope(method: &str, params: Value, id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

async fn single_call(
    transport: &Transport,
    path: &str,
    body: Value,
    default_kind: PolicyKindRef,
    case_id: &str,
    auth_header: Option<&str>,
    style: CallStyle,
) -> Result<ActualDecision> {
    let mut req = transport
        .client()
        .post(transport.url(path))
        .header(CASE_ID_HEADER, case_id)
        .json(&body);
    if let Some(token) = auth_header {
        req = req.header("authorization", token);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("POST {path}: transport error"))?;
    let parts = match style {
        CallStyle::HttpStatus => response_to_decision(resp, default_kind).await,
        CallStyle::McpJsonRpc => mcp_response_to_decision(resp, default_kind).await,
    };
    Ok(ActualDecision {
        decision: parts.decision,
        by_policy_kind: parts.by_policy_kind,
        reason: parts.reason,
        observations: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn burst_call(
    transport: &Transport,
    path: &str,
    body: Value,
    default_kind: PolicyKindRef,
    burst: &Burst,
    case_id: &str,
    auth_header: Option<&str>,
    style: CallStyle,
) -> Result<ActualDecision> {
    if burst.count == 0 {
        anyhow::bail!("burst.count must be >= 1 (parser rejects 0)");
    }

    let start = Instant::now();
    let window = Duration::from_millis(u64::from(burst.window_ms));

    let mut observations: Vec<ObservedSample> = Vec::with_capacity(burst.count as usize);

    for i in 0..burst.count {
        if window > Duration::ZERO && start.elapsed() >= window {
            break;
        }

        let mut req = transport
            .client()
            .post(transport.url(path))
            .header(CASE_ID_HEADER, format!("{case_id}#{i}"))
            .json(&body);
        if let Some(token) = auth_header {
            req = req.header("authorization", token);
        }
        let send = req.send().await;

        let observation = match send {
            Ok(resp) => {
                let parts = match style {
                    CallStyle::HttpStatus => response_to_decision(resp, default_kind).await,
                    CallStyle::McpJsonRpc => mcp_response_to_decision(resp, default_kind).await,
                };
                ObservedSample {
                    seq: i,
                    decision: parts.decision,
                    reason: parts.reason,
                }
            }
            Err(e) => ObservedSample {
                seq: i,
                decision: Decision::Blocked,
                reason: Some(format!("transport error: {e}")),
            },
        };
        observations.push(observation);
    }

    if observations.is_empty() {
        anyhow::bail!("burst produced 0 observations (window={window:?})");
    }

    let first = observations[0].clone();
    Ok(ActualDecision {
        decision: first.decision,
        by_policy_kind: Some(default_kind),
        reason: None,
        observations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kars_eval_corpus::{Burst, Decision, PolicyKindRef, Scenario};
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn transport(server: &MockServer) -> Transport {
        Transport::new(server.uri(), Duration::from_secs(5)).unwrap()
    }

    /// Stand up a tiny TCP listener that pretends to be the inference
    /// router's forward proxy on :8444. The first response line is
    /// configurable; the listener accepts exactly one connection then
    /// returns the address it was bound to.
    async fn spawn_fake_forward_proxy(response: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Read until \r\n\r\n (end of request headers).
                let mut buf = [0u8; 1024];
                let mut total = 0;
                use tokio::io::AsyncReadExt;
                while let Ok(n) = sock.read(&mut buf[total..]).await {
                    if n == 0 {
                        break;
                    }
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if total == buf.len() {
                        break;
                    }
                }
                use tokio::io::AsyncWriteExt;
                let _ = sock.write_all(response).await;
                let _ = sock.shutdown().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn egress_connect_200_maps_to_allowed() {
        let addr = spawn_fake_forward_proxy(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
        let t = Transport::new("http://router.invalid:8443", Duration::from_secs(2))
            .unwrap()
            .with_forward_proxy(addr);
        let scen = Scenario::EgressConnect {
            host: "api.openai.com".into(),
            port: 443,
        };
        let actual = replay(&t, &scen, "case-egress-ok", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Allowed);
        assert_eq!(actual.by_policy_kind, None);
    }

    #[tokio::test]
    async fn egress_connect_403_maps_to_blocked_egress_kind() {
        let addr =
            spawn_fake_forward_proxy(b"HTTP/1.1 403 Blocked by kars egress policy\r\n\r\n").await;
        let t = Transport::new("http://router.invalid:8443", Duration::from_secs(2))
            .unwrap()
            .with_forward_proxy(addr);
        let scen = Scenario::EgressConnect {
            host: "evil.example.com".into(),
            port: 443,
        };
        let actual = replay(&t, &scen, "case-egress-block", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Blocked);
        assert_eq!(actual.by_policy_kind, Some(PolicyKindRef::EgressAllowlist));
        assert!(
            actual
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("Blocked by kars")
        );
    }

    #[tokio::test]
    async fn egress_connect_502_dns_fail_maps_to_blocked() {
        let addr = spawn_fake_forward_proxy(b"HTTP/1.1 502 DNS validation failed\r\n\r\n").await;
        let t = Transport::new("http://router.invalid:8443", Duration::from_secs(2))
            .unwrap()
            .with_forward_proxy(addr);
        let scen = Scenario::EgressConnect {
            host: "169.254.169.254".into(),
            port: 80,
        };
        let actual = replay(&t, &scen, "case-egress-imds", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Blocked);
        assert_eq!(actual.by_policy_kind, Some(PolicyKindRef::EgressAllowlist));
    }

    #[tokio::test]
    async fn egress_connect_without_proxy_addr_errors_clearly() {
        let t = Transport::new("http://router.invalid:8443", Duration::from_secs(2)).unwrap();
        let scen = Scenario::EgressConnect {
            host: "anywhere".into(),
            port: 443,
        };
        let err = replay(&t, &scen, "case-egress", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--forward-proxy"),
            "error must call out --forward-proxy: {msg}"
        );
    }

    #[tokio::test]
    async fn chat_completion_allowed() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ChatCompletion {
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hello".into(),
            }],
            model: None,
        };
        let actual = replay(&t, &scen, "case-002", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Allowed);
    }

    #[tokio::test]
    async fn tool_call_posts_jsonrpc_to_mcp_endpoint() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(body_partial_json(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {"name": "echo"},
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ToolCall {
            tool: "echo".into(),
            args: None,
            burst: None,
        };
        let actual = replay(&t, &scen, "case-tool-ok", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Allowed);
    }

    #[tokio::test]
    async fn tool_call_jsonrpc_error_maps_to_blocked() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"tool not found in registry: forbidden_tool"}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ToolCall {
            tool: "forbidden_tool".into(),
            args: None,
            burst: None,
        };
        let actual = replay(&t, &scen, "case-tool-blocked", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Blocked);
        assert_eq!(actual.by_policy_kind, Some(PolicyKindRef::ToolPolicy));
        assert!(
            actual
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("forbidden_tool")
        );
    }

    #[tokio::test]
    async fn tool_call_is_error_content_maps_to_blocked() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"policy denied: tool not in allowlist"}],"isError":true}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ToolCall {
            tool: "fs.write".into(),
            args: None,
            burst: None,
        };
        let actual = replay(&t, &scen, "case-tool-iserr", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Blocked);
        assert!(
            actual
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("policy denied")
        );
    }

    #[tokio::test]
    async fn tool_call_burst_records_all_observations_via_mcp() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"rate limit exceeded"}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ToolCall {
            tool: "echo".into(),
            args: None,
            burst: Some(Burst {
                count: 3,
                window_ms: 5000,
            }),
        };
        let actual = replay(&t, &scen, "case-burst", None).await.unwrap();
        assert_eq!(actual.observations.len(), 3);
        assert!(
            actual
                .observations
                .iter()
                .all(|o| o.decision == Decision::RateLimited)
        );
        assert_eq!(actual.decision, Decision::RateLimited);
        assert_eq!(actual.by_policy_kind, Some(PolicyKindRef::ToolPolicy));
    }

    #[tokio::test]
    async fn memory_read_posts_foundry_memory_search_to_platform_mcp() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/platform/mcp"))
            .and(body_partial_json(serde_json::json!({
                "method": "tools/call",
                "params": {
                    "name": "foundry.memory",
                    "arguments": {"operation": "search"},
                },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"[]"}],"isError":false}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::MemoryRead {
            scope: "self".into(),
            key: "user.preferences".into(),
        };
        let actual = replay(&t, &scen, "case-mem-ok", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Allowed);
    }

    #[tokio::test]
    async fn memory_read_blocked_carries_karsmemory_kind() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/platform/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"cross-sandbox read denied"}],"isError":true}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::MemoryRead {
            scope: "other-sandbox".into(),
            key: "secret".into(),
        };
        let actual = replay(&t, &scen, "case-mem-deny", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Blocked);
        assert_eq!(actual.by_policy_kind, Some(PolicyKindRef::KarsMemory));
        assert!(
            actual
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("cross-sandbox read denied")
        );
    }

    #[tokio::test]
    async fn case_id_header_is_sent_on_mcp_call() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header(CASE_ID_HEADER, "case-id-007"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[],"isError":false}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ToolCall {
            tool: "echo".into(),
            args: None,
            burst: None,
        };
        let actual = replay(&t, &scen, "case-id-007", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Allowed);
    }

    #[tokio::test]
    async fn auth_header_is_forwarded() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer xyz"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ChatCompletion {
            messages: vec![],
            model: None,
        };
        let actual = replay(&t, &scen, "auth-1", Some("Bearer xyz"))
            .await
            .unwrap();
        assert_eq!(actual.decision, Decision::Allowed);
    }

    #[tokio::test]
    async fn memory_read_scope_lands_in_search_text() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/platform/mcp"))
            .and(body_partial_json(serde_json::json!({
                "params": {
                    "arguments": {"text": "[scope=other] private-key"},
                },
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":1,"result":{"content":[],"isError":false}}"#,
            ))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::MemoryRead {
            scope: "other".into(),
            key: "private-key".into(),
        };
        let actual = replay(&t, &scen, "case-mem-scope", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Allowed);
    }
}
