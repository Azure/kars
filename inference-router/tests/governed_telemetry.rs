// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "common/governed_services.rs"]
mod support;
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use kars_inference_router::{
    auth::WorkloadIdentityAuth,
    config::ProviderEndpoint,
    proxy::{self, AuthenticationProvenance, UpstreamConfig},
    routes::{self, McpRouteState},
    task_telemetry::{TaskTelemetry, observe::wrap_stream},
};
use serde_json::{Value, json};
use std::sync::Arc;
use support::*;
use tower::ServiceExt;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

fn telemetry() -> Arc<TaskTelemetry> {
    Arc::new(TaskTelemetry::new("scope-a".into()))
}
fn upstream(endpoint: String, telemetry: Arc<TaskTelemetry>) -> UpstreamConfig {
    let mut target = UpstreamConfig::azure(endpoint, "model".into(), "test".into());
    target.authentication = AuthenticationProvenance::Named {
        provider_id: "qualified-provider".into(),
    };
    target.telemetry = Some(telemetry);
    target
}
fn events(telemetry: &TaskTelemetry) -> Vec<Value> {
    telemetry.snapshot("scope-a", 0).unwrap()["events"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn buffered_http_producer_observes_usage_but_never_api_bodies_or_tool_arguments() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "choices":[{"finish_reason":"tool_calls","message":{"content":"output-secret-marker","tool_calls":[
            {"id":"call-1","function":{"name":"fetch","arguments":"argument-secret-marker"}}
        ]}}],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}
    }))).mount(&server).await;
    let telemetry = telemetry();
    let target = upstream(server.uri(), telemetry.clone());
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let (_, _, body) = proxy::forward(
        &WorkloadIdentityAuth::new(),
        None,
        &client,
        &target,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from(r#"{"messages":[{"role":"user","content":"prompt-secret-marker"}]}"#),
    )
    .await
    .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("output-secret-marker"));
    let trace = events(&telemetry);
    assert_eq!(trace[0]["usage"]["total_tokens"], 18);
    assert_eq!(trace[0]["provider"], "qualified-provider");
    assert_eq!(trace[1]["kind"], "tool_proposed");
    assert!(trace[1]["ok"].is_null());
    proxy::forward(&WorkloadIdentityAuth::new(),None,&client,&target,Method::POST,
        "chat/completions",&HeaderMap::new(),Bytes::from(r#"{"messages":[{"role":"tool","tool_call_id":"call-1","content":"result-secret-marker","is_error":true}]}"#)).await.unwrap();
    let trace = events(&telemetry);
    assert!(trace.iter().any(|event| event["kind"] == "tool_result"
        && event["ok"] == false
        && event["source"] == "harness-reported"));
    let serialized = serde_json::to_string(&trace).unwrap();
    for secret in [
        "prompt-secret-marker",
        "output-secret-marker",
        "argument-secret-marker",
        "result-secret-marker",
    ] {
        assert!(!serialized.contains(secret));
    }
}

#[tokio::test]
async fn buffered_http_200_responses_distinguish_failed_and_incomplete_model_outcomes() {
    for (status, outcome) in [("failed", "upstream_error"), ("incomplete", "incomplete")] {
        let server = MockServer::start().await;
        let body = json!({
            "status":status,"error":{"message":"credential-secret-marker"},
            "incomplete_details":{"reason":"private-details-marker"},
            "usage":if status=="failed"{json!({"input_tokens":7})}else{Value::Null},
            "output":[],
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
            .expect(1)
            .mount(&server)
            .await;
        let telemetry = telemetry();
        let target = upstream(server.uri(), telemetry.clone());
        let (http, _, bytes) = proxy::forward(
            &WorkloadIdentityAuth::new(),
            None,
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            &target,
            Method::POST,
            "responses",
            &HeaderMap::new(),
            Bytes::from("{}"),
        )
        .await
        .unwrap();
        assert_eq!(http, StatusCode::OK);
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), body);
        let trace = events(&telemetry);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0]["accepted"], true);
        assert_eq!(trace[0]["outcome"], outcome);
        assert_eq!(trace[0]["finish_reason"], status);
        assert_eq!(
            trace[0]["usage_state"],
            if status == "failed" {
                "partial"
            } else {
                "missing"
            }
        );
        assert!(trace[0]["usage"]["completion_tokens"].is_null());
        assert!(trace[0]["usage"]["total_tokens"].is_null());
        assert!(!trace[0].to_string().contains("secret-marker"));
        assert!(!trace[0].to_string().contains("private-details-marker"));
    }
}

