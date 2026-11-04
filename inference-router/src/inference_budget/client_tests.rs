// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::inference_budget_contract::{
    AccountReference, BudgetScope, Limits, ResourceIdentity, RootIdentity, RootKind, TaskAuthority,
    TaskBudgetBinding,
    ledger::Ledger,
    tariffs::{MaximumPrice, ModelContract, OutputField},
};
use crate::{auth::WorkloadIdentityAuth, provider::ProviderKind, proxy::UpstreamConfig};
use axum::http::{HeaderMap, Method};
use futures::TryStreamExt;
use serde_json::json;
use std::sync::{Mutex as StdMutex, atomic::AtomicUsize};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[path = "unsupported_routes_tests.rs"]
mod unsupported_routes_tests;

#[path = "cancellation_tests.rs"]
mod cancellation_tests;

#[path = "denial_tests.rs"]
mod denial_tests;

#[path = "content_filter_tests.rs"]
mod content_filter_tests;

#[tokio::test]
async fn responses_contract_routing_matches_current_exact_route_without_reserving() {
    for operation in [Operation::ChatCompletions, Operation::Responses] {
        let fixture = Fixture::for_operation(100, false, operation).await;
        let upstream = fixture.upstream();
        assert_eq!(
            fixture
                .client
                .requires_responses_contract(&upstream)
                .await
                .unwrap(),
            operation == Operation::Responses
        );
        for field in ["provider", "endpoint", "model"] {
            let mut different = upstream.clone();
            match field {
                "provider" => different.provider = ProviderKind::AzureOpenAI,
                "endpoint" => different.endpoint.push_str("/other"),
                _ => different.deployment = "other".into(),
            }
            assert!(
                !fixture
                    .client
                    .requires_responses_contract(&different)
                    .await
                    .unwrap()
            );
        }
        let mut catalog = fixture.client.catalog().await.unwrap();
        if operation == Operation::Responses {
            let mut chat = catalog.contracts[0].clone();
            chat.id = "chat".into();
            chat.operation = Operation::ChatCompletions;
            chat.output_field = OutputField::Tokens;
            catalog.contracts.push(chat);
            fixture._broker.reset().await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "catalog": catalog, "moneyRequired": true
                })))
                .mount(&fixture._broker)
                .await;
            assert!(
                !fixture
                    .client
                    .requires_responses_contract(&upstream)
                    .await
                    .unwrap()
            );
            catalog.contracts.pop();
        }
        catalog.contracts[0].valid_until = "2020-01-01T00:00:00Z".into();
        fixture._broker.reset().await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "catalog": catalog, "moneyRequired": true
            })))
            .mount(&fixture._broker)
            .await;
        assert!(
            !fixture
                .client
                .requires_responses_contract(&upstream)
                .await
                .unwrap()
        );
        fixture._broker.reset().await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&fixture._broker)
            .await;
        assert!(
            fixture
                .client
                .requires_responses_contract(&upstream)
                .await
                .is_err()
        );
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.reserved.tokens, 0);
        assert_eq!(ledger.meters.settled.tokens, 0);
        assert_eq!(ledger.meters.uncertain.tokens, 0);
    }
}

#[tokio::test]
async fn models_endpoint_projects_governed_route_without_provider_requests() {
    use crate::inference_policy_loader::{LoadedInferencePolicy, ModelPreference, ModelRef};
    use tower::ServiceExt;

    for case in [
        "selected",
        "model-mismatch",
        "expired",
        "broker-unavailable",
        "record-only",
        "metadata-absent",
    ] {
        let recording = matches!(case, "record-only" | "metadata-absent");
        let fixture = if recording {
            Fixture::record_only(false, Operation::ChatCompletions).await
        } else {
            Fixture::new(100, false).await
        };
        if recording {
            Mock::given(wiremock::matchers::path("/v1/models"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(if case == "record-only" {
                        json!({"data":[{"id":"model","context_window":10,"max_output_tokens":20}]})
                    } else {
                        json!({"data":[{"id":"model"}]})
                    }),
                )
                .mount(&fixture.provider)
                .await;
        }
        let mut config = crate::config::Config::from_env().unwrap();
        config.default_model = "ambient-not-selected".into();
        config.azure_openai_endpoint = Some("http://127.0.0.1:1/unexpected-azure".into());
        config.ollama_endpoint = Some(fixture.provider.uri());
        let mut state = crate::routes::test_state(config);
        state.inference_budget = Some(fixture.client.clone());
        *state.inference_policy.write().await = Some(LoadedInferencePolicy {
            digest: "model-capacity-test".into(),
            source_path: "model-capacity-test".into(),
            per_request_tokens: None,
            daily_tokens: None,
            monthly_tokens: None,
            content_safety: Default::default(),
            model_preference: Some(ModelPreference {
                primary: ModelRef {
                    provider: "ollama".into(),
                    deployment: "model".into(),
                },
                fallback: vec![],
            }),
            provider: Some("ollama".into()),
            guardrails: vec![],
            raw: json!({}),
        });
        if case != "selected" && !recording {
            let mut catalog = fixture.client.catalog().await.unwrap();
            if case == "model-mismatch" {
                catalog.contracts[0].model = "other".into();
            }
            if case == "expired" {
                catalog.contracts[0].valid_until = "2020-01-01T00:00:00Z".into();
            }
            fixture._broker.reset().await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(if case == "broker-unavailable" {
                    ResponseTemplate::new(503)
                } else {
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"catalog": catalog, "moneyRequired": true}))
                })
                .mount(&fixture._broker)
                .await;
        }
        let app = axum::Router::new()
            .merge(crate::routes::inference_routes())
            .with_state(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/models")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            if matches!(case, "selected" | "record-only") {
                200
            } else {
                503
            },
            "{case}"
        );
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        if matches!(case, "selected" | "record-only") {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                json!({
                    "object": "list", "data": [{"id": "model", "object": "model", "owned_by": "kars",
                        "context_window": 10, "max_output_tokens": 20}],
                })
            );
        }
        assert_eq!(
            fixture.provider.received_requests().await.unwrap().len(),
            usize::from(recording)
        );
        assert!(
            fixture
                ._broker
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| r.url.path() == "/v1/catalog")
        );
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.reserved.tokens, 0);
        assert_eq!(ledger.meters.settled.tokens, 0);
        assert_eq!(ledger.meters.uncertain.tokens, 0);
    }
}

