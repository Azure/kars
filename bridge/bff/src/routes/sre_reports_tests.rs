// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    response::Response,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::{ServiceExt, service_fn};

const SECRET: &str = "test-only-sre-report-principal";
const REG: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const CM: &str = "/api/v1/namespaces/kars-sre/configmaps/sre-health-report";

fn registration() -> Value {
    json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
        "metadata":{"name":"canonical","uid":"reg","resourceVersion":"1","generation":1},
        "spec":{"enabled":true,"sandbox":{"namespace":"kars-system","name":"sre","uid":"source"},
            "runtimeNamespace":{"name":"kars-sre","uid":"ns"}},
        "status":{"phase":"Ready","observedGeneration":1,"privacyEpoch":"epoch",
            "legacySecretAccessDenied":true,"privacyRevision":crate::sre_privacy::REVISION}})
}

fn target() -> Value {
    let identity = Identity::from_registration(&registration()).unwrap();
    let mut target = identity.target();
    target["metadata"]["uid"] = json!("cm");
    target["metadata"]["resourceVersion"] = json!("1");
    let now = chrono::Utc::now().timestamp_millis() as u64;
    let input = sre_report::Input {
        sweep_id: "a".repeat(32),
        started_at_ms: now - 1000,
        finished_at_ms: now,
        turn_status: "blocked".into(),
        text: "Budget gate blocked; diagnostic coverage unknown".into(),
        truncated: false,
        interval_seconds: 900,
        timeout_seconds: 180,
    };
    let report = Report {
        version: 1,
        identity,
        privacy_epoch: "epoch".into(),
        accepted_at_ms: now,
        digest: input.digest(),
        report: input,
    };
    target["data"] = json!({(sre_report::KEY):serde_json::to_string(&report).unwrap()});
    target
}

struct Store {
    reg: Value,
    target: Option<Value>,
    calls: Vec<String>,
    failure: bool,
    race: bool,
    replaced: bool,
}

fn fixture() -> (AppState, Arc<Mutex<Store>>) {
    let store = Arc::new(Mutex::new(Store {
        reg: registration(),
        target: Some(target()),
        calls: vec![],
        failure: false,
        race: false,
        replaced: false,
    }));
    let data = store.clone();
    let service = service_fn(move |request: Request<kube::client::Body>| {
        let data = data.clone();
        async move {
            assert_eq!(request.method(), Method::GET);
            let mut store = data.lock().unwrap();
            let path = request.uri().path();
            store.calls.push(path.into());
            let (status, value) = if store.failure {
                (
                    403,
                    json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":403,"reason":"Forbidden","message":"private upstream error"}),
                )
            } else {
                match path {
                    REG => {
                        if store.race
                            && store
                                .calls
                                .iter()
                                .filter(|path| path.as_str() == REG)
                                .count()
                                > 1
                        {
                            store.reg["spec"]["enabled"] = json!(false);
                        }
                        (200, store.reg.clone())
                    }
                    CM => match &store.target {
                        Some(value) => (200, value.clone()),
                        None => (
                            404,
                            json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":404,"reason":"NotFound","message":"missing"}),
                        ),
                    },
                    "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre" => (
                        200,
                        json!({
                    "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox","metadata":{"name":"sre","namespace":"kars-system",
                        "uid":if store.replaced {"replacement"}else{"source"},"annotations":{"kars.azure.com/namespace-uid":"ns"}}}),
                    ),
                    "/api/v1/namespaces/kars-sre" => (
                        200,
                        json!({"apiVersion":"v1","kind":"Namespace",
                    "metadata":{"name":"kars-sre","uid":"ns","annotations":{
                        "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"kars-system",
                        "kars.azure.com/sandbox-name":"sre","kars.azure.com/sandbox-uid":"source"}}}),
                    ),
                    _ => panic!("unexpected report request: {path}"),
                }
            };
            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
            )
        }
    });
    (
        AppState::for_test_client(kube::Client::new(service, "kars-system"), "kars-system")
            .with_principal_secret(Some(SECRET.into())),
        store,
    )
}

async fn request(
    state: AppState,
    role: Option<&str>,
    valid_signature: bool,
    method: Method,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri("/api/operator/sre-report");
    if let Some(role) = role {
        let token=jsonwebtoken::encode(&jsonwebtoken::Header::default(),
            &json!({"sub":"reader","name":"Reader","roles":[role],"exp":chrono::Utc::now().timestamp()+3600}),
            &jsonwebtoken::EncodingKey::from_secret(if valid_signature {SECRET.as_bytes()}else{b"wrong-principal-key"})).unwrap();
        request = request.header(crate::auth::PRINCIPAL_HEADER, token);
    }
    let response = crate::routes::router(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::require_token,
        ))
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn report_reads_require_authenticated_operator_before_any_cluster_access() {
    for (role, signature, expected) in [
        (None, true, 401),
        (Some("operator"), false, 401),
        (Some("user"), true, 403),
        (Some("viewer"), true, 403),
        (Some("auditor"), true, 403),
    ] {
        let (state, store) = fixture();
        assert_eq!(
            request(state, role, signature, Method::GET)
                .await
                .0
                .as_u16(),
            expected
        );
        assert!(store.lock().unwrap().calls.is_empty());
    }
    for role in ["operator", "admin"] {
        let (state, _) = fixture();
        let (status, body) = request(state, Some(role), true, Method::GET).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "observed");
        assert_eq!(body["latest"]["report"]["turn_status"], "blocked");
    }
}

#[tokio::test]
async fn report_reads_hide_revoked_replaced_invalid_and_unavailable_evidence() {
    for case in [
        "revoked",
        "replaced",
        "race",
        "unavailable",
        "missing",
        "invalid",
    ] {
        let (state, store) = fixture();
        {
            let mut store = store.lock().unwrap();
            match case {
                "revoked" => store.reg["spec"]["enabled"] = json!(false),
                "replaced" => store.replaced = true,
                "race" => store.race = true,
                "unavailable" => store.failure = true,
                "missing" => store.target = None,
                _ => store.target.as_mut().unwrap()["data"][sre_report::KEY] = json!("invalid"),
            }
        }
        let (status, body) = request(state, Some("operator"), true, Method::GET).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], if case == "race" { "revoked" } else { case });
        assert!(body["latest"].is_null());
        assert!(!body.to_string().contains("private upstream error"));
    }
}

#[test]
fn report_classifier_retains_stale_blocked_status_and_rejects_old_source_or_corruption() {
    let reg = registration();
    let identity = Identity::from_registration(&reg).unwrap();
    let mut target = target();
    let now = chrono::Utc::now().timestamp_millis() as u64;
    let dto = classify(&reg, &identity, Some(&target), now + 1_200_000);
    assert_eq!(dto.state, "stale");
    assert_eq!(dto.latest.unwrap().report.turn_status, "blocked");
    let mut reg = reg;
    reg["status"]["privacyEpoch"] = json!("new-epoch");
    assert_eq!(classify(&reg, &identity, Some(&target), now).state, "stale");
    target["metadata"]["annotations"][sre_report::SOURCE] = json!("replacement");
    assert_eq!(
        classify(&reg, &identity, Some(&target), now).state,
        "replaced"
    );
}

#[tokio::test]
async fn report_operator_route_never_accepts_writes() {
    let (state, store) = fixture();
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        assert_eq!(
            request(state.clone(), Some("operator"), true, method)
                .await
                .0,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    assert!(store.lock().unwrap().calls.is_empty());
}
