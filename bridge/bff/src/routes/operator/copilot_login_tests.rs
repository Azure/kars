// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    http::{Method, Request, Uri},
    routing::post,
};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const PRIVATE: &str = "PRIVATE_OAUTH_VALUE_NOT_RETURNED";
const SECRET: &str = "/api/v1/namespaces/kars-system/secrets/kars-inference-providers";
const GRANT: &str =
    "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karscredentialgrants/workspace";
const START: &str = "/login/device/code";
const POLL: &str = "/login/oauth/access_token";
const SEAT: &str = "/copilot_internal/v2/token";
const MODELS: &str = "/models";

fn contract() -> Value {
    serde_json::from_str(include_str!("../../../../contracts/copilot-login.json")).unwrap()
}

struct Api {
    grant: Value,
    secret: Value,
    start: Value,
    poll: Value,
    seat: Value,
    seat_status: u16,
    status: u16,
    location: Option<String>,
    invalid_json: bool,
    revoke_on_exchange: bool,
    fail_patch: bool,
    missing_grant: bool,
    calls: Vec<(Method, String)>,
}

fn api_failure(code: u16) -> Response {
    (
        StatusCode::from_u16(code).unwrap(),
        Json(json!({
            "apiVersion":"v1", "kind":"Status", "status":"Failure",
            "reason":"Forbidden", "code":code, "message":PRIVATE,
        })),
    )
        .into_response()
}

async fn handle(
    State(state): State<Arc<Mutex<Api>>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let mut api = state.lock().unwrap();
    api.calls.push((method.clone(), uri.path().into()));
    match uri.path() {
        START | POLL => {
            assert_eq!(method, Method::POST);
            let input: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(input["client_id"], CLIENT_ID);
            if uri.path() == POLL {
                assert_eq!(input["grant_type"], "urn:ietf:params:oauth:grant-type:device_code");
                if api.revoke_on_exchange { api.grant["spec"]["enabled"] = false.into(); }
            }
            let status = StatusCode::from_u16(api.status).unwrap();
            if let Some(location) = &api.location {
                return (status, [(axum::http::header::LOCATION, location.clone())]).into_response();
            }
            if api.invalid_json { return (status, PRIVATE).into_response(); }
            let value = if uri.path() == START { &api.start } else { &api.poll };
            (status, Json(value.clone())).into_response()
        }
        SEAT => (
            StatusCode::from_u16(api.seat_status).unwrap(),
            Json(api.seat.clone()),
        ).into_response(),
        MODELS => Json(json!({"data":[]})).into_response(),
        GRANT if api.missing_grant => api_failure(404),
        GRANT => Json(api.grant.clone()).into_response(),
        "/api/v1/namespaces/kars-system" => Json(json!({
            "apiVersion":"v1", "kind":"Namespace", "metadata":{"name":"kars-system","uid":"namespace"}
        })).into_response(),
        SECRET if method == Method::PATCH => {
            if api.fail_patch { return api_failure(403); }
            let patch: json_patch::Patch = serde_json::from_slice(&body).unwrap();
            if json_patch::patch(&mut api.secret, &patch).is_err() { return api_failure(422); }
            Json(api.secret.clone()).into_response()
        }
        SECRET => Json(api.secret.clone()).into_response(),
        _ => api_failure(404),
    }
}

struct Fixture {
    cluster: Cluster,
    client: kube::Client,
    oauth: OAuth,
    api: Arc<Mutex<Api>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn fixture() -> Fixture {
    let api = Arc::new(Mutex::new(Api {
        grant: json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsCredentialGrant",
            "metadata":{"name":"workspace","namespace":"kars-system","uid":"grant","resourceVersion":"1","generation":1},
            "spec":{"enabled":true,"workspaceUid":"namespace","agentKeys":[],
                "integrationStores":[{"secret":{"name":"kars-inference-providers","uid":"secret"},"purpose":"inference"}]},
            "status":{"phase":"Ready","observedGeneration":1,"sources":[],"legacySources":[]}
        }),
        secret: json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"kars-inference-providers","namespace":"kars-system","uid":"secret","resourceVersion":"1"},
            "data":{"UNRELATED":"cHJlc2VydmVk"}
        }),
        start: contract()["start"].clone(),
        poll: json!({"error":"authorization_pending"}),
        seat: json!({"token":PRIVATE, "chat_enabled":true}),
        seat_status: 200,
        status: 200,
        location: None,
        invalid_json: false,
        revoke_on_exchange: false,
        fail_patch: false,
        missing_grant: false,
        calls: Vec::new(),
    }));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server_api = api.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(handle).with_state(server_api),
        )
        .await
        .unwrap();
    });
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = kube::Client::try_from(kube::Config::new(
        format!("http://{address}").parse().unwrap(),
    ))
    .unwrap();
    let oauth = OAuth {
        client: CopilotClient::loopback(address),
    };
    Fixture {
        cluster: Cluster::for_test_client(client.clone()),
        client,
        oauth,
        api,
        server,
    }
}

