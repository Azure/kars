//! AP2 IntentMandate detached-JWS signing primitive.
//!
//! Today [`IntentMandate::signature`](super::ap2::IntentMandate) is an
//! opaque string carried unchanged through [`validate_payment_attempt`]
//! (super::ap2). This module turns that opaque field into a real,
//! verifier-checkable detached JWS so a tampered or forged mandate
//! is rejected before a [`PaymentRecord`](super::ap2::PaymentRecord)
//! is appended.
//!
//! ## Canonical payload
//!
//! The signing payload is the JSON serialisation of the mandate with
//! its `signature` field set to the empty string. This matches the
//! pattern used by [`card_signing`](super::card_signing) for AgentCard
//! signatures: the signature does not protect itself, and verifiers
//! re-serialise the mandate they received (with `signature` cleared)
//! before validating.
//!
//! ## Wire format
//!
//! `IntentMandate.signature` carries `<protected_b64u>.<signature_b64u>`
//! — i.e. an RFC 7515 §3.1 detached JWS without the payload segment
//! (because the payload is the mandate body itself). Detached form is
//! used here so the `signature` field stays a single string and we
//! don't change the wire shape of `IntentMandate`.
//!
//! ## Algorithm pinning
//!
//! Only `alg = "EdDSA"` is accepted. RFC 8725 §3.1 considerations
//! (no `none`, explicit allow-list) apply identically.
//!
//! ## What this module does **not** do
//!
//! - It does not call any signing service. Callers pass an explicit
//!   [`SigningKey`] (router-side: today via the `SigningProvider`
//!   trait; tomorrow via AGT). Key custody stays at the call site.
//! - It does not check expiry, cap consistency, or counterparty
//!   policy — those stay in [`validate_payment_attempt`](super::ap2).
//!   This module only answers "is the mandate body authentic?".

use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::ap2::IntentMandate;
use super::signature::{SignatureError, base64url_decode, base64url_encode, build_signing_input};

/// Errors raised by [`sign_mandate`] and [`verify_mandate`].
#[derive(Debug, thiserror::Error)]
pub enum MandateSignError {
    /// Underlying JWS primitive failed (alg pinning, base64, header
    /// parse, etc.).
    #[error("jws: {0}")]
    Jws(#[from] SignatureError),

    /// Mandate serialisation produced invalid JSON. Should not happen
    /// for any mandate that round-trips through `serde_json`.
    #[error("serialise mandate: {0}")]
    Serialise(String),

    /// Protected-header construction emitted invalid JSON.
    #[error("protected header serialise: {0}")]
    ProtectedSerialise(String),

    /// `signature` field is the empty string.
    #[error("mandate carries no signature")]
    Unsigned,

    /// `signature` field is not in the expected `<protected>.<sig>`
    /// detached JWS form.
    #[error("malformed detached jws: expected `<protected>.<sig>` form")]
    MalformedDetached,

    /// Protected header decoded but its alg is not `EdDSA`, or it is
    /// missing required fields, or the header JSON is not valid.
    #[error("malformed protected header: {0}")]
    MalformedHeader(String),

    /// Signature decoded but is not 64 bytes (Ed25519).
    #[error("signature wrong length: expected 64 bytes, got {0}")]
    SignatureLength(usize),

    /// Signature was decoded and the kid resolved to a trusted key,
    /// but the key did not authenticate the canonical payload. This
    /// is the "tampered mandate" signal.
    #[error("signature does not verify against trusted key for kid `{0}`")]
    SignatureInvalid(String),

    /// Header carried a `kid` not present in the trust map.
    #[error("kid `{0}` is not in the trusted-key map")]
    UnknownKid(String),

    /// Header carried no `kid`. We require it so verifiers can pick
    /// a single trust anchor unambiguously.
    #[error("protected header missing `kid`")]
    MissingKid,
}

/// Protected header per RFC 7515 §4. Pinned to `alg = "EdDSA"`,
/// `kid` required.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProtectedHeader {
    alg: String,
    kid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    typ: Option<String>,
}

/// Sign `unsigned` with `signing_key`, identifying the key as `kid` in
/// the JWS protected header. Returns a clone of `unsigned` with its
/// `signature` field populated as `<protected_b64u>.<signature_b64u>`.
///
/// Whatever value `unsigned.signature` carried is ignored — the
/// canonical payload always uses an empty signature field.
pub fn sign_mandate(
    unsigned: &IntentMandate,
    signing_key: &SigningKey,
    kid: &str,
) -> Result<IntentMandate, MandateSignError> {
    let payload_bytes = canonicalise_payload(unsigned)?;

    let header = ProtectedHeader {
        alg: "EdDSA".to_string(),
        kid: kid.to_string(),
        typ: Some("JWS".to_string()),
    };
    let header_json = serde_json::to_vec(&header)
        .map_err(|e| MandateSignError::ProtectedSerialise(e.to_string()))?;

    let signing_input = build_signing_input(&header_json, &payload_bytes)?;
    let sig: Ed25519Signature = signing_key.sign(&signing_input.signing_input);

    let detached = format!(
        "{}.{}",
        signing_input.protected_b64u,
        base64url_encode(&sig.to_bytes())
    );

    let mut signed = unsigned.clone();
    signed.signature = detached;
    Ok(signed)
}

