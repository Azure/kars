// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::memory_binding_loader::LoadedMemoryBinding;
use crate::providers::{AuditError, AuditEvent, AuditReceipt, AuditSink, ReceiptId};
use serde_json::Value;

async fn memory_state(fixture: &Fixture) -> AppState {
    let mut state = endpoint_state(fixture, true).await;
    state.auth = Arc::new(WorkloadIdentityAuth::for_test(
        Some("memory-test-key"),
        None,
    ));
    Arc::get_mut(&mut state.config)
        .unwrap()
        .foundry_project_endpoint = Some(fixture.provider.uri());
    *state.memory_binding.write().await = Ok(Some(LoadedMemoryBinding {
        digest: format!("sha256:{}", "b".repeat(64)),
        source_path: "binding.json".into(),
        store_name: "bound".into(),
        scope: "agent_task".into(),
        shared_scopes: vec!["team_beta".into()],
        raw: json!({}),
    }));
    state
}

fn app(state: AppState) -> Router {
    Router::new()
        .merge(crate::routes::foundry_standalone_routes())
        .merge(crate::routes::foundry_agent_routes())
        .with_state(state)
}

async fn send(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

async fn authority(fixture: &Fixture, unbounded: bool, money_required: bool) {
    let catalog = fixture.client.catalog().await.unwrap();
    fixture._broker.reset().await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/catalog"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer private-test-token",
        ))
        .and(wiremock::matchers::body_partial_json(json!({"payload": {
            "accountUid":"account-uid", "identity":{"taskUid":"task-uid", "sandbox":{"name":"task"}}
        }})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalog":catalog, "unboundedTokens":unbounded, "moneyRequired":money_required,
            "memoryServiceAllowed":unbounded && !money_required
        })))
        .mount(&fixture._broker)
        .await;
}