async fn response(result: impl IntoResponse) -> (StatusCode, Value) {
    let response = result.into_response();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 16384).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(PRIVATE));
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn poll_response(f: &Fixture, interval: Option<u64>) -> (StatusCode, Value) {
    response(
        poll(
            &f.cluster,
            &f.oauth,
            CopilotLoginPollRequest {
                device_code: contract()["start"]["device_code"].as_str().unwrap().into(),
                interval,
            },
        )
        .await
        .map(Json),
    )
    .await
}

#[tokio::test]
async fn device_login_wire_contract_and_cumulative_slowdown_use_real_http_client() {
    let f = fixture().await;
    let (status, body) = response(start(&f.cluster, &f.oauth).await.map(Json)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, contract()["start"]);
    assert_eq!(poll_response(&f, None).await.1, contract()["pending"]);
    f.api.lock().unwrap().poll = json!({"error":"slow_down"});
    assert_eq!(
        poll_response(&f, Some(5)).await.1,
        contract()["slow_down_1"]
    );
    f.api.lock().unwrap().poll = json!({"error":"slow_down","interval":15});
    assert_eq!(
        poll_response(&f, Some(10)).await.1,
        contract()["slow_down_2"]
    );
    f.api.lock().unwrap().poll = json!({"error":"slow_down","interval":40});
    assert_eq!(poll_response(&f, Some(15)).await.1["interval"], 40);
    assert_eq!(
        poll_response(&f, Some(900)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn missing_or_unready_grant_and_replaced_store_prevent_all_oauth_requests() {
    for fault in [
        "missing",
        "disabled",
        "phase",
        "generation",
        "workspace",
        "store_uid",
        "store_type",
        "unregistered",
        "utf8",
    ] {
        let f = fixture().await;
        {
            let mut api = f.api.lock().unwrap();
            match fault {
                "missing" => api.missing_grant = true,
                "disabled" => api.grant["spec"]["enabled"] = false.into(),
                "phase" => api.grant["status"]["phase"] = "Pending".into(),
                "generation" => api.grant["status"]["observedGeneration"] = 0.into(),
                "workspace" => api.grant["spec"]["workspaceUid"] = "replacement".into(),
                "store_uid" => api.secret["metadata"]["uid"] = "replacement".into(),
                "store_type" => api.secret["type"] = "kubernetes.io/dockerconfigjson".into(),
                "unregistered" => api.grant["spec"]["integrationStores"] = json!([]),
                "utf8" => api.secret["data"]["OTHER"] = "/w==".into(),
                _ => unreachable!(),
            }
        }
        let (status, body) = response(start(&f.cluster, &f.oauth).await.map(Json)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{fault}");
        assert_eq!(body, contract()["not_ready"]);
        assert_eq!(poll_response(&f, None).await.1, contract()["not_ready"]);
        assert!(
            !f.api.lock().unwrap().calls.iter().any(
                |(m, p)| *m != Method::GET || [START, POLL, SEAT, MODELS].contains(&p.as_str())
            )
        );
    }
}

#[tokio::test]
async fn invalid_and_non_success_upstream_responses_are_never_pending_or_echoed() {
    let f = fixture().await;
    for status in [302, 400, 401, 403, 429, 500, 502] {
        {
            let mut api = f.api.lock().unwrap();
            api.status = status;
            api.poll = json!({"error":"authorization_pending","error_description":PRIVATE});
        }
        let (actual, body) = poll_response(&f, None).await;
        assert!(actual.is_client_error() || actual.is_server_error());
        assert!(body.get("status").is_none());
    }
    f.api.lock().unwrap().status = 200;
    for body in [
        json!({}),
        json!([]),
        json!(null),
        json!({"error":PRIVATE}),
        json!({"error":17}),
        json!({"access_token":PRIVATE}),
        json!({"access_token":"","token_type":"bearer"}),
        json!({"access_token":PRIVATE,"token_type":"bearer","error":"authorization_pending"}),
        json!({"error":"slow_down","interval":"10"}),
        json!({"error":"slow_down","interval":0}),
    ] {
        f.api.lock().unwrap().poll = body;
        let (status, _) = poll_response(&f, None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }
    f.api.lock().unwrap().invalid_json = true;
    assert_eq!(poll_response(&f, None).await.0, StatusCode::BAD_GATEWAY);
    assert!(
        !f.api
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(_, p)| p == SEAT || p == MODELS)
    );
}

#[tokio::test]
async fn start_response_validation_blocks_empty_codes_and_untrusted_verification_urls() {
    let f = fixture().await;
    for (key, value) in [
        ("device_code", json!("")),
        ("user_code", json!(PRIVATE.to_lowercase())),
        (
            "verification_uri",
            json!("https://untrusted.example/?token=PRIVATE"),
        ),
        ("interval", json!(0)),
        ("expires_in", json!(901)),
        ("error", json!(PRIVATE)),
    ] {
        let mut value_body = contract()["start"].clone();
        value_body[key] = value;
        f.api.lock().unwrap().start = value_body;
        assert_eq!(
            response(start(&f.cluster, &f.oauth).await.map(Json))
                .await
                .0,
            StatusCode::BAD_GATEWAY
        );
    }
}

#[tokio::test]
async fn expiry_and_denial_are_terminal_not_pending() {
    let f = fixture().await;
    for (error, code) in [
        ("expired_token", "copilot_expired"),
        ("access_denied", "copilot_denied"),
    ] {
        f.api.lock().unwrap().poll = json!({"error":error,"error_description":PRIVATE});
        let (status, body) = poll_response(&f, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], code);
    }
}

#[tokio::test]
async fn granted_token_is_stored_before_authorized_and_late_failures_never_claim_success() {
    for fault in ["none", "grant_race", "patch_denied"] {
        let f = fixture().await;
        {
            let mut api = f.api.lock().unwrap();
            api.poll =
                json!({"access_token":PRIVATE,"token_type":"bearer","refresh_token":PRIVATE});
            api.revoke_on_exchange = fault == "grant_race";
            api.fail_patch = fault == "patch_denied";
        }

        let (status, body) = poll_response(&f, None).await;
        let api = f.api.lock().unwrap();
        assert_eq!(api.calls.iter().filter(|(_, p)| p == POLL).count(), 1);
        assert_eq!(api.calls.iter().filter(|(_, p)| p == SEAT).count(), 1);
        assert_eq!(api.secret["data"]["UNRELATED"], "cHJlc2VydmVk");
        if fault == "none" {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, contract()["authorized"]);
            assert!(api.secret["data"]["COPILOT_GITHUB_TOKEN"].is_string());
        } else {
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body, contract()["storage_unconfirmed"]);
            assert!(api.secret["data"].get("COPILOT_GITHUB_TOKEN").is_none());
            assert!(!api.calls.iter().any(|(_, p)| p == MODELS));
        }
    }
}

#[tokio::test]
async fn production_handlers_reject_malformed_inputs_and_missing_grants_without_oauth() {
    let f = fixture().await;
    f.api.lock().unwrap().missing_grant = true;
    let app = Router::new()
        .route("/start", post(copilot_login_start))
        .route("/poll", post(copilot_login_poll))
        .with_state(AppState::for_test_client(f.client.clone(), "kars-system"));
    for body in [
        json!({"device_code":{"private":PRIVATE}}),
        json!({"device_code":PRIVATE,"interval":"invalid"}),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/poll")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, body) = response(app.clone().oneshot(request).await.unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "copilot_invalid_request");
    }
    assert!(f.api.lock().unwrap().calls.is_empty());
    for path in ["/start", "/poll"] {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(json!({"device_code":PRIVATE}).to_string()))
            .unwrap();
        let (status, body) = response(app.clone().oneshot(request).await.unwrap()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, contract()["not_ready"]);
    }
    assert!(
        f.api
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, path)| *method == Method::GET && path == GRANT)
    );
}

