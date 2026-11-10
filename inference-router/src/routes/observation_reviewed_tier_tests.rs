// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn control_closes_reviewed_tier_without_grant_and_preserves_auth_and_payload_gates() {
    let (_server, state, metadata) = fixture().await;
    let scope = state.services.requests.scope().unwrap().id;
    let (entry, _) = state
        .services
        .requests
        .record(
            serde_json::from_value(json!({
                "scope_id":scope,"kind":"tier","tier":3,"reason":"Need more autonomy"
            }))
            .unwrap(),
        )
        .unwrap();
    let (egress, _) = state
        .services
        .requests
        .record(
            serde_json::from_value(json!({
                "scope_id":scope,"kind":"egress","target":"example.com","port":443
            }))
            .unwrap(),
        )
        .unwrap();
    let app = routes(state.clone())
        .merge(crate::routes::access_request::control_routes(state.clone()))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            purpose_boundary,
        ))
        .with_state(state.clone());
    let send = |body: Value, token: Option<String>| {
        let app = app.clone();
        let mut request = Request::builder()
            .method("POST")
            .uri("/internal/access-requests/decision")
            .header("content-type", "application/json")
            .extension(ConnectInfo(
                "192.0.2.10:43210".parse::<SocketAddr>().unwrap(),
            ));
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request.body(Body::from(body.to_string())).unwrap();
        async move { app.oneshot(request).await.unwrap() }
    };
    let explanation = "requires-fresh-run: Human review approved Tier 3; current authority is unchanged. A fresh authorized run is required.";
    let body = json!({"scope_id":scope,"request_id":entry.request_id,"verdict":"cancelled","explanation":explanation});
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("legacy-agent-token".into()), StatusCode::UNAUTHORIZED),
        (Some(observer_token()), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(send(body.clone(), token).await.status(), expected);
    }
    for (key, value, expected) in [
        ("explanation", Value::Null, StatusCode::BAD_REQUEST),
        ("explanation", json!(" \n"), StatusCode::BAD_REQUEST),
        (
            "explanation",
            json!("é".repeat(4097)),
            StatusCode::BAD_REQUEST,
        ),
        (
            "answer",
            json!("clarification only"),
            StatusCode::BAD_REQUEST,
        ),
        ("verdict", json!("approved"), StatusCode::BAD_REQUEST),
        ("verdict", json!("denied"), StatusCode::BAD_REQUEST),
        (
            "request_id",
            json!(egress.request_id),
            StatusCode::BAD_REQUEST,
        ),
        (
            "scope_id",
            json!(format!("{}:1", "b".repeat(32))),
            StatusCode::CONFLICT,
        ),
        (
            "extra",
            json!("not allowed"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let mut invalid = body.clone();
        invalid[key] = value;
        assert_eq!(
            send(invalid, Some(control_token())).await.status(),
            expected,
            "{key}"
        );
        let pending = state
            .services
            .requests
            .entry(&scope, &entry.request_id)
            .unwrap();
        assert_eq!(pending.status, crate::access_request::Status::Pending);
        assert!(pending.explanation.is_none());
    }
    let response = send(body.clone(), Some(control_token())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let outcome: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(outcome["enforcement_changed"], false);
    assert_eq!(outcome["request"]["status"], "cancelled");
    assert_eq!(outcome["request"]["explanation"], explanation);
    assert!(outcome["request"]["decision"].is_null());
    assert!(outcome["request"].get("answer").is_none());
    assert_eq!(
        send(body, Some(control_token())).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(
            json!({"scope_id":scope,"request_id":entry.request_id,"verdict":"approved"}),
            Some(control_token())
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert!(metadata.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn local_agent_cannot_submit_review_explanation_or_control_cancellation() {
    let (_server, state, _) = fixture().await;
    let scope = state.services.requests.scope().unwrap().id;
    let (entry, _) = state
        .services
        .requests
        .record(
            serde_json::from_value(json!({
                "scope_id":scope,"kind":"tier","tier":3
            }))
            .unwrap(),
        )
        .unwrap();
    let app = crate::routes::access_request::routes(state.clone()).with_state(state.clone());
    for (path, body, expected) in [
        (
            format!("/v1/access-requests/{}/cancel", entry.request_id),
            json!({"scope_id":scope,"explanation":"requires-fresh-run"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/v1/access-request".into(),
            json!({"scope_id":scope,"kind":"tier","tier":3,"explanation":"requires-fresh-run"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/internal/access-requests/decision".into(),
            json!({"scope_id":scope,"request_id":entry.request_id,"verdict":"cancelled","explanation":"requires-fresh-run"}),
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .extension(ConnectInfo(
                "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
            ))
            .body(Body::from(body.to_string()))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            expected
        );
    }
    let pending = state
        .services
        .requests
        .entry(&scope, &entry.request_id)
        .unwrap();
    assert_eq!(pending.status, crate::access_request::Status::Pending);
    assert!(pending.explanation.is_none());
}
