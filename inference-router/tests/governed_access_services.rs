// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "common/governed_services.rs"]
mod support;
use axum::http::StatusCode;
use kars_inference_router::access_request::{
    AccessRequestBuffer, Error, Identity, Request, Status,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use support::*;
use tower::ServiceExt;

#[tokio::test]
async fn privileged_http_endpoints_reject_agent_admin_even_on_loopback() {
    let state = state("workspace-a", "uid-a");
    let scope = state.services.requests.scope().unwrap().id;
    let app = app(state);
    for token in [None, Some(AGENT_ADMIN)] {
        for path in [
            "/internal/access-requests/reset",
            "/internal/access-requests/decision",
        ] {
            let (status, _) = send(
                &app,
                request("POST", path, json!({"scope_id":scope}), token, None, true),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        assert_eq!(
            send(
                &app,
                request(
                    "GET",
                    "/internal/access-requests",
                    json!({}),
                    token,
                    None,
                    true
                )
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(
            &app,
            request("GET", "/v1/access-requests", json!({}), None, None, false)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &app,
            request(
                "GET",
                "/internal/access-requests",
                json!({}),
                Some(CONTROL),
                None,
                false
            )
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn reset_fences_old_requests_and_preserves_uid_qualified_identity() {
    let state = state("workspace-a", "uid-a");
    let scope = state.services.requests.scope().unwrap().id;
    state.budget.record_usage("test", 17).await;
    let app = app(state.clone());
    let entry = queue(&app, &scope, "egress", "example.test", Some(443)).await;
    let (status, reset) = send(
        &app,
        request(
            "POST",
            "/internal/access-requests/reset",
            json!({"scope_id":scope,"assignment_id":"assignment-2"}),
            Some(CONTROL),
            None,
            true,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reset["scope"]["identity"]["sandbox"]["uid"], "uid-a");
    assert_eq!(reset["scope"]["identity"]["task"]["uid"], "task-uid-a");
    assert_eq!(reset["enforcement_changed"], false);
    assert_ne!(reset["scope"]["id"], scope);
    assert_eq!(
        decision(
            &app,
            &scope,
            entry["request_id"].as_str().unwrap(),
            "approved"
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
                "/v1/access-request",
                json!({"scope_id":scope,"kind":"egress","target":"example.test"}),
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
                json!({"scope_id":scope}),
                Some(CONTROL),
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(state.budget.get_usage("test").await.0, 17);
}

#[tokio::test]
async fn request_identity_port_tier_and_terminal_decisions_cannot_be_rebound() {
    let state = state("workspace-a", "uid-a");
    let scope = state.services.requests.scope().unwrap().id;
    let app = app(state);
    let first = queue(&app, &scope, "egress", "example.test", Some(443)).await;
    let id = first["request_id"].as_str().unwrap();
    assert_eq!(
        decision(&app, &scope, id, "approved").await.0,
        StatusCode::OK
    );
    assert_eq!(
        decision(&app, &scope, id, "denied").await.0,
        StatusCode::CONFLICT
    );
    let other = queue(&app, &scope, "egress", "example.test", Some(80)).await;
    assert_ne!(first["request_id"], other["request_id"]);
    assert_eq!(other["status"], "pending");
    let (status,body)=send(&app,request("POST","/v1/access-request",
        json!({"scope_id":scope,"kind":"egress","target":"example.test","reason":"changed","port":443}),None,None,true)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["request"]["reason"], "needed");
    assert_eq!(body["request"]["status"], "approved");
    for (invalid, expected) in [
        (
            json!({"scope_id":scope,"kind":"tier","tier":6}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"scope_id":scope,"kind":"egress","target":"https://example.test/token"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"scope_id":scope,"kind":"tool","target":"test","sandbox_uid":"other"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        assert_eq!(
            send(
                &app,
                request("POST", "/v1/access-request", invalid, None, None, true)
            )
            .await
            .0,
            expected
        );
    }
}

#[tokio::test]
async fn same_named_sandboxes_in_other_workspaces_cannot_share_request_scope() {
    let a = state("workspace-a", "uid-a");
    let b = state("workspace-b", "uid-b");
    let scope = a.services.requests.scope().unwrap().id;
    let app_b = app(b);
    assert_eq!(
        send(
            &app_b,
            request(
                "POST",
                "/v1/access-request",
                json!({"scope_id":scope,"kind":"tool","target":"fetch"}),
                None,
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn decision_wait_wakes_and_agent_cancel_does_not_rewrite_an_operator_decision() {
    let state = state("workspace-a", "uid-a");
    let scope = state.services.requests.scope().unwrap().id;
    let app = app(state.clone());
    let entry = queue(&app, &scope, "tool", "fetch", None).await;
    let id = entry["request_id"].as_str().unwrap();
    let waiting = app.clone().oneshot(request(
        "GET",
        &format!("/v1/access-requests/{id}/wait?scope_id={scope}&timeout_ms=1000"),
        json!({}),
        None,
        None,
        true,
    ));
    let wait = tokio::spawn(waiting);
    assert_eq!(
        decision(&app, &scope, id, "approved").await.0,
        StatusCode::OK
    );
    assert_eq!(wait.await.unwrap().unwrap().status(), StatusCode::OK);
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
    let cancelled = state.services.requests.entry(&scope, id).unwrap();
    assert_eq!(cancelled.status, Status::Cancelled);
    assert_eq!(cancelled.decision, Some(Status::Approved));
}

#[tokio::test]
async fn storage_rate_and_expiry_limits_fail_closed() {
    let buffer = AccessRequestBuffer::with_limits(
        Identity::standalone("test"),
        1,
        128,
        Duration::from_millis(10),
    );
    let scope = buffer.scope().unwrap().id;
    let request = |target: &str| Request {
        scope_id: scope.clone(),
        kind: "tool".into(),
        target: target.into(),
        reason: "reason".into(),
        tier: None,
        port: None,
    };
    let (first, _) = buffer.record(request("first")).unwrap();
    assert!(matches!(buffer.record(request("second")), Err(Error::Full)));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(matches!(
        buffer.transition(&scope, &first.request_id, Status::Approved),
        Err(Error::Expired)
    ));
    let rate = AccessRequestBuffer::with_limits(
        Identity::standalone("test"),
        64,
        1,
        Duration::from_secs(30),
    );
    let scope = rate.scope().unwrap().id;
    let request = Request {
        scope_id: scope,
        kind: "tool".into(),
        target: "first".into(),
        reason: String::new(),
        tier: None,
        port: None,
    };
    rate.record(request.clone()).unwrap();
    assert!(matches!(rate.record(request), Err(Error::RateLimited)));
}

#[tokio::test]
async fn dropping_waiter_and_reset_release_bounded_wait_capacity() {
    let state = state("workspace-a", "uid-a");
    let scope = state.services.requests.scope().unwrap().id;
    let app = app(state.clone());
    let entry = queue(&app, &scope, "tool", "fetch", None).await;
    let id = entry["request_id"].as_str().unwrap().to_string();
    let services = state.services.clone();
    let scope_copy = scope.clone();
    let task = tokio::spawn(async move {
        services
            .wait_for_decision(&scope_copy, &id, Duration::from_secs(30))
            .await
    });
    for _ in 0..30 {
        if state.services.wait_slots() == 15 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(state.services.wait_slots(), 15);
    task.abort();
    let _ = task.await;
    assert_eq!(state.services.wait_slots(), 16);
    let scope_copy = scope.clone();
    let id = entry["request_id"].as_str().unwrap().to_string();
    let services = state.services.clone();
    let task = tokio::spawn(async move {
        services
            .wait_for_decision(&scope_copy, &id, Duration::from_secs(30))
            .await
    });
    state.services.reset(&scope, Some("next".into())).unwrap();
    assert!(matches!(task.await.unwrap(), Err(Error::StaleScope)));
    assert_eq!(state.services.wait_slots(), 16);
}

#[tokio::test]
async fn missing_control_configuration_never_falls_back_to_shared_agent_admin() {
    let mut state = state("workspace-a", "uid-a");
    state.services = Arc::new(
        kars_inference_router::governed_services::GovernedServices::new(
            Identity::standalone("test"),
            None,
        ),
    );
    let app = app(state);
    assert_eq!(
        send(
            &app,
            request(
                "GET",
                "/internal/access-requests",
                json!({}),
                Some(AGENT_ADMIN),
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn http_payload_rate_and_operator_origin_limits_are_enforced() {
    let mut state = state("workspace-a", "uid-a");
    let identity = state.services.requests.scope().unwrap().identity;
    let mut services = kars_inference_router::governed_services::GovernedServices::new(
        identity,
        Some(CONTROL.into()),
    );
    services.allow_ips = Some(vec!["192.0.2.11".parse().unwrap()]);
    state.services = Arc::new(services);
    let scope = state.services.requests.scope().unwrap().id;
    let app = app(state.clone());
    assert_eq!(
        send(
            &app,
            request(
                "GET",
                "/internal/access-requests",
                json!({}),
                Some(CONTROL),
                None,
                false
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            request(
                "GET",
                "/telemetry/cursor",
                json!({}),
                Some(CONTROL),
                None,
                false
            )
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            request(
                "POST",
                "/v1/access-request",
                json!({
                    "scope_id":scope,"kind":"tool","target":"fetch","reason":"x".repeat(9000)
                }),
                None,
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    for _ in 0..32 {
        assert_eq!(
            send(
                &app,
                request(
                    "POST",
                    "/v1/access-request",
                    json!({
                        "scope_id":scope,"kind":"tool","target":"fetch"
                    }),
                    None,
                    None,
                    true
                )
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
    }
    assert_eq!(
        send(
            &app,
            request(
                "POST",
                "/v1/access-request",
                json!({
                    "scope_id":scope,"kind":"tool","target":"fetch"
                }),
                None,
                None,
                true
            )
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn unknown_decision_cannot_synthesize_an_approved_request() {
    let state = state("workspace-a", "uid-a");
    let scope = state.services.requests.scope().unwrap().id;
    let app = app(state.clone());
    assert_eq!(
        decision(&app, &scope, "unknown-id", "approved").await.0,
        StatusCode::NOT_FOUND
    );
    assert!(state.services.requests.snapshot(&scope).unwrap().is_empty());
}

#[tokio::test]
async fn live_http_control_auth_and_reset_use_the_same_production_router() {
    let state = state("workspace-a", "uid-a");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let stop = shutdown.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(stop.cancelled_owned())
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{address}");
    let info = client
        .get(format!("{base}/v1/access-requests"))
        .send()
        .await
        .unwrap();
    assert_eq!(info.status(), StatusCode::OK);
    let scope = info.json::<serde_json::Value>().await.unwrap()["scope"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let response = client
        .post(format!("{base}/internal/access-requests/reset"))
        .bearer_auth(AGENT_ADMIN)
        .json(&json!({"scope_id":scope}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = client
        .post(format!("{base}/internal/access-requests/reset"))
        .bearer_auth(CONTROL)
        .json(&json!({"scope_id":scope,"assignment_id":"live-test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(
        response.json::<serde_json::Value>().await.unwrap()["scope"]["id"],
        scope
    );
    shutdown.cancel();
    server.await.unwrap();
}
