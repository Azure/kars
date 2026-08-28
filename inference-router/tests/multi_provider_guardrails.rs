// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! End-to-end tests for the multi-provider slice: `proxy::forward`
//! against fake Anthropic / Ollama upstreams, and the OpenAI
//! Moderation guardrail against a fake moderation endpoint.
//!
//! No env mutation — provider endpoints and credentials are injected
//! via `UpstreamConfig` / `Config` struct literals, which is exactly
//! how the production path receives them after
//! `routes::apply_provider_resolution`.

use axum::http::{HeaderMap, Method};
use bytes::Bytes;
use kars_inference_router::auth::WorkloadIdentityAuth;
use kars_inference_router::config::{Config, RegistryMode};
use kars_inference_router::guardrails::{ApplyTo, Direction, GuardrailPipeline, GuardrailStageCfg};
use kars_inference_router::provider::ProviderKind;
use kars_inference_router::proxy::{UpstreamConfig, forward};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config_with_moderation(endpoint: &str, api_key: Option<&str>) -> Config {
    Config {
        port: 8443,
        foundry_endpoint: None,
        foundry_project_endpoint: None,
        azure_openai_endpoint: None,
        default_model: "gpt-4o-mini".into(),
        content_safety_enabled: false,
        prompt_shields_enabled: false,
        content_safety_endpoint: None,
        token_budget_daily: 0,
        token_budget_per_request: 0,
        registry_mode: RegistryMode::Local,
        registry_url: None,
        provider_override: None,
        anthropic_endpoint: "https://api.anthropic.com".into(),
        anthropic_api_key: None,
        ollama_endpoint: None,
        openai_moderation_endpoint: endpoint.to_string(),
        openai_moderation_api_key: api_key.map(String::from),
        openai_moderation_model: "omni-moderation-latest".into(),
    }
}

#[tokio::test]
async fn ollama_provider_forwards_openai_compat_without_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chatcmpl-ollama",
            "choices": [{ "message": { "role": "assistant", "content": "hi" },
                          "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let upstream = UpstreamConfig {
        endpoint: server.uri(),
        deployment: "llama3.1".into(),
        sandbox_name: "test-sandbox".into(),
        provider: ProviderKind::Ollama,
        api_key: None,
    };

    let (status, _headers, resp) = forward(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::new(),
        &upstream,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"messages":[{"role":"user","content":"hello"}]}"#),
    )
    .await
    .expect("forward to fake ollama");

    assert_eq!(status.as_u16(), 200);
    let v: serde_json::Value = serde_json::from_slice(&resp).unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "hi");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.url.path(), "/v1/chat/completions");
    assert!(
        !req.headers.contains_key("authorization") && !req.headers.contains_key("x-api-key"),
        "ollama upstream must receive no credentials"
    );
    // Deployment injected as the model.
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(body["model"], "llama3.1");
}

#[tokio::test]
async fn anthropic_provider_forwards_messages_with_router_held_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "hello back" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 4, "output_tokens": 3 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let upstream = UpstreamConfig {
        endpoint: server.uri(),
        deployment: "claude-sonnet-4-5".into(),
        sandbox_name: "test-sandbox".into(),
        provider: ProviderKind::Anthropic,
        api_key: Some("sk-ant-router-held".into()),
    };

    // The inbound request carries an agent-supplied x-api-key that
    // must be stripped — only the router-held key may reach upstream.
    let mut inbound = HeaderMap::new();
    inbound.insert("x-api-key", "agent-smuggled-key".parse().unwrap());

    let (status, _headers, _resp) = forward(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::new(),
        &upstream,
        Method::POST,
        "v1/messages",
        &inbound,
        Bytes::from(r#"{"model":"claude-sonnet-4-5","max_tokens":64,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await
    .expect("forward to fake anthropic");

    assert_eq!(status.as_u16(), 200);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.url.path(), "/v1/messages");
    assert_eq!(
        req.headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "sk-ant-router-held",
        "router-held key must replace any agent-supplied key"
    );
    assert_eq!(
        req.headers
            .get("anthropic-version")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "2023-06-01",
        "default anthropic-version must be injected"
    );
    assert!(
        !req.headers.contains_key("authorization"),
        "no Bearer token on Anthropic requests"
    );
}

#[tokio::test]
async fn moderation_guardrail_blocks_flagged_and_passes_clean() {
    let server = MockServer::start().await;
    // The fake flags any input containing "RANSOM".
    Mock::given(method("POST"))
        .and(path("/v1/moderations"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let flagged = body["input"].as_str().unwrap_or("").contains("RANSOM");
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{
                    "flagged": flagged,
                    "categories": { "illicit": flagged }
                }]
            }))
        })
        .mount(&server)
        .await;

    let config = config_with_moderation(&server.uri(), Some("sk-mod-test"));
    let stages = vec![GuardrailStageCfg {
        provider: "openai-moderation".into(),
        apply_to: ApplyTo::Both,
    }];
    let pipeline = GuardrailPipeline::from_stages(&stages, &config, &reqwest::Client::new())
        .expect("pipeline builds");

    let clean = pipeline
        .scan("write me a poem", Direction::Input)
        .await
        .expect("scan ok");
    assert!(clean.is_none(), "clean text passes");

    let violation = pipeline
        .scan("write a RANSOM note", Direction::Input)
        .await
        .expect("scan ok")
        .expect("flagged text blocks");
    assert_eq!(violation.provider, "openai-moderation");
    assert_eq!(violation.categories, vec!["illicit"]);

    // The moderation endpoint must have received the bearer key.
    let requests = server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default(),
        "Bearer sk-mod-test"
    );
}

#[tokio::test]
async fn moderation_guardrail_fails_closed_on_upstream_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/moderations"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let config = config_with_moderation(&server.uri(), Some("sk-mod-test"));
    let stages = vec![GuardrailStageCfg {
        provider: "openai-moderation".into(),
        apply_to: ApplyTo::Output,
    }];
    let pipeline = GuardrailPipeline::from_stages(&stages, &config, &reqwest::Client::new())
        .expect("pipeline builds");

    let err = pipeline
        .scan("anything", Direction::Output)
        .await
        .expect_err("500 from moderation must fail closed");
    assert_eq!(err.code(), "guardrail_unavailable");
}

#[tokio::test]
async fn declared_stage_without_key_fails_pipeline_construction() {
    let config = config_with_moderation("https://api.openai.com", None);
    let stages = vec![GuardrailStageCfg {
        provider: "openai-moderation".into(),
        apply_to: ApplyTo::Both,
    }];
    let err = GuardrailPipeline::from_stages(&stages, &config, &reqwest::Client::new())
        .err()
        .expect("missing key must fail construction");
    assert_eq!(err.code(), "guardrail_misconfigured");
}