#[tokio::test]
async fn model_limits_match_current_exact_route_without_inference_or_reservation() {
    for operation in [
        Operation::ChatCompletions,
        Operation::Responses,
        Operation::AnthropicMessages,
    ] {
        let fixture = Fixture::for_operation(100, false, operation).await;
        let mut upstream = fixture.upstream();
        let mut catalog = fixture.client.catalog().await.unwrap();
        if operation == Operation::AnthropicMessages {
            upstream.provider = ProviderKind::Anthropic;
            catalog.contracts[0].provider_id = "anthropic".into();
            fixture._broker.reset().await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"catalog": catalog, "moneyRequired": true})),
                )
                .mount(&fixture._broker)
                .await;
        }
        assert_eq!(
            fixture.client.model_limits(&upstream).await.unwrap(),
            Some((10, 20))
        );
        for field in ["provider", "endpoint", "model"] {
            let mut different = upstream.clone();
            match field {
                "provider" => different.provider = ProviderKind::AzureOpenAI,
                "endpoint" => different.endpoint.push_str("/other"),
                _ => different.deployment = "other".into(),
            }
            assert!(fixture.client.model_limits(&different).await.is_err());
        }
        if operation == Operation::Responses {
            let mut chat = catalog.contracts[0].clone();
            chat.id = "chat".into();
            chat.operation = Operation::ChatCompletions;
            chat.output_field = OutputField::Tokens;
            chat.maximum_input_tokens = 7;
            chat.maximum_output_tokens = 9;
            catalog.contracts.push(chat);
            fixture._broker.reset().await;
            Mock::given(wiremock::matchers::method("POST"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"catalog": catalog, "moneyRequired": true})),
                )
                .mount(&fixture._broker)
                .await;
            assert_eq!(
                fixture.client.model_limits(&upstream).await.unwrap(),
                Some((7, 9))
            );
            catalog.contracts.pop();
        }
        catalog.contracts[0].valid_until = "2020-01-01T00:00:00Z".into();
        fixture._broker.reset().await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"catalog": catalog, "moneyRequired": true})),
            )
            .mount(&fixture._broker)
            .await;
        assert!(fixture.client.model_limits(&upstream).await.is_err());
        fixture._broker.reset().await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&fixture._broker)
            .await;
        assert!(fixture.client.model_limits(&upstream).await.is_err());
        assert!(
            fixture
                .provider
                .received_requests()
                .await
                .unwrap()
                .is_empty()
        );
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.reserved.tokens, 0);
        assert_eq!(ledger.meters.settled.tokens, 0);
        assert_eq!(ledger.meters.uncertain.tokens, 0);
    }
}

#[tokio::test]
async fn missing_usage_logs_safe_upstream_status_without_changing_liability() {
    use super::super::{calendar::tests::accounting_server, dispatch};
    use std::io::Write;
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct LogWriter(Arc<StdMutex<Vec<u8>>>);
    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let private = "PRIVATE_PROVIDER_CONTENT";
    let mut cases = Vec::new();
    for parameter in [
        "tool_choice",
        "tools",
        "parallel_tool_calls",
        "max_tokens",
        "max_completion_tokens",
        "messages",
        "model",
        "temperature",
        "top_p",
        "reasoning_effort",
        "response_format",
        "stream_options",
        private,
    ] {
        cases.push((
            400,
            json!({"error":{"param":parameter,"message":private}}).to_string(),
            if parameter == private {
                "other"
            } else {
                parameter
            },
        ));
    }
    cases.extend([
        (
            503,
            json!({"error":{"param":"tools","message":private}}).to_string(),
            "tools",
        ),
        (
            200,
            json!({"error":{"param":"tools","message":private}}).to_string(),
            "unavailable",
        ),
        (400, private.to_owned(), "unavailable"),
        (
            400,
            json!({"error":{"param":[private],"message":private}}).to_string(),
            "unavailable",
        ),
        (
            400,
            json!({"error":{"param":"tools","message":"x".repeat(65_536)}}).to_string(),
            "unavailable",
        ),
    ]);
    for (status, body, parameter) in cases {
        let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
        let (_server, calendar) = accounting_server().await;
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = Some(calendar.clone());
        let (_, attempt) = dispatch::begin(
            &upstream,
            &Method::POST,
            "/responses",
            bytes::Bytes::from_static(br#"{"input":"test"}"#),
        )
        .await
        .unwrap();
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let writer = LogWriter(captured.clone());
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        let status_code = axum::http::StatusCode::from_u16(status).unwrap();
        let result = dispatch::finish(attempt, status_code, body.as_bytes())
            .with_subscriber(subscriber)
            .await;
        if crate::proxy::failure::retryable_rejection(status_code) {
            result.unwrap();
        } else {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("maximum charge retained"));
            assert!(!crate::proxy::failure::retryable_failure(&error));
        }
        let log = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(!log.contains(private));
        let event: serde_json::Value = log
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| event["fields"]["upstream_status"] == status)
            .unwrap();
        assert_eq!(event["fields"]["provider_error_parameter"], parameter);
        assert_eq!(event["fields"]["rejected_before_inference"], false);
        assert_eq!(calendar.read().await.unwrap().usage.daily, 30);
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.uncertain.tokens, 30);
        assert_eq!(ledger.meters.settled.tokens, 0);
        assert_eq!(ledger.meters.reserved.tokens, 0);
    }
}

