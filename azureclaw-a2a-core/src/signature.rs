// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! JWS detached-content signature primitives for AgentCard signing.
//!
//! Implements the small subset of RFC 7515 needed to sign and verify
//! [`AgentCardSignature`](super::agent_card::AgentCardSignature) entries:
//!
//! 1. Build the JWS signing input: `BASE64URL(protected) || '.' || BASE64URL(payload)`.
//! 2. Sign that input bytewise with EdDSA (Ed25519, `alg = "EdDSA"`)
//!    per RFC 8037.
//! 3. Verify symmetrically.
//!
//! ## Why not `jsonwebtoken`?
//!
//! The `jsonwebtoken` crate is geared toward the **compact** JWS
//! serialisation (one signed JSON token). A2A AgentCards use the
//! **flat JSON** serialisation: the signature is embedded back into
//! the manifest itself. We only need the signing-input construction
//! plus an Ed25519 sign/verify primitive, both of which are already
//! workspace deps.
//!
//! ## Algorithm allow-list
//!
//! Only `alg = "EdDSA"` is accepted. We do **not** parse `alg = "none"`
//! and **never** dispatch on `alg` — the verifier is hard-coded to
//! Ed25519, so an attacker substituting `{"alg":"none"}` produces
//! invalid signature bytes that fail `verify_strict`. RFC 8725 §3.1
//! "alg confusion" attack is therefore structurally precluded.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Errors raised when constructing or validating a JWS signing input.
#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("base64url decode failed: {0}")]
    Base64(String),
    #[error("protected header is not valid JSON: {0}")]
    ProtectedHeaderJson(String),
    #[error("protected header missing or non-string `alg`")]
    ProtectedAlgMissing,
    #[error("unsupported alg `{0}`; only `EdDSA` is permitted")]
    UnsupportedAlg(String),
    #[error("payload bytes are empty")]
    EmptyPayload,
}

/// The canonical bytes that an Ed25519 signer must sign and a verifier
/// must verify, per RFC 7515 §5.1.
///
/// Returned as an owned `Vec<u8>` so callers can pass it straight to
/// `ed25519_dalek::SigningKey::sign`.
pub struct SignatureInput {
    pub protected_b64u: String,
    pub payload_b64u: String,
    pub signing_input: Vec<u8>,
}

impl std::fmt::Debug for SignatureInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignatureInput")
            .field("protected_b64u", &self.protected_b64u)
            .field("payload_b64u_len", &self.payload_b64u.len())
            .field("signing_input_len", &self.signing_input.len())
            .finish()
    }
}

/// Build the JWS signing input from a protected header (already JSON
/// bytes) and a payload (the AgentCard JSON bytes minus its
/// `signatures` field).
///
/// Validates that the protected header parses as JSON, contains
/// `alg = "EdDSA"`, and the payload is non-empty. Wire-format
/// validation lives here so that any caller — signer or verifier —
/// gets the same checks.
pub fn build_signing_input(
    protected_header_json: &[u8],
    payload: &[u8],
) -> Result<SignatureInput, SignatureError> {
    if payload.is_empty() {
        return Err(SignatureError::EmptyPayload);
    }
    let header: serde_json::Value = serde_json::from_slice(protected_header_json)
        .map_err(|e| SignatureError::ProtectedHeaderJson(e.to_string()))?;
    let alg = header
        .get("alg")
        .and_then(|v| v.as_str())
        .ok_or(SignatureError::ProtectedAlgMissing)?;
    if alg != "EdDSA" {
        return Err(SignatureError::UnsupportedAlg(alg.to_string()));
    }

    let protected_b64u = base64url_encode(protected_header_json);
    let payload_b64u = base64url_encode(payload);
    let mut signing_input = Vec::with_capacity(protected_b64u.len() + 1 + payload_b64u.len());
    signing_input.extend_from_slice(protected_b64u.as_bytes());
    signing_input.push(b'.');
    signing_input.extend_from_slice(payload_b64u.as_bytes());

    Ok(SignatureInput {
        protected_b64u,
        payload_b64u,
        signing_input,
    })
}

