// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget::response::denial;
use axum::http::StatusCode;

async fn forward(
    fixture: &Fixture,
    streaming: bool,
) -> anyhow::Result<(StatusCode, HeaderMap, Vec<u8>)> {
    if streaming {
        let (status, headers, stream) = crate::proxy::forward_stream(
            Arc::new(WorkloadIdentityAuth::new()),
            None,
            reqwest::Client::new(),
            fixture.upstream(),
            "responses",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"stream":true,"input":"test"}"#),
        )
        .await?;
        let chunks: Vec<_> = stream.try_collect().await?;
        Ok((status, headers, chunks.concat()))
    } else {
        let (status, headers, body) = fixture.buffered().await?;
        Ok((status, headers, body.to_vec()))
    }
}

async fn fixture(streaming: bool, limit: u64) -> Fixture {
    Fixture::for_operation(
        limit,
        false,
        if streaming {
            Operation::Responses
        } else {
            Operation::ChatCompletions
        },
    )
    .await
}

#[tokio::test]
async fn reserve_contention_replays_identical_request_and_committed_reservation_once() {
    for streaming in [false, true] {
        for recording in [false, true] {
            for committed in [false, true] {
                let fixture = if recording {
                    Fixture::record_only(
                        false,
                        if streaming {
                            Operation::Responses
                        } else {
                            Operation::ChatCompletions
                        },
                    )
                    .await
                } else {
                    fixture(streaming, 100).await
                };
                let response = if streaming {
                    ResponseTemplate::new(200).set_body_raw(
                        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"total_tokens\":8}}}\n\n",
                        "text/event-stream",
                    )
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "usage":{"prompt_tokens":3,"completion_tokens":5,"total_tokens":8}
                    }))
                };
                Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(response)
                    .mount(&fixture.provider)
                    .await;
                let ledger = fixture.ledger.clone();
                Mock::given(wiremock::matchers::path("/v1/reserve"))
                    .respond_with(move |request: &Request| {
                        if committed {
                            // Model a committed store write whose acknowledgement was lost,
                            // not a router transport error (which must never be retried).
                            let request: BrokerRequest<ReserveRequest> =
                                serde_json::from_slice(&request.body).unwrap();
                            let mut ledger = ledger.lock().unwrap();
                            *ledger = ledger
                                .reserve(&request.payload, chrono::Utc::now().timestamp())
                                .unwrap()
                                .next;
                        }
                        ResponseTemplate::new(503).set_body_json(json!({
                            "error":{"code":"inference_budget_contention"}
                        }))
                    })
                    .up_to_n_times(1)
                    .with_priority(1)
                    .mount(&fixture._broker)
                    .await;
                let started = tokio::time::Instant::now();
                assert!(forward(&fixture, streaming).await.unwrap().0.is_success());
                assert!(started.elapsed() >= std::time::Duration::from_secs(1));
                let requests = fixture._broker.received_requests().await.unwrap();
                let reserves: Vec<_> = requests
                    .iter()
                    .filter(|r| r.url.path() == "/v1/reserve")
                    .collect();
                assert_eq!(reserves.len(), 2);
                assert_eq!(reserves[0].body, reserves[1].body);
                for path in ["/v1/session", "/v1/begin", "/v1/settle"] {
                    assert_eq!(requests.iter().filter(|r| r.url.path() == path).count(), 1);
                }
                let reserve: BrokerRequest<ReserveRequest> =
                    serde_json::from_slice(&reserves[0].body).unwrap();
                assert_eq!(reserve.root, fixture.client.binding.task.root);
                assert_eq!(
                    reserve.payload.account_uid,
                    fixture.client.binding.task.account.uid
                );
                assert_eq!(reserve.payload.identity, fixture.client.identity);
                assert_eq!(reserve.payload.quote.is_record_only(), recording);
                let sent = fixture.provider.received_requests().await.unwrap();
                assert_eq!(sent.len(), 1);
                assert_eq!(
                    reserve.payload.wire_digest,
                    format!(
                        "sha256:{}",
                        crate::providers::signing::sha256_hex(&sent[0].body)
                    )
                );
                let ledger = fixture.ledger.lock().unwrap();
                assert_eq!(ledger.meters.settled.tokens, 8);
                assert_eq!(
                    ledger.meters.settled.usd_micros,
                    if recording { 0 } else { 5 }
                );
                assert_eq!(ledger.meters.reserved.tokens, 0);
                assert_eq!(ledger.meters.uncertain.tokens, 0);
                assert_eq!(ledger.meters.unknown_usage_attempts, 0);
                assert_eq!(ledger.meters.unpriced_attempts, u64::from(recording));
                let session = &ledger.sessions[&fixture.client.identity.pod_uid];
                assert_eq!(session.next_sequence, reserve.payload.sequence + 1);
                assert_eq!(session.closed_through, reserve.payload.sequence);
                assert!(ledger.attempts.is_empty());
            }
        }
    }
}

