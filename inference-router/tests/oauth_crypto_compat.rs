// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use jsonwebtoken::Algorithm;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use kars_inference_router::mcp::{OAuthError, OAuthVerifierConfig, verify_access_token};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

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
fn legacy_1024_bit_rsa_jwk_remains_accepted() {
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
    let verified = verify_access_token(
        include_str!("fixtures/oauth/rs256-1024.jwt").trim(),
        &config,
    )
    .expect("rejecting an existing issuer's 1024-bit key is a breaking change");
    assert_eq!(verified.subject, "oauth-fixture-user");
}

#[derive(serde::Deserialize)]
struct RsaFixture {
    bits: usize,
    n: String,
    e: String,
    der: String,
    spki: String,
    tokens: BTreeMap<String, String>,
    wrong_salt_tokens: BTreeMap<String, String>,
}

#[test]
fn legacy_rsa_jwk_sizes_and_algorithms_remain_compatible() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let fixtures: Vec<RsaFixture> =
        serde_json::from_str(include_str!("fixtures/oauth/rsa-compat.json")).unwrap();
    assert_eq!(fixtures.len(), 7);
    assert_eq!(
        fixtures
            .iter()
            .map(|fixture| fixture.tokens.len())
            .sum::<usize>(),
        28,
    );

    for fixture in fixtures {
        for (name, token) in fixture.tokens {
            let algorithm: Algorithm = serde_json::from_value(json!(name)).unwrap();
            let jwk = json!({
                "kty": "RSA",
                "n": fixture.n,
                "e": fixture.e,
                "alg": name,
                "kid": "oauth-rsa-compat-2026",
                "use": "sig",
            });
            let config = fixture_config(
                algorithm,
                JwkSet {
                    keys: vec![serde_json::from_value(jwk.clone()).unwrap()],
                },
            );
            let verified = verify_access_token(&token, &config).unwrap_or_else(|error| {
                panic!(
                    "{name} with {}-bit RSA key rejected: {error:?}",
                    fixture.bits
                )
            });
            assert_eq!(verified.subject, "oauth-fixture-user");
            assert_eq!(verified.audience, TEST_AUDIENCE);

            for modified in [alter_token_signature(&token), alter_token_subject(&token)] {
                assert!(
                    verify_access_token(&modified, &config).is_err(),
                    "{name} {}-bit key accepted a modified token",
                    fixture.bits,
                );
            }

            let (signing_input, encoded_signature) = token.rsplit_once('.').unwrap();
            let signature = URL_SAFE_NO_PAD.decode(encoded_signature).unwrap();
            let mut extended = vec![0];
            extended.extend_from_slice(&signature);
            for malformed in [Vec::new(), signature[1..].to_vec(), extended] {
                let modified = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(malformed));
                assert!(
                    verify_access_token(&modified, &config).is_err(),
                    "{name} {}-bit key accepted an invalid signature length",
                    fixture.bits,
                );
            }

            let mut padded_jwk = jwk;
            for field in ["n", "e"] {
                let mut padded = vec![0];
                padded.extend_from_slice(
                    &URL_SAFE_NO_PAD
                        .decode(padded_jwk[field].as_str().unwrap())
                        .unwrap(),
                );
                padded_jwk[field] = json!(URL_SAFE_NO_PAD.encode(padded));
            }
            let padded_config = fixture_config(
                algorithm,
                JwkSet {
                    keys: vec![serde_json::from_value(padded_jwk).unwrap()],
                },
            );
            verify_access_token(&token, &padded_config)
                .expect("leading-zero public components were accepted by the baseline");
        }
    }
}

