// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::github_app::tests::{TOKEN, exchange, minted};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

async fn fixture(server: &MockServer) -> (tempfile::TempDir, AppState) {
    let (directory, config) = crate::github_services::tests::fixture(&server.uri()).await;
    let state = crate::routes::test_state(crate::config::Config::from_env().unwrap());
    assert!(state.services.github.set(Ok(config)).is_ok());
    state
        .blocklist
        .replace_allowlist(vec!["public.example".into()])
        .await;
    (directory, state)
}

async fn fetch(state: &AppState, peer: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/egress/fetch")
        .header("content-type", "application/json");
    if let Some(peer) = peer {
        request = request.extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    }
    let response = super::super::egress::egress_routes()
        .with_state(state.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), API_LIMIT * 2).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn github_fetch_accepts_only_original_canonical_get_without_credentials() {
    for url in [
        "https://api.github.com/repos/owner/repo",
        "https://api.github.com/repos/owner/repo/issues?state=open&per_page=10",
    ] {
        for headers in [
            json!({"Accept":"application/json"}),
            json!({"Accept":"application/vnd.github+json","User-Agent":"kars"}),
            json!({"uSeR-aGeNt":"caller-controlled"}),
        ] {
            let request = fetch_request(&json!({"url":url,"method":"GET","headers":headers}))
                .expect("canonical GET with discarded presentation headers");
            assert!(request.headers().is_empty());
        }
    }
    for url in [
        "http://api.github.com/repos/owner/repo",
        "https://api.github.com:443/repos/owner/repo",
        "https://API.GITHUB.COM/repos/owner/repo",
        "https://api.github.com./repos/owner/repo",
        "https://api.github.com.evil/repos/owner/repo",
        "https://user:password@api.github.com/repos/owner/repo",
        "https://github.com/repos/owner/repo",
        "http://127.0.0.1:8443/gh-api/repos/owner/repo",
        "https://api.github.com/repos/owner/repo/../repo",
        "https://api.github.com/repos/owner/repo/%2e%2e",
        "https://api.github.com/repos/owner/repo?access_token=secret",
        "https://api.github.com/repos/owner/repo?page=1&page=2",
        "https://api.github.com/repos/owner/repo#fragment",
        "https://api.github.com/repos/owner/repo/contents",
        "https://api.github.com/user",
        "https://api.github.com/graphql",
    ] {
        assert!(fetch_request(&json!({"url":url})).is_none(), "{url}");
    }
    for extra in [
        json!({"method":"POST"}),
        json!({"method":"HEAD"}),
        json!({"method":"get"}),
        json!({"body":"payload"}),
        json!({"body":{}}),
        json!({"headers":null}),
        json!({"headers":{"User-Agent":42}}),
        json!({"headers":{"User-Agent":null}}),
        json!({"headers":{"User-Agent":"kars","Authorization":"Bearer caller"}}),
        json!({"headers":{"Authorization":"Bearer caller"}}),
        json!({"headers":{"Cookie":"caller"}}),
        json!({"headers":{"Host":"evil"}}),
        json!({"headers":{"X-Api-Key":"caller"}}),
        json!({"headers":{"Proxy-Authorization":"caller"}}),
    ] {
        let mut req = json!({"url":"https://api.github.com/repos/owner/repo"});
        req.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(fetch_request(&req).is_none(), "{extra}");
    }
}