/// Trust anchors for verification: a map from `kid` to the verifying
/// key the caller is willing to trust for that key id.
pub type TrustedKeys<'a> = HashMap<&'a str, &'a VerifyingKey>;

/// Verify `mandate.signature` against `trusted`. Returns `Ok(kid)` of
/// the signing key on success.
pub fn verify_mandate(
    mandate: &IntentMandate,
    trusted: &TrustedKeys<'_>,
) -> Result<String, MandateSignError> {
    if mandate.signature.is_empty() {
        return Err(MandateSignError::Unsigned);
    }

    let mut parts = mandate.signature.splitn(2, '.');
    let protected_b64u = parts.next().ok_or(MandateSignError::MalformedDetached)?;
    let sig_b64u = parts.next().ok_or(MandateSignError::MalformedDetached)?;
    if protected_b64u.is_empty() || sig_b64u.is_empty() {
        return Err(MandateSignError::MalformedDetached);
    }

    let header_bytes = base64url_decode(protected_b64u)
        .map_err(|e| MandateSignError::MalformedHeader(e.to_string()))?;
    let header: ProtectedHeader = serde_json::from_slice(&header_bytes)
        .map_err(|e| MandateSignError::MalformedHeader(e.to_string()))?;
    if header.alg != "EdDSA" {
        return Err(MandateSignError::MalformedHeader(format!(
            "unsupported alg `{}`",
            header.alg
        )));
    }
    if header.kid.is_empty() {
        return Err(MandateSignError::MissingKid);
    }

    let vk = trusted
        .get(header.kid.as_str())
        .copied()
        .ok_or_else(|| MandateSignError::UnknownKid(header.kid.clone()))?;

    let payload_bytes = canonicalise_payload(mandate)?;
    let signing_input = build_signing_input(&header_bytes, &payload_bytes)?;

    let sig_bytes =
        base64url_decode(sig_b64u).map_err(|e| MandateSignError::MalformedHeader(e.to_string()))?;
    if sig_bytes.len() != 64 {
        return Err(MandateSignError::SignatureLength(sig_bytes.len()));
    }
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| MandateSignError::SignatureLength(sig_bytes.len()))?;
    let ed_sig = Ed25519Signature::from_bytes(&sig_arr);

    vk.verify(&signing_input.signing_input, &ed_sig)
        .map_err(|_| MandateSignError::SignatureInvalid(header.kid.clone()))?;

    Ok(header.kid)
}