#[test]
fn invalid_rsa_components_and_oversized_moduli_remain_rejected() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let fixtures: Vec<RsaFixture> =
        serde_json::from_str(include_str!("fixtures/oauth/rsa-compat.json")).unwrap();
    let fixture = fixtures
        .iter()
        .find(|fixture| fixture.bits == 2048)
        .unwrap();
    let token = &fixture.tokens["RS256"];
    for (n, e) in [
        (vec![0], vec![3]),
        (vec![1], vec![3]),
        (vec![16], vec![3]),
        (vec![17], vec![0]),
        (vec![17], vec![1]),
        (vec![17], vec![2]),
        (vec![17], vec![19]),
        (vec![255; 1024], vec![1, 0, 1]),
    ] {
        let jwk = json!({
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(n),
            "e": URL_SAFE_NO_PAD.encode(e),
            "alg": "RS256",
            "kid": "oauth-rsa-compat-2026",
            "use": "sig",
        });
        let config = fixture_config(
            Algorithm::RS256,
            JwkSet {
                keys: vec![serde_json::from_value(jwk).unwrap()],
            },
        );
        assert!(
            verify_access_token(token, &config).is_err(),
            "invalid public RSA components were accepted",
        );
    }
}

#[test]
fn pss_requires_the_jose_digest_length_salt() {
    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let fixtures: Vec<RsaFixture> =
        serde_json::from_str(include_str!("fixtures/oauth/rsa-compat.json")).unwrap();
    let fixture = fixtures
        .iter()
        .find(|fixture| fixture.bits == 2048)
        .unwrap();
    assert_eq!(fixture.wrong_salt_tokens.len(), 3);
    for (name, token) in &fixture.wrong_salt_tokens {
        let algorithm: Algorithm = serde_json::from_value(json!(name)).unwrap();
        let jwk = json!({
            "kty": "RSA",
            "n": fixture.n,
            "e": fixture.e,
            "alg": name,
            "kid": "oauth-rsa-compat-2026",
            "use": "sig",
        });
        let config = fixture_config(
            algorithm,
            JwkSet {
                keys: vec![serde_json::from_value(jwk).unwrap()],
            },
        );
        assert!(
            matches!(
                verify_access_token(token, &config),
                Err(OAuthError::ValidationFailed(_)),
            ),
            "{name} accepted a cryptographically signed token with a non-JOSE PSS salt length",
        );
    }
}

#[test]
fn heavily_padded_public_components_still_verify() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let fixtures: Vec<RsaFixture> =
        serde_json::from_str(include_str!("fixtures/oauth/rsa-compat.json")).unwrap();
    let fixture = fixtures
        .iter()
        .find(|fixture| fixture.bits == 2048)
        .unwrap();
    let padded = |component: &str| {
        let mut bytes = vec![0; 1024];
        bytes.extend_from_slice(&URL_SAFE_NO_PAD.decode(component).unwrap());
        URL_SAFE_NO_PAD.encode(bytes)
    };
    let jwk = json!({
        "kty": "RSA",
        "n": padded(&fixture.n),
        "e": padded(&fixture.e),
        "alg": "RS256",
        "kid": "oauth-rsa-compat-2026",
        "use": "sig",
    });
    let config = fixture_config(
        Algorithm::RS256,
        JwkSet {
            keys: vec![serde_json::from_value(jwk).unwrap()],
        },
    );
    let verified = verify_access_token(&fixture.tokens["RS256"], &config)
        .expect("legacy JWK zero padding must not change signature verification");
    assert_eq!(verified.subject, "oauth-fixture-user");
}

