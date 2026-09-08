// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    config::{Config, ProviderEndpoint},
    failover::{Candidate, resolve_candidate},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn named_target(endpoint: String, credential: Option<&str>, id: &str) -> UpstreamConfig {
    let mut config = Config::from_env().unwrap();
    config.providers.insert(
        id.into(),
        ProviderEndpoint {
            tag: id.into(),
            endpoint,
            api_key: credential.map(str::to_string),
        },
    );
    resolve_candidate(
        &UpstreamConfig::azure(
            "https://default.openai.azure.com".into(),
            "model".into(),
            "test".into(),
        ),
        &config,
        &Candidate {
            provider: Some(id.into()),
            deployment: "model".into(),
            routing_intent: crate::failover::RoutingIntent::Explicit,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn keyless_named_local_and_remote_routes_never_consult_default_api_key_or_sidecar() {
    let inference = MockServer::start().await;
    let sidecar = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&sidecar)
        .await;
    Mock::given(|request: &wiremock::Request| {
        !request.headers.contains_key("authorization")
            && !request.headers.contains_key("api-key")
            && !request.headers.contains_key("x-api-key")
    })
    .respond_with(ResponseTemplate::new(200))
    .expect(4)
    .mount(&inference)
    .await;

    for host in ["model.models.svc.cluster.local", "model.example.test"] {
        let client = Client::builder()
            .no_proxy()
            .resolve(host, *inference.address())
            .build()
            .unwrap();
        for auth in [
            WorkloadIdentityAuth::for_test(Some("ambient-default-key"), None),
            WorkloadIdentityAuth::for_test(
                None,
                Some(crate::sidecar_client::SidecarClient::for_test(
                    sidecar.uri(),
                )),
            ),
        ] {
            let upstream = named_target(
                format!("http://{host}:{}", inference.address().port()),
                None,
                "keyless",
            );
            let headers = HeaderMap::from_iter([
                (
                    "authorization".parse().unwrap(),
                    HeaderValue::from_static("Bearer inbound-secret"),
                ),
                (
                    "api-key".parse().unwrap(),
                    HeaderValue::from_static("inbound-key"),
                ),
            ]);
            let (status, _, _) = forward(
                &auth,
                None,
                &client,
                &upstream,
                Method::POST,
                "chat/completions",
                &headers,
                Bytes::from("{}"),
            )
            .await
            .unwrap();
            assert_eq!(status, StatusCode::OK);
        }
    }
}

#[tokio::test]
async fn named_route_uses_only_its_own_key_and_legacy_default_still_uses_ambient_key() {
    let server = MockServer::start().await;
    for key in ["named-key", "ambient-default-key"] {
        Mock::given(header("authorization", format!("Bearer {key}")))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    let auth = WorkloadIdentityAuth::for_test(Some("ambient-default-key"), None);
    let named = named_target(server.uri(), Some("named-key"), "named");
    let default = UpstreamConfig::azure(server.uri(), "model".into(), "test".into());
    for upstream in [named, default] {
        assert_eq!(
            forward(
                &auth,
                None,
                &Client::new(),
                &upstream,
                Method::POST,
                "chat/completions",
                &HeaderMap::new(),
                Bytes::from("{}"),
            )
            .await
            .unwrap()
            .0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn real_copilot_host_selects_the_named_exchange_cache_not_the_default_account() {
    let exchange = MockServer::start().await;
    let inference = MockServer::start().await;
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    for (seat, jwt) in [
        ("default-seat", "default-jwt"),
        ("seat-a", "jwt-a"),
        ("seat-b", "jwt-b"),
    ] {
        Mock::given(header("authorization", format!("token {seat}")))
            .and(path("/exchange"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": jwt, "expires_at": expires_at, "refresh_in": 1500,
            })))
            .expect(1)
            .mount(&exchange)
            .await;
        Mock::given(header("authorization", format!("Bearer {jwt}")))
            .and(header("copilot-integration-id", COPILOT_INTEGRATION_ID))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&inference)
            .await;
    }
    let url = format!("{}/exchange", exchange.uri());
    let cache = CopilotTokenCache::with_test_exchange("default-seat", url);
    let endpoint = format!(
        "http://api.githubcopilot.com:{}",
        inference.address().port()
    );
    assert!(is_copilot_endpoint(&endpoint));
    let client = Client::builder()
        .no_proxy()
        .resolve("api.githubcopilot.com", *inference.address())
        .build()
        .unwrap();
    let auth = WorkloadIdentityAuth::for_test(Some("unrelated-azure-key"), None);
    for upstream in [
        named_target(endpoint.clone(), Some("seat-a"), "account-a"),
        named_target(endpoint.clone(), Some("seat-b"), "account-b"),
        UpstreamConfig::azure(endpoint, "model".into(), "test".into()),
    ] {
        assert_eq!(
            forward(
                &auth,
                Some(&cache),
                &client,
                &upstream,
                Method::POST,
                "chat/completions",
                &HeaderMap::new(),
                Bytes::from("{}"),
            )
            .await
            .unwrap()
            .0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn named_copilot_without_a_seat_token_cannot_borrow_the_default_account() {
    let exchange = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&exchange)
        .await;
    let upstream = named_target("https://api.githubcopilot.com".into(), None, "missing-seat");
    let result = credential_for_upstream(
        &WorkloadIdentityAuth::for_test(Some("azure-key"), None),
        Some(&CopilotTokenCache::with_test_exchange(
            "other-account",
            exchange.uri(),
        )),
        &upstream,
    )
    .await;
    assert!(result.is_err());
}
