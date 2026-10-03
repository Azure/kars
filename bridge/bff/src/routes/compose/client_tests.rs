// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{OrchestratorPrompt, call_llm, orchestrator_via_router};
use crate::kars::cluster::Cluster;
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<(Method, String, Value)>>>;

async fn fixture(response: Value) -> (String, Cluster, Requests, tokio::task::JoinHandle<()>) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    async fn handle(
        State((requests, response)): State<(Requests, Value)>,
        method: Method,
        uri: Uri,
        body: Bytes,
    ) -> (StatusCode, Json<Value>) {
        let body: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        };
        requests
            .lock()
            .unwrap()
            .push((method, uri.path().to_string(), body.clone()));
        if uri.path().ends_with("/chat/completions") && body.get("max_tokens").is_some() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {
                    "code": "unsupported_parameter", "param": "max_tokens",
                    "message": "Unsupported parameter: 'max_tokens'; use 'max_completion_tokens' instead."
                }})),
            );
        }
        (StatusCode::OK, Json(response))
    }
    let requests = Requests::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let config = kube::Config::new(endpoint.parse().unwrap());
    let cluster = Cluster::for_test_client(kube::Client::try_from(config).unwrap());
    let app = Router::new()
        .fallback(handle)
        .with_state((requests.clone(), response));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (endpoint, cluster, requests, server)
}

fn chat_response() -> Value {
    json!({"choices": [{"message": {"content": "a real response body"}}]})
}

fn expected_chat(model: &str, limit: u32) -> Value {
    json!({"model": model, "messages": [
        {"role": "system", "content": "system prompt"},
        {"role": "user", "content": "user prompt"}
    ], "max_completion_tokens": limit})
}

#[tokio::test]
async fn direct_chat_bounds_completion_tokens_for_legacy_reasoning_and_deployment_aliases() {
    let (endpoint, _, requests, server) = fixture(chat_response()).await;
    for model in ["gpt-4.1", "gpt-5.4-mini", "o3", "operator-deployment"] {
        let result = call_llm(
            &format!("{endpoint}/v1/"),
            "fixture-token",
            model,
            "system prompt",
            "user prompt",
            1200,
        )
        .await
        .unwrap();
        assert_eq!(result, "a real response body");
        assert_eq!(
            requests.lock().unwrap().last().unwrap(),
            &(
                Method::POST,
                "/v1/chat/completions".into(),
                expected_chat(model, 1200)
            )
        );
    }
    assert_eq!(requests.lock().unwrap().len(), 4);
    server.abort();
}

#[tokio::test]
async fn native_chat_bounds_reasoning_output_through_the_exact_governed_proxy_path() {
    let (_, cluster, requests, server) = fixture(chat_response()).await;
    let result = orchestrator_via_router(
        &cluster,
        "work",
        "agent-pod",
        "operator-deployment",
        OrchestratorPrompt {
            system: "system prompt",
            user: "user prompt",
        },
        false,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(result, "a real response body");
    assert_eq!(
        *requests.lock().unwrap(),
        vec![(
            Method::POST,
            "/api/v1/namespaces/work/pods/agent-pod:8443/proxy/v1/chat/completions".into(),
            expected_chat("operator-deployment", 4096)
        )]
    );
    server.abort();
}

#[tokio::test]
async fn anthropic_messages_preserve_the_native_token_limit_and_text_blocks() {
    let (_, cluster, requests, server) = fixture(json!({"content": [
        {"type": "text", "text": "first"}, {"type": "tool_use", "name": "ignored"}, {"type": "text", "text": "second"}
    ]})).await;
    let result = orchestrator_via_router(
        &cluster,
        "work",
        "agent-pod",
        "claude-sonnet-5",
        OrchestratorPrompt {
            system: "system prompt",
            user: "user prompt",
        },
        true,
        4096,
    )
    .await
    .unwrap();
    assert_eq!(result, "firstsecond");
    assert_eq!(
        *requests.lock().unwrap(),
        vec![(
            Method::POST,
            "/api/v1/namespaces/work/pods/agent-pod:8443/proxy/v1/messages".into(),
            json!({
                "model": "claude-sonnet-5", "system": "system prompt", "messages": [{"role": "user", "content": "user prompt"}], "max_tokens": 4096
            })
        )]
    );
    server.abort();
}

#[tokio::test]
async fn router_candidates_exclude_terminating_pods_even_while_the_router_is_ready() {
    let pod = |name: &str, deleting: bool, ready: bool, phase: &str| {
        let mut value = json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
            "name": name, "namespace": "work", "labels": {"kars.azure.com/sandbox": name}
        }, "status": {"phase": phase, "containerStatuses": [{"name": "inference-router", "image": "router:latest", "imageID": "fixture", "ready": ready, "restartCount": 0}]}});
        if deleting {
            value["metadata"]["deletionTimestamp"] = json!("2026-10-02T14:25:00Z");
        }
        value
    };
    let (_, cluster, requests, server) = fixture(
        json!({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": [
            pod("old-router", true, true, "Running"),
            pod("new-router", false, true, "Running"),
            pod("warming-router", false, false, "Running"),
            pod("pending-router", false, true, "Pending")
        ]}),
    )
    .await;
    assert_eq!(
        cluster.running_sandbox_candidates().await,
        vec![("work".into(), "new-router".into())]
    );
    assert_eq!(
        *requests.lock().unwrap(),
        vec![(Method::GET, "/api/v1/pods".into(), Value::Null)]
    );
    server.abort();
}