#[test]
fn legacy_ed25519_jwk_trailing_bytes_remain_compatible() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};

    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let signer = SigningKey::from_bytes(&[42; 32]);
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "alg": "EdDSA",
            "typ": "JWT",
            "kid": "oauth-fixture-2026",
        }))
        .unwrap(),
    );
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iss": TEST_ISSUER,
            "sub": "oauth-fixture-user",
            "aud": TEST_AUDIENCE,
            "exp": 4102444800_u64,
            "scope": "mcp.tools.read",
        }))
        .unwrap(),
    );
    let message = format!("{header}.{claims}");
    let signature = signer.sign(message.as_bytes());
    let token = format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));

    for extra_bytes in [0, 1, 1024] {
        let mut public = signer.verifying_key().to_bytes().to_vec();
        public.extend(std::iter::repeat_n(0, extra_bytes));
        let jwk = json!({
            "kty": "OKP",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(public),
            "alg": "EdDSA",
            "kid": "oauth-fixture-2026",
            "use": "sig",
        });
        let config = fixture_config(
            Algorithm::EdDSA,
            JwkSet {
                keys: vec![serde_json::from_value(jwk).unwrap()],
            },
        );
        verify_access_token(&token, &config).unwrap_or_else(|error| {
            panic!("Ed25519 JWK with {extra_bytes} trailing bytes rejected: {error:?}")
        });
        assert!(verify_access_token(&alter_token_signature(&token), &config).is_err());
        assert!(verify_access_token(&alter_token_subject(&token), &config).is_err());
    }

    let original = signer.verifying_key().to_bytes();
    let mut wrong_key = original;
    wrong_key[0] ^= 1;
    for public in [&original[..31], &wrong_key[..]] {
        let jwk = json!({
            "kty": "OKP",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(public),
            "alg": "EdDSA",
            "kid": "oauth-fixture-2026",
            "use": "sig",
        });
        let config = fixture_config(
            Algorithm::EdDSA,
            JwkSet {
                keys: vec![serde_json::from_value(jwk).unwrap()],
            },
        );
        assert!(verify_access_token(&token, &config).is_err());
    }
}

#[test]
fn legacy_rsa_der_and_pem_keys_remain_compatible() {
    use base64::{
        Engine as _,
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    };
    use jsonwebtoken::{DecodingKey, Validation, decode};

    fn pem(label: &str, bytes: &[u8]) -> String {
        let encoded = STANDARD.encode(bytes);
        let lines: Vec<_> = encoded
            .as_bytes()
            .chunks(64)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect();
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            lines.join("\n")
        )
    }

    kars_inference_router::install_jsonwebtoken_crypto_provider();
    let fixtures: Vec<RsaFixture> =
        serde_json::from_str(include_str!("fixtures/oauth/rsa-compat.json")).unwrap();
    for fixture in fixtures {
        let der = URL_SAFE_NO_PAD.decode(fixture.der).unwrap();
        let spki = URL_SAFE_NO_PAD.decode(fixture.spki).unwrap();
        let keys = [
            DecodingKey::from_rsa_der(&der),
            DecodingKey::from_rsa_pem(pem("RSA PUBLIC KEY", &der).as_bytes()).unwrap(),
            DecodingKey::from_rsa_pem(pem("PUBLIC KEY", &spki).as_bytes()).unwrap(),
        ];
        for (name, token) in fixture.tokens {
            let algorithm: Algorithm = serde_json::from_value(json!(name)).unwrap();
            let mut validation = Validation::new(algorithm);
            validation.set_audience(&[TEST_AUDIENCE]);
            validation.set_issuer(&[TEST_ISSUER]);
            for key in &keys {
                let verified = decode::<serde_json::Value>(&token, key, &validation)
                    .unwrap_or_else(|error| {
                        panic!(
                            "{name} with {}-bit encoded RSA key rejected: {error}",
                            fixture.bits
                        )
                    });
                assert_eq!(verified.claims["sub"], "oauth-fixture-user");
                assert!(
                    decode::<serde_json::Value>(&alter_token_signature(&token), key, &validation)
                        .is_err(),
                );
            }
            for invalid_key in [
                DecodingKey::from_rsa_der(&[]),
                DecodingKey::from_rsa_der(&der[..der.len() - 1]),
                DecodingKey::from_rsa_der(&spki),
                DecodingKey::from_secret(&der),
            ] {
                assert!(decode::<serde_json::Value>(&token, &invalid_key, &validation).is_err(),);
            }
        }
    }
}
