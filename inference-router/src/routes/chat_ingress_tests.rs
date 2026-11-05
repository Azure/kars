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
    matchers::{method, path},
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
