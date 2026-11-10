// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget::{
    calendar::tests::{accounting_server, record_only_server},
    dispatch,
};
use axum::http::StatusCode;

#[tokio::test]
async fn provider_errors_survive_acknowledged_paired_accounting() {
    for mode in ["bounded", "record-only", "calendar-only", "lifetime-only"] {
        for streaming in [false, true] {
            for (status, code) in [
                (429, "server_error"),
                (500, "server_error"),
                (502, "server_error"),
                (503, "server_error"),
                (504, "server_error"),
                (400, "unsupported_parameter"),
                (400, "unsupported_value"),
                (400, "invalid_request_error"),
                (400, "invalid_value"),
                (400, "InvalidParameter"),
            ]
            .into_iter()
            .map(|(status, code)| (status, Some(code)))
            .chain([(400, None)])
            {
                let fixture = if mode == "bounded" {
                    Fixture::new(100, false).await
                } else {
                    Fixture::record_only(false, Operation::ChatCompletions).await
                };
                let (_server, calendar) = if mode == "bounded" {
                    accounting_server().await
                } else {
                    record_only_server().await
                };
                let mut upstream = fixture.upstream();
                if mode == "calendar-only" {
                    upstream.inference_budget = None;
                }
                upstream.calendar_budget = (mode != "lifetime-only").then_some(calendar.clone());
                let body = json!({"error":{
                    "code":code,"param":"temperature","message":"PRIVATE_PROVIDER_DETAIL"
                }})
                .to_string();
                Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(
                        ResponseTemplate::new(status)
                            .set_body_raw(body.clone(), "application/json")
                            .insert_header("retry-after", "7")
                            .insert_header("x-should-retry", "true"),
                    )
                    .expect(1)
                    .mount(&fixture.provider)
                    .await;
                let request = bytes::Bytes::from(
                    json!({
                        "stream":streaming,"messages":[{"role":"user","content":"test"}]
                    })
                    .to_string(),
                );
                let (actual_status, headers, actual_body) = if streaming {
                    let (status, headers, stream) = crate::proxy::forward_stream(
                        Arc::new(WorkloadIdentityAuth::new()),
                        None,
                        reqwest::Client::new(),
                        upstream,
                        "chat/completions",
                        HeaderMap::new(),
                        request,
                    )
                    .await
                    .unwrap();
                    let chunks: Vec<_> = stream.try_collect().await.unwrap();
                    (status, headers, chunks.concat())
                } else {
                    let (status, headers, body) = crate::proxy::forward(
                        &WorkloadIdentityAuth::new(),
                        None,
                        &reqwest::Client::new(),
                        &upstream,
                        Method::POST,
                        "chat/completions",
                        &HeaderMap::new(),
                        request,
                    )
                    .await
                    .unwrap();
                    (status, headers, body.to_vec())
                };
                assert_eq!(actual_status.as_u16(), status);
                assert_eq!(
                    crate::proxy::failure::retryable_rejection(actual_status),
                    status != 400
                );
                assert_eq!(headers["retry-after"], "7");
                assert_eq!(headers["x-should-retry"], "true");
                assert_eq!(actual_body, body.as_bytes());
                assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
                let observation = calendar.read().await.unwrap();
                assert_eq!(
                    observation.usage.daily,
                    if mode == "bounded" { 30 } else { 0 }
                );
                if matches!(mode, "record-only" | "calendar-only") {
                    assert_eq!(observation.pending_requests, Some(1));
                }
                let ledger = fixture.ledger.lock().unwrap();
                assert_eq!(
                    ledger.meters.uncertain.tokens,
                    if mode == "bounded" { 30 } else { 0 }
                );
                assert_eq!(
                    ledger.meters.unknown_usage_attempts,
                    u64::from(matches!(mode, "record-only" | "lifetime-only"))
                );
                assert_eq!(ledger.meters.settled.tokens, 0);
                assert_eq!(ledger.meters.reserved.tokens, 0);
            }
        }
    }
}

