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

            let signing_key = SigningKey::from_bytes(&[7; 32]);
            let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
            let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let proof_input = format!("{public_key}{timestamp}");
            let proof = signing_key.sign(proof_input.as_bytes());
            signing_key
                .verifying_key()
                .verify_strict(proof_input.as_bytes(), &proof)
                .unwrap();
            let registration = json!({
                "public_key": public_key,
                "proof": URL_SAFE_NO_PAD.encode(proof.to_bytes()),
                "proof_timestamp": timestamp,
                "metadata": {"display_name": "viz"},
                "capabilities": ["viz", "kars-agent"],
            });
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
