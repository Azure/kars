// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::regressions::{config, policy, post_chat, read_request, router, state};
use super::*;
use crate::inference_policy_loader::ModelRef;
use crate::provider::ProviderKind;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

fn chat_response(stream: bool, text: &str) -> ResponseTemplate {
    if stream {
        ResponseTemplate::new(200).set_body_raw(
            format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":{"content":text}}]})
            ),
            "text/event-stream",
        )
    } else {
        ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":{"content":text}}]}))
    }
}

fn responses_response(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],
    }))
}

#[tokio::test]
async fn legacy_primary_metadata_keeps_foundry_with_or_without_native_credentials() {
    for tag in ["anthropic", "ollama", "bedrock", "unknown-informational"] {
        for native_key in [None, Some("native-key")] {
            for stream in [false, true] {
                let default = MockServer::start().await;
                let native = MockServer::start().await;
                Mock::given(method("POST"))
                    .respond_with(ResponseTemplate::new(200))
                    .expect(0)
                    .mount(&native)
                    .await;
                let responses_only = tag == "anthropic" && native_key.is_some();
                let chat = if responses_only {
                    ResponseTemplate::new(400)
                        .set_body_json(json!({"error":{"code":"unsupported_api_for_model"}}))
                } else {
                    chat_response(stream, "legacy-default")
                };
                Mock::given(path("/openai/v1/chat/completions"))
                    .and(header("authorization", "Bearer ambient-default-key"))
                    .and(body_partial_json(json!({"model":"claude-prod"})))
                    .respond_with(chat)
                    .expect(1)
                    .mount(&default)
                    .await;
                if responses_only {
                    Mock::given(path("/openai/v1/responses"))
                        .and(header("authorization", "Bearer ambient-default-key"))
                        .and(body_partial_json(json!({"model":"claude-prod"})))
                        .respond_with(responses_response("legacy-default"))
                        .expect(1)
                        .mount(&default)
                        .await;
                }
                let mut config = config(&format!("{}/openai/v1", default.uri()), &[]);
                config.anthropic_endpoint = native.uri();
                config.anthropic_api_key = native_key.map(str::to_string);
                config.ollama_endpoint = Some(native.uri());
                let policy = policy(tag, "claude-prod");
                let (status, body) = post_chat(router(state(config), &policy).await, stream).await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{tag}, native key {}, stream {stream}",
                    native_key.is_some()
                );
                assert!(String::from_utf8_lossy(&body).contains("legacy-default"));
            }
        }
    }
}

#[tokio::test]
async fn registered_named_intent_and_explicit_native_intent_remain_separate() {
    let default = MockServer::start().await;
    let registered = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&default)
        .await;
    Mock::given(path("/chat/completions"))
        .and(header("authorization", "Bearer registered-key"))
        .respond_with(chat_response(false, "registered"))
        .expect(1)
        .mount(&registered)
        .await;
    let mut config = config(
        &default.uri(),
        &[("anthropic", &registered.uri(), Some("registered-key"))],
    );
    config.anthropic_endpoint = registered.uri();
    config.anthropic_api_key = Some("native-key".into());
    let state = state(config);
    let metadata = policy("anthropic", "claude-prod");
    let mut explicit = metadata.clone();
    explicit.provider = Some("anthropic".into());
    let base = UpstreamConfig::azure(default.uri(), "default".into(), "test".into());
    let named = primary_target(&state, &base, &metadata, b"{}").unwrap();
    let native = primary_target(&state, &base, &explicit, b"{}").unwrap();
    assert_eq!(named.provider, ProviderKind::AzureOpenAI);
    assert_eq!(named.provider_api_key.as_deref(), Some("registered-key"));
    assert_eq!(native.provider, ProviderKind::Anthropic);
    assert_eq!(native.api_key.as_deref(), Some("native-key"));
    assert_ne!(model_capability_key(&named), model_capability_key(&native));
    let (status, body) = post_chat(router(state, &metadata).await, false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("registered"));
}