#[tokio::test]
async fn reserve_contention_requires_exact_status_path_and_bounded_error_code() {
    let contention = json!({"error":{"code":"inference_budget_contention"}}).to_string();
    let mut cases: Vec<_> = [400, 401, 403, 409, 429, 500, 502, 504]
        .into_iter()
        .map(|status| ("/v1/reserve", status, contention.clone()))
        .collect();
    for path in [
        "/v1/catalog",
        "/v1/session",
        "/v1/begin",
        "/v1/settle",
        "/v1/calendar/read",
        "/v1/calendar/commit",
        "/v1/reserve/",
        "/other",
    ] {
        cases.push((path, 503, contention.clone()));
    }
    for body in [
        String::new(),
        "<html>inference_budget_contention</html>".into(),
        "{\"error\":{\"code\":\"inference_budget_contention\"}".into(),
        json!({"code":"inference_budget_contention"}).to_string(),
        json!({"error":{"code":503}}).to_string(),
        json!({"error":{"code":"inference_budget_unavailable"}}).to_string(),
        json!({"error":{"code":"inference_budget_contention_extra"}}).to_string(),
        json!({"error":{"code":"inference_budget_contention","message":"x".repeat(8192)}})
            .to_string(),
    ] {
        cases.push(("/v1/reserve", 503, body));
    }
    for (path, status, body) in cases {
        let fixture = fixture(false, 100).await;
        Mock::given(wiremock::matchers::path(path))
            .respond_with(ResponseTemplate::new(status).set_body_raw(body, "application/json"))
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        let error = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            fixture
                .client
                .rpc::<_, serde_json::Value>(path, fixture.client.session_request()),
        )
        .await
        .expect("unclassified error entered the retry loop")
        .unwrap_err();
        assert_eq!(error.stage, path);
        assert_eq!(error.status, Some(status));
        assert_eq!(error.refusal, None);
        let error: anyhow::Error = error.into();
        assert!(!crate::proxy::failure::retryable_failure(&error));
        assert_eq!(fixture._broker.received_requests().await.unwrap().len(), 1);
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .ledger
                .lock()
                .unwrap()
                .meters
                .total()
                .unwrap()
                .tokens,
            0
        );
    }
}