#[tokio::test]
async fn github_fetch_reuses_keyless_dispatch_cache_and_records_normal_observations() {
    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("owner/repo"), 1).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"number":42}]))
                .insert_header("date", "Mon, 01 Jan 2001 00:00:00 GMT")
                .insert_header("set-cookie", "private-cookie")
                .insert_header("location", "https://signed.example/secret"),
        )
        .expect(2)
        .mount(&server)
        .await;
    let (_directory, state) = fixture(&server).await;
    let scope = state.services.telemetry.cursor().0;
    for url in [
        "https://api.github.com/repos/owner/repo/issues",
        "https://github.com/owner/repo",
    ] {
        assert!(state.blocklist.check_egress(url, "agent").await.is_err());
    }
    let before = chrono::Utc::now().timestamp_millis();
    let (status, observation) = fetch(
        &state,
        Some("127.0.0.1:12"),
        json!({
            "url":"https://api.github.com/repos/owner/repo/issues", "method":"GET",
            "headers":{"Accept":"application/vnd.github+json","User-Agent":"kars"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(observation["status"], 200);
    assert_eq!(observation["body"], "[{\"number\":42}]");
    let observed_at = observation["observed_at"].as_str().unwrap();
    assert!(observed_at.ends_with('Z'));
    let observed = chrono::DateTime::parse_from_rfc3339(observed_at)
        .unwrap()
        .timestamp_millis();
    assert!((before..=chrono::Utc::now().timestamp_millis()).contains(&observed));
    assert_eq!(observation["truncated"], false);
    assert!(observation["headers"].get("set-cookie").is_none());
    assert!(observation["headers"].get("location").is_none());
    assert!(!observation.to_string().contains(TOKEN));
    let result = routes(state.clone())
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/gh-api/repos/owner/repo/issues")
                .extension(ConnectInfo("127.0.0.1:12".parse::<SocketAddr>().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    let calls = server.received_requests().await.unwrap();
    assert_eq!(
        calls.len(),
        4,
        "one discovery/mint shared by both fetch and gh-api"
    );
    for call in &calls[2..] {
        assert_eq!(call.headers["authorization"], format!("Bearer {TOKEN}"));
        assert!(!call.headers.contains_key("cookie"));
        assert_eq!(call.headers["user-agent"], "kars-inference-router");
        assert_eq!(call.headers["accept"], "application/vnd.github+json");
    }
    let events = state.services.telemetry.snapshot(&scope, 0).unwrap();
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["name"] == "http_fetch"
                && event["source"] == "router"
                && event["ok"] == true
                && event["complete"] == true)
    );
    server.verify().await;
}

#[tokio::test]
async fn github_fetch_and_rest_strip_nested_repository_credentials() {
    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("owner/repo"), 1).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name":"owner/repo", "private":true, "default_branch":"main",
            "temp_clone_token":"fixture-clone-credential",
            "parent":{"id":42,"temp_clone_token":"fixture-parent-credential"},
            "related":[{"repo":{"temp_clone_token":"fixture-nested-credential","id":43}},
                {"temp_clone_token":null,"id":44}],
            "description":"temp_clone_token is a field name, not a credential here"
        })))
        .expect(2)
        .mount(&server)
        .await;
    let expected = json!({
        "full_name":"owner/repo", "private":true, "default_branch":"main",
        "parent":{"id":42}, "related":[{"repo":{"id":43}},{"id":44}],
        "description":"temp_clone_token is a field name, not a credential here"
    });
    let (_directory, state) = fixture(&server).await;
    let scope = state.services.telemetry.cursor().0;
    let (status, observation) = fetch(
        &state,
        Some("127.0.0.1:12"),
        json!({"url":"https://api.github.com/repos/owner/repo"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(observation["status"], 200);
    assert_eq!(
        serde_json::from_str::<Value>(observation["body"].as_str().unwrap()).unwrap(),
        expected
    );
    let response = routes(state.clone())
        .with_state(state.clone())
        .oneshot(
            Request::builder()
                .uri("/gh-api/repos/owner/repo")
                .extension(ConnectInfo("127.0.0.1:12".parse::<SocketAddr>().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/json; charset=utf-8"
    );
    let bytes = to_bytes(response.into_body(), API_LIMIT).await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected);
    let events = state.services.telemetry.snapshot(&scope, 0).unwrap();
    for value in [
        "fixture-clone-credential",
        "fixture-parent-credential",
        "fixture-nested-credential",
    ] {
        assert!(!observation.to_string().contains(value));
        assert!(!events.to_string().contains(value));
    }
    server.verify().await;
}

#[tokio::test]
async fn github_fetch_preserves_failed_and_truncated_upstream_evidence() {
    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("owner/repo"), 1).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/42"))
        .respond_with(ResponseTemplate::new(404).set_body_string("private diagnostic"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/jobs/42/logs"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("discarded prefix{}", "x".repeat(LOG_TAIL))),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (_directory, state) = fixture(&server).await;
    let (_, denied) = fetch(
        &state,
        Some("127.0.0.1:12"),
        json!({
            "url":"https://api.github.com/repos/owner/repo/issues/42"
        }),
    )
    .await;
    assert_eq!(denied["status"], 404);
    assert!(!denied.to_string().contains("private diagnostic"));
    let (_, partial) = fetch(
        &state,
        Some("127.0.0.1:12"),
        json!({
            "url":"https://api.github.com/repos/owner/repo/actions/jobs/42/logs"
        }),
    )
    .await;
    assert_eq!(partial["status"], 200);
    assert_eq!(partial["truncated"], true);
    let body = partial["body"].as_str().unwrap();
    let marker = format!("... [truncated at {LOG_TAIL} bytes]");
    let payload = body
        .strip_suffix(&marker)
        .expect("fetch appends its truncation marker after the bounded log tail");
    assert_eq!(payload.len(), LOG_TAIL);
    assert!(payload.bytes().all(|byte| byte == b'x'));
    let scope = state.services.telemetry.cursor().0;
    let events = state.services.telemetry.snapshot(&scope, 0).unwrap();
    let events = events["events"].as_array().unwrap();
    assert!(events.iter().any(|event| event["name"] == "http_fetch"
        && event["ok"] == false
        && event["http_status"] == 404));
    assert!(events.iter().any(|event| event["name"] == "http_fetch"
        && event["ok"].is_null()
        && event["complete"] == false));
    server.verify().await;
}

#[tokio::test]
async fn github_fetch_denies_peer_scope_threat_capacity_and_shutdown_before_upstream() {
    let server = MockServer::start().await;
    let (directory, mut state) = fixture(&server).await;
    let body = json!({"url":"https://api.github.com/repos/owner/repo", "method":"GET"});
    for peer in [None, Some("10.0.0.2:12")] {
        assert_eq!(
            fetch(&state, peer, body.clone()).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (_, denied) = fetch(
        &state,
        Some("127.0.0.1:12"),
        json!({"url":"https://api.github.com/repos/other/repo"}),
    )
    .await;
    assert_eq!(denied["status"], 403);
    let seed = directory.path().join("blocked-domains");
    std::fs::write(&seed, "api.github.com\n").unwrap();
    state.blocklist = crate::blocklist::Blocklist::new(seed.to_str()).await;
    assert_eq!(
        fetch(&state, Some("127.0.0.1:12"), body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    state.blocklist = crate::blocklist::Blocklist::new(None).await;
    let config = state.services.github.get().unwrap().as_ref().unwrap();
    let permit = config.slots.acquire_many(2).await.unwrap();
    assert_eq!(
        fetch(&state, Some("127.0.0.1:12"), body.clone()).await.1["status"],
        429
    );
    drop(permit);
    state.services.shutdown.cancel();
    assert_ne!(
        fetch(&state, Some("127.0.0.1:12"), body).await.0,
        StatusCode::OK
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn github_fetch_invalid_or_removed_configuration_never_falls_back() {
    let server = MockServer::start().await;
    let (directory, state) = fixture(&server).await;
    let body = json!({"url":"https://api.github.com/repos/owner/repo"});
    assert!(
        prepare_fetch(&state, Some("127.0.0.1:12".parse().unwrap()), &body)
            .await
            .unwrap()
            .is_some()
    );
    std::fs::write(directory.path().join("config.json"), "invalid").unwrap();
    assert_eq!(
        fetch(&state, Some("127.0.0.1:12"), body.clone()).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    std::fs::remove_file(directory.path().join("config.json")).unwrap();
    assert_eq!(
        fetch(&state, Some("127.0.0.1:12"), body).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn github_fetch_unbound_and_other_hosts_remain_ordinary_public_fetch() {
    let server = MockServer::start().await;
    let (directory, mut state) = fixture(&server).await;
    std::fs::remove_file(directory.path().join("config.json")).unwrap();
    assert!(
        prepare_fetch(
            &state,
            None,
            &json!({"url":"https://api.github.com/repos/owner/repo"})
        )
        .await
        .unwrap()
        .is_none()
    );
    Mock::given(method("GET"))
        .and(path("/public"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-kars-log-truncated", "true")
                .set_body_string("Public evidence"),
        )
        .expect(1)
        .mount(&server)
        .await;
    state.client = reqwest::Client::builder()
        .no_proxy()
        .resolve("public.example", *server.address())
        .build()
        .unwrap();
    let (status, observation) = fetch(
        &state,
        None,
        json!({
            "url":format!("http://public.example:{}/public", server.address().port())
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(observation["body"], "Public evidence");
    assert_eq!(observation["truncated"], false);
    assert!(
        !server.received_requests().await.unwrap()[0]
            .headers
            .contains_key("authorization")
    );
    server.verify().await;
}
