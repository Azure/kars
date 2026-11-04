// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn output(body: Value) -> ToolCallOutput {
    ToolCallOutput {
        content: vec![ToolContent::text(body.to_string())],
        ..Default::default()
    }
}

fn body(output: &ToolCallOutput) -> Value {
    let ToolContent::Text { text, .. } = &output.content[0] else {
        panic!("expected text")
    };
    serde_json::from_str(text).unwrap()
}

async fn bound_dispatcher(server: &MockServer, scope: &str) -> PlatformDispatcher {
    use crate::memory_binding_loader::{LoadedMemoryBinding, empty_handle};
    let handle = empty_handle();
    *handle.write().await = Ok(Some(LoadedMemoryBinding {
        digest: "test".into(), source_path: "binding.json".into(), store_name: "bound-store".into(),
        scope: scope.into(), shared_scopes: vec![], raw: json!({}),
    }));
    PlatformDispatcher::with_base_url(server.uri()).with_memory_binding(handle)
}

#[tokio::test]
async fn static_search_omits_model_input_and_preserves_custom_scope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/memory_stores/bound-store:search_memories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"memories": []})))
        .expect(1)
        .mount(&server)
        .await;
    let dispatcher = bound_dispatcher(&server, "custom_partition").await;
    let result = dispatcher
        .memory(
            &json!({"operation": "search", "text": "  ", "scope": "custom_partition", "top_k": 20}),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    let requests = server.received_requests().await.unwrap();
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        request,
        json!({"scope": "custom_partition", "options": {"max_memories": 20}})
    );
}

#[tokio::test]
async fn update_waits_for_completed_and_preserves_assistant_role_and_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/memory_stores/bound-store:update_memories"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"update_id": "u-123", "status": "queued"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/memory_stores/bound-store/updates/u-123"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "completed", "usage": {"total_tokens": 17}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dispatcher = bound_dispatcher(&server, "handoff_partition").await;
    let result = dispatcher.memory(&json!({"operation": "update", "text": "handoff context", "role": "assistant", "scope": "handoff_partition"})).await.unwrap();
    assert!(!result.is_error);
    assert_eq!(
        body(&result),
        json!({"status": "completed", "update_id": "u-123", "usage": {"total_tokens": 17}, "backend": "foundry"})
    );
    let requests = server.received_requests().await.unwrap();
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(request["items"][0]["role"], "assistant");
    assert_eq!(request["scope"], "handoff_partition");
    assert_eq!(
        requests[1].url.query(),
        Some("api-version=2025-11-15-preview")
    );
    assert_eq!(requests[1].headers["x-kars-platform-mcp"], "1");
}

#[tokio::test]
async fn failed_update_is_an_error_not_successful_persistence() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/memory_stores/store/updates/u-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"status": "failed", "error": {"code": "InternalServerError"}}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dispatcher = PlatformDispatcher::with_base_url(server.uri());
    let result = dispatcher
        .observe_memory_update(
            "store",
            output(json!({"update_id": "u-1", "status": "queued"})),
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await;
    assert!(result.is_error);
    assert_eq!(body(&result)["update_id"], "u-1");
    assert_eq!(body(&result)["error"]["code"], "InternalServerError");
}

#[tokio::test]
async fn in_progress_update_can_complete_without_repeating_write() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    Mock::given(method("GET"))
        .and(path("/memory_stores/store/updates/u-1"))
        .respond_with(move |_: &wiremock::Request| {
            let status = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                "in_progress"
            } else {
                "completed"
            };
            ResponseTemplate::new(200).set_body_json(json!({"status": status}))
        })
        .expect(2)
        .mount(&server)
        .await;
    let dispatcher = PlatformDispatcher::with_base_url(server.uri());
    let result = dispatcher
        .observe_memory_update(
            "store",
            output(json!({"update_id": "u-1", "status": "queued"})),
            tokio::time::Instant::now() + Duration::from_secs(3),
        )
        .await;
    assert!(!result.is_error);
    assert_eq!(body(&result)["status"], "completed");
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn expired_poll_budget_returns_pending_without_claiming_completion() {
    let server = MockServer::start().await;
    let dispatcher = PlatformDispatcher::with_base_url(server.uri());
    let pending = json!({"update_id": "u-1", "status": "queued"});
    let result = dispatcher
        .observe_memory_update(
            "store",
            output(pending.clone()),
            tokio::time::Instant::now(),
        )
        .await;
    assert!(!result.is_error);
    assert_eq!(body(&result), pending);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn slow_status_request_cannot_exceed_poll_deadline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(json!({"status": "completed"})),
        )
        .mount(&server)
        .await;
    let dispatcher = PlatformDispatcher::with_base_url(server.uri());
    let pending = json!({"update_id": "u-1", "status": "queued"});
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        dispatcher.observe_memory_update(
            "store",
            output(pending.clone()),
            tokio::time::Instant::now() + Duration::from_millis(25),
        ),
    )
    .await
    .unwrap();
    assert_eq!(body(&result), pending);
}

