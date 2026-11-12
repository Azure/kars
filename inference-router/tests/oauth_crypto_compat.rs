use jsonwebtoken::Algorithm;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use kars_inference_router::mcp::{OAuthError, OAuthVerifierConfig, verify_access_token};
use serde_json::json;
use std::collections::HashMap;

const TEST_ISSUER: &str = "https://idp.example.com";
const TEST_AUDIENCE: &str = "https://router.example.com/mcp";

fn fixture_jwks(algorithm: Algorithm) -> JwkSet {
    let jwk = match algorithm {
        Algorithm::ES256 => json!({
            "kty": "EC",
            "crv": "P-256",
            "x": "e4RBn495ue5pxUPATOJtdDvUyCXx4sLqYDEn7IHDgps",
            "y": "1BfDK6NxwN3DpKVM9wigXFPJLwvoJ0DxL8mvePbzgQg",
            "alg": "ES256",
            "kid": "oauth-fixture-2026",
            "use": "sig",
        }),
        Algorithm::RS256 => rsa_fixture_jwk("RS256"),
        Algorithm::PS256 => rsa_fixture_jwk("PS256"),
        _ => panic!("unsupported OAuth fixture algorithm: {algorithm:?}"),
    };
    JwkSet {
        keys: vec![serde_json::from_value(jwk).unwrap()],
    }
}

fn rsa_fixture_jwk(algorithm: &str) -> serde_json::Value {
    json!({
        "kty": "RSA",
        "n": "6HiOI0VlxQ4cOGfFeuXACs6Zi2IzauJzDfFhtOXxvzoev2Vwwb7YDVfPJkdCqNYPcfLu-LDN0_w4Z9yLAv7HoTIQlrpY7Mf40RO5hMgd7-oV4R3dzkcTVC_KFqtuCXrdfH2kG_dzHXqn__7NEz4_GtFSmO2ImjYu5Lp6gACIhcbwDd7XYZNU9uOOqQ4s4YGZ4CweT-MfoVtE1W5OY8tlf857Afz0kmazT95WDUzva91SpTcaNlgTLGHo7E7sLOsZ05W-tqB4yC9qNjF76c_tSre4dVVDp8jkl7nm4NWIGkpZ6qjc3MgK-Vs4eoTdxKXkZPUqchg4H7rQZbs66v5dbQ",
        "e": "AQAB",
        "alg": algorithm,
        "kid": "oauth-fixture-2026",
        "use": "sig",
    })
}

fn fixture_config(algorithm: Algorithm, jwks: JwkSet) -> OAuthVerifierConfig {
    let mut trusted_issuers = HashMap::new();
    trusted_issuers.insert(TEST_ISSUER.to_string(), jwks);
    OAuthVerifierConfig {
        trusted_issuers,
        expected_audience: TEST_AUDIENCE.into(),
        per_issuer_audience: HashMap::new(),
        allowed_algorithms: vec![algorithm],
        leeway_seconds: 30,
        required_scopes: vec!["mcp.tools.read".into()],
        per_issuer_scopes: HashMap::new(),
    }
}

fn alter_token_signature(token: &str) -> String {
    use base64::Engine;
    let parts: Vec<_> = token.trim().split('.').collect();
    let mut signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .unwrap();
    signature[0] ^= 1;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature);
    format!("{}.{}.{}", parts[0], parts[1], signature)
}

fn alter_token_subject(token: &str) -> String {
    use base64::Engine;
    let parts: Vec<_> = token.trim().split('.').collect();
    let mut claims: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[1])
            .unwrap(),
    )
    .unwrap();
    claims["sub"] = json!("tampered-subject");
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).unwrap());
    format!("{}.{}.{}", parts[0], payload, parts[2])
}

#[test]
fn supported_jwk_algorithms_verify_and_reject_signature_and_claim_tampering() {
    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let vectors = [
        (
            Algorithm::ES256,
            include_str!("fixtures/oauth/es256.jwt"),
            include_str!("fixtures/oauth/es256-wrong-aud.jwt"),
        ),
        (
            Algorithm::RS256,
            include_str!("fixtures/oauth/rs256.jwt"),
            include_str!("fixtures/oauth/rs256-wrong-aud.jwt"),
        ),
        (
            Algorithm::PS256,
            include_str!("fixtures/oauth/ps256.jwt"),
            include_str!("fixtures/oauth/ps256-wrong-aud.jwt"),
        ),
    ];

    for (algorithm, token, wrong_audience_token) in vectors {
        let config = fixture_config(algorithm, fixture_jwks(algorithm));
        let verified = verify_access_token(token.trim(), &config)
            .unwrap_or_else(|error| panic!("{algorithm:?} fixture rejected: {error:?}"));
        assert_eq!(verified.subject, "oauth-fixture-user");
        assert_eq!(verified.issuer, TEST_ISSUER);
        assert_eq!(verified.audience, TEST_AUDIENCE);
        assert_eq!(verified.scopes, vec!["mcp.tools.read"]);

        assert!(
            matches!(
                verify_access_token(&alter_token_signature(token), &config),
                Err(OAuthError::ValidationFailed(_))
            ),
            "{algorithm:?} accepted a modified signature"
        );
        assert!(
            matches!(
                verify_access_token(&alter_token_subject(token), &config),
                Err(OAuthError::ValidationFailed(_))
            ),
            "{algorithm:?} accepted a modified claim"
        );
        assert!(
            matches!(
                verify_access_token(wrong_audience_token.trim(), &config),
                Err(OAuthError::ValidationFailed(_))
            ),
            "{algorithm:?} accepted a correctly signed token with the wrong audience"
        );
    }
}

#[test]
fn aws_lc_rejects_1024_bit_rsa_jwk() {
    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let jwk: Jwk = serde_json::from_value(json!({
        "kty": "RSA",
        "n": "yohmLjAATalnNtb4iBfLmSJ-zmXTKRlhGQGX6PFvNzJDP8ldE1gZZqSmqpLTbn5AEQmrLDqbPqjIwbLAPSavugTugya2NteYIxPYs2cFIUh1dF3P2QIU7syzCoSa31hfjUEyb9JBBgU_JfoLGyDqXogAvmi2ZaVMW0boC1VWayk",
        "e": "AQAB",
        "alg": "RS256",
        "kid": "oauth-fixture-2026",
        "use": "sig",
    }))
    .unwrap();
    let config = fixture_config(Algorithm::RS256, JwkSet { keys: vec![jwk] });
    let error = verify_access_token(
        include_str!("fixtures/oauth/rs256-1024.jwt").trim(),
        &config,
    )
    .expect_err("AWS-LC must reject RSA keys below 2048 bits");
    assert!(
        matches!(
            error,
            OAuthError::DecodingKey(_) | OAuthError::ValidationFailed(_)
        ),
        "unexpected rejection: {error:?}"
    );
}
