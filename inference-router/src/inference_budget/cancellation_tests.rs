// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget::{calendar, dispatch};
use crate::inference_budget_contract::calendar::{Commit, Event};
use axum::http::StatusCode;
use bytes::Bytes;
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::Notify;

async fn begin(fixture: &Fixture, calendar: &Arc<calendar::Client>) -> Option<dispatch::Attempt> {
    let mut upstream = fixture.upstream();
    upstream.calendar_budget = Some(calendar.clone());
    dispatch::begin(
        &upstream,
        &Method::POST,
        "/responses",
        Bytes::from_static(br#"{"input":"test"}"#),
    )
    .await
    .unwrap()
    .1
}

async fn wait_for_usage(calendar: &calendar::Client, tokens: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let usage = calendar.read().await.unwrap().usage;
            if usage.daily == tokens && usage.monthly == tokens {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("paired worker did not acknowledge calendar completion");
}

async fn wait_for_blocked(calendar: &calendar::Client) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while calendar.read().await.is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("unproven settlement did not retain calendar liability");
}

async fn settlements(server: &MockServer) -> Vec<Settlement> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/v1/settle")
        .map(|request| {
            serde_json::from_slice::<BrokerRequest<Settlement>>(&request.body)
                .unwrap()
                .payload
        })
        .collect()
}

async fn finishes(server: &MockServer) -> Vec<Commit> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/v1/calendar/commit")
        .map(|request| serde_json::from_slice::<Commit>(&request.body).unwrap())
        .filter(|commit| commit.event == Event::Finish)
        .collect()
}

#[tokio::test]
async fn reserve_contention_dropped_rpc_has_no_detached_retry() {
    let fixture = Fixture::new(100, false).await;
    let received = Arc::new(Notify::new());
    let notify = received.clone();
    Mock::given(wiremock::matchers::path("/v1/reserve"))
        .respond_with(move |_: &Request| {
            notify.notify_one();
            ResponseTemplate::new(503).set_body_json(json!({
                "error":{"code":"inference_budget_contention"}
            }))
        })
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    let client = fixture.client.clone();
    let caller = tokio::spawn(async move {
        client
            .rpc::<_, serde_json::Value>("/v1/reserve", client.session_request())
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), received.notified())
        .await
        .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(fixture._broker.received_requests().await.unwrap().len(), 1);
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

#[tokio::test]
async fn reserve_contention_cancelled_admission_keeps_worker_ownership() {
    let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
    let (server, calendar) = calendar::tests::accounting_server().await;
    let received = Arc::new(Notify::new());
    let notify = received.clone();
    let ledger = fixture.ledger.clone();
    Mock::given(wiremock::matchers::path("/v1/reserve"))
        .respond_with(move |request: &Request| {
            let request: BrokerRequest<ReserveRequest> =
                serde_json::from_slice(&request.body).unwrap();
            let mut ledger = ledger.lock().unwrap();
            *ledger = ledger
                .reserve(&request.payload, chrono::Utc::now().timestamp())
                .unwrap()
                .next;
            notify.notify_one();
            ResponseTemplate::new(503).set_body_json(json!({
                "error":{"code":"inference_budget_contention"}
            }))
        })
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    let mut upstream = fixture.upstream();
    upstream.calendar_budget = Some(calendar.clone());
    let caller = tokio::spawn(async move {
        dispatch::begin(
            &upstream,
            &Method::POST,
            "/responses",
            Bytes::from_static(br#"{"input":"test"}"#),
        )
        .await
        .is_ok()
    });
    tokio::time::timeout(Duration::from_secs(5), received.notified())
        .await
        .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        while finishes(&server).await.len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("abandoned admission failed to complete its accounting handshake");
    wait_for_usage(&calendar, 30).await;
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
    let settled = settlements(&fixture._broker).await;
    assert!(settled[0].usage.is_none());
    let commits = finishes(&server).await;
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sequence, commits[1].sequence);
    assert!(commits.iter().all(|commit| commit.tokens == 30));
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.reserved.tokens, 0);
    assert_eq!(ledger.meters.settled.tokens, 0);
    assert_eq!(ledger.meters.uncertain.tokens, 30);
    drop(ledger);
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn reserve_contention_admission_uses_the_existing_90_second_deadline() {
    // Exercise the production deadline rather than adding a test-only timeout policy.
    let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
    let (server, calendar) = calendar::tests::accounting_server().await;
    let ledger = fixture.ledger.clone();
    let calls = AtomicUsize::new(0);
    Mock::given(wiremock::matchers::path("/v1/reserve"))
        .respond_with(move |request: &Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
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
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    let mut upstream = fixture.upstream();
    upstream.calendar_budget = Some(calendar.clone());
    let started = tokio::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(95),
        dispatch::begin(
            &upstream,
            &Method::POST,
            "/responses",
            Bytes::from_static(br#"{"input":"test"}"#),
        ),
    )
    .await
    .expect("reserve retries outlived the admission deadline")
    .err()
    .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(90));
    assert_eq!(
        error.downcast_ref::<Error>().unwrap().stage,
        "admission timeout; liability retained"
    );
    assert!(!crate::proxy::failure::retryable_failure(&error));
    let requests = fixture._broker.received_requests().await.unwrap();
    let reserves: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/v1/reserve")
        .collect();
    assert!(reserves.len() >= 2);
    assert!(reserves.iter().all(|r| r.body == reserves[0].body));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/v1/session")
            .count(),
        1
    );
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r.url.path(), "/v1/begin" | "/v1/settle"))
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        fixture._broker.received_requests().await.unwrap().len(),
        requests.len()
    );
    assert!(fixture.client.reservation_lock.try_lock().is_ok());
    assert!(finishes(&server).await.is_empty());
    assert!(calendar.read().await.is_err());
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.reserved.tokens, 30);
    assert_eq!(ledger.meters.settled.tokens, 0);
    assert_eq!(ledger.meters.uncertain.tokens, 0);
    assert_eq!(ledger.attempts.len(), 1);
}

