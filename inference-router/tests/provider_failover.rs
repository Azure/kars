// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use axum::http::{HeaderMap, Method, StatusCode};
use bytes::Bytes;
use futures::TryStreamExt;
use kars_inference_router::{
    auth::WorkloadIdentityAuth,
    config::{Config, ProviderEndpoint},
    deployment_health::DeploymentHealthRegistry,
    failover::{forward_stream_with_failover, forward_with_failover},
    inference_policy_loader::{InferencePolicySnapshot, ModelPreference, ModelRef},
    proxy::UpstreamConfig,
};
use serde_json::json;
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

fn config(providers: &[(&str, &str, &str)]) -> Config {
    let mut config = Config::from_env().unwrap();
    config.providers = providers
        .iter()
        .map(|(tag, endpoint, key)| {
            (
                tag.to_string(),
                ProviderEndpoint {
                    tag: tag.to_string(),
                    endpoint: endpoint.to_string(),
                    api_key: Some(key.to_string()),
                },
            )
        })
        .collect();
    config
}

fn base(endpoint: &str) -> UpstreamConfig {
    let mut base = UpstreamConfig::azure(
        endpoint.into(),
        "default-model".into(),
        "test-sandbox".into(),
    );
    base.provider_api_key = Some("default-key".into());
    base
}

fn policy() -> InferencePolicySnapshot {
    InferencePolicySnapshot {
        digest: "sha256:routing-test".into(),
        model_preference: Some(ModelPreference {
            primary: ModelRef {
                provider: "first".into(),
                deployment: "shared-model".into(),
            },
            fallback: vec![ModelRef {
                provider: "second".into(),
                deployment: "shared-model".into(),
            }],
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn no_model_preference_preserves_an_explicit_client_model() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(json!({"model":"client-selected-model"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let (_, _, _, selected) = forward_with_failover(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::new(),
        &Arc::new(DeploymentHealthRegistry::new()),
        &base(&server.uri()),
        &config(&[]),
        &InferencePolicySnapshot::default(),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"model":"client-selected-model"}"#),
    )
    .await
    .unwrap();
    assert_eq!(selected.deployment, "client-selected-model");
}

#[tokio::test]
async fn buffered_failover_retains_same_model_on_distinct_providers_and_uses_own_keys() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer first-key"))
        .and(body_partial_json(json!({"model":"shared-model"})))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer second-key"))
        .and(body_partial_json(json!({"model":"shared-model"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices":[{"message":{"content":"fallback"}}]})),
        )
        .expect(1)
        .mount(&second)
        .await;
    let config = config(&[
        ("first", &first.uri(), "first-key"),
        ("second", &second.uri(), "second-key"),
    ]);
    let health = Arc::new(DeploymentHealthRegistry::new());
    let (status, _, body, selected) = forward_with_failover(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::new(),
        &health,
        &base(&first.uri()),
        &config,
        &policy(),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"model":"caller-model","messages":[]}"#),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected.endpoint, second.uri());
    assert_eq!(selected.deployment, "shared-model");
    assert!(String::from_utf8_lossy(&body).contains("fallback"));
    assert_eq!(
        health
            .snapshot()
            .iter()
            .find(|s| s.deployment == "first::shared-model")
            .unwrap()
            .failure_streak,
        1
    );
}

#[tokio::test]
async fn same_endpoint_different_provider_credentials_are_not_reused() {
    let server = MockServer::start().await;
    Mock::given(header("authorization", "Bearer first-key"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer second-key"))
        .respond_with(ResponseTemplate::new(200).set_body_string("second"))
        .expect(1)
        .mount(&server)
        .await;
    let config = config(&[
        ("first", &server.uri(), "first-key"),
        ("second", &server.uri(), "second-key"),
    ]);
    let (status, _, _, selected) = forward_with_failover(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::new(),
        &Arc::new(DeploymentHealthRegistry::new()),
        &base(&server.uri()),
        &config,
        &policy(),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected.provider_api_key.as_deref(), Some("second-key"));
}

#[tokio::test]
async fn streaming_failover_retains_the_winning_provider_and_headers() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(header("authorization", "Bearer first-key"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(header("authorization", "Bearer second-key"))
        .and(body_partial_json(json!({"model":"shared-model"})))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&second)
        .await;
    let config = config(&[
        ("first", &first.uri(), "first-key"),
        ("second", &second.uri(), "second-key"),
    ]);
    let (status, headers, stream, selected) = forward_stream_with_failover(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        reqwest::Client::new(),
        &Arc::new(DeploymentHealthRegistry::new()),
        &base(&first.uri()),
        &config,
        &policy(),
        "chat/completions",
        HeaderMap::new(),
        Bytes::from(r#"{"stream":true}"#),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/event-stream");
    assert_eq!(selected.endpoint, second.uri());
    let chunks: Vec<_> = stream.try_collect().await.unwrap();
    assert!(
        chunks
            .iter()
            .any(|chunk| String::from_utf8_lossy(chunk).contains("[DONE]"))
    );
}

#[tokio::test]
async fn accepted_stream_failure_is_never_replayed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 8192];
        let _ = socket.read(&mut request).await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 10000\r\nConnection: close\r\n\r\ndata: partial\n\n").await.unwrap();
        socket.shutdown().await.unwrap();
    });
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&second)
        .await;
    let config = config(&[
        ("first", &first, "first-key"),
        ("second", &second.uri(), "second-key"),
    ]);
    let (status, _, stream, selected) = forward_stream_with_failover(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        reqwest::Client::new(),
        &Arc::new(DeploymentHealthRegistry::new()),
        &base(&first),
        &config,
        &policy(),
        "chat/completions",
        HeaderMap::new(),
        Bytes::from(r#"{"stream":true}"#),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected.endpoint, first);
    assert!(stream.try_collect::<Vec<_>>().await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn auth_errors_are_not_failover_triggers() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_string("denied"))
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&second)
        .await;
    let config = config(&[
        ("first", &first.uri(), "first-key"),
        ("second", &second.uri(), "second-key"),
    ]);
    let (status, _, body, selected) = forward_with_failover(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::new(),
        &Arc::new(DeploymentHealthRegistry::new()),
        &base(&first.uri()),
        &config,
        &policy(),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, "denied");
    assert_eq!(selected.endpoint, first.uri());
}

#[tokio::test]
async fn exhausted_named_providers_restore_true_default_endpoint_model_and_key() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    let default = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&second)
        .await;
    Mock::given(header("authorization", "Bearer default-key"))
        .and(body_partial_json(json!({"model":"default-model"})))
        .respond_with(ResponseTemplate::new(200).set_body_string("default"))
        .expect(1)
        .mount(&default)
        .await;
    let config = config(&[
        ("first", &first.uri(), "first-key"),
        ("second", &second.uri(), "second-key"),
    ]);
    let (status, _, _, selected) = forward_with_failover(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::new(),
        &Arc::new(DeploymentHealthRegistry::new()),
        &base(&default.uri()),
        &config,
        &policy(),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(selected.endpoint, default.uri());
    assert_eq!(selected.deployment, "default-model");
}