#[tokio::test]
async fn failed_or_malformed_seat_verification_never_stores_credentials() {
    let f = fixture().await;
    f.api.lock().unwrap().poll = json!({"access_token":PRIVATE,"token_type":"bearer"});
    for (status, seat) in [
        (403, json!({"message":PRIVATE})),
        (500, json!({"token":PRIVATE})),
        (200, json!({"token":"","chat_enabled":true})),
        (200, json!({"token":PRIVATE,"chat_enabled":false})),
        (200, json!({"error":PRIVATE})),
    ] {
        {
            let mut api = f.api.lock().unwrap();
            api.seat_status = status;
            api.seat = seat;
        }
        let (status, body) = poll_response(&f, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "copilot_seat_unavailable");
    }
    assert!(
        !f.api
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(method, path)| *method == Method::PATCH || path == MODELS)
    );
}

#[tokio::test]
async fn device_login_redirects_never_replay_codes_or_mutate_credentials() {
    let f = fixture().await;
    let sink = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    for status in [301, 302, 303, 307, 308] {
        for location in [
            format!("http://{}/capture", sink.local_addr().unwrap()),
            "/same-origin-capture".into(),
        ] {
            {
                let mut api = f.api.lock().unwrap();
                api.status = status;
                api.location = Some(location);
                api.calls.clear();
            }
            let (status, body) = response(start(&f.cluster, &f.oauth).await.map(Json)).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(body["error"]["code"], "copilot_upstream");
            let (status, body) = poll_response(&f, None).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(body["error"]["code"], "copilot_upstream");
            let api = f.api.lock().unwrap();
            let calls: Vec<_> = api
                .calls
                .iter()
                .filter(|(_, path)| !path.starts_with("/api"))
                .map(|(method, path)| (method.clone(), path.as_str()))
                .collect();
            assert_eq!(calls, vec![(Method::POST, START), (Method::POST, POLL)]);
            assert!(!api.calls.iter().any(|(method, _)| *method == Method::PATCH));
            assert!(api.secret["data"].get("COPILOT_GITHUB_TOKEN").is_none());
        }
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), sink.accept())
            .await
            .is_err()
    );
}
