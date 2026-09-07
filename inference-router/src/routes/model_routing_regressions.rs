// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    auth::WorkloadIdentityAuth,
    config::{Config, ProviderEndpoint},
    inference_policy_loader::{LoadedInferencePolicy, ModelPreference, ModelRef},
    proxy::failure::{Acceptance, FailureCategory, ForwardFailure},
};
use axum::{Router, body::Body, http::Request};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method},
};

fn config(default: &str, providers: &[(&str, &str, Option<&str>)]) -> Config {
    let mut config = Config::from_env().unwrap();
    config.azure_openai_endpoint = Some(default.into());
    config.default_model = "true-default-model".into();
    config.content_safety_enabled = true;
    config.prompt_shields_enabled = true;
    config.providers = providers
        .iter()
        .map(|(id, endpoint, credential)| {
            (
                id.to_string(),
                ProviderEndpoint {
                    tag: id.to_string(),
                    endpoint: endpoint.to_string(),
                    api_key: credential.map(str::to_string),
                },
            )
        })
        .collect();
    config
}

fn policy(provider: &str, model: &str) -> InferencePolicySnapshot {
    InferencePolicySnapshot {
        model_preference: Some(ModelPreference {
            primary: ModelRef {
                provider: provider.into(),
                deployment: model.into(),
            },
            fallback: vec![],
        }),
        ..Default::default()
    }
}

fn state(config: Config) -> AppState {
    let mut state = super::tests::test_state(config);
    state.auth = Arc::new(WorkloadIdentityAuth::for_test(
        Some("ambient-default-key"),
        None,
    ));
    state.copilot = Arc::new(crate::copilot_auth::CopilotTokenCache::with_test_exchange(
        "test-default-seat",
        "http://127.0.0.1:1/unexpected-exchange".into(),
    ));
    state.client = reqwest::Client::builder().no_proxy().build().unwrap();
    state.budget = crate::budget::TokenBudgetTracker::new(1_000_000, 1_000_000);
    state
}

async fn router(state: AppState, policy: &InferencePolicySnapshot) -> Router {
    *state.inference_policy.write().await = Some(LoadedInferencePolicy {
        digest: "routing-regression".into(),
        source_path: "routing-regression".into(),
        per_request_tokens: None,
        daily_tokens: None,
        monthly_tokens: None,
        content_safety: policy.content_safety.clone(),
        model_preference: policy.model_preference.clone(),
        provider: policy.provider.clone(),
        guardrails: vec![],
        raw: json!({}),
    });
    Router::new()
        .merge(crate::routes::inference_routes())
        .with_state(state)
}