#[tokio::test]
async fn calendar_closes_timeout_only_at_acknowledged_lifetime_maximum() {
    use super::super::{calendar::tests::accounting_server, dispatch};
    for limit in [30, 100] {
        let fixture = Fixture::for_operation(limit, false, Operation::Responses).await;
        let (_server, calendar) = accounting_server().await;
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = Some(calendar.clone());
        let (_, attempt) = dispatch::begin(
            &upstream,
            &Method::POST,
            "/responses",
            bytes::Bytes::from_static(br#"{"input":"test"}"#),
        )
        .await
        .unwrap();
        let error = dispatch::finish(attempt, axum::http::StatusCode::REQUEST_TIMEOUT, b"")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("maximum charge retained"));
        let usage = calendar.read().await.unwrap().usage;
        assert_eq!((usage.daily, usage.monthly), (30, 30));
        {
            let ledger = fixture.ledger.lock().unwrap();
            assert_eq!(ledger.meters.uncertain.tokens, 30);
            assert_eq!(ledger.meters.settled.tokens, 0);
            assert_eq!(ledger.meters.reserved.tokens, 0);
        }
        let next = dispatch::begin(
            &upstream,
            &Method::POST,
            "/responses",
            bytes::Bytes::from_static(br#"{"input":"test"}"#),
        )
        .await;
        if limit == 30 {
            assert!(
                next.is_err(),
                "calendar recovery must not reset lifetime funding"
            );
            assert_eq!(calendar.read().await.unwrap().usage.daily, 30);
        } else {
            let (_, attempt) = next.unwrap();
            dispatch::finish(
                attempt,
                axum::http::StatusCode::OK,
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
}

#[tokio::test]
async fn calendar_conservative_charge_does_not_make_incomplete_streams_successful() {
    for (name, wire) in super::super::anthropic_cases::incomplete() {
        let fixture = Fixture::for_operation(100, false, Operation::AnthropicMessages).await;
        let (_server, calendar) = super::super::calendar::tests::accounting_server().await;
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = Some(calendar.clone());
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(wire, "text/event-stream"))
            .mount(&fixture.provider)
            .await;
        let (_, _, stream) = crate::proxy::forward_stream(
            Arc::new(WorkloadIdentityAuth::new()),
            None,
            reqwest::Client::new(),
            upstream,
            "v1/messages",
            HeaderMap::new(),
            bytes::Bytes::from_static(
                br#"{"stream":true,"messages":[{"role":"user","content":"text"}]}"#,
            ),
        )
        .await
        .unwrap();
        assert!(stream.try_collect::<Vec<_>>().await.is_err(), "{name}");
        assert_eq!(calendar.read().await.unwrap().usage.daily, 30, "{name}");
        assert_eq!(
            fixture.ledger.lock().unwrap().meters.uncertain.tokens,
            30,
            "{name}"
        );
    }
}

#[tokio::test]
async fn calendar_retains_pending_when_lifetime_settlement_is_not_proven() {
    use super::super::{calendar::tests::accounting_server, dispatch};
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
    ] {
        let fixture = Fixture::for_operation(100, false, Operation::Responses).await;
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
            "/responses",
            bytes::Bytes::from_static(br#"{"input":"test"}"#),
        )
        .await
        .unwrap();
        let error = dispatch::finish(attempt, axum::http::StatusCode::REQUEST_TIMEOUT, b"")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("pending liability retained"));
        assert!(calendar.read().await.is_err());
        assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
    }
}

#[tokio::test]
async fn native_stream_settlement_never_refunds_missing_or_inconsistent_final_usage() {
    for (name, wire) in super::super::anthropic_cases::incomplete() {
        let fixture = Fixture::for_operation(100, false, Operation::AnthropicMessages).await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(wire, "text/event-stream"))
            .mount(&fixture.provider)
            .await;
        let (_, _, stream) = crate::proxy::forward_stream(
            Arc::new(WorkloadIdentityAuth::new()),
            None,
            reqwest::Client::new(),
            fixture.upstream(),
            "v1/messages",
            HeaderMap::new(),
            bytes::Bytes::from_static(
                br#"{"stream":true,"messages":[{"role":"user","content":"text"}]}"#,
            ),
        )
        .await
        .unwrap();
        let _: Vec<_> = stream.try_collect().await.unwrap();
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.uncertain.tokens, 30, "{name}");
        assert_eq!(ledger.meters.uncertain.usd_micros, 30, "{name}");
        assert_eq!(ledger.meters.settled.tokens, 0, "{name}");
        assert_eq!(ledger.meters.reserved.tokens, 0, "{name}");
    }
}

#[tokio::test]
async fn native_stream_with_final_usage_settles_exact_evidence_once() {
    let fixture = Fixture::for_operation(100, false, Operation::AnthropicMessages).await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            super::super::anthropic_cases::complete(),
            "text/event-stream",
        ))
        .mount(&fixture.provider)
        .await;
    let (_, _, stream) = crate::proxy::forward_stream(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        reqwest::Client::new(),
        fixture.upstream(),
        "v1/messages",
        HeaderMap::new(),
        bytes::Bytes::from_static(
            br#"{"stream":true,"messages":[{"role":"user","content":"text"}]}"#,
        ),
    )
    .await
    .unwrap();
    let _: Vec<_> = stream.try_collect().await.unwrap();
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.settled.tokens, 9);
    assert_eq!(ledger.meters.settled.usd_micros, 9);
    assert_eq!(ledger.meters.uncertain.tokens, 0);
    assert_eq!(ledger.meters.reserved.tokens, 0);
}