/// Serialise `mandate` with `signature` set to the empty string.
/// This is the canonical signing payload — verifier and signer both
/// re-serialise this way, so any tampering of any other field is
/// detected.
fn canonicalise_payload(mandate: &IntentMandate) -> Result<Vec<u8>, MandateSignError> {
    let mut m = mandate.clone();
    m.signature.clear();
    serde_json::to_vec(&m).map_err(|e| MandateSignError::Serialise(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn unsigned() -> IntentMandate {
        IntentMandate {
            mandate_id: "m-1".into(),
            principal: "agent-a".into(),
            currency: "USD".into(),
            daily_cap: 10_000,
            monthly_cap: 100_000,
            per_transfer_cap: 1_000,
            counterparty_allowlist: {
                let mut s = BTreeSet::new();
                s.insert("counter-a".to_string());
                s
            },
            exp: 1_800_000_000,
            signature: String::new(),
        }
    }

    #[test]
    fn round_trip_signs_and_verifies() {
        let sk = key(1);
        let vk = sk.verifying_key();
        let signed = sign_mandate(&unsigned(), &sk, "k1").unwrap();
        assert!(!signed.signature.is_empty());
        assert!(signed.signature.contains('.'));

        let mut trusted = TrustedKeys::new();
        trusted.insert("k1", &vk);
        let kid = verify_mandate(&signed, &trusted).unwrap();
        assert_eq!(kid, "k1");
    }

    #[test]
    fn unsigned_mandate_rejected() {
        let m = unsigned();
        let trusted = TrustedKeys::new();
        let err = verify_mandate(&m, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::Unsigned));
    }

    #[test]
    fn malformed_detached_form_rejected() {
        let mut m = unsigned();
        m.signature = "no-dot-here".into();
        let trusted = TrustedKeys::new();
        let err = verify_mandate(&m, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::MalformedDetached));
    }

    #[test]
    fn empty_protected_segment_rejected() {
        let mut m = unsigned();
        m.signature = ".sig".into();
        let trusted = TrustedKeys::new();
        let err = verify_mandate(&m, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::MalformedDetached));
    }

    #[test]
    fn empty_signature_segment_rejected() {
        let mut m = unsigned();
        m.signature = "header.".into();
        let trusted = TrustedKeys::new();
        let err = verify_mandate(&m, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::MalformedDetached));
    }

    #[test]
    fn unknown_kid_rejected() {
        let sk = key(2);
        let signed = sign_mandate(&unsigned(), &sk, "kA").unwrap();
        // Trust only kB, not kA.
        let other = key(99).verifying_key();
        let mut trusted = TrustedKeys::new();
        trusted.insert("kB", &other);
        let err = verify_mandate(&signed, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::UnknownKid(ref k) if k == "kA"));
    }

    #[test]
    fn tampered_principal_rejected() {
        let sk = key(3);
        let vk = sk.verifying_key();
        let mut signed = sign_mandate(&unsigned(), &sk, "k1").unwrap();
        signed.principal = "evil-agent".into();

        let mut trusted = TrustedKeys::new();
        trusted.insert("k1", &vk);
        let err = verify_mandate(&signed, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::SignatureInvalid(_)));
    }

    #[test]
    fn tampered_caps_rejected() {
        let sk = key(4);
        let vk = sk.verifying_key();
        let mut signed = sign_mandate(&unsigned(), &sk, "k1").unwrap();
        signed.daily_cap = u64::MAX;

        let mut trusted = TrustedKeys::new();
        trusted.insert("k1", &vk);
        let err = verify_mandate(&signed, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::SignatureInvalid(_)));
    }

    #[test]
    fn wrong_key_under_correct_kid_rejected() {
        let sk_a = key(5);
        let signed = sign_mandate(&unsigned(), &sk_a, "k1").unwrap();
        let sk_b = key(6);
        let vk_b = sk_b.verifying_key();
        let mut trusted = TrustedKeys::new();
        trusted.insert("k1", &vk_b);
        let err = verify_mandate(&signed, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::SignatureInvalid(_)));
    }

    #[test]
    fn alg_other_than_eddsa_rejected() {
        // Build a hand-rolled detached form with alg=HS256.
        let header = serde_json::json!({"alg": "HS256", "kid": "k1"});
        let header_b64u = base64url_encode(header.to_string().as_bytes());
        let mut m = unsigned();
        m.signature = format!("{}.{}", header_b64u, base64url_encode(&[0u8; 64]));
        let vk = key(7).verifying_key();
        let mut trusted = TrustedKeys::new();
        trusted.insert("k1", &vk);
        let err = verify_mandate(&m, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::MalformedHeader(_)));
    }

    #[test]
    fn missing_kid_in_header_rejected() {
        // Header without kid (empty kid is not serialisable via our
        // ProtectedHeader because kid is non-Option, but a hostile
        // peer can craft any bytes — we still reject).
        let header = serde_json::json!({"alg": "EdDSA", "kid": ""});
        let header_b64u = base64url_encode(header.to_string().as_bytes());
        let mut m = unsigned();
        m.signature = format!("{}.{}", header_b64u, base64url_encode(&[0u8; 64]));
        let vk = key(8).verifying_key();
        let mut trusted = TrustedKeys::new();
        trusted.insert("k1", &vk);
        let err = verify_mandate(&m, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::MissingKid));
    }

    #[test]
    fn signature_wrong_length_rejected() {
        let header = serde_json::json!({"alg": "EdDSA", "kid": "k1"});
        let header_b64u = base64url_encode(header.to_string().as_bytes());
        let mut m = unsigned();
        m.signature = format!("{}.{}", header_b64u, base64url_encode(&[0u8; 32]));
        let vk = key(9).verifying_key();
        let mut trusted = TrustedKeys::new();
        trusted.insert("k1", &vk);
        let err = verify_mandate(&m, &trusted).unwrap_err();
        assert!(matches!(err, MandateSignError::SignatureLength(32)));
    }

    #[test]
    fn signature_field_overwritten_on_each_sign() {
        let sk = key(10);
        let mut m = unsigned();
        m.signature = "stale".into();
        let signed = sign_mandate(&m, &sk, "k1").unwrap();
        assert_ne!(signed.signature, "stale");
        // Re-signing replaces, doesn't append.
        let signed2 = sign_mandate(&signed, &sk, "k1").unwrap();
        assert!(signed2.signature.matches('.').count() == 1);
    }
}
