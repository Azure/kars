// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![cfg(test)]

use super::*;
use axum::{body::Body, http::Request};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

struct RegistryEnvironment(Option<String>);

impl Drop for RegistryEnvironment {
    fn drop(&mut self) {
        // SAFETY: the enclosing with_env closure holds the mesh ENV_GUARD.
        unsafe {
            match &self.0 {
                Some(value) => std::env::set_var("AGT_REGISTRY_URL", value),
                None => std::env::remove_var("AGT_REGISTRY_URL"),
            }
        }
    }
}

fn signed_registration() -> serde_json::Value {
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let proof_input = format!("{public_key}{timestamp}");
    let proof = signing_key.sign(proof_input.as_bytes());
    signing_key
        .verifying_key()
        .verify_strict(proof_input.as_bytes(), &proof)
        .unwrap();
    json!({
        "public_key": public_key,
        "proof": URL_SAFE_NO_PAD.encode(proof.to_bytes()),
        "proof_timestamp": timestamp,
        "metadata": {"display_name": "viz"},
        "capabilities": ["viz", "kars-agent"],
    })
}

#[test]
fn registration_rejects_signed_cross_name_deep_json_before_upstream_dispatch() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    super::tests::with_env("SANDBOX_NAME", "viz", || {
        runtime.block_on(async {
            let registry = MockServer::start().await;
            let _restore = RegistryEnvironment(std::env::var("AGT_REGISTRY_URL").ok());
            // SAFETY: with_env holds the same guard as other mesh environment tests.
            unsafe { std::env::set_var("AGT_REGISTRY_URL", registry.uri()) };
            Mock::given(method("POST"))
                .and(path("/v1/agents"))
                .respond_with(ResponseTemplate::new(201))
                .expect(1)
                .mount(&registry)
                .await;

            let registration = signed_registration();
            let mut impersonation = registration.clone();
            impersonation["metadata"]["display_name"] = json!("victim");
            impersonation["capabilities"] = json!(["victim", "kars-agent"]);
            let encoded = serde_json::to_string(&impersonation).unwrap();
            let deep = format!(
                "{},\"ignored\":{}0{}}}",
                encoded.strip_suffix('}').unwrap(),
                "[".repeat(140),
                "]".repeat(140),
            );
            assert!(
                serde_json::from_str::<serde_json::Value>(&deep)
                    .unwrap_err()
                    .to_string()
                    .contains("recursion limit")
            );
            let mut state = super::super::model_routing::tests::test_state(
                crate::config::Config::from_env().unwrap(),
            );
            state.client = reqwest::Client::builder().no_proxy().build().unwrap();
            let router = mesh_routes().with_state(state);
            for (body, expected) in [
                (deep, StatusCode::BAD_REQUEST),
                ("not json".into(), StatusCode::BAD_REQUEST),
                (encoded, StatusCode::FORBIDDEN),
                (registration.to_string(), StatusCode::CREATED),
            ] {
                let response = router
                    .clone()
                    .oneshot(
                        Request::post("/agt/registry/v1/agents")
                            .header("content-type", "application/json")
                            .body(Body::from(body))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected);
            }
            let requests = registry.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests[0].body_json::<serde_json::Value>().unwrap(),
                registration
            );
        });
    });
}