#[tokio::test]
async fn cancelled_admission_finishes_refused_or_granted_accounting_without_provider_send() {
    for granted in [false, true] {
        let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
        let (server, calendar) = calendar::tests::accounting_server().await;
        let received = Arc::new(Notify::new());
        let notify = received.clone();
        let ledger = fixture.ledger.clone();
        let route = if granted { "/v1/begin" } else { "/v1/reserve" };
        Mock::given(wiremock::matchers::path(route))
            .respond_with(move |request: &Request| {
                let response = if granted {
                    let request: BrokerRequest<AttemptCommand> =
                        serde_json::from_slice(&request.body).unwrap();
                    let mut ledger = ledger.lock().unwrap();
                    let change = ledger
                        .begin_dispatch(&request.payload, chrono::Utc::now().timestamp())
                        .unwrap();
                    *ledger = change.next;
                    ResponseTemplate::new(200).set_body_json(change.value)
                } else {
                    ResponseTemplate::new(429).set_body_json(json!({
                        "error": {"code": "inference_budget_exhausted"}
                    }))
                };
                notify.notify_one();
                response.set_delay(Duration::from_millis(200))
            })
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = Some(calendar.clone());
        let caller = tokio::spawn(async move {
            dispatch::begin(
                &upstream,
                &Method::POST,
                "/responses",
                Bytes::from_static(br#"{"input":"test"}"#),
            )
            .await
            .is_ok()
        });
        tokio::time::timeout(Duration::from_secs(5), received.notified())
            .await
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            while finishes(&server).await.len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled admission left calendar pending");
        let tokens = if granted { 30 } else { 0 };
        wait_for_usage(&calendar, tokens).await;
        let commits = finishes(&server).await;
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sequence, commits[1].sequence);
        assert!(commits.iter().all(|commit| commit.tokens == tokens));
        let settled = settlements(&fixture._broker).await;
        assert_eq!(settled.len(), usize::from(granted));
        {
            let ledger = fixture.ledger.lock().unwrap();
            assert_eq!(ledger.meters.reserved.tokens, 0);
            assert_eq!(ledger.meters.uncertain.tokens, tokens);
            assert_eq!(ledger.meters.settled.tokens, 0);
        }
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        calendar.begin().await.unwrap().finish(0).await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_closes_calendar_at_the_acknowledged_maximum_once() {
    for cancellation in [
        "request",
        "unpolled stream",
        "polled stream",
        "unpolled finish",
    ] {
        let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
        let (server, calendar) = calendar::tests::accounting_server().await;
        let attempt = begin(&fixture, &calendar).await;
        match cancellation {
            "request" => drop(attempt),
            "unpolled finish" => drop(dispatch::finish(attempt, StatusCode::OK, b"{}")),
            _ => {
                let mut stream =
                    dispatch::stream(futures::stream::pending().boxed(), attempt, true);
                if cancellation == "polled stream" {
                    assert!(futures::poll!(stream.next()).is_pending());
                }
                drop(stream);
            }
        }
        wait_for_usage(&calendar, 30).await;
        let requests = settlements(&fixture._broker).await;
        assert_eq!(requests.len(), 1, "{cancellation}");
        assert!(requests[0].usage.is_none());
        let commits = finishes(&server).await;
        // The calendar fixture loses its first Finish acknowledgement. Only the
        // same sequenced Finish may be retried; the ledger must count it once.
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sequence, commits[1].sequence);
        assert_eq!(commits[0].writer, commits[1].writer);
        assert!(commits.iter().all(|commit| commit.tokens == 30));
        {
            let ledger = fixture.ledger.lock().unwrap();
            assert_eq!(ledger.meters.uncertain.tokens, 30);
            assert_eq!(ledger.meters.uncertain.usd_micros, 5);
            assert_eq!(ledger.meters.settled.tokens, 0);
            assert_eq!(ledger.meters.reserved.tokens, 0);
        }
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );

        let next = begin(&fixture, &calendar).await;
        dispatch::finish(
            next,
            StatusCode::OK,
            br#"{"usage":{"input_tokens":4,"output_tokens":5,"total_tokens":9}}"#,
        )
        .await
        .unwrap();
        assert_eq!(calendar.read().await.unwrap().usage.daily, 39);
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.uncertain.tokens, 30);
        assert_eq!(ledger.meters.settled.tokens, 9);
    }
}