#[tokio::test]
async fn reserve_contention_stops_on_live_refusal_after_waiting() {
    for (status, code, refusal) in [
        (403, "inference_budget_authority", None),
        (403, "inference_budget_closed", None),
        (409, "inference_budget_attempt_state", None),
        (503, "inference_budget_contract", None),
        (503, "inference_budget_unavailable", None),
        (503, "inference_budget_capacity", Some(Refusal::Capacity)),
        (429, "inference_budget_exhausted", Some(Refusal::Exhausted)),
    ] {
        let fixture = fixture(false, 100).await;
        Mock::given(wiremock::matchers::path("/v1/reserve"))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "error":{"code":"inference_budget_contention"}
            })))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        Mock::given(wiremock::matchers::path("/v1/reserve"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "error":{"code":code}
            })))
            .with_priority(2)
            .mount(&fixture._broker)
            .await;
        let error =
            tokio::time::timeout(std::time::Duration::from_secs(5), forward(&fixture, false))
                .await
                .unwrap()
                .unwrap_err();
        let budget_error = error.downcast_ref::<Error>().unwrap();
        assert_eq!(budget_error.status, Some(status));
        assert_eq!(budget_error.refusal, refusal);
        assert!(!crate::proxy::failure::retryable_failure(&error));
        let requests = fixture._broker.received_requests().await.unwrap();
        let reserves: Vec<_> = requests
            .iter()
            .filter(|r| r.url.path() == "/v1/reserve")
            .collect();
        assert_eq!(reserves.len(), 2);
        assert_eq!(reserves[0].body, reserves[1].body);
        assert!(!requests.iter().any(|r| r.url.path() == "/v1/begin"));
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(fixture.ledger.lock().unwrap().attempts.is_empty());
    }
}

#[tokio::test]
async fn reserve_contention_does_not_retry_transport_or_success_decode_errors() {
    for transport in [false, true] {
        let mut fixture = fixture(false, 100).await;
        Arc::get_mut(&mut fixture.client).unwrap().http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        Mock::given(wiremock::matchers::path("/v1/reserve"))
            .respond_with(if transport {
                ResponseTemplate::new(503)
                    .set_body_json(json!({
                        "error":{"code":"inference_budget_contention"}
                    }))
                    .set_delay(std::time::Duration::from_secs(1))
            } else {
                ResponseTemplate::new(200).set_body_raw("not JSON", "application/json")
            })
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture
                .client
                .rpc::<_, serde_json::Value>("/v1/reserve", fixture.client.session_request()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.stage,
            if transport {
                "/v1/reserve"
            } else {
                "broker response"
            }
        );
        assert_eq!(error.status, None);
        assert_eq!(error.refusal, None);
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert_eq!(fixture._broker.received_requests().await.unwrap().len(), 1);
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn reserve_contention_never_retries_committed_begin_or_settlement() {
    for recording in [false, true] {
        for stage in ["/v1/begin", "/v1/settle"] {
            let fixture = if recording {
                Fixture::record_only(false, Operation::ChatCompletions).await
            } else {
                fixture(false, 100).await
            };
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "usage":{"prompt_tokens":3,"completion_tokens":5,"total_tokens":8}
                })))
                .mount(&fixture.provider)
                .await;
            let ledger = fixture.ledger.clone();
            Mock::given(wiremock::matchers::path(stage))
                .respond_with(move |request: &Request| {
                    let mut ledger = ledger.lock().unwrap();
                    *ledger = if stage == "/v1/begin" {
                        let request: BrokerRequest<AttemptCommand> =
                            serde_json::from_slice(&request.body).unwrap();
                        ledger
                            .begin_dispatch(&request.payload, chrono::Utc::now().timestamp())
                            .unwrap()
                            .next
                    } else {
                        let request: BrokerRequest<Settlement> =
                            serde_json::from_slice(&request.body).unwrap();
                        ledger.settle(&request.payload).unwrap().next
                    };
                    ResponseTemplate::new(503).set_body_json(json!({
                        "error":{"code":"inference_budget_contention"}
                    }))
                })
                .with_priority(1)
                .mount(&fixture._broker)
                .await;
            let error =
                tokio::time::timeout(std::time::Duration::from_secs(5), forward(&fixture, false))
                    .await
                    .unwrap()
                    .unwrap_err();
            let budget_error = error.downcast_ref::<Error>().unwrap();
            assert_eq!(budget_error.stage, stage);
            assert_eq!(budget_error.status, Some(503));
            assert_eq!(budget_error.refusal, None);
            assert!(!crate::proxy::failure::retryable_failure(&error));
            let requests = fixture._broker.received_requests().await.unwrap();
            assert_eq!(requests.iter().filter(|r| r.url.path() == stage).count(), 1);
            assert_eq!(
                fixture.provider.received_requests().await.unwrap().len(),
                usize::from(stage == "/v1/settle")
            );
            let ledger = fixture.ledger.lock().unwrap();
            assert_eq!(
                ledger.meters.settled.tokens,
                if stage == "/v1/settle" { 8 } else { 0 }
            );
            assert_eq!(
                ledger.meters.reserved.tokens,
                if stage == "/v1/begin" && !recording {
                    30
                } else {
                    0
                }
            );
            assert_eq!(ledger.attempts.len(), usize::from(stage == "/v1/begin"));
        }
    }
}

