use std::io::Write;
use std::process::{Command, Stdio};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde_json::{Value, json};

fn openssl(args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut child = Command::new("openssl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("OpenSSL is required for JWT backend interoperability tests");
    if let Some(input) = input {
        child.stdin.as_mut().unwrap().write_all(input).unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "OpenSSL failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    output.stdout
}

fn claims() -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "iss": "bridge-backend-test",
        "aud": "bridge-test",
        "iat": now,
        "exp": now + 300,
        "sub": "test-principal",
    })
}

fn validation(algorithm: Algorithm) -> Validation {
    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&["bridge-backend-test"]);
    validation.set_audience(&["bridge-test"]);
    validation
}

#[test]
fn rsa_app_jwts_support_pkcs1_and_pkcs8_keys_without_the_rustcrypto_rsa_dependency() {
    let pkcs8 = openssl(
        &[
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
        ],
        None,
    );
    let pkcs1 = openssl(&["pkey", "-traditional"], Some(&pkcs8));
    let public = openssl(&["pkey", "-pubout"], Some(&pkcs8));
    let claims = claims();
    for private in [&pkcs1, &pkcs8] {
        let token = encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &EncodingKey::from_rsa_pem(private).unwrap(),
        )
        .unwrap();
        let decoded = decode::<Value>(
            &token,
            &DecodingKey::from_rsa_pem(&public).unwrap(),
            &validation(Algorithm::RS256),
        )
        .unwrap();
        assert_eq!(decoded.claims, claims);
        let mut wrong_audience = validation(Algorithm::RS256);
        wrong_audience.set_audience(&["different-audience"]);
        assert!(
            decode::<Value>(
                &token,
                &DecodingKey::from_rsa_pem(&public).unwrap(),
                &wrong_audience,
            )
            .is_err()
        );
    }
}

#[test]
fn hmac_principal_jwts_still_verify_and_reject_a_different_key() {
    let key = openssl(&["rand", "32"], None);
    let claims = claims();
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&key),
    )
    .unwrap();
    assert_eq!(
        decode::<Value>(
            &token,
            &DecodingKey::from_secret(&key),
            &validation(Algorithm::HS256),
        )
        .unwrap()
        .claims,
        claims
    );
    let other_key = openssl(&["rand", "32"], None);
    assert!(
        decode::<Value>(
            &token,
            &DecodingKey::from_secret(&other_key),
            &validation(Algorithm::HS256),
        )
        .is_err()
    );
}