#[tokio::test]
async fn cancellation_during_broker_settlement_keeps_the_same_rpc_and_evidence() {
    for measured in [false, true] {
        let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
        let (server, calendar) = calendar::tests::accounting_server().await;
        let received = Arc::new(Notify::new());
        let notify = received.clone();
        let ledger = fixture.ledger.clone();
        Mock::given(wiremock::matchers::path("/v1/settle"))
            .respond_with(move |request: &Request| {
                let request: BrokerRequest<Settlement> =
                    serde_json::from_slice(&request.body).unwrap();
                let mut ledger = ledger.lock().unwrap();
                let change = ledger.settle(&request.payload).unwrap();
                *ledger = change.next;
                notify.notify_one();
                ResponseTemplate::new(200)
                    .set_body_json(change.value)
                    .set_delay(Duration::from_millis(200))
            })
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        let attempt = begin(&fixture, &calendar).await;
        let caller = tokio::spawn(async move {
            if measured {
                let source = futures::stream::iter([Ok(Bytes::from_static(
                    b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":4,\"output_tokens\":5,\"total_tokens\":9}}}\n\n",
                ))])
                .boxed();
                let _ = dispatch::stream(source, attempt, true).next().await;
            } else {
                let _ = dispatch::finish(attempt, StatusCode::REQUEST_TIMEOUT, b"").await;
            }
        });
        tokio::time::timeout(Duration::from_secs(5), received.notified())
            .await
            .unwrap();
        assert!(
            finishes(&server).await.is_empty(),
            "receipt is not delivered yet"
        );
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let tokens = if measured { 9 } else { 30 };
        wait_for_usage(&calendar, tokens).await;
        let requests = settlements(&fixture._broker).await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].usage.is_some(), measured);
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.reserved.tokens, 0);
        assert_eq!(
            ledger.meters.uncertain.tokens,
            if measured { 0 } else { 30 }
        );
        assert_eq!(ledger.meters.settled.tokens, if measured { 9 } else { 0 });
    }
}

#[tokio::test]
async fn cancellation_during_calendar_commit_preserves_sequenced_retry() {
    for lose_second_ack in [false, true] {
        let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
        let (server, calendar) = calendar::tests::accounting_server().await;
        if !lose_second_ack {
            // Consume the fixture's one lost acknowledgement before the delayed
            // commit, so the cancelled caller can also exercise a successful retry.
            let warmup = begin(&fixture, &calendar).await;
            dispatch::finish(
                warmup,
                StatusCode::OK,
                br#"{"usage":{"input_tokens":4,"output_tokens":5,"total_tokens":9}}"#,
            )
            .await
            .unwrap();
        }
        let received = Arc::new(Notify::new());
        let notify = received.clone();
        Mock::given(wiremock::matchers::path("/v1/calendar/commit"))
            .and(wiremock::matchers::body_partial_json(
                json!({"event":"finish"}),
            ))
            .respond_with(move |_: &Request| {
                notify.notify_one();
                ResponseTemplate::new(503).set_delay(Duration::from_millis(200))
            })
            .with_priority(1)
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let attempt = begin(&fixture, &calendar).await;
        let caller = tokio::spawn(dispatch::finish(attempt, StatusCode::REQUEST_TIMEOUT, b""));
        tokio::time::timeout(Duration::from_secs(5), received.notified())
            .await
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        if lose_second_ack {
            // Both replies failed, even though the second commit was durable.
            // Keep the charge and local uncertainty; do not send a third Finish.
            wait_for_blocked(&calendar).await;
            assert!(calendar.begin().await.is_err());
        } else {
            wait_for_usage(&calendar, 39).await;
        }
        let commits = finishes(&server).await;
        assert_eq!(commits.len(), if lose_second_ack { 2 } else { 4 });
        let cancelled = &commits[commits.len() - 2..];
        assert!(cancelled.iter().all(|commit| commit.tokens == 30));
        assert_eq!(cancelled[0].sequence, cancelled[1].sequence);
        assert_eq!(
            settlements(&fixture._broker).await.len(),
            if lose_second_ack { 1 } else { 2 },
        );
        assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
    }
}