#[tokio::test]
async fn reservation_codes_stop_native_retries_without_sending_or_charging() {
    for streaming in [false, true] {
        for (stage, status, code, reason) in [
            (
                "/v1/reserve",
                429,
                "inference_budget_exhausted",
                "maximum input plus output",
            ),
            (
                "/v1/reserve",
                503,
                "inference_budget_capacity",
                "ledger has reached",
            ),
            (
                "/v1/session",
                503,
                "inference_budget_capacity",
                "ledger has reached",
            ),
        ] {
            let fixture = fixture(streaming, 100).await;
            Mock::given(wiremock::matchers::path(stage))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "error":{"code":code,"message":"PRIVATE_BROKER_DETAIL"}
                })))
                .with_priority(1)
                .mount(&fixture._broker)
                .await;
            let error = forward(&fixture, streaming).await.unwrap_err();
            assert!(!crate::proxy::failure::retryable_failure(&error));
            let response = denial(&error).unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(response.headers()["x-should-retry"], "false");
            assert!(!response.headers().contains_key("retry-after"));
            let body = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"]["type"], "invalid_request_error");
            assert_eq!(body["error"]["code"], code);
            let message = body["error"]["message"].as_str().unwrap();
            assert!(message.contains(reason));
            assert!(!message.contains("PRIVATE_BROKER_DETAIL"));
            assert!(!message.contains("unavailable"));
            assert!(!message.contains("429"));
            assert!(!message.contains("503"));
            assert!(!message.contains("rate limit"));
            assert!(
                fixture
                    .provider
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
            let requests = fixture._broker.received_requests().await.unwrap();
            assert_eq!(requests.iter().filter(|r| r.url.path() == stage).count(), 1);
            assert!(!requests.iter().any(|r| r.url.path() == "/v1/begin"));
            let ledger = fixture.ledger.lock().unwrap();
            assert_eq!(ledger.meters.total().unwrap().tokens, 0);
            assert!(ledger.attempts.is_empty());
        }
    }
}

#[tokio::test]
async fn unclassified_broker_faults_remain_retryable_without_provider_fallback() {
    for (status, body) in [
        (
            503,
            json!({"error":{"code":"inference_budget_unavailable"}}).to_string(),
        ),
        (
            503,
            json!({"error":{"code":"inference_budget_contract"}}).to_string(),
        ),
        (
            429,
            json!({"error":{"code":"rate_limit_exceeded"}}).to_string(),
        ),
        (429, String::new()),
        (
            429,
            json!({"error":{"code":"inference_privacy_pending"}}).to_string(),
        ),
        (
            503,
            json!({"error":{"code":"inference_privacy_pending","message":"x".repeat(8192)}})
                .to_string(),
        ),
        (503, "<html>PRIVATE_BROKER_DETAIL</html>".into()),
        (503, json!({"error":{"code":5}}).to_string()),
        (
            503,
            json!({"error":{"code":"inference_budget_exhausted"}}).to_string(),
        ),
        (
            429,
            json!({"error":{"code":"inference_budget_capacity"}}).to_string(),
        ),
        (
            503,
            json!({"error":{"code":"inference_budget_capacity","message":"x".repeat(8192)}})
                .to_string(),
        ),
    ] {
        let fixture = fixture(false, 100).await;
        Mock::given(wiremock::matchers::path("/v1/reserve"))
            .respond_with(ResponseTemplate::new(status).set_body_raw(body, "application/json"))
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        let error = forward(&fixture, false).await.unwrap_err();
        assert!(!crate::proxy::failure::retryable_failure(&error));
        let response = denial(&error).unwrap();
        assert_eq!(response.status().as_u16(), status);
        assert!(!response.headers().contains_key("x-should-retry"));
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "inference_budget_unavailable");
        assert!(!body.to_string().contains("PRIVATE_BROKER_DETAIL"));
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .ledger
                .lock()
                .unwrap()
                .meters
                .total()
                .unwrap()
                .tokens,
            0
        );
    }
}