async fn post_chat(app: Router, stream: bool) -> (StatusCode, Bytes) {
    let response = app.oneshot(Request::builder().method("POST")
        .uri("/v1/chat/completions").header("content-type", "application/json")
        .body(Body::from(json!({
            "model": "client-requested", "messages": [{"role":"user","content":"hello"}],
            "stream": stream,
        }).to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .unwrap();
    (status, body)
}

#[tokio::test]
async fn explicit_provider_wins_over_informational_primary_for_buffered_and_streaming_chat() {
    for stream in [false, true] {
        let local = MockServer::start().await;
        let default = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&default)
            .await;
        let response = if stream {
            ResponseTemplate::new(200).set_body_raw(
                "data: {\"choices\":[{\"delta\":{\"content\":\"local\"}}]}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            )
        } else {
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices":[{"message":{"content":"local"}}]}))
        };
        Mock::given(body_partial_json(json!({"model":"policy-model"})))
            .and(|request: &wiremock::Request| {
                request.url.path() == "/v1/chat/completions"
                    && !request.headers.contains_key("authorization")
            })
            .respond_with(response)
            .expect(1)
            .mount(&local)
            .await;
        let mut config = config(&default.uri(), &[]);
        config.ollama_endpoint = Some(local.uri());
        let mut policy = policy("azure-openai", "policy-model");
        policy.provider = Some("ollama".into());
        let base = UpstreamConfig::azure(default.uri(), "true-default-model".into(), "test".into());
        let candidates = failover::build_candidates(&base, &policy);
        assert_eq!(candidates[0].provider.as_deref(), Some("ollama"));
        assert_eq!(candidates.last().unwrap().provider, None);
        assert_eq!(candidates.last().unwrap().deployment, "true-default-model");
        let (status, body) = post_chat(router(state(config), &policy).await, stream).await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8_lossy(&body).contains("local"));
    }
}

#[tokio::test]
async fn availability_cache_does_not_skip_another_credential_on_the_same_endpoint_and_model() {
    let shared = MockServer::start().await;
    let default = MockServer::start().await;
    Mock::given(header("authorization", "Bearer credential-a"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error":{"code":"model_not_found"}})),
        )
        .expect(1)
        .mount(&shared)
        .await;
    Mock::given(header("authorization", "Bearer credential-b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[]})))
        .expect(2)
        .mount(&shared)
        .await;
    Mock::given(header("authorization", "Bearer ambient-default-key"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&default)
        .await;
    let state = state(config(
        &default.uri(),
        &[
            ("account-a", &shared.uri(), Some("credential-a")),
            ("account-b", &shared.uri(), Some("credential-b")),
        ],
    ));
    let base = UpstreamConfig::azure(default.uri(), "true-default-model".into(), "test".into());
    let a = policy("account-a", "same-model");
    let b = policy("account-b", "same-model");
    forward_chat(&state, &base, &a, &HeaderMap::new(), Bytes::from("{}"))
        .await
        .unwrap();
    let (status, _, _, selected) =
        forward_chat(&state, &base, &b, &HeaderMap::new(), Bytes::from("{}"))
            .await
            .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected.endpoint, shared.uri());
    let (_, _, stream, selected) =
        forward_stream_chat(&state, &base, &b, HeaderMap::new(), Bytes::from("{}"))
            .await
            .unwrap();
    let _: Vec<_> = stream.try_collect().await.unwrap();
    assert_eq!(selected.endpoint, shared.uri());
    let a = primary_target(&state, &base, &a, b"{}").unwrap();
    let b = primary_target(&state, &base, &b, b"{}").unwrap();
    assert_ne!(model_capability_key(&a), model_capability_key(&b));
    let cache = state.unavailable_models.read().unwrap();
    assert!(cache.contains(&model_capability_key(&a)));
    assert!(!cache.contains(&model_capability_key(&b)));
    for key in cache.iter() {
        assert!(!key.contains("credential-a") && !key.contains("credential-b"));
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let size = socket.read(&mut chunk).await.unwrap();
        assert!(size > 0, "request closed before its body was sent");
        bytes.extend_from_slice(&chunk[..size]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn actual_chat_to_responses_recovery_never_replays_after_success_headers() {
    use tokio::io::AsyncWriteExt;
    for stream in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut chat, _) = listener.accept().await.unwrap();
            assert!(
                read_request(&mut chat)
                    .await
                    .starts_with("POST /chat/completions ")
            );
            let body = r#"{"error":{"code":"unsupported_api_for_model"}}"#;
            chat.write_all(format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            ).as_bytes()).await.unwrap();
            chat.shutdown().await.unwrap();
            for _ in 0..2 {
                let (mut responses, _) = listener.accept().await.unwrap();
                assert!(
                    read_request(&mut responses)
                        .await
                        .starts_with("POST /responses ")
                );
                responses.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n{\"output\":[").await.unwrap();
                responses.shutdown().await.unwrap();
            }
        });
        let fallback = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"replayed"}]}],
        }))).expect(0).mount(&fallback).await;
        let state = state(config(
            &fallback.uri(),
            &[
                ("primary", &endpoint, Some("primary-key")),
                ("fallback", &fallback.uri(), Some("fallback-key")),
            ],
        ));
        let mut policy = policy("primary", "primary-model");
        policy
            .model_preference
            .as_mut()
            .unwrap()
            .fallback
            .push(ModelRef {
                provider: "fallback".into(),
                deployment: "fallback-model".into(),
            });
        let app = router(state, &policy).await;
        let (status, body) = post_chat(app.clone(), stream).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("error"));
        assert!(!body.contains("replayed"));
        let (status, body) = post_chat(app, true).await;
        assert_eq!(status, StatusCode::OK);
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("error"));
        assert!(!body.contains("replayed"));
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("both chat and Responses requests must reach the test backend")
            .unwrap();
    }
}

