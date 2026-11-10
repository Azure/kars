// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn privacy_pending_resumes_the_same_admission_without_duplicate_dispatch() {
    for streaming in [false, true] {
        for stage in ["/v1/catalog", "/v1/session", "/v1/reserve", "/v1/begin"] {
            let fixture = fixture(streaming, 100).await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "usage":{"prompt_tokens":3,"completion_tokens":5,
                        "input_tokens":3,"output_tokens":5,"total_tokens":8}
                })))
                .mount(&fixture.provider)
                .await;
            Mock::given(wiremock::matchers::path(stage))
                .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                    "error":{"code":"inference_privacy_pending"}
                })))
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&fixture._broker)
                .await;
            assert!(forward(&fixture, streaming).await.unwrap().0.is_success());
            let requests = fixture._broker.received_requests().await.unwrap();
            let attempts: Vec<_> = requests.iter().filter(|r| r.url.path() == stage).collect();
            assert!(attempts.len() >= 2);
            assert_eq!(attempts[0].body, attempts[1].body);
            assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
            let ledger = fixture.ledger.lock().unwrap();
            assert!(ledger.meters.settled.tokens > 0);
            assert_eq!(ledger.meters.reserved.tokens, 0);
            assert_eq!(ledger.meters.uncertain.tokens, 0);
        }
    }
}

#[tokio::test]
async fn privacy_wait_rechecks_authority_and_never_retries_revocation() {
    let fixture = fixture(false, 100).await;
    Mock::given(wiremock::matchers::path("/v1/catalog"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error":{"code":"inference_privacy_pending"}
        })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    Mock::given(wiremock::matchers::path("/v1/catalog"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error":{"code":"inference_budget_authority"}
        })))
        .with_priority(2)
        .mount(&fixture._broker)
        .await;
    let error = forward(&fixture, false).await.unwrap_err();
    assert_eq!(denial(&error).unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(fixture._broker.received_requests().await.unwrap().len(), 2);
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

#[tokio::test]
async fn privacy_retry_rereads_the_projected_audience_token() {
    let fixture = fixture(false, 100).await;
    let token_path = fixture.client.token_path.clone();
    let calls = AtomicUsize::new(0);
    Mock::given(wiremock::matchers::path("/v1/catalog"))
        .respond_with(move |request: &Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                std::fs::write(&token_path, "rotated-test-token").unwrap();
                ResponseTemplate::new(503).set_body_json(json!({
                    "error":{"code":"inference_privacy_pending"}
                }))
            } else {
                assert_eq!(
                    request.headers.get("authorization").unwrap(),
                    "Bearer rotated-test-token"
                );
                ResponseTemplate::new(200).set_body_json(json!({"recovered":true}))
            }
        })
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    let response: serde_json::Value = fixture
        .client
        .rpc("/v1/catalog", fixture.client.session_request())
        .await
        .unwrap();
    assert_eq!(response, json!({"recovered":true}));
    assert_eq!(fixture._broker.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn privacy_pending_is_not_retried_on_wrong_status_or_settlement() {
    for (path, status) in [
        ("/v1/catalog", 403),
        ("/v1/catalog", 429),
        ("/v1/settle", 503),
    ] {
        let fixture = fixture(false, 100).await;
        Mock::given(wiremock::matchers::path(path))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "error":{"code":"inference_privacy_pending"}
            })))
            .with_priority(1)
            .mount(&fixture._broker)
            .await;
        let result: Result<serde_json::Value, _> = fixture
            .client
            .rpc(path, fixture.client.session_request())
            .await;
        assert_eq!(result.unwrap_err().status, Some(status));
        assert_eq!(fixture._broker.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancelling_privacy_wait_leaves_no_detached_retry_or_provider_request() {
    let fixture = fixture(false, 100).await;
    Mock::given(wiremock::matchers::path("/v1/catalog"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error":{"code":"inference_privacy_pending"}
        })))
        .with_priority(1)
        .mount(&fixture._broker)
        .await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fixture
                .client
                .rpc::<_, serde_json::Value>("/v1/catalog", fixture.client.session_request()),
        )
        .await
        .is_err()
    );
    let count = fixture._broker.received_requests().await.unwrap().len();
    assert_eq!(count, 1);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        fixture._broker.received_requests().await.unwrap().len(),
        count
    );
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