#[tokio::test]
async fn fragmented_openai_error_frames_cannot_be_overwritten_by_done() {
    for error in [
        "data: {\"error\":{\"message\":\"credential-secret-marker🚫\"}}\n\n",
        "event: error\ndata: {\"message\":\"credential-secret-marker🚫\"}\n\n",
    ] {
        let body = format!(
            "data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":3}}}}\n\n{error}data: [DONE]\n\n"
        );
        let telemetry = telemetry();
        let mut observation = telemetry
            .begin("chat/completions", "provider", "model", b"{}")
            .unwrap();
        observation.headers(200);
        let chunks = body
            .as_bytes()
            .iter()
            .map(|byte| Ok::<_, reqwest::Error>(Bytes::from(vec![*byte])))
            .collect::<Vec<_>>();
        let returned = wrap_stream(
            futures::stream::iter(chunks).boxed(),
            Some(observation),
            true,
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        assert_eq!(returned.concat(), body.as_bytes());
        let trace = events(&telemetry);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0]["accepted"], true);
        assert_eq!(trace[0]["outcome"], "upstream_error");
        assert_eq!(trace[0]["usage"]["prompt_tokens"], 3);
        assert!(trace[0]["usage"]["completion_tokens"].is_null());
        assert!(trace[0]["usage"]["total_tokens"].is_null());
        assert!(!trace[0].to_string().contains("credential-secret-marker"));
    }
}

#[tokio::test]
async fn responses_incomplete_stream_is_not_reported_as_a_complete_model_response() {
    let telemetry = telemetry();
    let mut observation = telemetry
        .begin("responses", "provider", "model", b"{}")
        .unwrap();
    observation.headers(200);
    let body = "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2},\"output\":[]}}\n\n";
    let returned = wrap_stream(
        futures::stream::once(async move {
            Ok::<_, reqwest::Error>(Bytes::from_static(body.as_bytes()))
        })
        .boxed(),
        Some(observation),
        true,
    )
    .try_collect::<Vec<_>>()
    .await
    .unwrap();
    assert_eq!(returned.concat(), body.as_bytes());
    let trace = events(&telemetry);
    assert_eq!(trace[0]["accepted"], true);
    assert_eq!(trace[0]["outcome"], "incomplete");
    assert_eq!(trace[0]["usage_state"], "present");
    assert!(trace[0]["usage"]["total_tokens"].is_null());
}

#[tokio::test]
async fn fragmented_streams_preserve_bytes_and_report_each_supported_usage_shape_once() {
    let cases = [
        (
            "chat/completions",
            concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"🚀stream-secret-marker\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":5,\"total_tokens\":9}}\n\n",
                "data: [DONE]\n\n"
            ),
            4,
            5,
            Some(9),
        ),
        (
            "v1/messages",
            concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":3}}}\n\n",
                "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":8},\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            ),
            12,
            8,
            None,
        ),
        (
            "responses",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5},\"output\":[]}}\n\n",
            2,
            3,
            Some(5),
        ),
    ];
    for (path, body, input, output, total) in cases {
        let telemetry = telemetry();
        let mut observation = telemetry.begin(path, "provider", "model", b"{}").unwrap();
        observation.headers(200);
        let chunks = body
            .as_bytes()
            .iter()
            .map(|byte| Ok::<_, reqwest::Error>(Bytes::from(vec![*byte])))
            .collect::<Vec<_>>();
        let bytes = wrap_stream(
            futures::stream::iter(chunks).boxed(),
            Some(observation),
            true,
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        assert_eq!(bytes.concat(), body.as_bytes());
        let events = events(&telemetry);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["usage"]["prompt_tokens"], input);
        assert_eq!(events[0]["usage"]["completion_tokens"], output);
        assert_eq!(events[0]["usage"]["total_tokens"], json!(total));
        assert_eq!(events[0]["outcome"], "complete");
        assert!(!events[0].to_string().contains("stream-secret-marker"));
    }
}