#[tokio::test]
async fn cancellation_without_a_trustworthy_broker_receipt_keeps_calendar_pending() {
    for receipt in [
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
        Some(
            json!({"finalized":true,"uncertain":true,"breach":false,"charged":{"tokens":30,"usdMicros":4}}),
        ),
    ] {
        for during_settlement in [false, true] {
            let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
            let (server, calendar) = calendar::tests::accounting_server().await;
            let ledger = fixture.ledger.clone();
            let receipt = receipt.clone();
            let received = Arc::new(Notify::new());
            let notify = received.clone();
            Mock::given(wiremock::matchers::path("/v1/settle"))
                .respond_with(move |request: &Request| {
                    let request: BrokerRequest<Settlement> =
                        serde_json::from_slice(&request.body).unwrap();
                    let mut ledger = ledger.lock().unwrap();
                    *ledger = ledger.settle(&request.payload).unwrap().next;
                    notify.notify_one();
                    match &receipt {
                        Some(value) => ResponseTemplate::new(200).set_body_json(value),
                        // The lifetime debit is durable, but the response is lost.
                        None => ResponseTemplate::new(503),
                    }
                    .set_delay(Duration::from_millis(200))
                })
                .with_priority(1)
                .mount(&fixture._broker)
                .await;
            let attempt = begin(&fixture, &calendar).await;
            if during_settlement {
                let caller =
                    tokio::spawn(dispatch::finish(attempt, StatusCode::REQUEST_TIMEOUT, b""));
                tokio::time::timeout(Duration::from_secs(5), received.notified())
                    .await
                    .unwrap();
                caller.abort();
                assert!(caller.await.unwrap_err().is_cancelled());
            } else {
                drop(dispatch::stream(
                    futures::stream::pending().boxed(),
                    attempt,
                    true,
                ));
            }
            wait_for_blocked(&calendar).await;
            assert!(finishes(&server).await.is_empty());
            assert_eq!(settlements(&fixture._broker).await.len(), 1);
            assert!(calendar.begin().await.is_err());
            let ledger = fixture.ledger.lock().unwrap();
            assert_eq!(ledger.meters.uncertain.tokens, 30);
            assert_eq!(ledger.meters.settled.tokens, 0);
            assert_eq!(ledger.meters.reserved.tokens, 0);
        }
    }
}

#[tokio::test]
async fn cancellation_with_failed_broker_settlement_retains_the_reservation() {
    let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
    let (server, calendar) = calendar::tests::accounting_server().await;
    Mock::given(wiremock::matchers::path("/v1/settle"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    drop(begin(&fixture, &calendar).await);
    wait_for_blocked(&calendar).await;
    assert!(finishes(&server).await.is_empty());
    assert_eq!(settlements(&fixture._broker).await.len(), 1);
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.reserved.tokens, 30);
    assert_eq!(ledger.meters.uncertain.tokens, 0);
    assert_eq!(ledger.meters.settled.tokens, 0);
}

#[tokio::test]
async fn cancellation_outside_a_runtime_retains_both_liabilities() {
    let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
    let (server, calendar) = calendar::tests::accounting_server().await;
    let attempt = begin(&fixture, &calendar).await;
    std::thread::spawn(move || drop(attempt)).join().unwrap();
    assert!(calendar.read().await.is_err());
    assert!(finishes(&server).await.is_empty());
    assert!(settlements(&fixture._broker).await.is_empty());
    assert_eq!(fixture.ledger.lock().unwrap().meters.reserved.tokens, 30);
}

#[tokio::test]
async fn calendar_only_cancellation_stays_synchronously_fail_closed() {
    let (server, calendar) = calendar::tests::accounting_server().await;
    let mut upstream = UpstreamConfig::azure(
        "https://unused.invalid".into(),
        "model".into(),
        "sandbox".into(),
    );
    upstream.calendar_budget = Some(calendar.clone());
    let (_, attempt) = dispatch::begin(
        &upstream,
        &Method::POST,
        "/responses",
        Bytes::from_static(br#"{"input":"test"}"#),
    )
    .await
    .unwrap();
    drop(dispatch::stream(
        futures::stream::pending().boxed(),
        attempt,
        true,
    ));
    assert!(calendar.read().await.is_err());
    assert!(calendar.begin().await.is_err());
    assert!(finishes(&server).await.is_empty());
}
