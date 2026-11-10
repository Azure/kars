// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::super::{AppState, inference::inference_routes, model_routing::tests::test_state};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, method, path},
};

async fn post_chat(app: &Router, body: Vec<u8>) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn padded_request(request: &Value) -> Vec<u8> {
    let encoded = request.to_string();
    let deep = format!(
        "{},\"padding\":{}0{}}}",
        encoded.strip_suffix('}').unwrap(),
        "[".repeat(140),
        "]".repeat(140),
    );
    assert!(
        serde_json::from_str::<Value>(&deep)
            .unwrap_err()
            .to_string()
            .contains("recursion limit")
    );
    deep.into_bytes()
}

async fn install_ollama_policy(state: &AppState) {
    *state.inference_policy.write().await =
        Some(crate::inference_policy_loader::LoadedInferencePolicy {
            digest: "sha256:test".into(),
            source_path: "test-policy".into(),
            per_request_tokens: Some(64),
            daily_tokens: None,
            monthly_tokens: None,
            content_safety: Default::default(),
            model_preference: None,
            provider: Some("ollama".into()),
            guardrails: vec![],
            raw: json!({}),
        });
}

#[tokio::test]
async fn unparsable_chat_cannot_bypass_model_token_or_tool_policy_without_lifetime_admission() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })))
        .expect(2)
        .mount(&upstream)
        .await;
    let mut config = crate::config::Config::from_env().unwrap();
    config.default_model = "allowed-model".into();
    config.provider_override = Some("ollama".into());
    config.ollama_endpoint = Some(upstream.uri());
    config.content_safety_enabled = false;
    config.providers.clear();
    let mut state = test_state(config);
    state.client = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(state.inference_budget.is_none());
    install_ollama_policy(&state).await;
    let policies = tempfile::tempdir().unwrap();
    std::fs::write(
        policies.path().join("policy.yaml"),
        r#"version: "1.0"
agent: test
policies:
  - name: deny-model-and-tool
    type: capability
    denied_actions:
      - "inference:chat_completions:blocked-model"
      - "tool.invoke:blocked-tool"
    priority: 100
  - name: allow-inference
    type: capability
    allowed_actions:
      - "inference:*"
      - "tool.invoke:*"
      - "output:*"
    priority: 1
"#,
    )
    .unwrap();
    assert!(
        state
            .governance
            .load_policies_from_dir(policies.path().to_str().unwrap())
            .unwrap()
            > 0
    );
    for (action, allowed) in [
        ("inference:chat_completions:blocked-model", false),
        ("inference:chat_completions:", true),
        ("tool.invoke:blocked-tool", false),
    ] {
        assert_eq!(
            state.governance.evaluate("test", action, None)["allowed"],
            json!(allowed),
        );
    }
    let app = Router::new().merge(inference_routes()).with_state(state);
    let allowed =
        json!({"model": "allowed-model", "messages": [{"role": "user", "content": "hello"}]});
    let mut blocked_model = allowed.clone();
    blocked_model["model"] = json!("blocked-model");
    let mut excess_tokens = allowed.clone();
    excess_tokens["max_tokens"] = json!(65);
    let mut blocked_tool = allowed.clone();
    blocked_tool["tools"] = json!([
        {"type": "function", "function": {"name": "blocked-tool", "parameters": {"type": "object"}}},
    ]);
    for body in [
        padded_request(&blocked_model),
        padded_request(&excess_tokens),
        padded_request(&blocked_tool),
        b"not-json-private-request".to_vec(),
        vec![],
        vec![0xff],
    ] {
        let (status, error) = post_chat(&app, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert_eq!(error["error"]["code"], "invalid_json");
        assert_eq!(
            error["error"]["message"],
            "Request must be valid JSON within router parsing limits",
        );
    }
    for (body, expected) in [
        (blocked_model, StatusCode::FORBIDDEN),
        (excess_tokens, StatusCode::TOO_MANY_REQUESTS),
    ] {
        let (status, error) = post_chat(&app, body.to_string().into_bytes()).await;
        assert_eq!(status, expected, "{error}");
    }
    assert!(upstream.received_requests().await.unwrap().is_empty());
    for body in [allowed, blocked_tool] {
        let (status, response) = post_chat(&app, body.to_string().into_bytes()).await;
        assert_eq!(status, StatusCode::OK, "{response}");
    }
    let requests = upstream.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(
            request.body_json::<Value>().unwrap()["model"],
            "allowed-model"
        );
    }
    assert_eq!(
        requests[1].body_json::<Value>().unwrap()["tools"],
        json!([])
    );
}