#[tokio::test]
async fn streaming_http_producer_reports_missing_usage_and_no_replay_after_incomplete_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"private\"}}]}\n\n",
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let telemetry = telemetry();
    let (_, _, stream) = proxy::forward_stream(
        Arc::new(WorkloadIdentityAuth::new()),
        None,
        reqwest::Client::builder().no_proxy().build().unwrap(),
        upstream(server.uri(), telemetry.clone()),
        "chat/completions",
        HeaderMap::new(),
        Bytes::from(r#"{"stream":true}"#),
    )
    .await
    .unwrap();
    let _: Vec<_> = stream.try_collect().await.unwrap();
    let events = events(&telemetry);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["outcome"], "incomplete");
    assert_eq!(events[0]["accepted"], true);
    assert_eq!(events[0]["usage_state"], "missing");
    assert!(events[0]["usage"]["total_tokens"].is_null());
}

#[tokio::test]
async fn reset_discards_old_inflight_observations_and_ring_overflow_is_explicit() {
    let telemetry = telemetry();
    let mut old = telemetry
        .begin("responses", "provider", "model", b"{}")
        .unwrap();
    telemetry.reset("scope-b".into());
    old.buffered(
        200,
        br#"{"usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14}}"#,
    );
    assert!(telemetry.snapshot("scope-a", 0).is_none());
    assert_eq!(
        telemetry.snapshot("scope-b", 0).unwrap()["events"],
        json!([])
    );
    for _ in 0..1100 {
        telemetry.record_policy("scope-b", "tool:fetch", false);
    }
    let snapshot = telemetry.snapshot("scope-b", 0).unwrap();
    assert_eq!(snapshot["dropped_events"], 76);
    assert_eq!(snapshot["events"].as_array().unwrap().len(), 256);
    assert_eq!(snapshot["has_more"], true);
}

#[tokio::test]
async fn physical_failover_attempts_keep_actual_provider_identity_without_counting_false_successes()
{
    use kars_inference_router::{
        failover::forward_with_failover,
        inference_policy_loader::{InferencePolicySnapshot, ModelPreference, ModelRef},
    };
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(json!({"error":{"message":"secret-error-body"}})),
        )
        .expect(1)
        .mount(&first)
        .await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}))).expect(1).mount(&second).await;
    let state = state("workspace", "uid");
    let mut config = Arc::try_unwrap(state.config)
        .ok()
        .expect("fixture config is uniquely owned");
    for (id, endpoint) in [("first", first.uri()), ("second", second.uri())] {
        config.providers.insert(
            id.into(),
            ProviderEndpoint {
                tag: id.into(),
                endpoint,
                api_key: None,
            },
        );
    }
    let telemetry = telemetry();
    let base = upstream("http://127.0.0.1:1".into(), telemetry.clone());
    let policy = InferencePolicySnapshot {
        model_preference: Some(ModelPreference {
            primary: ModelRef {
                provider: "first".into(),
                deployment: "same-model".into(),
            },
            fallback: vec![ModelRef {
                provider: "second".into(),
                deployment: "same-model".into(),
            }],
        }),
        ..Default::default()
    };
    let result = forward_with_failover(
        &state.auth,
        None,
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &state.deployment_health,
        &base,
        &config,
        &policy,
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await
    .unwrap();
    assert_eq!(result.0, StatusCode::OK);
    let events = events(&telemetry);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["provider"], "first");
    assert_eq!(events[0]["outcome"], "http_error");
    assert_eq!(events[1]["provider"], "second");
    assert_eq!(events[1]["usage"]["total_tokens"], 3);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("secret-error-body")
    );
}