#[tokio::test]
async fn explicit_native_intent_still_requires_native_credentials() {
    let default = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&default)
        .await;
    let mut config = config(
        &default.uri(),
        &[("anthropic", &default.uri(), Some("registered-key"))],
    );
    config.anthropic_api_key = None;
    let state = state(config);
    let mut policy = policy("azure-openai", "claude-prod");
    policy.provider = Some("anthropic".into());
    let (status, _) = post_chat(router(state, &policy).await, false).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn unhealthy_legacy_metadata_does_not_suppress_an_explicit_native_fallback() {
    let default = MockServer::start().await;
    let native = MockServer::start().await;
    Mock::given(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&default)
        .await;
    Mock::given(path("/v1/chat/completions"))
        .and(body_partial_json(json!({"model":"llama-prod"})))
        .respond_with(chat_response(false, "native-fallback"))
        .expect(4)
        .mount(&native)
        .await;
    let mut config = config(&default.uri(), &[]);
    config.ollama_endpoint = Some(native.uri());
    let state = state(config);
    let mut policy = policy("ollama", "llama-prod");
    policy
        .model_preference
        .as_mut()
        .unwrap()
        .fallback
        .push(ModelRef {
            provider: "ollama".into(),
            deployment: "llama-prod".into(),
        });
    let app = router(state, &policy).await;
    for _ in 0..4 {
        let (status, body) = post_chat(app.clone(), false).await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8_lossy(&body).contains("native-fallback"));
    }
}

async fn rejecting_backend(
    status: u16,
    responses_recovery: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(
            read_request(&mut socket)
                .await
                .starts_with("POST /chat/completions ")
        );
        if responses_recovery {
            let body = r#"{"error":{"code":"unsupported_api_for_model"}}"#;
            socket.write_all(format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            ).as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            let (next, _) = listener.accept().await.unwrap();
            socket = next;
            assert!(
                read_request(&mut socket)
                    .await
                    .starts_with("POST /responses ")
            );
        }
        socket.write_all(format!(
            "HTTP/1.1 {status} Rejected\r\nContent-Type: application/json\r\nContent-Length: 10000\r\nConnection: close\r\n\r\n{{\"error\":"
        ).as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    (endpoint, server)
}

#[tokio::test]
async fn truncated_known_503_and_429_rejections_reach_healthy_fallback_in_both_modes() {
    for status in [503, 429] {
        for stream in [false, true] {
            for responses_recovery in [false, true] {
                let (endpoint, server) = rejecting_backend(status, responses_recovery).await;
                let backup = MockServer::start().await;
                let path_name = if responses_recovery {
                    "/responses"
                } else {
                    "/chat/completions"
                };
                let reply = if responses_recovery {
                    responses_response("healthy-fallback")
                } else {
                    chat_response(stream, "healthy-fallback")
                };
                Mock::given(path(path_name))
                    .and(header("authorization", "Bearer backup-key"))
                    .and(body_partial_json(json!({"model":"backup-model"})))
                    .respond_with(reply)
                    .expect(1)
                    .mount(&backup)
                    .await;
                let state = state(config(
                    &backup.uri(),
                    &[
                        ("primary", &endpoint, Some("primary-key")),
                        ("backup", &backup.uri(), Some("backup-key")),
                    ],
                ));
                let mut policy = policy("primary", "primary-model");
                policy
                    .model_preference
                    .as_mut()
                    .unwrap()
                    .fallback
                    .push(ModelRef {
                        provider: "backup".into(),
                        deployment: "backup-model".into(),
                    });
                let (actual, body) = post_chat(router(state, &policy).await, stream).await;
                assert_eq!(
                    actual,
                    StatusCode::OK,
                    "{status}, stream {stream}, Responses {responses_recovery}"
                );
                assert!(String::from_utf8_lossy(&body).contains("healthy-fallback"));
                tokio::time::timeout(std::time::Duration::from_secs(5), server)
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }
}

#[tokio::test]
async fn truncated_ordinary_client_rejections_are_never_replayed() {
    for status in [400, 401, 403, 404, 422] {
        for stream in [false, true] {
            let (endpoint, server) = rejecting_backend(status, false).await;
            let backup = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200))
                .expect(0)
                .mount(&backup)
                .await;
            let state = state(config(
                &backup.uri(),
                &[
                    ("primary", &endpoint, Some("primary-key")),
                    ("backup", &backup.uri(), Some("backup-key")),
                ],
            ));
            let mut policy = policy("primary", "primary-model");
            policy
                .model_preference
                .as_mut()
                .unwrap()
                .fallback
                .push(ModelRef {
                    provider: "backup".into(),
                    deployment: "backup-model".into(),
                });
            let (actual, _) = post_chat(router(state, &policy).await, stream).await;
            assert_eq!(actual, StatusCode::BAD_GATEWAY);
            tokio::time::timeout(std::time::Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
        }
    }
}