#[tokio::test]
async fn finite_fence_cannot_be_bypassed_by_disabling_blocklist_or_enabling_learning() {
    let fixture = Fixture::new(100, false).await;
    let blocklist = crate::blocklist::Blocklist::disabled();
    assert!(blocklist.opaque_proxy_allowed());
    blocklist
        .bind_inference_budget(fixture.client.clone(), vec![])
        .unwrap();
    assert!(!blocklist.opaque_proxy_allowed());
    blocklist.set_learn_mode(true);
    assert!(!blocklist.opaque_proxy_allowed());
    assert!(
        blocklist
            .check_egress("unknown-model.example", "forged-agent-header")
            .await
            .is_err()
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

#[tokio::test]
async fn reviewed_egress_works_without_a_second_catalog_allowlist_and_revocation_closes_it() {
    let fixture = Fixture::new(100, false).await;
    let blocklist = crate::blocklist::Blocklist::new(None).await;
    blocklist
        .bind_inference_budget(fixture.client.clone(), vec![])
        .unwrap();
    let target = "https://docs.vendor.example/pricing";
    assert!(blocklist.check_egress(target, "sandbox").await.is_err());
    blocklist
        .replace_allowlist(vec!["vendor.example".into()])
        .await;
    assert!(blocklist.check_egress(target, "sandbox").await.is_ok());
    assert!(
        blocklist
            .check_egress("https://unreviewed.example", "sandbox")
            .await
            .is_err()
    );
    assert!(!blocklist.opaque_proxy_allowed());
    blocklist.replace_allowlist(vec![]).await;
    assert!(blocklist.check_egress(target, "sandbox").await.is_err());
    blocklist.set_learn_mode(true);
    assert!(blocklist.check_egress(target, "sandbox").await.is_err());
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
async fn reviewed_egress_cannot_override_model_routes_or_threat_denials() {
    let fixture = Fixture::new(100, false).await;
    let blocklist = crate::blocklist::Blocklist::new(None).await;
    blocklist
        .bind_inference_budget(fixture.client.clone(), vec!["api.vendor.example".into()])
        .unwrap();
    let catalog_host = reqwest::Url::parse(&fixture.provider.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_owned();
    blocklist
        .replace_allowlist(vec![
            "vendor.example".into(),
            "blocked.top".into(),
            catalog_host,
        ])
        .await;
    for learning in [false, true] {
        blocklist.set_learn_mode(learning);
        for target in [
            "https://api.vendor.example",
            "https://API.VENDOR.EXAMPLE./",
            "https://blocked.top",
            &fixture.provider.uri(),
        ] {
            assert!(
                blocklist.check_egress(target, "sandbox").await.is_err(),
                "{target}"
            );
        }
    }
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
async fn reviewed_egress_fails_closed_when_the_budget_catalog_is_unavailable() {
    let fixture = Fixture::new(100, false).await;
    fixture._broker.reset().await;
    let blocklist = crate::blocklist::Blocklist::disabled();
    blocklist
        .bind_inference_budget(fixture.client.clone(), vec![])
        .unwrap();
    blocklist
        .replace_allowlist(vec!["vendor.example".into()])
        .await;
    assert!(
        blocklist
            .check_egress("https://vendor.example", "sandbox")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn github_service_authority_does_not_open_ordinary_egress_or_other_origins() {
    let fixture = Fixture::new(100, false).await;
    let blocklist = crate::blocklist::Blocklist::new(None).await;
    blocklist
        .bind_inference_budget(fixture.client.clone(), vec![])
        .unwrap();
    for url in [
        "https://api.github.com",
        "https://api.github.com/repos/owner/repo/issues",
        "https://github.com/owner/repo.git/info/refs?service=git-upload-pack",
    ] {
        assert!(
            blocklist
                .check_github_service_egress(url, "sandbox")
                .await
                .is_ok()
        );
        assert!(blocklist.check_egress(url, "sandbox").await.is_err());
    }
    for url in [
        "http://api.github.com/repos/owner/repo",
        "https://api.github.com.evil/repos/owner/repo",
        "https://api.github.com:444/repos/owner/repo",
        "https://api.github.com@evil/repos/owner/repo",
        "https://github.com.evil/owner/repo.git",
        "https://signed.blob.core.windows.net/log",
    ] {
        assert!(
            blocklist
                .check_github_service_egress(url, "sandbox")
                .await
                .is_err()
        );
    }
    assert!(!blocklist.opaque_proxy_allowed());
    assert!(blocklist.get_allowlist().await.is_empty());
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
async fn github_service_authority_preserves_model_and_unavailable_catalog_denials() {
    let fixture = Fixture::new(100, false).await;
    let blocklist = crate::blocklist::Blocklist::disabled();
    blocklist
        .bind_inference_budget(fixture.client.clone(), vec!["api.github.com".into()])
        .unwrap();
    assert!(
        blocklist
            .check_github_service_egress("https://api.github.com/repos/owner/repo", "sandbox")
            .await
            .is_err()
    );
    fixture._broker.reset().await;
    let unavailable = crate::blocklist::Blocklist::disabled();
    unavailable
        .bind_inference_budget(fixture.client.clone(), vec![])
        .unwrap();
    assert!(
        unavailable
            .check_github_service_egress("https://api.github.com/repos/owner/repo", "sandbox")
            .await
            .is_err()
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

#[derive(Clone)]
struct BrokerFixture {
    ledger: Arc<StdMutex<Ledger>>,
    catalog: Catalog,
    lose_begin_ack: bool,
}

impl Respond for BrokerFixture {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer private-test-token"
        );
        let now = chrono::Utc::now().timestamp();
        let mut ledger = self.ledger.lock().unwrap();
        let result = match request.url.path() {
            "/v1/catalog" => {
                let request: BrokerRequest<SessionRequest> =
                    serde_json::from_slice(&request.body).unwrap();
                Ok(json!({"catalog":self.catalog,
                    "moneyRequired":ledger.requires_price(&request.payload.identity.task_uid).unwrap(),
                    "recordOnlyAllowed":ledger.allows_unbounded_inference(&request.payload.identity, now).unwrap_or(false),
                    "unboundedTokens":ledger.unbounded_tokens.is_some()}))
            }
            "/v1/session" => {
                let request: BrokerRequest<SessionRequest> =
                    serde_json::from_slice(&request.body).unwrap();
                ledger
                    .register_session(request.payload.identity)
                    .map(|change| {
                        *ledger = change.next;
                        json!(change.value)
                    })
            }
            "/v1/reserve" => {
                let request: BrokerRequest<ReserveRequest> =
                    serde_json::from_slice(&request.body).unwrap();
                if let Some(quote) = request.payload.quote.bounded() {
                    self.catalog
                        .accepts_quote(
                            quote,
                            now,
                            ledger
                                .requires_price(&request.payload.identity.task_uid)
                                .unwrap(),
                        )
                        .unwrap();
                }
                ledger.reserve(&request.payload, now).map(|change| {
                    *ledger = change.next;
                    json!(change.value)
                })
            }
            "/v1/begin" => {
                let request: BrokerRequest<AttemptCommand> =
                    serde_json::from_slice(&request.body).unwrap();
                ledger.begin_dispatch(&request.payload, now).map(|change| {
                    *ledger = change.next;
                    json!(change.value)
                })
            }
            "/v1/settle" => {
                let request: BrokerRequest<Settlement> =
                    serde_json::from_slice(&request.body).unwrap();
                ledger.settle(&request.payload).map(|change| {
                    *ledger = change.next;
                    json!(change.value)
                })
            }
            _ => panic!("unexpected broker route"),
        };
        if self.lose_begin_ack && request.url.path() == "/v1/begin" {
            return ResponseTemplate::new(503);
        }
        match result {
            Ok(response) => ResponseTemplate::new(200).set_body_json(response),
            Err(error) => {
                let (status, code) = match error {
                    crate::inference_budget_contract::BudgetError::Exhausted => {
                        (429, "inference_budget_exhausted")
                    }
                    crate::inference_budget_contract::BudgetError::Capacity => {
                        (503, "inference_budget_capacity")
                    }
                    _ => (503, "inference_budget_unavailable"),
                };
                ResponseTemplate::new(status)
                    .set_body_json(json!({"error":{"code":code,"message":error.to_string()}}))
            }
        }
    }
}

struct Fixture {
    provider: MockServer,
    _broker: MockServer,
    client: Arc<Client>,
    ledger: Arc<StdMutex<Ledger>>,
    directory: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

impl Fixture {
    async fn new(limit: u64, lose_begin_ack: bool) -> Self {
        Self::for_operation(limit, lose_begin_ack, Operation::ChatCompletions).await
    }

    async fn for_operation(limit: u64, lose_begin_ack: bool, operation: Operation) -> Self {
        Self::with_limits(
            Limits {
                tokens: Some(limit),
                usd_micros: Some(1000),
            },
            lose_begin_ack,
            operation,
            false,
        )
        .await
    }

    async fn record_only(lose_begin_ack: bool, operation: Operation) -> Self {
        Self::with_limits(Limits::default(), lose_begin_ack, operation, true).await
    }

    async fn with_limits(
        limits: Limits,
        lose_begin_ack: bool,
        operation: Operation,
        no_contracts: bool,
    ) -> Self {
        let provider = MockServer::start().await;
        let broker = MockServer::start().await;
        let task = ResourceIdentity {
            namespace: "workspace".into(),
            name: "task".into(),
            uid: "task-uid".into(),
        };
        let root = RootIdentity {
            kind: RootKind::KarsTask,
            resource: task.clone(),
            workspace_uid: "workspace-uid".into(),
            cluster_uid: "cluster-uid".into(),
        };
        let digest = format!("sha256:{}", "a".repeat(64));
        let authority = TaskAuthority {
            task,
            parent_uid: None,
            root_task_uid: "task-uid".into(),
            authorization_digest: digest.clone(),
            effective_authorization: json!({"test":true}),
            limits,
        };
        let ledger = Ledger::new("account-uid".into(), root.clone(), authority.limits)
            .unwrap()
            .register_task(authority)
            .unwrap()
            .next;
        let ledger = Arc::new(StdMutex::new(ledger));
        let model = ModelContract {
            id: "model".into(),
            version: "v1".into(),
            valid_until: "2030-01-01T00:00:00Z".into(),
            provider_id: "ollama".into(),
            endpoint: provider.uri(),
            model: "model".into(),
            operation,
            output_field: if operation == Operation::Responses {
                OutputField::Output
            } else {
                OutputField::Tokens
            },
            maximum_input_tokens: 10,
            maximum_output_tokens: 20,
            maximum_wire_bytes: 4096,
            output_bound_includes_reasoning: true,
            maximum_price: Some(if operation == Operation::AnthropicMessages {
                MaximumPrice::TokenRates {
                    input_micros_per_million: 1_000_000,
                    output_micros_per_million: 1_000_000,
                    fixed_micros: 0,
                }
            } else {
                MaximumPrice::PerRequest { maximum_micros: 5 }
            }),
        };
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(BrokerFixture {
                ledger: ledger.clone(),
                catalog: Catalog {
                    version: "v1".into(),
                    contracts: if no_contracts { vec![] } else { vec![model] },
                    non_inference_egress_hosts: vec![],
                },
                lose_begin_ack,
            })
            .mount(&broker)
            .await;
        let identity = ExecutionIdentity {
            task_uid: "task-uid".into(),
            authorization_digest: digest.clone(),
            sandbox: ResourceIdentity {
                namespace: "workspace".into(),
                name: "task".into(),
                uid: "sandbox-uid".into(),
            },
            runtime_namespace_uid: "runtime-uid".into(),
            pod_name: "pod".into(),
            pod_uid: "pod-uid".into(),
        };
        let directory = std::env::temp_dir().join(format!(
            ".budget-client-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory).unwrap();
        let token_path = directory.join("token");
        std::fs::write(&token_path, "private-test-token").unwrap();
        let client = Arc::new(Client {
            binding: RouterBinding {
                task: TaskBudgetBinding {
                    scope: BudgetScope::GovernedInference,
                    account: AccountReference {
                        namespace: "accounting".into(),
                        name: "account".into(),
                        uid: "account-uid".into(),
                    },
                    root,
                    task_uid: "task-uid".into(),
                    parent_task_uid: None,
                    root_task_uid: "task-uid".into(),
                    authorization_digest: digest,
                },
                sandbox: identity.sandbox.clone(),
                runtime_namespace: "kars-task".into(),
                runtime_namespace_uid: "runtime-uid".into(),
                privacy_epoch: None,
            },
            identity,
            endpoint: broker.uri(),
            token_path,
            http: reqwest::Client::new(),
            reservation_lock: Mutex::new(()),
        });
        Self {
            provider,
            _broker: broker,
            client,
            ledger,
            directory,
        }
    }

    fn upstream(&self) -> UpstreamConfig {
        let mut upstream =
            UpstreamConfig::azure(self.provider.uri(), "model".into(), "task".into());
        upstream.provider = ProviderKind::Ollama;
        upstream.inference_budget = Some(self.client.clone());
        upstream
    }

    async fn buffered(&self) -> anyhow::Result<(axum::http::StatusCode, HeaderMap, bytes::Bytes)> {
        crate::proxy::forward(
            &WorkloadIdentityAuth::new(),
            None,
            &reqwest::Client::new(),
            &self.upstream(),
            Method::POST,
            "chat/completions",
            &HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"text"}]}"#),
        )
        .await
    }
}

#[tokio::test]
async fn reserve_contention_replay_preserves_ledger_bindings_and_fences() {
    use crate::inference_budget_contract::{AttemptPhase, BudgetError};

    for recording in [false, true] {
        let fixture = if recording {
            Fixture::record_only(false, Operation::ChatCompletions).await
        } else {
            Fixture::new(100, false).await
        };
        let now = chrono::Utc::now().timestamp();
        let quote = if recording {
            AttemptQuote::record_only(
                "ollama",
                &fixture.provider.uri(),
                "model",
                Operation::ChatCompletions,
            )
        } else {
            let catalog = fixture.client.catalog().await.unwrap();
            catalog.contracts[0]
                .normalize(
                    br#"{"messages":[{"role":"user","content":"text"}]}"#,
                    now,
                    true,
                )
                .unwrap()
                .1
                .into()
        };
        let ledger = fixture
            .ledger
            .lock()
            .unwrap()
            .clone()
            .register_session(fixture.client.identity.clone())
            .unwrap()
            .next;
        let request = ReserveRequest {
            account_uid: fixture.client.binding.task.account.uid.clone(),
            identity: fixture.client.identity.clone(),
            sequence: ledger.sessions[&fixture.client.identity.pod_uid].next_sequence,
            wire_digest: format!("sha256:{}", "a".repeat(64)),
            quote,
        };
        let reserved = ledger.reserve(&request, now).unwrap();
        let ledger = reserved.next;
        let replay = ledger.reserve(&request, now + 1).unwrap();
        assert!(!replay.changed);
        assert_eq!(replay.next, ledger);
        assert_eq!(replay.value, reserved.value);
        for field in [
            "account",
            "pod",
            "authorization",
            "namespace",
            "wire",
            "quote",
            "sequence",
        ] {
            let mut changed = request.clone();
            match field {
                "account" => changed.account_uid = "other".into(),
                "pod" => changed.identity.pod_uid = "other".into(),
                "authorization" => {
                    changed.identity.authorization_digest = format!("sha256:{}", "b".repeat(64))
                }
                "namespace" => changed.identity.runtime_namespace_uid = "other".into(),
                "wire" => changed.wire_digest = format!("sha256:{}", "b".repeat(64)),
                "quote" => {
                    changed.quote = AttemptQuote::record_only(
                        "ollama",
                        &fixture.provider.uri(),
                        "other",
                        Operation::ChatCompletions,
                    )
                }
                _ => changed.sequence = 0,
            }
            assert!(ledger.reserve(&changed, now + 1).is_err(), "{field}");
        }
        let command = AttemptCommand {
            account_uid: request.account_uid.clone(),
            identity: request.identity.clone(),
            key: reserved.value.key.clone(),
            wire_digest: request.wire_digest.clone(),
        };
        let expired = ledger
            .expire_undispatched(reserved.value.expires_at)
            .unwrap()
            .next;
        assert!(matches!(
            expired.reserve(&request, now + 1),
            Err(BudgetError::Sequence)
        ));
        assert!(matches!(
            ledger.begin_dispatch(&command, reserved.value.expires_at),
            Err(BudgetError::Expired)
        ));
        let closed = ledger
            .close_subtree(&request.identity.task_uid)
            .unwrap()
            .next;
        assert!(closed.sessions[&request.identity.pod_uid].closed);
        assert!(closed.reserve(&request, now + 1).is_err());
        let inflight = ledger.begin_dispatch(&command, now + 1).unwrap().next;
        assert_eq!(
            inflight.reserve(&request, now + 1).unwrap().value.phase,
            AttemptPhase::InFlight
        );
        assert!(matches!(
            inflight.begin_dispatch(&command, now + 1),
            Err(BudgetError::AlreadyDispatched)
        ));
        let settled = inflight
            .settle(&Settlement {
                attempt: command,
                usage: Some(Usage {
                    input_tokens: 3,
                    output_tokens: 5,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    reasoning_output_tokens: 0,
                }),
            })
            .unwrap()
            .next;
        assert!(matches!(
            settled.reserve(&request, now + 1),
            Err(BudgetError::Sequence)
        ));
        assert_eq!(settled.meters.settled.tokens, 8);
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
async fn reserve_contention_refreshes_projected_token_with_the_same_envelope() {
    let fixture = Fixture::record_only(false, Operation::Responses).await;
    let token_path = fixture.client.token_path.clone();
    let calls = AtomicUsize::new(0);
    Mock::given(wiremock::matchers::path("/v1/reserve"))
        .respond_with(move |request: &Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                std::fs::write(&token_path, "rotated-test-token").unwrap();
                ResponseTemplate::new(503).set_body_json(json!({
                    "error":{"code":"inference_budget_contention"}
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
    let request = ReserveRequest {
        account_uid: fixture.client.binding.task.account.uid.clone(),
        identity: fixture.client.identity.clone(),
        sequence: 1,
        wire_digest: format!("sha256:{}", "a".repeat(64)),
        quote: AttemptQuote::record_only(
            "ollama",
            &fixture.provider.uri(),
            "model",
            Operation::Responses,
        ),
    };
    let result: serde_json::Value = fixture.client.rpc("/v1/reserve", request).await.unwrap();
    assert_eq!(result, json!({"recovered":true}));
    let requests = fixture._broker.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
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
async fn record_only_dispatch_records_actual_usage_without_model_contract_or_injected_cap() {
    let fixture = Fixture::record_only(false, Operation::ChatCompletions).await;
    fixture.client.ready_for(&fixture.upstream()).await.unwrap();
    assert_eq!(
        fixture
            .client
            .model_limits(&fixture.upstream())
            .await
            .unwrap(),
        None
    );
    assert!(
        !fixture
            .client
            .requires_responses_contract(&fixture.upstream())
            .await
            .unwrap()
    );
    Mock::given(wiremock::matchers::path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "usage":{"prompt_tokens":1234,"completion_tokens":567,"total_tokens":1801}
        })))
        .mount(&fixture.provider)
        .await;
    assert!(fixture.buffered().await.unwrap().0.is_success());
    let sent = fixture.provider.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert!(body.get("max_tokens").is_none());
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.settled.tokens, 1801);
    assert_eq!(ledger.meters.reserved.tokens, 0);
    assert_eq!(ledger.meters.unknown_usage_attempts, 0);
    assert_eq!(ledger.meters.unpriced_attempts, 1);
}

#[tokio::test]
async fn record_only_missing_usage_retains_unknown_instead_of_zero_charge() {
    for mode in ["none", "enforced", "record-only", "cancelled"] {
        let fixture = Fixture::record_only(false, Operation::Responses).await;
        let calendar = match mode {
            "enforced" => Some(super::super::calendar::tests::accounting_server().await),
            "record-only" | "cancelled" => {
                Some(super::super::calendar::tests::record_only_server().await)
            }
            _ => None,
        };
        let mut upstream = fixture.upstream();
        upstream.calendar_budget = calendar.as_ref().map(|(_, client)| client.clone());
        let (_, attempt) = super::super::dispatch::begin(
            &upstream,
            &Method::POST,
            "/responses",
            bytes::Bytes::from_static(br#"{"input":"test"}"#),
        )
        .await
        .unwrap();
        if mode == "cancelled" {
            drop(attempt);
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if fixture.ledger.lock().unwrap().meters.unknown_usage_attempts == 1 {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        } else {
            assert!(
                super::super::dispatch::finish(attempt, axum::http::StatusCode::OK, b"{}")
                    .await
                    .is_err()
            );
        }
        let ledger = fixture.ledger.lock().unwrap();
        assert_eq!(ledger.meters.settled.tokens, 0, "{mode}");
        assert_eq!(ledger.meters.uncertain.tokens, 0, "{mode}");
        assert_eq!(ledger.meters.unknown_usage_attempts, 1, "{mode}");
        assert_eq!(ledger.meters.unpriced_attempts, 1, "{mode}");
    }
}

#[tokio::test]
async fn record_only_preserves_explicit_provider_refusals_without_erasing_unknown_usage() {
    for with_calendar in [false, true] {
        for code in ["unsupported_api_for_model", "content_filter"] {
            let fixture = Fixture::record_only(false, Operation::ChatCompletions).await;
            let (_server, calendar) = super::super::calendar::tests::record_only_server().await;
            let mut upstream = fixture.upstream();
            upstream.calendar_budget = with_calendar.then_some(calendar);
            Mock::given(wiremock::matchers::path("/v1/chat/completions"))
                .respond_with(
                    ResponseTemplate::new(400).set_body_json(json!({"error":{"code":code}})),
                )
                .mount(&fixture.provider)
                .await;
            let response = crate::proxy::forward(
                &WorkloadIdentityAuth::new(),
                None,
                &reqwest::Client::new(),
                &upstream,
                Method::POST,
                "chat/completions",
                &HeaderMap::new(),
                bytes::Bytes::from_static(br#"{"messages":[{"role":"user","content":"text"}]}"#),
            )
            .await
            .unwrap();
            assert_eq!(response.0, axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&response.2).unwrap()["error"]["code"],
                code
            );
            assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
            let ledger = fixture.ledger.lock().unwrap();
            assert_eq!(ledger.meters.unknown_usage_attempts, 1);
            assert_eq!(ledger.meters.settled.tokens, 0);
            assert_eq!(ledger.meters.unpriced_attempts, 1);
        }
    }
}

#[test]
fn record_only_normalization_preserves_protocol_caps_and_rejects_unsupported_requests() {
    use crate::inference_budget_dispatch::normalize_record_only;
    let normalize = |operation, body: serde_json::Value| {
        normalize_record_only(
            "provider",
            "https://models.example",
            "model",
            operation,
            &serde_json::to_vec(&body).unwrap(),
            1,
        )
    };
    for (operation, body, key) in [
        (
            Operation::ChatCompletions,
            json!({"messages":[{"role":"user","content":"text"}],"max_completion_tokens":7}),
            "max_completion_tokens",
        ),
        (
            Operation::Responses,
            json!({"input":"text","max_output_tokens":7}),
            "max_output_tokens",
        ),
        (
            Operation::AnthropicMessages,
            json!({"messages":[{"role":"user","content":"text"}],"max_tokens":7}),
            "max_tokens",
        ),
    ] {
        let (bytes, quote) = normalize(operation, body).unwrap();
        assert!(quote.is_record_only());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()[key],
            7
        );
    }
    for body in [
        json!({"input":"text","model":"different"}),
        json!({"input":"text","max_output_tokens":0}),
        json!({"input":"text","background":true}),
        json!({"input":"text","tools":[{"type":"web_search"}]}),
        json!({"input":"x".repeat(1_048_576)}),
    ] {
        assert!(normalize(Operation::Responses, body).is_err());
    }
    assert!(normalize(Operation::ChatCompletions, json!({"messages":[{"role":"user","content":"text"}],"max_tokens":7,"max_completion_tokens":7})).is_err());
}

#[tokio::test]
async fn record_only_lost_begin_ack_never_dispatches() {
    let fixture = Fixture::record_only(true, Operation::ChatCompletions).await;
    assert!(fixture.buffered().await.is_err());
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.ledger.lock().unwrap().attempts.len(), 1);
}

#[tokio::test]
async fn record_only_model_capacity_comes_from_exact_provider_metadata() {
    let fixture = Fixture::record_only(false, Operation::ChatCompletions).await;
    Mock::given(wiremock::matchers::path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[
            {"id":"other","context_window":1,"max_output_tokens":2},
            {"id":"model","capabilities":{"limits":{"max_context_window_tokens":128000,"max_output_tokens":16000}}}
        ]}))).mount(&fixture.provider).await;
    assert_eq!(
        crate::proxy::model_capacity(
            &WorkloadIdentityAuth::new(),
            None,
            &reqwest::Client::new(),
            &fixture.upstream()
        )
        .await
        .unwrap(),
        (128000, 16000)
    );
    assert!(fixture.ledger.lock().unwrap().attempts.is_empty());
}

#[tokio::test]
async fn record_only_inconsistent_catalog_authority_fails_closed() {
    let fixture = Fixture::record_only(false, Operation::ChatCompletions).await;
    fixture._broker.reset().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalog":{"version":"v1","contracts":[]},"moneyRequired":true,"recordOnlyAllowed":true
        })))
        .mount(&fixture._broker)
        .await;
    assert!(fixture.client.ready_for(&fixture.upstream()).await.is_err());
    assert!(fixture.buffered().await.is_err());
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
async fn actual_buffered_send_injects_output_bound_and_settles_before_returning() {
    let fixture = Fixture::new(100, false).await;
    Mock::given(wiremock::matchers::path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "usage":{"prompt_tokens":3,"completion_tokens":5,"total_tokens":8}
        })))
        .mount(&fixture.provider)
        .await;
    assert!(fixture.buffered().await.unwrap().0.is_success());
    let sent = fixture.provider.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body["max_tokens"], 20);
    let ledger = fixture.ledger.lock().unwrap();
    assert_eq!(ledger.meters.reserved.tokens, 0);
    assert_eq!(ledger.meters.settled.tokens, 8);
    assert_eq!(ledger.meters.settled.usd_micros, 5);
}

#[tokio::test]
async fn unknown_usage_consumes_full_maximum_and_second_send_is_denied_without_health_failover() {
    let fixture = Fixture::new(50, false).await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[]})))
        .mount(&fixture.provider)
        .await;
    fixture.buffered().await.unwrap();
    assert_eq!(fixture.ledger.lock().unwrap().meters.uncertain.tokens, 30);
    let error = fixture.buffered().await.err().unwrap();
    assert!(!crate::proxy::failure::retryable_failure(&error));
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn lost_begin_ack_funds_uncertain_work_but_never_sends_or_regrants_that_attempt() {
    let fixture = Fixture::new(50, true).await;
    assert!(fixture.buffered().await.is_err());
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 0);
    assert_eq!(fixture.ledger.lock().unwrap().meters.reserved.tokens, 30);
    assert!(fixture.buffered().await.is_err());
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn actual_stream_eof_requires_terminal_usage_and_charges_output_once() {
    let fixture = Fixture::new(100, false).await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5}}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&fixture.provider)
        .await;
    let (_, _, stream) = crate::proxy::forward_stream(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        reqwest::Client::new(),
        fixture.upstream(),
        "chat/completions",
        HeaderMap::new(),
        bytes::Bytes::from_static(
            br#"{"stream":true,"messages":[{"role":"user","content":"text"}]}"#,
        ),
    )
    .await
    .unwrap();
    let _: Vec<_> = stream.try_collect().await.unwrap();
    assert_eq!(fixture.ledger.lock().unwrap().meters.settled.tokens, 8);
}