#[tokio::test]
async fn telemetry_http_requires_scope_and_only_correlated_native_outcomes_are_accepted() {
    let state = state("workspace", "uid");
    let scope = state.services.telemetry.cursor().0;
    let app = app(state.clone());
    assert_eq!(
        send(
            &app,
            request("GET", "/telemetry/trace", json!({}), None, None, true)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let body = json!({"scope_id":scope,"call_id":"call-a","ok":true,"latency_ms":5});
    assert_eq!(
        send(
            &app,
            request("POST", "/telemetry/tool", body.clone(), None, None, true)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status,evaluation)=send(&app,request("POST","/agt/evaluate",
        json!({"agent_id":"test","action":"tool:fetch","context":{
            "scope_id":scope,"tool_call_id":"call-a","tool_name":"spoofed-name","args_preview":"native-secret-marker"
        }}),None,None,true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(evaluation["allowed"], true);
    assert_eq!(
        send(
            &app,
            request("POST", "/telemetry/tool", body.clone(), None, None, true)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            request("POST", "/telemetry/tool", body, None, None, true)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, trace) = send(
        &app,
        request(
            "GET",
            "/telemetry/trace",
            json!({}),
            None,
            Some(&scope),
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        trace["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["source"] == "harness-reported" && event["name"] == "fetch")
    );
    assert!(!trace.to_string().contains("native-secret-marker"));
    assert!(!trace.to_string().contains("spoofed-name"));
    assert_eq!(trace["durable"], false);
}

#[tokio::test]
async fn actual_mcp_http_errors_are_not_mistaken_for_success_and_bodies_are_not_retained() {
    let telemetry = telemetry();
    let mut state = McpRouteState::standard();
    state.task_telemetry = Some(telemetry.clone());
    let app = routes::mcp_route().with_state(state);
    for (id, name) in [(1, "echo"), (2, "missing-tool")] {
        let mut request = Request::post("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":name,"arguments":{"text":"mcp-secret-body"}}})
                .to_string(),
            ))
            .unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                12345,
            ))));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }

    let events = events(&telemetry);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["ok"], true);
    assert_eq!(events[1]["ok"], false);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("mcp-secret-body")
    );
}

#[tokio::test]
async fn mcp_is_error_and_dispatch_transport_failure_are_both_failure_observations() {
    use kars_inference_router::mcp::tools::{
        DispatchError, SyncToAsync, ToolCallOutput, ToolCatalog, ToolDefinition, ToolDispatcher,
    };
    struct Outcomes(ToolCatalog);
    impl ToolDispatcher for Outcomes {
        fn catalog(&self) -> &ToolCatalog {
            &self.0
        }
        fn invoke(&self, name: &str, _args: &Value) -> Result<ToolCallOutput, DispatchError> {
            if name == "tool_error" {
                Ok(ToolCallOutput {
                    content: vec![],
                    is_error: true,
                    ..Default::default()
                })
            } else {
                Err(DispatchError::ExecutionFailed {
                    tool: name.into(),
                    reason: "transport-secret-marker".into(),
                })
            }
        }
    }
    let catalog = ToolCatalog::new(
        ["tool_error", "transport_error"]
            .map(|name| ToolDefinition {
                name: name.into(),
                description: "test".into(),
                input_schema: json!({"type":"object"}),
            })
            .to_vec(),
    )
    .unwrap();
    let telemetry = telemetry();
    let mut state =
        McpRouteState::standard().with_tools(Arc::new(SyncToAsync::new(Outcomes(catalog))));
    state.task_telemetry = Some(telemetry.clone());
    let app = routes::mcp_route().with_state(state);
    for (id, name) in [(1, "tool_error"), (2, "transport_error")] {
        let mut request=Request::post("/mcp").header("content-type","application/json").header("accept","application/json, text/event-stream")
                .body(Body::from(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}}).to_string())).unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                12345,
            ))));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }
    let events = events(&telemetry);
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event["ok"] == false && event["http_status"] == 200)
    );
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("transport-secret-marker")
    );
}

#[tokio::test]
async fn transport_failures_and_cancelled_partial_streams_never_become_success() {
    let telemetry = telemetry();
    let result = proxy::forward(
        &WorkloadIdentityAuth::new(),
        None,
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &upstream("http://127.0.0.1:1".into(), telemetry.clone()),
        Method::POST,
        "chat/completions",
        &HeaderMap::new(),
        Bytes::from("{}"),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(events(&telemetry)[0]["outcome"], "transport_error");
    let mut observation = telemetry
        .begin("v1/messages", "provider", "model", b"{}")
        .unwrap();
    observation.headers(200);
    let chunks = futures::stream::once(async {
        Ok::<_, reqwest::Error>(Bytes::from_static(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
        ))
    })
    .chain(futures::stream::pending())
    .boxed();
    let mut wrapped = wrap_stream(chunks, Some(observation), true);
    wrapped.next().await.unwrap().unwrap();
    drop(wrapped);
    let trace = events(&telemetry);
    assert_eq!(trace[1]["outcome"], "cancelled");
    assert_eq!(trace[1]["usage"]["prompt_tokens"], 12);
    assert!(trace[1]["usage"]["completion_tokens"].is_null());
}
