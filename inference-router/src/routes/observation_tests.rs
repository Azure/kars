// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    access_request::Identity,
    governed_services::GovernedServices,
    service_observation::Observer,
    service_observer::{Binding, Grant, Recipient},
};
use axum::{body::Body, http::Request};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "observation_privacy_tests.rs"]
mod privacy;

const SANDBOX: &str = "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karssandboxes/agent";
const GRANT: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karscredentialgrants/workspace";
const REGISTRATION: &str = "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical";
const RECIPIENT: &str = "/api/v1/namespaces/bridge/serviceaccounts/bff";
const REVIEWS: &str = "/apis/authorization.k8s.io/v1/subjectaccessreviews";

#[derive(Default)]
struct Metadata {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, Value)>,
    allow: Option<String>,
    fail: Option<String>,
    verifier: Option<Arc<crate::observation_privacy_client::tests::Verifier>>,
}

fn observer_token() -> String {
    "o".repeat(64)
}
fn control_token() -> String {
    "c".repeat(64)
}

async fn fixture() -> (MockServer, AppState, Arc<Mutex<Metadata>>) {
    let server = MockServer::start().await;
    let verifier = crate::observation_privacy_client::tests::Verifier::start().await;
    let identity: Identity = serde_json::from_value(json!({
        "sandbox":{"namespace":"workspace","name":"agent","uid":"sandbox-uid"},
        "namespace_uid":"runtime-uid","task":null,"task_authorization":null,
        "task_generation":null,"managed":true
    }))
    .unwrap();
    let binding = Binding {
        capability: CAPABILITY.into(),
        identity: serde_json::to_value(&identity).unwrap(),
        grant: Grant {
            namespace: "workspace".into(),
            name: "workspace".into(),
            uid: "grant-uid".into(),
            generation: 1,
        },
        recipients: vec![Recipient {
            namespace: "bridge".into(),
            namespace_uid: "bridge-uid".into(),
            name: "bff".into(),
            uid: "bff-uid".into(),
        }],
        privacy_revision: crate::sre_privacy::REVISION.into(),
        privacy_epoch: None,
        server_name: "observer-sandbox-uid.kars.internal".into(),
        ca_pem: "-----BEGIN CERTIFICATE-----test".into(),
        workspace_uid: "workspace-uid".into(),
        expires_at: chrono::Utc::now().timestamp() + 600,
        verifier: Some(verifier.endpoint.clone()),
    };
    let metadata = Arc::new(Mutex::new(Metadata::default()));
    {
        let mut data = metadata.lock().unwrap();
        data.objects.extend(verifier.objects());
        data.verifier = Some(verifier);
        data.objects.insert(SANDBOX.into(),json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"agent","namespace":"workspace","uid":"sandbox-uid","resourceVersion":"1"},
            "status":{"serviceObservation":{"capability":CAPABILITY,"version":"secret-uid:1","phase":"Ready",
                "grant":{"uid":"grant-uid"},"namespaceUid":"runtime-uid",
                "privacyRevision":crate::sre_privacy::REVISION,"privacyEpoch":null}}
        }));
        data.objects.insert(GRANT.into(),json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"workspace","uid":"grant-uid","generation":1,"resourceVersion":"1"},
            "spec":{"enabled":true,"workspaceUid":"workspace-uid","observationTargets":[{"kind":"KarsSandbox","namespace":"workspace","name":"agent","uid":"sandbox-uid"}]},
            "status":{"phase":"Ready","observedGeneration":1,
                "conditions":[{"type":"WriterReady","status":"True","observedGeneration":1}]}
        }));
        for (name, uid) in [
            ("kars-agent", "runtime-uid"),
            ("bridge", "bridge-uid"),
            ("workspace", "workspace-uid"),
        ] {
            data.objects.insert(format!("/api/v1/namespaces/{name}"),json!({
                "apiVersion":"v1","kind":"Namespace","metadata":{"name":name,"uid":uid,"resourceVersion":"1"}
            }));
        }
        data.objects.insert(RECIPIENT.into(),json!({
            "apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":"bff","namespace":"bridge","uid":"bff-uid","resourceVersion":"1"}
        }));
    }
    let recorded = metadata.clone();
    Mock::given(|_: &wiremock::Request| true).respond_with(move |request:&wiremock::Request| {
        let mut data = recorded.lock().unwrap();
        let path = request.url.path();
        let body:Value = request.body_json().unwrap_or(Value::Null);
        data.calls.push((request.method.to_string(),path.into(),body.clone()));
        if data.fail.as_deref() == Some(path) {
            return ResponseTemplate::new(403).set_body_json(json!({"kind":"Status","apiVersion":"v1","code":403,"reason":"Forbidden","message":"PRIVATE_ERROR_SENTINEL"}));
        }
        if request.method == "POST" && path == REVIEWS {
            return ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview","spec":body["spec"],
                "status":{"allowed":data.allow.as_deref()==body["spec"]["resourceAttributes"]["verb"].as_str()}
            }));
        }
        if request.method == "GET" && let Some(object) = data.objects.get(path) {
            return ResponseTemplate::new(200).set_body_json(object);
        }
        ResponseTemplate::new(404).set_body_json(json!({"kind":"Status","apiVersion":"v1","code":404,"reason":"NotFound","message":"not found"}))
    }).mount(&server).await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = kube::Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let mut services = GovernedServices::new(identity, Some(control_token()));
    services.observer = Some(Observer::for_test(
        binding,
        observer_token(),
        "secret-uid:1".into(),
        client,
    ));
    let mut state =
        crate::routes::model_routing::tests::test_state(crate::config::Config::from_env().unwrap());
    state.services = Arc::new(services);
    (server, state, metadata)
}

fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes(state.clone()))
        .merge(crate::routes::access_request::routes(state.clone()))
        .merge(crate::routes::egress::egress_routes())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            purpose_boundary,
        ))
        .with_state(state)
}

async fn call(
    state: &AppState,
    path: &str,
    method: &str,
    token: Option<&str>,
    scope: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .uri(path)
        .method(method)
        .extension(ConnectInfo(
            "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
        ));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(scope) = scope {
        request = request.header("x-kars-service-scope", scope);
    }
    let response = router(state.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_ERROR_SENTINEL"));
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn observation_reads_only_sanitized_domains_with_live_metadata_and_get_list_watch_denials() {
    let (_server, state, metadata) = fixture().await;
    state.blocklist.set_learn_mode(true);
    state
        .blocklist
        .record_learned("https://example.com/private?token=NEVER_PUBLISH")
        .await;
    let (status, scope) = call(&state, SCOPE, "GET", Some(&observer_token()), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &state,
        LEARNED,
        "GET",
        Some(&observer_token()),
        scope["scope_id"].as_str(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["domains"], json!(["example.com"]));
    assert!(!body.to_string().contains("NEVER_PUBLISH"));
    let metadata = metadata.lock().unwrap();
    for verb in ["get", "list", "watch"] {
        assert!(
            metadata
                .calls
                .iter()
                .any(|(method, path, body)| method == "POST"
                    && path == REVIEWS
                    && body["spec"]["resourceAttributes"]["verb"] == verb)
        );
    }
    assert!(
        metadata
            .calls
            .iter()
            .all(|(method, path, _)| method == "GET" || path == REVIEWS)
    );
    assert!(
        !metadata
            .calls
            .iter()
            .any(|(_, path, _)| path.contains("/secrets"))
    );
}

#[tokio::test]
async fn observation_tokens_cannot_authorize_mutations_control_or_legacy_even_on_loopback() {
    let (_server, state, metadata) = fixture().await;
    for (method, path) in [
        ("POST", "/internal/access-requests/reset"),
        ("POST", "/internal/access-requests/decision"),
        ("GET", "/internal/access-requests"),
        ("POST", "/egress/learn"),
        ("POST", "/egress/learned/clear"),
        ("GET", "/egress/learned"),
        ("POST", LEARNED),
    ] {
        assert_eq!(
            call(&state, path, method, Some(&observer_token()), None)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{method} {path}"
        );
    }
    for token in [
        None,
        Some("legacy-agent-token".into()),
        Some(control_token()),
    ] {
        assert_eq!(
            call(&state, SCOPE, "GET", token.as_deref(), None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert!(metadata.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn observation_rejects_replaced_foreign_or_revoked_authority_and_stale_rollout() {
    for (path, pointer, replacement) in [
        (SANDBOX, "/metadata/uid", json!("replacement")),
        (
            SANDBOX,
            "/status/serviceObservation/version",
            json!("secret-uid:2"),
        ),
        (
            SANDBOX,
            "/status/serviceObservation/phase",
            json!("Retired"),
        ),
        (
            SANDBOX,
            "/status/serviceObservation/grant/uid",
            json!("foreign"),
        ),
        (GRANT, "/metadata/uid", json!("replacement")),
        (GRANT, "/metadata/generation", json!(2)),
        (GRANT, "/status/observedGeneration", json!(0)),
        (GRANT, "/status/conditions/0/status", json!("False")),
        (GRANT, "/status/conditions/0/observedGeneration", json!(0)),
        (GRANT, "/spec/enabled", json!(false)),
        (GRANT, "/spec/observationTargets", json!([])),
        (
            "/api/v1/namespaces/kars-agent",
            "/metadata/uid",
            json!("replacement"),
        ),
        (
            "/api/v1/namespaces/bridge",
            "/metadata/uid",
            json!("replacement"),
        ),
        (RECIPIENT, "/metadata/uid", json!("replacement")),
    ] {
        let (_server, state, metadata) = fixture().await;
        *metadata
            .lock()
            .unwrap()
            .objects
            .get_mut(path)
            .unwrap()
            .pointer_mut(pointer)
            .unwrap() = replacement;
        assert_eq!(
            call(&state, SCOPE, "GET", Some(&observer_token()), None)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{path}{pointer}"
        );
    }
}

#[tokio::test]
async fn observation_privacy_pending_null_ready_or_authorized_legacy_subject_fails_closed() {
    for phase in ["Migrating", "Pending", "Ready"] {
        let (_server, state, metadata) = fixture().await;
        metadata.lock().unwrap().objects.insert(REGISTRATION.into(),json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
            "metadata":{"name":"canonical","uid":"registration","generation":1},
            "spec":{"enabled":true},"status":{"phase":phase,"observedGeneration":1,
                "privacyRevision":crate::sre_privacy::REVISION,"privacyEpoch":null,"legacySecretAccessDenied":true}
        }));
        assert_eq!(
            call(&state, SCOPE, "GET", Some(&observer_token()), None)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{phase}"
        );
    }
    for verb in ["get", "list", "watch"] {
        let (_server, state, metadata) = fixture().await;
        metadata.lock().unwrap().allow = Some(verb.into());
        assert_eq!(
            call(&state, SCOPE, "GET", Some(&observer_token()), None)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{verb}"
        );
    }
    let (_server, state, metadata) = fixture().await;
    metadata.lock().unwrap().fail = Some(GRANT.into());
    assert_eq!(
        call(&state, SCOPE, "GET", Some(&observer_token()), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn observation_scope_resets_are_cas_fenced_and_missing_capability_is_unavailable() {
    let (_server, mut state, _metadata) = fixture().await;
    let current = state.services.requests.scope().unwrap();
    state.services.reset(&current.id, None).unwrap();
    assert_eq!(
        call(
            &state,
            LEARNED,
            "GET",
            Some(&observer_token()),
            Some(&current.id)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let fresh = state.services.requests.scope().unwrap();
    assert_eq!(
        call(
            &state,
            LEARNED,
            "GET",
            Some(&observer_token()),
            Some(&fresh.id)
        )
        .await
        .0,
        StatusCode::OK
    );
    Arc::get_mut(&mut state.services).unwrap().observer = None;
    assert_eq!(
        call(&state, SCOPE, "GET", Some(&observer_token()), None)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