#[tokio::test]
async fn content_filter_refusal_survives_retained_accounting_in_both_proxy_paths() {
    for calendar_only in [false, true] {
        for streaming in [false, true] {
            for status in [400, 403] {
                for code in ["content_filter", "responsible_ai_policy_violation"] {
                    let fixture = Fixture::new(100, false).await;
                    let (_server, calendar) = if calendar_only {
                        record_only_server().await
                    } else {
                        accounting_server().await
                    };
                    let mut upstream = fixture.upstream();
                    if calendar_only {
                        upstream.inference_budget = None;
                    }
                    upstream.calendar_budget = Some(calendar.clone());
                    let body = json!({"error":{
                        "code":code,"param":"prompt","message":"PRIVATE_PROVIDER_CONTENT",
                        "innererror":{"code":"ResponsibleAIPolicyViolation",
                            "content_filter_result":{"jailbreak":{"detected":true,"filtered":true}}}
                    }})
                    .to_string();
                    Mock::given(wiremock::matchers::method("POST"))
                        .respond_with(
                            ResponseTemplate::new(status)
                                .set_body_raw(body.clone(), "application/json"),
                        )
                        .expect(1)
                        .mount(&fixture.provider)
                        .await;
                    let request = bytes::Bytes::from(
                        json!({
                            "stream":streaming,"messages":[{"role":"user","content":"test"}]
                        })
                        .to_string(),
                    );
                    let (actual_status, actual_body) = if streaming {
                        let (status, _, stream) = crate::proxy::forward_stream(
                            Arc::new(WorkloadIdentityAuth::new()),
                            None,
                            reqwest::Client::new(),
                            upstream,
                            "chat/completions",
                            HeaderMap::new(),
                            request,
                        )
                        .await
                        .unwrap();
                        let chunks: Vec<_> = stream.try_collect().await.unwrap();
                        (status, chunks.concat())
                    } else {
                        let (status, _, bytes) = crate::proxy::forward(
                            &WorkloadIdentityAuth::new(),
                            None,
                            &reqwest::Client::new(),
                            &upstream,
                            Method::POST,
                            "chat/completions",
                            &HeaderMap::new(),
                            request,
                        )
                        .await
                        .unwrap();
                        (status, bytes.to_vec())
                    };
                    assert_eq!(actual_status.as_u16(), status);
                    assert_eq!(actual_body, body.as_bytes());
                    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
                    let observation = calendar.read().await.unwrap();
                    assert_eq!(observation.usage.daily, if calendar_only { 0 } else { 30 });
                    if calendar_only {
                        assert_eq!(observation.pending_requests, Some(1));
                    }
                    let ledger = fixture.ledger.lock().unwrap();
                    assert_eq!(
                        ledger.meters.uncertain.tokens,
                        if calendar_only { 0 } else { 30 }
                    );
                    assert_eq!(ledger.meters.settled.tokens, 0);
                    assert_eq!(ledger.meters.reserved.tokens, 0);
                }
            }
        }
    }
}

#[tokio::test]
async fn provider_parameter_error_without_code_retains_acknowledged_liability() {
    let fixture = Fixture::new(100, false).await;
    let (_server, calendar) = accounting_server().await;
    let mut upstream = fixture.upstream();
    upstream.calendar_budget = Some(calendar.clone());
    let (_, attempt) = dispatch::begin(
        &upstream,
        &Method::POST,
        "/chat/completions",
        bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"test"}]}"#),
    )
    .await
    .unwrap();
    dispatch::finish(
        attempt,
        StatusCode::BAD_REQUEST,
        br#"{"error":{"type":"invalid_request_error","param":"reasoning_effort"}}"#,
    )
    .await
    .unwrap();
    assert_eq!(calendar.read().await.unwrap().usage.daily, 30);
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.uncertain.tokens, 30);
    assert_eq!(ledger.meters.settled.tokens, 0);
    assert_eq!(ledger.meters.reserved.tokens, 0);
}

#[tokio::test]
async fn content_filter_like_responses_do_not_hide_missing_usage_errors() {
    let recognized = json!({"error":{"code":"content_filter"}}).to_string();
    for (status, body) in [
        (200, recognized.clone()),
        (402, recognized.clone()),
        (404, recognized),
        (400, json!({"error":{"code":"invalid_request","message":"content management policy"}}).to_string()),
        (403, json!({"error":{"innererror":{"content_filter_result":{"jailbreak":{"detected":true}}}}}).to_string()),
        (400, json!({"error":{"code":["content_filter"]}}).to_string()),
        (400, json!({"error":{"code":"content_filter","message":"x".repeat(65_536)}}).to_string()),
        (400, "not JSON".into()),
        (200, json!({"error":{"code":"unsupported_value","param":"temperature"}}).to_string()),
        (403, json!({"error":{"code":"unsupported_value","param":"temperature"}}).to_string()),
        (400, json!({"error":{"code":"unsupported_value","param":null}}).to_string()),
        (400, json!({"error":{"code":"unsupported_value","param":["temperature"]}}).to_string()),
        (400, json!({"error":{"code":"unsupported_value","param":"  "}}).to_string()),
        (400, json!({"error":{"code":["unsupported_value"],"param":"temperature"}}).to_string()),
        (400, json!({"error":{"code":"unsupported_value","param":"temperature","message":"x".repeat(65_536)}}).to_string()),
    ] {
        let fixture = Fixture::new(100, false).await;
        let (_server, calendar) = accounting_server().await;
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = Some(calendar.clone());
        let (_, attempt) = dispatch::begin(
            &upstream, &Method::POST, "/chat/completions",
            bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"test"}]}"#),
        ).await.unwrap();
        let error = dispatch::finish(attempt, StatusCode::from_u16(status).unwrap(), body.as_bytes())
            .await.unwrap_err();
        assert!(error.to_string().contains("maximum charge retained"));
        assert!(!crate::proxy::failure::retryable_failure(&error));
        assert_eq!(calendar.read().await.unwrap().usage.daily, 30);
        assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
    }
}

