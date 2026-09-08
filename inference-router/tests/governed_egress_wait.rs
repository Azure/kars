// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "common/governed_services.rs"]
mod support;
use axum::http::StatusCode;
use kars_inference_router::{blocklist::Blocklist, routes::AppState};
use serde_json::{Value, json};
use std::time::Duration;
use support::*;
use tower::ServiceExt;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

async fn setup() -> (AppState, axum::Router, MockServer, String, String) {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("response-secret-marker"))
        .mount(&upstream)
        .await;
    let address = reqwest::Url::parse(&upstream.uri())
        .unwrap()
        .socket_addrs(|| None)
        .unwrap()[0];
    let mut state = state("workspace", "uid");
    state.blocklist = Blocklist::new(None).await;
    state.client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve("approved.example", address)
        .build()
        .unwrap();
    let scope = state.services.requests.scope().unwrap().id;
    let url = format!(
        "http://approved.example:{}/data?key=request-secret-marker",
        address.port()
    );
    (state.clone(), app(state), upstream, scope, url)
}

async fn pending(app: &axum::Router) -> Value {
    for _ in 0..100 {
        let (_, body) = send(
            app,
            request(
                "GET",
                "/internal/access-requests",
                json!({}),
                Some(CONTROL),
                None,
                true,
            ),
        )
        .await;
        if let Some(entry) = body["entries"]
            .as_array()
            .and_then(|entries| entries.first())
        {
            return entry.clone();
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("request was not queued");
}

#[tokio::test]
async fn approved_decision_alone_cannot_widen_egress_but_signed_policy_wakes_the_request() {
    let (state, app, upstream, scope, url) = setup().await;
    let waiting = tokio::spawn(app.clone().oneshot(request(
        "POST",
        "/egress/fetch",
        json!({"url":url,"scope_id":scope,"wait_for_approval_ms":2000,
            "headers":{"Authorization":"Bearer header-secret-marker"}}),
        None,
        None,
        true,
    )));
    let entry = pending(&app).await;
    assert_eq!(
        decision(
            &app,
            &scope,
            entry["request_id"].as_str().unwrap(),
            "approved"
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!waiting.is_finished());
    assert!(upstream.received_requests().await.unwrap().is_empty());
    // This is the primitive used by the existing signed allowlist loader; the
    // access-request service has no API capable of writing it.
    state
        .blocklist
        .replace_allowlist(vec!["approved.example".into()])
        .await;
    let response = waiting.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
    let trace = state
        .services
        .telemetry
        .snapshot(&scope, 0)
        .unwrap()
        .to_string();
    for secret in [
        "request-secret-marker",
        "response-secret-marker",
        "header-secret-marker",
        "approved.example",
    ] {
        assert!(!trace.contains(secret), "{trace}");
    }
    assert!(!state.blocklist.is_learn_mode());
}

#[tokio::test]
async fn denial_cancel_reset_and_timeout_do_not_send_unapproved_traffic() {
    for action in ["deny", "cancel", "reset", "timeout"] {
        let (state, app, upstream, scope, url) = setup().await;
        let waiting=tokio::spawn(app.clone().oneshot(request("POST","/egress/fetch",
            json!({"url":url,"scope_id":scope,"wait_for_approval_ms":if action=="timeout"{30}else{1000}}),None,None,true)));
        let entry = pending(&app).await;
        let id = entry["request_id"].as_str().unwrap();
        match action {
            "deny" => {
                assert_eq!(decision(&app, &scope, id, "denied").await.0, StatusCode::OK);
            }
            "cancel" => {
                assert_eq!(
                    decision(&app, &scope, id, "approved").await.0,
                    StatusCode::OK
                );
                assert_eq!(
                    send(
                        &app,
                        request(
                            "POST",
                            &format!("/v1/access-requests/{id}/cancel"),
                            json!({"scope_id":scope}),
                            None,
                            None,
                            true
                        )
                    )
                    .await
                    .0,
                    StatusCode::OK
                );
            }
            "reset" => {
                assert_eq!(
                    send(
                        &app,
                        request(
                            "POST",
                            "/internal/access-requests/reset",
                            json!({"scope_id":scope,"assignment_id":"next"}),
                            Some(CONTROL),
                            None,
                            true
                        )
                    )
                    .await
                    .0,
                    StatusCode::OK
                );
            }
            _ => {}
        }
        let response = waiting.await.unwrap().unwrap();
        assert_eq!(
            response.status(),
            if action == "timeout" {
                StatusCode::REQUEST_TIMEOUT
            } else {
                StatusCode::CONFLICT
            }
        );
        assert!(upstream.received_requests().await.unwrap().is_empty());
        assert_eq!(state.services.wait_slots(), 16);
    }
}

#[tokio::test]
async fn ordinary_denied_fetch_is_still_immediate_and_private_targets_never_enter_approval_wait() {
    let (_state, app, upstream, scope, url) = setup().await;
    let response = tokio::time::timeout(
        Duration::from_millis(500),
        send(
            &app,
            request(
                "POST",
                "/egress/fetch",
                json!({"url":url}),
                None,
                None,
                true,
            ),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.0, StatusCode::FORBIDDEN);
    let response = send(
        &app,
        request(
            "POST",
            "/egress/fetch",
            json!({"url":upstream.uri(),"scope_id":scope,"wait_for_approval_ms":1000}),
            None,
            None,
            true,
        ),
    )
    .await;
    assert_eq!(response.0, StatusCode::FORBIDDEN);
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn cancel_after_readiness_but_before_dispatch_claim_prevents_a_post() {
    let (state, app, upstream, scope, url) = setup().await;
    let port = reqwest::Url::parse(&url)
        .unwrap()
        .port_or_known_default()
        .unwrap();
    let entry = queue(&app, &scope, "egress", "approved.example", Some(port)).await;
    let id = entry["request_id"].as_str().unwrap();
    assert_eq!(
        decision(&app, &scope, id, "approved").await.0,
        StatusCode::OK
    );
    state
        .blocklist
        .replace_allowlist(vec!["approved.example".into()])
        .await;
    state
        .services
        .wait_for_egress(
            &state.blocklist,
            &scope,
            id,
            &url,
            "test",
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    let cancelled = send(
        &app,
        request(
            "POST",
            &format!("/v1/access-requests/{id}/cancel"),
            json!({"scope_id":scope}),
            None,
            None,
            true,
        ),
    )
    .await;
    assert_eq!(cancelled.0, StatusCode::OK);
    assert_eq!(cancelled.1["status"], "cancelled");
    assert!(matches!(
        state.services.claim_egress_dispatch(&scope, Some(id)),
        Err(kars_inference_router::access_request::Error::Terminal)
    ));
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn cancel_and_reset_do_not_acknowledge_prevention_after_dispatch_has_started() {
    let (state, app, upstream, scope, url) = setup().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("accepted")
                .set_delay(Duration::from_millis(300)),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let waiting=tokio::spawn(app.clone().oneshot(request("POST","/egress/fetch",
        json!({"url":url,"method":"POST","body":"side-effect","scope_id":scope,"wait_for_approval_ms":2000}),
        None,None,true)));
    let entry = pending(&app).await;
    let id = entry["request_id"].as_str().unwrap();
    assert_eq!(
        decision(&app, &scope, id, "approved").await.0,
        StatusCode::OK
    );
    state
        .blocklist
        .replace_allowlist(vec!["approved.example".into()])
        .await;
    for _ in 0..100 {
        if !upstream.received_requests().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(upstream.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        send(
            &app,
            request(
                "POST",
                &format!("/v1/access-requests/{id}/cancel"),
                json!({"scope_id":scope}),
                None,
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(
            &app,
            request(
                "POST",
                "/internal/access-requests/reset",
                json!({"scope_id":scope,"assignment_id":"next"}),
                Some(CONTROL),
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(waiting.await.unwrap().unwrap().status(), StatusCode::OK);
    let completed = state.services.requests.entry(&scope, id).unwrap();
    assert_eq!(
        completed.status,
        kars_inference_router::access_request::Status::DispatchClaimed
    );
    assert!(!completed.dispatch_active);
    assert_eq!(
        send(
            &app,
            request(
                "POST",
                &format!("/v1/access-requests/{id}/cancel"),
                json!({"scope_id":scope}),
                None,
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(
            &app,
            request(
                "POST",
                "/internal/access-requests/reset",
                json!({"scope_id":scope,"assignment_id":"next"}),
                Some(CONTROL),
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn wait_rejects_wrong_port_request_and_shutdown_cancels_waiters() {
    let (state, app, _upstream, scope, url) = setup().await;
    let entry = queue(&app, &scope, "egress", "approved.example", Some(443)).await;
    let id = entry["request_id"].as_str().unwrap();
    assert!(matches!(
        state
            .services
            .wait_for_egress(
                &state.blocklist,
                &scope,
                id,
                &url,
                "test",
                Duration::from_millis(20)
            )
            .await,
        Err(kars_inference_router::access_request::Error::Invalid)
    ));
    let services = state.services.clone();
    let id = id.to_string();
    let old = scope.clone();
    let waiter = tokio::spawn(async move {
        services
            .wait_for_decision(&old, &id, Duration::from_secs(30))
            .await
    });
    state.services.shutdown.cancel();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(kars_inference_router::access_request::Error::Unavailable)
    ));
}