pub fn base64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn base64url_decode(s: &str) -> Result<Vec<u8>, SignatureError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| SignatureError::Base64(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

    fn fixed_key() -> SigningKey {
        let bytes = [7u8; 32];
        SigningKey::from_bytes(&bytes)
    }

    #[test]
    fn build_signing_input_concatenates_with_dot() {
        let header = br#"{"alg":"EdDSA","kid":"k1"}"#;
        let payload = b"{\"name\":\"a\"}";
        let si = build_signing_input(header, payload).unwrap();
        assert!(si.signing_input.contains(&b'.'));
        let s = std::str::from_utf8(&si.signing_input).unwrap();
        let parts: Vec<&str> = s.split('.').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], si.protected_b64u);
        assert_eq!(parts[1], si.payload_b64u);
    }

    #[test]
    fn rejects_alg_none() {
        let header = br#"{"alg":"none"}"#;
        let payload = b"x";
        let err = build_signing_input(header, payload).unwrap_err();
        match err {
            SignatureError::UnsupportedAlg(a) => assert_eq!(a, "none"),
            other => panic!("expected UnsupportedAlg, got {other:?}"),
        }
    }

    #[test]
    fn rejects_alg_rs256() {
        let header = br#"{"alg":"RS256"}"#;
        let err = build_signing_input(header, b"x").unwrap_err();
        assert!(matches!(err, SignatureError::UnsupportedAlg(_)));
    }

    #[test]
    fn rejects_missing_alg() {
        let header = br#"{"kid":"k1"}"#;
        let err = build_signing_input(header, b"x").unwrap_err();
        assert!(matches!(err, SignatureError::ProtectedAlgMissing));
    }

    #[test]
    fn rejects_non_string_alg() {
        let header = br#"{"alg":123}"#;
        let err = build_signing_input(header, b"x").unwrap_err();
        assert!(matches!(err, SignatureError::ProtectedAlgMissing));
    }

    #[test]
    fn rejects_malformed_protected_header() {
        let header = b"not json";
        let err = build_signing_input(header, b"x").unwrap_err();
        assert!(matches!(err, SignatureError::ProtectedHeaderJson(_)));
    }

    #[test]
    fn rejects_empty_payload() {
        let header = br#"{"alg":"EdDSA"}"#;
        let err = build_signing_input(header, b"").unwrap_err();
        assert!(matches!(err, SignatureError::EmptyPayload));
    }

    #[test]
    fn base64url_round_trip() {
        let bytes = b"hello \xff\x00 world";
        let enc = base64url_encode(bytes);
        // No padding, no `+`, no `/`.
        assert!(!enc.contains('='));
        assert!(!enc.contains('+'));
        assert!(!enc.contains('/'));
        let dec = base64url_decode(&enc).unwrap();
        assert_eq!(dec, bytes);
    }

    #[test]
    fn ed25519_sign_verify_round_trip() {
        let key = fixed_key();
        let header = br#"{"alg":"EdDSA"}"#;
        let payload = b"{\"name\":\"a\",\"version\":\"0.1\"}";
        let si = build_signing_input(header, payload).unwrap();
        let sig = key.sign(&si.signing_input);
        let vk: VerifyingKey = key.verifying_key();
        assert!(vk.verify(&si.signing_input, &sig).is_ok());
    }

    #[test]
    fn ed25519_verification_fails_on_payload_tamper() {
        let key = fixed_key();
        let header = br#"{"alg":"EdDSA"}"#;
        let original = b"{\"name\":\"a\"}";
        let si = build_signing_input(header, original).unwrap();
        let sig = key.sign(&si.signing_input);

        let tampered = b"{\"name\":\"b\"}";
        let si2 = build_signing_input(header, tampered).unwrap();
        let vk: VerifyingKey = key.verifying_key();
        assert!(vk.verify(&si2.signing_input, &sig).is_err());
    }

    #[test]
    fn ed25519_verification_fails_on_header_tamper() {
        let key = fixed_key();
        let header_a = br#"{"alg":"EdDSA","kid":"k1"}"#;
        let header_b = br#"{"alg":"EdDSA","kid":"k2"}"#;
        let payload = b"{\"name\":\"a\"}";
        let si_a = build_signing_input(header_a, payload).unwrap();
        let sig = key.sign(&si_a.signing_input);

        let si_b = build_signing_input(header_b, payload).unwrap();
        let vk: VerifyingKey = key.verifying_key();
        assert!(vk.verify(&si_b.signing_input, &sig).is_err());
    }

    #[test]
    fn ed25519_verification_fails_with_wrong_key() {
        let key = fixed_key();
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let header = br#"{"alg":"EdDSA"}"#;
        let payload = b"{\"name\":\"a\"}";
        let si = build_signing_input(header, payload).unwrap();
        let sig = key.sign(&si.signing_input);

        let other_vk: VerifyingKey = other.verifying_key();
        assert!(other_vk.verify(&si.signing_input, &sig).is_err());
    }

    #[test]
    fn signing_input_is_ascii_safe_for_dot_split() {
        // The signing input must split on a single `.` byte cleanly,
        // no matter what bytes the payload contained — base64url
        // strips `+/=` and never emits `.`.
        let header = br#"{"alg":"EdDSA"}"#;
        let payload = b"\x00\x01\x02\xff\xfe.\xfd\x7f";
        let si = build_signing_input(header, payload).unwrap();
        let dots = si.signing_input.iter().filter(|b| **b == b'.').count();
        assert_eq!(dots, 1);
    }
}