#[tokio::test]
async fn auth_and_configuration_acquisition_failures_do_not_attempt_fallback() {
    for provider in ["copilot", "copilot-exchange", "ollama", "bedrock"] {
        let fallback = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&fallback)
            .await;
        let mut config = config(
            &fallback.uri(),
            &[
                ("copilot", "https://api.githubcopilot.com", None),
                (
                    "copilot-exchange",
                    "https://api.githubcopilot.com",
                    Some("test-seat"),
                ),
                ("fallback", &fallback.uri(), Some("fallback-key")),
            ],
        );
        config.ollama_endpoint = None;
        let state = state(config);
        let mut policy = policy(provider, "primary-model");
        policy
            .model_preference
            .as_mut()
            .unwrap()
            .fallback
            .push(ModelRef {
                provider: "fallback".into(),
                deployment: "fallback-model".into(),
            });
        let base =
            UpstreamConfig::azure(fallback.uri(), "true-default-model".into(), "test".into());
        let buffered = forward_chat(&state, &base, &policy, &HeaderMap::new(), Bytes::from("{}"))
            .await
            .err()
            .expect("auth/configuration must fail before inference HTTP");
        let streamed = forward_stream_chat(
            &state,
            &base,
            &policy,
            HeaderMap::new(),
            Bytes::from(r#"{"stream":true}"#),
        )
        .await
        .err()
        .expect("streaming auth/configuration must fail before inference HTTP");
        for error in [buffered, streamed] {
            let failure = error.downcast_ref::<ForwardFailure>().unwrap();
            assert!(matches!(
                failure.category,
                FailureCategory::Authentication | FailureCategory::Configuration
            ));
            assert_eq!(failure.acceptance, Acceptance::NotAccepted);
        }
        assert!(
            state
                .deployment_health
                .snapshot()
                .iter()
                .all(|entry| entry.failure_streak == 0)
        );
    }
}

#[tokio::test]
async fn connection_failure_before_acceptance_can_still_fail_over() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let fallback = MockServer::start().await;
    Mock::given(header("authorization", "Bearer fallback-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[]})))
        .expect(2)
        .mount(&fallback)
        .await;
    let state = state(config(
        &fallback.uri(),
        &[
            ("unavailable", &unavailable, None),
            ("fallback", &fallback.uri(), Some("fallback-key")),
        ],
    ));
    let mut policy = policy("unavailable", "model");
    policy
        .model_preference
        .as_mut()
        .unwrap()
        .fallback
        .push(ModelRef {
            provider: "fallback".into(),
            deployment: "model".into(),
        });
    let base = UpstreamConfig::azure(fallback.uri(), "true-default-model".into(), "test".into());
    let (status, _, _, selected) =
        forward_chat(&state, &base, &policy, &HeaderMap::new(), Bytes::from("{}"))
            .await
            .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected.endpoint, fallback.uri());
    let (status, _, stream, selected) =
        forward_stream_chat(&state, &base, &policy, HeaderMap::new(), Bytes::from("{}"))
            .await
            .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected.endpoint, fallback.uri());
    let _: Vec<_> = stream.try_collect().await.unwrap();
}

#[tokio::test]
async fn connection_closed_after_sending_has_unknown_acceptance_and_is_not_replayed() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.shutdown().await.unwrap();
    });
    let fallback = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&fallback)
        .await;
    let state = state(config(
        &fallback.uri(),
        &[
            ("primary", &endpoint, None),
            ("fallback", &fallback.uri(), Some("fallback-key")),
        ],
    ));
    let mut policy = policy("primary", "model");
    policy
        .model_preference
        .as_mut()
        .unwrap()
        .fallback
        .push(ModelRef {
            provider: "fallback".into(),
            deployment: "model".into(),
        });
    let base = UpstreamConfig::azure(fallback.uri(), "true-default-model".into(), "test".into());
    let result = forward_chat(&state, &base, &policy, &HeaderMap::new(), Bytes::from("{}")).await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("lost response must fail closed"),
    };
    let failure = error.downcast_ref::<ForwardFailure>().unwrap();
    assert_eq!(failure.category, FailureCategory::Transport);
    assert_eq!(failure.acceptance, Acceptance::Unknown);
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("the request must reach the test backend")
        .unwrap();
}