#[tokio::test]
async fn input_guardrail_reuses_validated_ingress_with_duplicate_keys_and_unicode() {
    let upstream = MockServer::start().await;
    let moderation = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })))
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/moderations"))
        .respond_with(|request: &wiremock::Request| {
            let body = request.body_json::<Value>().unwrap();
            let flagged = body["input"].as_str().unwrap().contains("blocked-text");
            ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"flagged": flagged, "categories": {"violence": flagged}}],
            }))
        })
        .expect(2)
        .mount(&moderation)
        .await;
    let mut config = crate::config::Config::from_env().unwrap();
    config.default_model = "selected-model".into();
    config.ollama_endpoint = Some(upstream.uri());
    config.content_safety_enabled = false;
    config.providers.clear();
    config.openai_moderation_endpoint = moderation.uri();
    config.openai_moderation_api_key = Some("test-moderation-key".into());
    let mut state = test_state(config);
    state.client = reqwest::Client::builder().no_proxy().build().unwrap();
    install_ollama_policy(&state).await;
    state
        .inference_policy
        .write()
        .await
        .as_mut()
        .unwrap()
        .guardrails = vec![crate::guardrails::GuardrailStageCfg {
        provider: "openai-moderation".into(),
        apply_to: crate::guardrails::ApplyTo::Input,
    }];
    let app = Router::new().merge(inference_routes()).with_state(state);
    for (body, expected) in [
        (
            r#"{"model":"selected-model","messages":[{"content":"ignored"}],"messages":[{"content":[{"text":"blocked\u002dtext"},{"text":"\u03bb"}]}]}"#,
            StatusCode::FORBIDDEN,
        ),
        (
            r#"{"model":"selected-model","messages":[{"content":"blocked-text"}],"messages":[{"content":"clean"}]}"#,
            StatusCode::OK,
        ),
    ] {
        let mut body = body.as_bytes().to_vec();
        body.resize(2 * 1024 * 1024, b' ');
        let (status, response) = post_chat(&app, body).await;
        assert_eq!(status, expected, "{response}");
    }
    let (status, _) = post_chat(&app, padded_request(&json!({"messages": []}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let requests = moderation.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].body_json::<Value>().unwrap()["input"],
        "blocked-text\nλ"
    );
    assert_eq!(requests[1].body_json::<Value>().unwrap()["input"], "clean");
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn tool_rewrite_growth_preserves_model_stream_and_token_policy_at_request_limit() {
    let upstream = MockServer::start().await;
    let completion = json!({
        "choices": [],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    });
    for stream in [false, true] {
        let template = if stream {
            ResponseTemplate::new(200).set_body_raw(
                format!("data: {completion}\n\ndata: [DONE]\n\n"),
                "text/event-stream",
            )
        } else {
            ResponseTemplate::new(200).set_body_json(&completion)
        };
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({"stream": stream})))
            .respond_with(template)
            .expect(1)
            .mount(&upstream)
            .await;
    }
    let mut config = crate::config::Config::from_env().unwrap();
    config.default_model = "default-model".into();
    config.provider_override = Some("ollama".into());
    config.ollama_endpoint = Some(upstream.uri());
    config.content_safety_enabled = false;
    config.providers.clear();
    let mut state = test_state(config);
    state.client = reqwest::Client::builder().no_proxy().build().unwrap();
    install_ollama_policy(&state).await;
    let policies = tempfile::tempdir().unwrap();
    std::fs::write(
        policies.path().join("policy.yaml"),
        r#"version: "1.0"
agent: test
policies:
  - name: deny-tool
    type: capability
    denied_actions:
      - "tool.invoke:blocked-tool"
    priority: 100
  - name: allow-inference
    type: capability
    allowed_actions:
      - "inference:*"
      - "tool.invoke:*"
      - "output:*"
    priority: 1
"#,
    )
    .unwrap();
    assert!(
        state
            .governance
            .load_policies_from_dir(policies.path().to_str().unwrap())
            .unwrap()
            > 0
    );
    let app = Router::new().merge(inference_routes()).with_state(state);
    let input_limit = 2 * 1024 * 1024;
    for stream in [false, true] {
        let request = json!({
            "model": "selected-model",
            "stream": stream,
            "messages": [{"role": "user", "content": "hello"}],
            "max_completion_tokens": 64,
            "tools": [{"type": "function", "function": {"name": "blocked-tool"}}],
        })
        .to_string();
        // Numeric normalization grows the tool-filtered body beyond the ingress limit.
        let mut body = format!(
            "{},\"padding\":[{}0]}}",
            request.strip_suffix('}').unwrap(),
            "1e2,".repeat(400_000),
        )
        .into_bytes();
        assert!(body.len() < input_limit);
        body.resize(input_limit, b' ');
        assert_eq!(super::extract_requested_max_tokens(&body), Some(64));
        assert_eq!(
            super::extract_advertised_tool_names(&body),
            ["blocked-tool"]
        );
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        if stream {
            assert_eq!(response.headers()["content-type"], "text/event-stream");
        }
        let bytes = axum::body::to_bytes(response.into_body(), 65_536)
            .await
            .unwrap();
        if stream {
            assert!(
                std::str::from_utf8(&bytes)
                    .unwrap()
                    .contains("data: [DONE]")
            );
        } else {
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), completion);
        }
    }
    let requests = upstream.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for (request, stream) in requests.iter().zip([false, true]) {
        assert!(request.body.len() > input_limit);
        let value = request.body_json::<Value>().unwrap();
        assert_eq!(value["model"], "selected-model");
        assert_eq!(value["stream"], stream);
        assert_eq!(value["max_completion_tokens"], 64);
        assert_eq!(value["tools"], json!([]));
        assert_eq!(value["padding"].as_array().unwrap().len(), 400_001);
    }
}