#[test]
fn registration_rejects_noncanonical_paths_before_upstream_dispatch() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    super::tests::with_env("SANDBOX_NAME", "viz", || {
        runtime.block_on(async {
            let registry = MockServer::start().await;
            let _restore = RegistryEnvironment(std::env::var("AGT_REGISTRY_URL").ok());
            // SAFETY: with_env holds the same guard as other mesh environment tests.
            unsafe { std::env::set_var("AGT_REGISTRY_URL", registry.uri()) };
            // Accept every POST so an unexpected dispatch cannot hide behind a
            // mock 404 or a redirect that a production registry might normalize.
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(201))
                .expect(3)
                .mount(&registry)
                .await;
            let mut state = super::super::model_routing::tests::test_state(
                crate::config::Config::from_env().unwrap(),
            );
            state.client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let router = mesh_routes().with_state(state);
            let registration = signed_registration();
            let mut impersonation = registration.clone();
            impersonation["metadata"]["display_name"] = json!("victim");
            impersonation["capabilities"] = json!(["victim", "kars-agent"]);

            for suffix in [
                "v1/agents%23x",
                "v1/agents%3Fx=1",
                "v1/agents%3fx=1",
                "v1/agents%23x?real=query",
                "v1/agents%3Fx=1?real=query",
                "v1%2Fagents",
                "v1%2fagents",
                "v1/agents%2F",
                "v1/agents%2f",
                "v1%5Cagents",
                "v1/agents%5c",
                "v1//agents",
                "v1/agents/",
                "v1/./agents",
                "v1/agents/.",
                "v1/unused/../agents",
                "v1/%2e/agents",
                "v1/agents/%2E",
                "v1/unused/%2e%2e/agents",
                "v1/%2fagents",
                "v1/agents%00",
                "v1/agents%09",
                "v1/agents%0A",
                "v1/agents%0D",
                "v1/agents%20",
                "v1/agents%2523x",
                "v1/agents%253Fx=1",
                "v1%252Fagents",
                "v1/agents%252f",
                "v1/agents%255c",
                "v1/%252e/agents",
            ] {
                for body in [&impersonation, &registration] {
                    let response = router
                        .clone()
                        .oneshot(
                            Request::post(format!("/agt/registry/{suffix}"))
                                .header("content-type", "application/json")
                                .body(Body::from(body.to_string()))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{suffix}");
                    assert!(registry.received_requests().await.unwrap().is_empty());
                }
            }

            let canonical_paths = [
                "/agt/registry/v1/agents",
                "/agt/registry/v1/agents?x=1&x=2&encoded=%23%3F%2F",
                "/agt/registry/%76%31/%61gents",
            ];
            // Percent-encoded ordinary characters still name the exact same
            // endpoint, so they must not skip name OR capability enforcement.
            let mut cross_capability = registration.clone();
            cross_capability["capabilities"] = json!(["victim"]);
            for uri in canonical_paths {
                for body in [&impersonation, &cross_capability] {
                    let response = router
                        .clone()
                        .oneshot(
                            Request::post(uri)
                                .header("content-type", "application/json")
                                .body(Body::from(body.to_string()))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
                    assert!(registry.received_requests().await.unwrap().is_empty());
                }
            }
            for uri in canonical_paths {
                let response = router
                    .clone()
                    .oneshot(
                        Request::post(uri)
                            .header("content-type", "application/json")
                            .body(Body::from(registration.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::CREATED, "{uri}");
            }
            let requests = registry.received_requests().await.unwrap();
            assert_eq!(requests.len(), 3);
            for request in &requests {
                assert_eq!(request.url.path(), "/v1/agents");
                assert_eq!(request.url.fragment(), None);
                assert_eq!(
                    request.body_json::<serde_json::Value>().unwrap(),
                    registration
                );
            }
            assert_eq!(requests[0].url.query(), None);
            assert_eq!(requests[1].url.query(), Some("x=1&x=2&encoded=%23%3F%2F"));
            assert_eq!(requests[2].url.query(), None);
        });
    });
}

#[test]
fn registry_proxy_preserves_supported_endpoints_base_path_and_raw_queries() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    super::tests::with_env("SANDBOX_NAME", "viz", || {
        runtime.block_on(async {
            let registry = MockServer::start().await;
            let _restore = RegistryEnvironment(std::env::var("AGT_REGISTRY_URL").ok());
            // SAFETY: with_env holds the same guard as other mesh environment tests.
            unsafe {
                std::env::set_var("AGT_REGISTRY_URL", format!("{}/gateway/", registry.uri()))
            };
            let cases = [
                ("GET", "health", "/gateway/health", None),
                (
                    "GET",
                    "v1/discover?capability=kars%3Aworker&capability=viz&limit=10&literal=a+b%20c%23%3F%2F",
                    "/gateway/v1/discover",
                    Some("capability=kars%3Aworker&capability=viz&limit=10&literal=a+b%20c%23%3F%2F"),
                ),
                ("GET", "v1/agents/did%3Amesh%3Apeer", "/gateway/v1/agents/did:mesh:peer", None),
                ("GET", "v1/agents/did:mesh:peer/prekeys", "/gateway/v1/agents/did:mesh:peer/prekeys", None),
                ("PUT", "v1/agents/did%3Amesh%3Apeer/prekeys", "/gateway/v1/agents/did:mesh:peer/prekeys", None),
                ("POST", "v1/agents/did%3Amesh%3Apeer/heartbeat", "/gateway/v1/agents/did:mesh:peer/heartbeat", None),
                ("POST", "v1/agents/did:mesh:peer/reputation", "/gateway/v1/agents/did:mesh:peer/reputation", None),
                ("DELETE", "v1/agents/did%3Amesh%3Apeer", "/gateway/v1/agents/did:mesh:peer", None),
                ("POST", "v1/sessions?mode=mesh", "/gateway/v1/sessions", Some("mode=mesh")),
                ("POST", "v1/registry/verify", "/gateway/v1/registry/verify", None),
                ("POST", "v1/agents?source=sdk", "/gateway/v1/agents", Some("source=sdk")),
            ];
            let mut state = super::super::model_routing::tests::test_state(
                crate::config::Config::from_env().unwrap(),
            );
            state.client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let router = mesh_routes().with_state(state);
            let registration = signed_registration().to_string();
            for (verb, suffix, upstream_path, _) in cases {
                Mock::given(method(verb))
                    .and(path(upstream_path))
                    .respond_with(ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&registry)
                    .await;
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(verb)
                            .uri(format!("/agt/registry/{suffix}"))
                            .header("content-type", "application/json")
                            .header("authorization", "Ed25519-Timestamp test-forwarded-unchanged")
                            .body(Body::from(registration.clone()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK, "{verb} {suffix}");
            }
            let requests = registry.received_requests().await.unwrap();
            assert_eq!(requests.len(), cases.len());
            for (request, (verb, _, upstream_path, query)) in requests.iter().zip(cases) {
                assert_eq!(request.method.as_str(), verb);
                assert_eq!(request.url.path(), upstream_path);
                assert_eq!(request.url.query(), query);
                assert_eq!(request.url.fragment(), None);
                assert_eq!(request.body, registration.as_bytes());
                assert_eq!(request.headers["content-type"], "application/json");
                assert_eq!(
                    request.headers["authorization"],
                    "Ed25519-Timestamp test-forwarded-unchanged"
                );
            }
        });
    });
}