#[tokio::test]
async fn memory_service_accepts_only_current_conventional_private_scope() {
    let fixture = Fixture::new(100, false).await;
    authority(&fixture, true, false).await;
    let state = memory_state(&fixture).await;
    *state.memory_binding.write().await = Ok(None);
    let binding = state.memory_binding.clone();
    let access = crate::memory_binding_loader::resolve_access(
        Some(&binding),
        &state.sandbox_name,
        &format!("memory-{}", state.sandbox_name),
    )
    .await
    .unwrap();
    let path = format!("/memory_stores/{}:search_memories", access.store_name);
    Mock::given(wiremock::matchers::path(path.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"memories":[]})))
        .expect(1)
        .mount(&fixture.provider)
        .await;
    let governance = state.governance.clone();
    let application = app(state);
    assert_eq!(
        send(&application, "POST", &path, json!({"scope":access.scope}))
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        governance
            .audit_json()
            .to_string()
            .contains("private:sha256:")
    );
    assert_eq!(
        send(
            &application,
            "POST",
            &path,
            json!({"scope":"agent_foreign"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    *binding.write().await = Err("required binding revoked".into());
    assert_eq!(
        send(&application, "POST", &path, json!({"scope":access.scope}))
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn memory_service_uses_explicit_broker_capability_not_calendar_override() {
    for allowed in [None, Some(false), Some(true)] {
        let fixture = Fixture::new(100, false).await;
        let catalog = fixture.client.catalog().await.unwrap();
        fixture._broker.reset().await;
        let mut response = json!({"catalog":catalog, "unboundedTokens":allowed != Some(true), "moneyRequired":false});
        if let Some(allowed) = allowed {
            response["memoryServiceAllowed"] = json!(allowed);
        }
        Mock::given(wiremock::matchers::path("/v1/catalog"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(&fixture._broker)
            .await;
        Mock::given(wiremock::matchers::path(
            "/memory_stores/bound:search_memories",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"memories":[]})))
        .mount(&fixture.provider)
        .await;
        let state = memory_state(&fixture).await;
        let status = send(
            &app(state),
            "POST",
            "/memory_stores/bound:search_memories",
            json!({"scope":"agent_task"}),
        )
        .await
        .0;
        assert_eq!(
            status,
            if allowed == Some(true) {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(
            fixture.provider.received_requests().await.unwrap().len(),
            usize::from(allowed == Some(true))
        );
    }
}

#[tokio::test]
async fn memory_service_requires_current_unbounded_authority_without_money_ceiling() {
    for (unbounded, money_required, allowed) in [
        (false, false, false),
        (false, true, false),
        (true, true, false),
        (true, false, true),
    ] {
        let fixture = Fixture::new(100, false).await;
        authority(&fixture, unbounded, money_required).await;
        Mock::given(wiremock::matchers::path(
            "/memory_stores/bound:search_memories",
        ))
        .and(wiremock::matchers::header("api-key", "memory-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"memories":[]})))
        .mount(&fixture.provider)
        .await;
        let state = memory_state(&fixture).await;
        let governance = state.governance.clone();
        let ledger_before = serde_json::to_value(&*fixture.ledger.lock().unwrap()).unwrap();
        let (status, _) = send(
            &app(state),
            "POST",
            "/memory_stores/bound:search_memories",
            json!({"scope":"agent_task","items":[{"role":"user","content":"private preference"}]}),
        )
        .await;
        assert_eq!(
            status,
            if allowed {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(
            fixture.provider.received_requests().await.unwrap().len(),
            usize::from(allowed)
        );
        assert_eq!(
            serde_json::to_value(&*fixture.ledger.lock().unwrap()).unwrap(),
            ledger_before,
            "Memory must not invent an inference settlement or zero usage"
        );
        let audit = governance.audit_json().to_string();
        assert!(!audit.contains("private preference"));
        if allowed {
            assert!(audit.contains("dispatch_intent"));
            assert!(audit.contains("response_received"));
            assert!(audit.contains("unreported"));
            assert!(audit.contains("external_service_not_inference_settlement"));
        }
    }
}

#[tokio::test]
async fn memory_service_allowance_is_revocable_and_never_covers_other_foundry_routes() {
    let fixture = Fixture::new(100, false).await;
    authority(&fixture, true, false).await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"memories":[]})))
        .mount(&fixture.provider)
        .await;
    let app = app(memory_state(&fixture).await);
    let body = json!({"scope":"agent_task","query":"preference"});
    assert_eq!(
        send(
            &app,
            "POST",
            "/memory_stores/bound:search_memories",
            body.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    for path in [
        "/agents",
        "/evaluations",
        "/knowledgebases",
        "/memory_stores/foreign:search_memories",
    ] {
        assert_eq!(
            send(&app, "POST", path, body.clone()).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    authority(&fixture, false, false).await;
    assert_eq!(
        send(
            &app,
            "POST",
            "/memory_stores/bound:search_memories",
            body.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    fixture._broker.reset().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&fixture._broker)
        .await;
    assert_eq!(
        send(&app, "POST", "/memory_stores/bound:search_memories", body)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn memory_service_denies_missing_binding_foreign_scope_update_and_identity() {
    for case in [
        "missing",
        "failed",
        "foreign-scope",
        "foreign-store",
        "unknown-update",
        "identity",
        "encoded-path",
    ] {
        let fixture = Fixture::new(100, false).await;
        authority(&fixture, true, false).await;
        let mut state = memory_state(&fixture).await;
        let mut path = "/memory_stores/bound:search_memories";
        let mut method = "POST";
        let mut scope = "agent_task";
        match case {
            "missing" => *state.memory_binding.write().await = Ok(None),
            "failed" => *state.memory_binding.write().await = Err("revoked".into()),
            "foreign-scope" => scope = "agent_other",
            "foreign-store" => path = "/memory_stores/foreign:search_memories",
            "unknown-update" => {
                path = "/memory_stores/bound/updates/unknown";
                method = "GET";
            }
            "identity" => state.sandbox_name = Arc::new("other".into()),
            "encoded-path" => path = "/memory_stores/bound%2f..%2fagents:search_memories",
            _ => unreachable!(),
        }
        let status = send(
            &app(state),
            method,
            path,
            json!({"scope":scope,"query":"text"}),
        )
        .await
        .0;
        assert!(
            status.is_client_error() || status == StatusCode::SERVICE_UNAVAILABLE,
            "{case}: {status}"
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
}

#[tokio::test]
async fn memory_service_update_polling_keeps_ownership_and_real_provider_usage() {
    let fixture = Fixture::new(100, false).await;
    authority(&fixture, true, false).await;
    Mock::given(wiremock::matchers::path(
        "/memory_stores/bound:update_memories",
    ))
    .respond_with(
        ResponseTemplate::new(202)
            .set_body_json(json!({"update_id":"owned-update","status":"queued"})),
    )
    .expect(1)
    .mount(&fixture.provider)
    .await;
    Mock::given(wiremock::matchers::path("/memory_stores/bound/updates/owned-update"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status":"completed","usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15,"private":"never-audit"}
        }))).expect(1).mount(&fixture.provider).await;
    let state = memory_state(&fixture).await;
    let governance = state.governance.clone();
    let binding = state.memory_binding.clone();
    let app = app(state);
    let response = send(
        &app,
        "POST",
        "/memory_stores/bound:update_memories",
        json!({"scope":"team_beta","items":[]}),
    )
    .await;
    assert_eq!(
        response,
        (
            StatusCode::ACCEPTED,
            json!({"update_id":"owned-update","status":"queued"})
        )
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/memory_stores/bound/updates/owned-update",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    binding
        .write()
        .await
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .shared_scopes
        .clear();
    assert_eq!(
        send(
            &app,
            "GET",
            "/memory_stores/bound/updates/owned-update",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let audit = governance.audit_json().to_string();
    assert!(audit.contains("provider_reported_unsettled"));
    assert!(audit.contains("total_tokens"));
    assert!(!audit.contains("never-audit"));
    assert_eq!(fixture.provider.received_requests().await.unwrap().len(), 2);
}

struct FailedAudit;
#[async_trait::async_trait]
impl AuditSink for FailedAudit {
    async fn append(&self, _: AuditEvent) -> Result<AuditReceipt, AuditError> {
        Err(AuditError::QueueFull)
    }
    async fn get(&self, _: &ReceiptId) -> Result<Option<AuditEvent>, AuditError> {
        Ok(None)
    }
}

#[tokio::test]
async fn memory_service_never_sends_when_dispatch_audit_cannot_be_recorded() {
    let fixture = Fixture::new(100, false).await;
    authority(&fixture, true, false).await;
    let mut state = memory_state(&fixture).await;
    state.audit_sink = Arc::new(FailedAudit);
    let status = send(
        &app(state),
        "POST",
        "/memory_stores/bound:search_memories",
        json!({"scope":"agent_task","query":"text"}),
    )
    .await
    .0;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        fixture
            .provider
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
}
