// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::ledger::UnboundedTokenAllowance;

fn authorize(fixture: &Fixture) {
    let mut ledger = fixture.ledger.lock().unwrap();
    let grant = UnboundedTokenAllowance {
        id: "calendar-beta".into(),
        config_uid: "catalog-uid".into(),
        account_uid: ledger.account_uid.clone(),
        root: ledger.root.clone(),
        original_limits: ledger.limits,
        ledger_digest: ledger.funding_renewal_state_digest().unwrap(),
        previous_token_top_up_ids: ledger.token_top_ups.keys().cloned().collect(),
    };
    *ledger = ledger
        .authorize_unbounded_tokens(&grant, chrono::Utc::now().timestamp())
        .unwrap()
        .next;
}

#[tokio::test]
async fn calendar_allowance_covers_all_inference_routes_without_losing_accounting() {
    for path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
        for monthly in [false, true] {
            for allowed in [false, true] {
                let responses = path == "/v1/responses";
                let mut fixture = Fixture::for_operation(
                    1,
                    false,
                    if responses {
                        Operation::Responses
                    } else {
                        Operation::ChatCompletions
                    },
                )
                .await;
                if allowed {
                    authorize(&fixture);
                }
                Arc::get_mut(&mut fixture.client)
                    .unwrap()
                    .identity
                    .sandbox
                    .name = "sandbox".into();
                let mut state = if responses {
                    let mut state = endpoint_state(&fixture, true).await;
                    Arc::get_mut(&mut state.config)
                        .unwrap()
                        .providers
                        .get_mut("budget-fixture")
                        .unwrap()
                        .api_key = Some("test-key".into());
                    let mut catalog = fixture.client.catalog().await.unwrap();
                    catalog.contracts[0].provider_id = "budget-fixture".into();
                    fixture._broker.reset().await;
                    Mock::given(wiremock::matchers::method("POST"))
                        .respond_with(BrokerFixture {
                            ledger: fixture.ledger.clone(),
                            catalog,
                            lose_begin_ack: false,
                        })
                        .mount(&fixture._broker)
                        .await;
                    state
                } else {
                    ollama_endpoint_state(&fixture).await
                };
                let (_calendar_server, calendar) =
                    crate::inference_budget::calendar::tests::accounting_server().await;
                calendar.begin().await.unwrap().finish(1000).await.unwrap();
                state.sandbox_name = Arc::new("sandbox".into());
                state.budget = crate::budget::TokenBudgetTracker::with_calendar(0, 0, calendar);
                assert_eq!(
                    state.budget.observed_usage("sandbox").await.unwrap(),
                    (1000, 1000)
                );
                {
                    let mut policy = state.inference_policy.write().await;
                    let policy = policy.as_mut().unwrap();
                    policy.daily_tokens = Some(if monthly { 0 } else { 1000 });
                    policy.monthly_tokens = Some(if monthly { 1000 } else { 0 });
                }
                let tracker = state.budget.clone();
                let provider_body = if responses {
                    json!({"id":"response", "model":"model", "status":"completed",
                        "output":[{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"accounted"}]}],
                        "usage":{"input_tokens":4,"output_tokens":5,"total_tokens":9}})
                } else {
                    json!({"id":"chat", "model":"model",
                        "choices":[{"message":{"role":"assistant","content":"accounted"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":4,"completion_tokens":5,"total_tokens":9}})
                };
                Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(provider_body))
                    .mount(&fixture.provider)
                    .await;
                let request = if responses {
                    json!({"model":"model","input":"text","max_output_tokens":20})
                } else {
                    json!({"model":"model","messages":[{"role":"user","content":"text"}],"max_tokens":20})
                };
                let response = Router::new()
                    .merge(crate::routes::inference_routes())
                    .with_state(state)
                    .oneshot(
                        HttpRequest::builder()
                            .method("POST")
                            .uri(path)
                            .header("content-type", "application/json")
                            .body(Body::from(request.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = response.status();
                let body = to_bytes(response.into_body(), 16384).await.unwrap();
                assert_eq!(
                    status,
                    if allowed {
                        StatusCode::OK
                    } else {
                        StatusCode::TOO_MANY_REQUESTS
                    },
                    "{path} monthly={monthly} allowed={allowed}: {}",
                    String::from_utf8_lossy(&body)
                );
                assert_eq!(
                    fixture.provider.received_requests().await.unwrap().len(),
                    usize::from(allowed)
                );
                assert_eq!(
                    tracker.observed_usage("sandbox").await.unwrap(),
                    if allowed { (1009, 1009) } else { (1000, 1000) }
                );
                let ledger = fixture.ledger.lock().unwrap();
                assert_eq!(ledger.meters.settled.tokens, if allowed { 9 } else { 0 });
                assert_eq!(
                    ledger.meters.settled.usd_micros,
                    if allowed { 5 } else { 0 }
                );
                assert_eq!(ledger.meters.reserved.tokens, 0);
                assert_eq!(ledger.meters.uncertain.tokens, 0);
            }
        }
    }
}

#[tokio::test]
async fn calendar_allowance_missing_revoked_malformed_or_expired_authority_fails_closed() {
    for case in [
        "legacy",
        "finite",
        "revoked",
        "unavailable",
        "malformed",
        "expired",
        "identity",
    ] {
        let fixture = Fixture::new(100, false).await;
        authorize(&fixture);
        let mut catalog = fixture.client.catalog().await.unwrap();
        if case == "expired" {
            catalog.contracts[0].valid_until = "2020-01-01T00:00:00Z".into();
        }
        let mut body = json!({"catalog":catalog,"moneyRequired":true});
        if case != "legacy" {
            body["unboundedTokens"] = match case {
                "finite" => json!(false),
                "malformed" => json!("true"),
                _ => json!(true),
            };
        }
        let tracker = crate::budget::TokenBudgetTracker::new(1, 0);
        let sandbox = if case == "identity" {
            "different"
        } else {
            "task"
        };
        tracker.record_usage(sandbox, 10).await;
        // A successful first read must not survive subsequent revocation or failure.
        assert!(fixture.client.tokens_unbounded("task").await.unwrap());
        fixture._broker.reset().await;
        Mock::given(wiremock::matchers::path("/v1/catalog"))
            .respond_with(match case {
                "revoked" => ResponseTemplate::new(403),
                "unavailable" => ResponseTemplate::new(503),
                _ => ResponseTemplate::new(200).set_body_json(body),
            })
            .mount(&fixture._broker)
            .await;
        let before = fixture.ledger.lock().unwrap().clone();
        let admission = tracker
            .check_governed_budget(sandbox, Some(1), Some(1), Some(&fixture.client))
            .await;
        if case == "expired" {
            // Funding authority and model-contract expiry are separate controls.
            admission.unwrap();
            let error = fixture.buffered().await.unwrap_err();
            assert_eq!(
                error.downcast_ref::<Error>().unwrap().stage,
                "provider/operation contract"
            );
        } else {
            assert_eq!(
                admission.expect_err(case).status(),
                if matches!(case, "legacy" | "finite") {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                "{case}"
            );
        }
        assert_eq!(tracker.observed_usage(sandbox).await.unwrap(), (10, 10));
        assert_eq!(*fixture.ledger.lock().unwrap(), before);
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        if case == "identity" {
            assert!(
                fixture
                    ._broker
                    .received_requests()
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[tokio::test]
async fn calendar_allowance_never_overrides_missing_or_uncertain_durable_accounting() {
    for pending in [false, true] {
        let fixture = Fixture::new(100, false).await;
        authorize(&fixture);
        let (server, calendar) =
            crate::inference_budget::calendar::tests::accounting_server().await;
        let tracker = crate::budget::TokenBudgetTracker::with_calendar(1, 0, calendar.clone());
        if pending {
            let attempt = calendar.begin().await.unwrap();
            drop(attempt);
        } else {
            server.reset().await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(ResponseTemplate::new(503))
                .mount(&server)
                .await;
        }
        let error = tracker
            .check_governed_budget("sandbox", Some(1), Some(1), Some(&fixture.client))
            .await
            .unwrap_err();
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            fixture
                ._broker
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
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
async fn calendar_allowance_does_not_override_final_send_currency_limits() {
    let fixture = Fixture::new(1, false).await;
    {
        let mut ledger = fixture.ledger.lock().unwrap();
        ledger.limits.usd_micros = Some(1);
        ledger
            .nodes
            .get_mut("task-uid")
            .unwrap()
            .authority
            .limits
            .usd_micros = Some(1);
        ledger.validate().unwrap();
    }
    authorize(&fixture);
    let tracker = crate::budget::TokenBudgetTracker::new(1, 0);
    tracker.record_usage("task", 10).await;
    tracker
        .check_governed_budget("task", Some(1), Some(1), Some(&fixture.client))
        .await
        .unwrap();
    assert!(fixture.buffered().await.is_err());
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.settled.usd_micros, 0);
    assert_eq!(ledger.effective_limits().unwrap().usd_micros, Some(1));
    assert_eq!(ledger.meters.reserved.tokens, 0);
}