#[tokio::test]
async fn positive_remaining_budget_still_refuses_an_unfundable_maximum() {
    for (streaming, root_limit) in [(false, 50), (true, 50), (false, 100), (true, 100)] {
        let fixture = fixture(streaming, root_limit).await;
        fixture
            .ledger
            .lock()
            .unwrap()
            .nodes
            .get_mut("task-uid")
            .unwrap()
            .authority
            .limits
            .tokens = Some(50);
        let usage = if streaming {
            json!({"input_tokens":10,"output_tokens":20,"total_tokens":30})
        } else {
            json!({"prompt_tokens":10,"completion_tokens":20,"total_tokens":30})
        };
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"usage":usage})))
            .mount(&fixture.provider)
            .await;
        assert_eq!(
            forward(&fixture, streaming).await.unwrap().0,
            StatusCode::OK
        );
        let before = fixture.ledger.lock().unwrap().clone();
        assert_eq!(before.meters.settled.tokens, 30);
        let node = &before.nodes["task-uid"];
        assert_eq!(
            node.authority.limits.tokens.unwrap() - node.meters.total().unwrap().tokens,
            20
        );
        assert_eq!(
            before.limits.tokens.unwrap() - before.meters.total().unwrap().tokens,
            root_limit - 30
        );
        let error = forward(&fixture, streaming).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<Error>().unwrap().refusal,
            Some(Refusal::Exhausted)
        );
        assert_eq!(denial(&error).unwrap().status(), StatusCode::BAD_REQUEST);
        assert_eq!(*fixture.ledger.lock().unwrap(), before);
        assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn actual_provider_429_keeps_its_body_headers_and_retry_classification() {
    for streaming in [false, true] {
        for code in ["rate_limit_exceeded", "inference_budget_exhausted"] {
            let fixture = fixture(streaming, 100).await;
            let body = json!({"error":{"type":"rate_limit_error","code":code,"message":"provider detail"}}).to_string();
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(
                    ResponseTemplate::new(429)
                        .set_body_raw(body.clone(), "application/json")
                        .insert_header("retry-after", "7")
                        .insert_header("x-should-retry", "true"),
                )
                .mount(&fixture.provider)
                .await;
            let (status, headers, received) = forward(&fixture, streaming).await.unwrap();
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert!(crate::proxy::failure::retryable_rejection(status));
            assert_eq!(headers["retry-after"], "7");
            assert_eq!(headers["x-should-retry"], "true");
            assert_eq!(received, body.as_bytes());
            assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
            assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
        }
    }
}

#[tokio::test]
async fn lost_begin_ack_is_still_unavailable_and_retains_the_reservation() {
    let fixture = Fixture::new(100, true).await;
    let error = forward(&fixture, false).await.unwrap_err();
    let response = denial(&error).unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!response.headers().contains_key("x-should-retry"));
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.ledger.lock().unwrap().meters.reserved.tokens, 30);
}

#[path = "denial_privacy_tests.rs"]
mod denial_privacy_tests;