#[tokio::test]
async fn provider_errors_cannot_hide_unacknowledged_or_breaching_lifetime_settlement() {
    for (status, code, receipt) in [
        None,
        Some(
            json!({"finalized":false,"uncertain":true,"breach":false,"charged":{"tokens":30,"usdMicros":5}}),
        ),
        Some(
            json!({"finalized":true,"uncertain":false,"breach":false,"charged":{"tokens":30,"usdMicros":5}}),
        ),
        Some(
            json!({"finalized":true,"uncertain":true,"breach":true,"charged":{"tokens":30,"usdMicros":5}}),
        ),
        Some(json!({"finalized":true,"uncertain":true,"breach":false,"charged":null})),
        Some(
            json!({"finalized":true,"uncertain":true,"breach":false,"charged":{"tokens":29,"usdMicros":5}}),
        ),
        Some(
            json!({"finalized":true,"uncertain":true,"breach":false,"charged":{"tokens":31,"usdMicros":5}}),
        ),
    ].into_iter().flat_map(|receipt| {
        [(400, "content_filter"), (400, "unsupported_value"), (400, "InvalidParameter"), (429, "server_error"), (503, "server_error")]
            .into_iter()
            .map(|(status, code)| (status, Some(code)))
            .chain([(400, None)])
            .map(move |(status, code)| (status, code, receipt.clone()))
    }) {
        let fixture = Fixture::new(100, false).await;
        let ledger = fixture.ledger.clone();
        Mock::given(wiremock::matchers::path("/v1/settle"))
            .respond_with(move |request: &Request| {
                let request: BrokerRequest<Settlement> =
                    serde_json::from_slice(&request.body).unwrap();
                let mut ledger = ledger.lock().unwrap();
                *ledger = ledger.settle(&request.payload).unwrap().next;
                match &receipt {
                    Some(value) => ResponseTemplate::new(200).set_body_json(value),
                    None => ResponseTemplate::new(503),
                }
            })
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        let (_server, calendar) = accounting_server().await;
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = Some(calendar.clone());
        let (_, attempt) = dispatch::begin(
            &upstream,
            &Method::POST,
            "/chat/completions",
            bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"test"}]}"#),
        )
        .await
        .unwrap();
        let error = dispatch::finish(
            attempt,
            StatusCode::from_u16(status).unwrap(),
            json!({"error":{"code":code,"param":"temperature"}})
                .to_string()
                .as_bytes(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("pending liability retained"));
        assert!(!crate::proxy::failure::retryable_failure(&error));
        assert!(calendar.read().await.is_err());
        assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
    }
}

#[tokio::test]
async fn provider_errors_cannot_hide_failed_calendar_commit() {
    for (status, code) in [
        (400, "content_filter"),
        (400, "unsupported_parameter"),
        (400, "InvalidParameter"),
        (429, "server_error"),
        (503, "server_error"),
    ]
    .into_iter()
    .map(|(status, code)| (status, Some(code)))
    .chain([(400, None)])
    {
        let fixture = Fixture::new(100, false).await;
        let (server, calendar) = accounting_server().await;
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = Some(calendar.clone());
        let (_, attempt) = dispatch::begin(
            &upstream,
            &Method::POST,
            "/chat/completions",
            bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"test"}]}"#),
        )
        .await
        .unwrap();
        Mock::given(wiremock::matchers::path("/v1/calendar/commit"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .mount(&server)
            .await;
        let error = dispatch::finish(
            attempt,
            StatusCode::from_u16(status).unwrap(),
            json!({"error":{"code":code,"param":"temperature"}})
                .to_string()
                .as_bytes(),
        )
        .await
        .unwrap_err();
        assert!(!crate::proxy::failure::retryable_failure(&error));
        assert!(calendar.read().await.is_err());
        assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
    }
}
