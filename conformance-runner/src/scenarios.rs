// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Per-scenario replay logic.
//!
//! One entry point: [`replay`] takes a [`Scenario`] + [`Transport`] and
//! returns a fully-built [`ActualDecision`] ready for [`azureclaw_eval_corpus::judge`].
//!
//! Endpoint conventions (matches `inference-router` routes):
//!
//! | Scenario kind     | HTTP                                       | Default `PolicyKindRef` |
//! |-------------------|--------------------------------------------|-------------------------|
//! | `EgressConnect`   | `POST /internal/egress/connect`            | `EgressAllowlist`       |
//! | `ChatCompletion`  | `POST /v1/chat/completions`                | `InferencePolicy`       |
//! | `ToolCall`        | `POST /v1/tools/{tool}`                    | `ToolPolicy`            |
//! | `MemoryRead`      | `POST /v1/memory/read`                     | `ClawMemory`            |
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
use azureclaw_eval_corpus::{
    ActualDecision, Burst, ChatMessage, Decision, ObservedSample, PolicyKindRef, Scenario,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use crate::transport::{CASE_ID_HEADER, Transport, response_to_decision};

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
            single_call(
                transport,
                "/internal/egress/connect",
                json!({ "host": host, "port": port }),
                PolicyKindRef::EgressAllowlist,
                case_id,
                auth_header,
            )
            .await
        }
        Scenario::ChatCompletion { messages, model } => {
            let body = json!({
                "messages": messages_to_json(messages),
                "model": model,
            });
            single_call(
                transport,
                "/v1/chat/completions",
                body,
                PolicyKindRef::InferencePolicy,
                case_id,
                auth_header,
            )
            .await
        }
        Scenario::ToolCall { tool, args, burst } => {
            let path = format!("/v1/tools/{}", urlencode_segment(tool));
            let body = args.clone().unwrap_or(json!({}));
            match burst {
                Some(b) => {
                    burst_call(
                        transport,
                        &path,
                        body,
                        PolicyKindRef::ToolPolicy,
                        b,
                        case_id,
                        auth_header,
                    )
                    .await
                }
                None => {
                    single_call(
                        transport,
                        &path,
                        body,
                        PolicyKindRef::ToolPolicy,
                        case_id,
                        auth_header,
                    )
                    .await
                }
            }
        }
        Scenario::MemoryRead { scope, key } => {
            single_call(
                transport,
                "/v1/memory/read",
                json!({ "scope": scope, "key": key }),
                PolicyKindRef::ClawMemory,
                case_id,
                auth_header,
            )
            .await
        }
    }
}

fn messages_to_json(msgs: &[ChatMessage]) -> Vec<Value> {
    msgs.iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect()
}

/// Minimal RFC 3986 path-segment encoder. Tools are author-controlled in
/// the corpora today, but we still escape anything that isn't an unreserved
/// character to keep URLs well-formed.
fn urlencode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let is_unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if is_unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

async fn single_call(
    transport: &Transport,
    path: &str,
    body: Value,
    default_kind: PolicyKindRef,
    case_id: &str,
    auth_header: Option<&str>,
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
    let parts = response_to_decision(resp, default_kind).await;
    Ok(ActualDecision {
        decision: parts.decision,
        by_policy_kind: parts.by_policy_kind,
        reason: parts.reason,
        observations: Vec::new(),
    })
}

async fn burst_call(
    transport: &Transport,
    path: &str,
    body: Value,
    default_kind: PolicyKindRef,
    burst: &Burst,
    case_id: &str,
    auth_header: Option<&str>,
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
                let parts = response_to_decision(resp, default_kind).await;
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
    use azureclaw_eval_corpus::{Burst, Decision, PolicyKindRef, Scenario};
    use std::time::Duration;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn transport(server: &MockServer) -> Transport {
        Transport::new(server.uri(), Duration::from_secs(5)).unwrap()
    }

    #[tokio::test]
    async fn egress_connect_blocked_carries_egress_kind() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/egress/connect"))
            .respond_with(
                ResponseTemplate::new(403).set_body_string(r#"{"reason":"host not in allowlist"}"#),
            )
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::EgressConnect {
            host: "evil.example.com".to_string(),
            port: 443,
        };
        let actual = replay(&t, &scen, "case-001", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Blocked);
        assert_eq!(actual.by_policy_kind, Some(PolicyKindRef::EgressAllowlist));
        assert_eq!(actual.reason.as_deref(), Some("host not in allowlist"));
        assert!(actual.observations.is_empty());
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
    async fn tool_call_with_burst_records_all_observations() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/echo"))
            .respond_with(ResponseTemplate::new(429))
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
        let actual = replay(&t, &scen, "case-003", None).await.unwrap();
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
    async fn memory_read_uses_clawmemory_kind() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/memory/read"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"reason":"cross-sandbox read denied"}"#),
            )
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::MemoryRead {
            scope: "other-sandbox".into(),
            key: "secret".into(),
        };
        let actual = replay(&t, &scen, "case-004", None).await.unwrap();
        assert_eq!(actual.decision, Decision::Blocked);
        assert_eq!(actual.by_policy_kind, Some(PolicyKindRef::ClawMemory));
        assert_eq!(actual.reason.as_deref(), Some("cross-sandbox read denied"));
    }

    #[tokio::test]
    async fn case_id_header_is_sent() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header(CASE_ID_HEADER, "case-id-007"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ChatCompletion {
            messages: vec![],
            model: None,
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
    async fn burst_aggregate_decision_is_first_observation() {
        // First call rate-limited, subsequent ones blocked → aggregate
        // should still be RateLimited (first wins). wiremock doesn't
        // easily support sequential responses; we approximate with two
        // mocks differentiated by path query (we test the simpler all-
        // same-status case here; ordering semantics are unit-tested in
        // the corpus crate via judge).
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tools/echo"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&s)
            .await;
        let t = transport(&s);
        let scen = Scenario::ToolCall {
            tool: "echo".into(),
            args: None,
            burst: Some(Burst {
                count: 2,
                window_ms: 5000,
            }),
        };
        let actual = replay(&t, &scen, "burst-agg", None).await.unwrap();
        assert_eq!(actual.decision, actual.observations[0].decision);
    }

    #[test]
    fn urlencode_segment_escapes_special_chars() {
        assert_eq!(urlencode_segment("tool name"), "tool%20name");
        assert_eq!(urlencode_segment("tool/sub"), "tool%2Fsub");
        assert_eq!(urlencode_segment("simple"), "simple");
        assert_eq!(urlencode_segment("a-b_c.d~e"), "a-b_c.d~e");
    }
}
