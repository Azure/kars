// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use jsonwebtoken::{Algorithm, jwk::JwkSet};
use kars_inference_router::mcp::{OAuthError, OAuthVerifierConfig, verify_access_token};
use serde_json::json;
use std::collections::HashMap;

fn config_for_key(public: &[u8]) -> OAuthVerifierConfig {
    let jwk = json!({
        "kty": "OKP",
        "crv": "Ed25519",
        "x": URL_SAFE_NO_PAD.encode(public),
        "alg": "EdDSA",
        "kid": "preinstalled-provider-fixture",
        "use": "sig",
    });
    OAuthVerifierConfig {
        trusted_issuers: HashMap::from([(
            "https://idp.example.com".into(),
            JwkSet {
                keys: vec![serde_json::from_value(jwk).unwrap()],
            },
        )]),
        expected_audience: "https://router.example.com/mcp".into(),
        per_issuer_audience: HashMap::new(),
        allowed_algorithms: vec![Algorithm::EdDSA],
        leeway_seconds: 30,
        required_scopes: Vec::new(),
        per_issuer_scopes: HashMap::new(),
    }
}

#[test]
fn preinstalled_provider_cannot_bypass_weak_key_rejection() {
    jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER
        .install_default()
        .expect("this isolated test installs AWS-LC before Kars");

    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "alg": "EdDSA",
            "kid": "preinstalled-provider-fixture",
        }))
        .unwrap(),
    );
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iss": "https://idp.example.com",
            "sub": "oauth-fixture-user",
            "aud": "https://router.example.com/mcp",
            "exp": 4102444800_u64,
        }))
        .unwrap(),
    );
    let message = format!("{header}.{claims}");
    let mut identity = [0_u8; 32];
    identity[0] = 1;
    let mut forged = [0_u8; 64];
    forged[0] = 1;
    let forged = format!("{message}.{}", URL_SAFE_NO_PAD.encode(forged));
    assert!(matches!(
        verify_access_token(&forged, &config_for_key(&identity)),
        Err(OAuthError::ValidationFailed(_))
    ));

    let signer = SigningKey::from_bytes(&[42; 32]);
    let signature = signer.sign(message.as_bytes()).to_bytes();
    let valid = format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature));
    let verified = verify_access_token(&valid, &config_for_key(&signer.verifying_key().to_bytes()))
        .expect("a valid issuer key still works with a preinstalled provider");
    assert_eq!(verified.subject, "oauth-fixture-user");
}