#[tokio::test]
async fn status_authorization_failure_preserves_accepted_update_identity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_string("denied"))
        .expect(1)
        .mount(&server)
        .await;
    let dispatcher = PlatformDispatcher::with_base_url(server.uri());
    let result = dispatcher
        .observe_memory_update(
            "store",
            output(json!({"update_id": "u-1", "status": "queued"})),
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await;
    assert!(result.is_error);
    assert!(format!("{:?}", result.content).contains("403"));
    assert!(format!("{:?}", result.content).contains("u-1"));
    assert!(format!("{:?}", result.content).contains("do not resubmit"));
}

#[tokio::test]
async fn malformed_status_response_preserves_accepted_update_identity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
        .expect(1)
        .mount(&server)
        .await;
    let dispatcher = PlatformDispatcher::with_base_url(server.uri());
    let result = dispatcher
        .observe_memory_update(
            "store",
            output(json!({"update_id": "u-1", "status": "queued"})),
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await;
    assert!(result.is_error);
    assert!(format!("{:?}", result.content).contains("u-1"));
    assert!(format!("{:?}", result.content).contains("do not resubmit"));
}

#[tokio::test]
async fn invalid_update_responses_fail_without_untrusted_status_requests() {
    let server = MockServer::start().await;
    let dispatcher = PlatformDispatcher::with_base_url(server.uri());
    for value in [
        json!(null),
        json!({"status": "queued"}),
        json!({"update_id": "../foreign", "status": "queued"}),
        json!({"update_id": "u-1", "status": "unknown"}),
    ] {
        let result = dispatcher
            .observe_memory_update(
                "store",
                output(value),
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await;
        assert!(result.is_error);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(
        dispatcher
            .memory(&json!({"operation": "update", "text": "x", "role": "system"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn provision_retry_observes_terminal_failure_and_uses_memory_models() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    Mock::given(method("POST"))
        .and(path("/memory_stores/store:update_memories"))
        .respond_with(move |_: &wiremock::Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(404)
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(json!({"update_id": "u-1", "status": "queued"}))
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/memory_stores"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/memory_stores/store/updates/u-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "failed", "error": {"code": "model_failure"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut dispatcher =
        PlatformDispatcher::with_base_url(server.uri()).with_memory_store_id("store");
    dispatcher.memory_chat_model = "memory-chat-deployment".into();
    dispatcher.memory_embedding_model = "memory-embedding-deployment".into();
    let result = dispatcher
        .memory(&json!({"operation": "update", "text": "new memory"}))
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(body(&result)["status"], "failed");
    let requests = server.received_requests().await.unwrap();
    let create: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(create["definition"]["chat_model"], "memory-chat-deployment");
    assert_eq!(
        create["definition"]["embedding_model"],
        "memory-embedding-deployment"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn backend_selection_is_explicit_and_pinned_without_provider_inference() {
    for (project, bound, expected) in [(false, false, false), (true, false, true), (false, true, true)] {
        let mut dispatcher = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        dispatcher.memory_foundry_configured = project;
        assert_eq!(dispatcher.select_memory_backend(bound, false).await.unwrap(), expected);
        dispatcher.memory_foundry_configured = !expected;
        assert!(dispatcher.select_memory_backend(false, false).await.is_err());
        assert_eq!(dispatcher.memory_backend.get(), Some(&expected));
    }
    let mut dispatcher = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
    dispatcher.memory_foundry_configured = false;
    assert!(dispatcher.select_memory_backend(false, true).await.is_err());
    assert!(dispatcher.memory_backend.get().is_none());
}

#[tokio::test]
async fn configured_foundry_outage_never_switches_to_builtin() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(503))
        .mount(&server).await;
    let dispatcher = bound_dispatcher(&server, "handoff_partition").await;
    let result = dispatcher.memory(&json!({"operation":"search"})).await.unwrap();
    assert!(result.is_error);
    assert_eq!(dispatcher.memory_backend.get(), Some(&true));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
