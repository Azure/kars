//! A2A 1.0.0 AgentCard signing and verification — end-to-end.
//!
//! Spec: <https://a2a-protocol.org/v1.0.0/specification#447-agentcardsignature>
//!
//! This module wires the [`signature`](super::signature) JWS primitives
//! and the [`AgentCard`](super::agent_card::AgentCard) data model into
//! a working sign/verify pipeline.
//!
//! ## Canonical payload
//!
//! The JWS payload is the JSON serialisation of the AgentCard with
//! its `signatures` field set to `None` (spec §4.4.7: the signature
//! protects the entire card except the signatures field itself, which
//! by definition cannot be self-referential). We use `serde_json` for
//! serialisation; signers and verifiers MUST use the same serialisation
//! for the signature to be interoperable. Since `serde_json` does not
//! guarantee a canonical form across language ecosystems, the verifier
//! re-serialises the card it received (with signatures stripped) before
//! validating — this is the canonical approach used by the A2A
//! reference implementations and is robust to whitespace differences
//! introduced by intermediaries.
//!
//! ## Multi-signer support
//!
//! Per RFC 7515 §3.2 (general JWS JSON serialisation), a JWS may carry
//! multiple signatures. The A2A spec uses an array (`signatures: [...]`)
//! to express this. [`sign_card`] **appends** to the existing array if
//! present (preserving prior signers); [`verify_card`] requires that
//! **at least one** signature in the array is valid against any of the
//! provided trust-anchor verifying keys identified by `kid`.
//!
//! ## Algorithm pinning
//!
//! Only `alg = "EdDSA"` is accepted, per `signature::build_signing_input`.
//! Unknown algs are rejected at JWS parse time.

use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::agent_card::{AgentCard, AgentCardSignature};
use super::signature::{
    self, SignatureError, base64url_decode, base64url_encode, build_signing_input,
};

/// Errors raised by [`sign_card`] and [`verify_card`].
#[derive(Debug, thiserror::Error)]
pub enum CardSignError {
    /// Underlying JWS primitive failed (alg pinning, base64, header
    /// parse, etc.).
    #[error("jws: {0}")]
    Jws(#[from] SignatureError),

    /// Card serialisation produced invalid JSON.
    #[error("serialise card: {0}")]
    Serialise(String),

    /// Protected header construction emitted invalid JSON.
    #[error("protected header serialise: {0}")]
    ProtectedSerialise(String),

    /// `signatures` field was absent on a card that the caller asked
    /// to verify.
    #[error("card has no signatures")]
    NoSignatures,

    /// No signature in the card matched any provided trust anchor by
    /// `kid`. Either the signing key isn't trusted, or every trusted
    /// signature was tampered with / forged.
    #[error("no trusted signature passed verification")]
    NoTrustedSignatureValid,

    /// Signature bytes did not decode to the expected length for
    /// Ed25519 (64 bytes).
    #[error("signature wrong length: expected 64 bytes, got {0}")]
    SignatureLength(usize),
}

/// Protected header per RFC 7515 §4. Pinned to `alg = "EdDSA"`,
/// optional `kid` (key id) so verifiers can pick the right trust
/// anchor in a multi-signer card. `typ` defaults to `JWS` (matches
/// what the reference implementations emit).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProtectedHeader {
    alg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    typ: Option<String>,
}

/// Sign `card` with `signing_key`, identifying the key as `kid` in the
/// JWS protected header.
///
/// Returns the card with one additional signature appended to
/// `card.signatures`. The original signatures (if any) are preserved.
///
/// The payload is the card's JSON serialisation with the `signatures`
/// field replaced by `None` (so the signature does not protect itself).
pub fn sign_card(
    mut card: AgentCard,
    signing_key: &SigningKey,
    kid: &str,
) -> Result<AgentCard, CardSignError> {
    let payload_bytes = canonicalise_payload(&card)?;

    let header = ProtectedHeader {
        alg: "EdDSA".to_string(),
        kid: Some(kid.to_string()),
        typ: Some("JWS".to_string()),
    };
    let header_json = serde_json::to_vec(&header)
        .map_err(|e| CardSignError::ProtectedSerialise(e.to_string()))?;

    let signing_input = build_signing_input(&header_json, &payload_bytes)?;
    let sig: Ed25519Signature = signing_key.sign(&signing_input.signing_input);

    let card_sig = AgentCardSignature {
        protected: signing_input.protected_b64u,
        signature: base64url_encode(&sig.to_bytes()),
        header: None,
    };

    match card.signatures.as_mut() {
        Some(existing) => existing.push(card_sig),
        None => card.signatures = Some(vec![card_sig]),
    }
    Ok(card)
}

/// Trust anchors for verification: a map from `kid` to the verifying
/// key the caller is willing to trust for that key id.
pub type TrustedKeys<'a> = HashMap<&'a str, &'a VerifyingKey>;

/// Verify `card` against `trusted` — the `kid → VerifyingKey` map of
/// trust anchors the caller is willing to accept.
///
/// Returns `Ok(kid)` (the `kid` of the first valid signature) on
/// success, or [`CardSignError::NoTrustedSignatureValid`] if no
/// signature in the card was both (a) issued under a `kid` present in
/// `trusted`, and (b) verifies cleanly against the canonical payload.
///
/// Signatures with unknown `kid` are skipped (not failures); signatures
/// with a known `kid` whose verification fails contribute to the
/// final "no trusted signature valid" outcome but are not individually
/// raised as errors. This matches the multi-signer semantics in RFC
/// 7515 and the A2A reference implementations.
pub fn verify_card(card: &AgentCard, trusted: &TrustedKeys<'_>) -> Result<String, CardSignError> {
    let signatures = card
        .signatures
        .as_ref()
        .ok_or(CardSignError::NoSignatures)?;

    // Strip signatures and re-serialise as the canonical payload.
    let payload_bytes = canonicalise_payload(card)?;

    for s in signatures {
        let header_bytes = match base64url_decode(&s.protected) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let header: ProtectedHeader = match serde_json::from_slice(&header_bytes) {
            Ok(h) => h,
            Err(_) => continue,
        };
        if header.alg != "EdDSA" {
            continue;
        }
        let kid = match header.kid.as_deref() {
            Some(k) => k,
            None => continue,
        };
        let vk = match trusted.get(kid) {
            Some(vk) => *vk,
            None => continue,
        };

        let signing_input = match build_signing_input(&header_bytes, &payload_bytes) {
            Ok(si) => si,
            Err(_) => continue,
        };
        let sig_bytes = match base64url_decode(&s.signature) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if sig_bytes.len() != 64 {
            continue;
        }
        let sig_arr: [u8; 64] = match sig_bytes.as_slice().try_into() {
            Ok(a) => a,
            Err(_) => continue,
        };
        let ed_sig = Ed25519Signature::from_bytes(&sig_arr);

        if vk.verify(&signing_input.signing_input, &ed_sig).is_ok() {
            return Ok(kid.to_string());
        }
    }
    Err(CardSignError::NoTrustedSignatureValid)
}

/// Serialise `card` with `signatures` field stripped. This is the
/// canonical JWS payload for both signing and verification — see the
/// module-level "Canonical payload" doc.
fn canonicalise_payload(card: &AgentCard) -> Result<Vec<u8>, CardSignError> {
    let mut card_for_payload = card.clone();
    card_for_payload.signatures = None;
    serde_json::to_vec(&card_for_payload).map_err(|e| CardSignError::Serialise(e.to_string()))
}

// Re-export so call sites can `use a2a::card_signing::*` cleanly.
pub use signature::SignatureInput;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_card::{
        AgentCapabilities, AgentInterface, KnownProtocolBinding, ProtocolBinding,
    };

    fn fixed_signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn minimal_card() -> AgentCard {
        AgentCard {
            name: "test-agent".into(),
            description: "test".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://example.com/a2a".into(),
                protocol_binding: ProtocolBinding::Known(KnownProtocolBinding::JsonRpc),
                tenant: None,
                protocol_version: "1.0".into(),
            }],
            provider: None,
            version: "0.1.0".into(),
            documentation_url: None,
            capabilities: AgentCapabilities::default(),
            security_schemes: None,
            security_requirements: None,
            default_input_modes: vec!["text/plain".into()],
            default_output_modes: vec!["text/plain".into()],
            skills: vec![],
            signatures: None,
            icon_url: None,
        }
    }

    #[test]
    fn sign_then_verify_round_trip() {
        let key = fixed_signing_key(7);
        let signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();

        // Card now has exactly one signature.
        let sigs = signed.signatures.as_ref().unwrap();
        assert_eq!(sigs.len(), 1);

        // Verification with matching trust anchor succeeds.
        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &vk);
        let kid = verify_card(&signed, &trusted).unwrap();
        assert_eq!(kid, "agent-key-1");
    }

    #[test]
    fn verify_fails_when_payload_tampered_after_sign() {
        let key = fixed_signing_key(7);
        let mut signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();

        // Tamper a signed field after signing.
        signed.description = "tampered".into();

        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn verify_fails_when_signature_bytes_tampered() {
        let key = fixed_signing_key(7);
        let mut signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();

        // Flip a bit in the signature.
        let sigs = signed.signatures.as_mut().unwrap();
        let mut bytes = base64url_decode(&sigs[0].signature).unwrap();
        bytes[0] ^= 0x01;
        sigs[0].signature = base64url_encode(&bytes);

        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn verify_fails_when_kid_unknown() {
        let key = fixed_signing_key(7);
        let signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();

        // Trust map references a different kid — unknown kids are
        // skipped, leaving zero candidates and the catch-all error.
        let other = fixed_signing_key(8);
        let other_vk = other.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("some-other-kid", &other_vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn verify_fails_when_kid_known_but_key_wrong() {
        let signer = fixed_signing_key(7);
        let signed = sign_card(minimal_card(), &signer, "agent-key-1").unwrap();

        // Trust anchor for `agent-key-1` is a *different* public key.
        let attacker = fixed_signing_key(99);
        let attacker_vk = attacker.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &attacker_vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn verify_fails_on_unsigned_card() {
        let card = minimal_card();
        let trusted: TrustedKeys = HashMap::new();
        let err = verify_card(&card, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoSignatures));
    }

    #[test]
    fn multi_signer_card_passes_when_any_trusted_sig_valid() {
        // Sign with key A, then key B. Trust only key B.
        let key_a = fixed_signing_key(1);
        let key_b = fixed_signing_key(2);
        let signed_once = sign_card(minimal_card(), &key_a, "kid-a").unwrap();
        let signed_twice = sign_card(signed_once, &key_b, "kid-b").unwrap();

        assert_eq!(signed_twice.signatures.as_ref().unwrap().len(), 2);

        let vk_b = key_b.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("kid-b", &vk_b);
        let kid = verify_card(&signed_twice, &trusted).unwrap();
        assert_eq!(kid, "kid-b");
    }

    #[test]
    fn multi_signer_card_fails_when_no_trusted_kid_matches() {
        let key_a = fixed_signing_key(1);
        let key_b = fixed_signing_key(2);
        let signed_once = sign_card(minimal_card(), &key_a, "kid-a").unwrap();
        let signed_twice = sign_card(signed_once, &key_b, "kid-b").unwrap();

        let other = fixed_signing_key(3);
        let other_vk = other.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("kid-c", &other_vk);
        let err = verify_card(&signed_twice, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn signatures_field_is_excluded_from_signed_payload() {
        // Two cards differing only in `signatures` produce the same
        // payload bytes — proves the canonicalisation strips signatures.
        let mut a = minimal_card();
        a.signatures = Some(vec![AgentCardSignature {
            protected: "x".into(),
            signature: "y".into(),
            header: None,
        }]);

        let b = minimal_card(); // signatures = None
        let pa = canonicalise_payload(&a).unwrap();
        let pb = canonicalise_payload(&b).unwrap();
        assert_eq!(pa, pb);
    }

    #[test]
    fn protected_header_uses_eddsa_alg() {
        let key = fixed_signing_key(7);
        let signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();
        let sig = &signed.signatures.as_ref().unwrap()[0];
        let header_bytes = base64url_decode(&sig.protected).unwrap();
        let header: ProtectedHeader = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header.alg, "EdDSA");
        assert_eq!(header.kid.as_deref(), Some("agent-key-1"));
    }

    #[test]
    fn signature_with_alg_none_is_rejected() {
        let key = fixed_signing_key(7);
        let card = minimal_card();
        let mut signed = sign_card(card, &key, "agent-key-1").unwrap();

        // Replace the protected header with alg=none — RFC 8725 attack.
        let bad_header = serde_json::to_vec(&ProtectedHeader {
            alg: "none".into(),
            kid: Some("agent-key-1".into()),
            typ: None,
        })
        .unwrap();
        signed.signatures.as_mut().unwrap()[0].protected = base64url_encode(&bad_header);

        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn signature_with_garbage_base64_in_signature_is_rejected() {
        let key = fixed_signing_key(7);
        let mut signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();
        signed.signatures.as_mut().unwrap()[0].signature = "!!!not-base64!!!".into();

        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn signature_with_truncated_signature_bytes_is_rejected() {
        let key = fixed_signing_key(7);
        let mut signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();
        // 32 bytes is not 64 — wrong length for Ed25519.
        signed.signatures.as_mut().unwrap()[0].signature = base64url_encode(&[0u8; 32]);

        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn empty_kid_signature_is_rejected() {
        let key = fixed_signing_key(7);
        let mut signed = sign_card(minimal_card(), &key, "agent-key-1").unwrap();

        // Replace protected header with one missing kid entirely.
        let bad_header = serde_json::to_vec(&ProtectedHeader {
            alg: "EdDSA".into(),
            kid: None,
            typ: None,
        })
        .unwrap();
        signed.signatures.as_mut().unwrap()[0].protected = base64url_encode(&bad_header);

        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("agent-key-1", &vk);
        let err = verify_card(&signed, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn camel_case_wire_format_for_optional_fields() {
        // Build a card with all optional fields populated and confirm
        // the wire JSON uses camelCase per A2A 1.0.0 spec §4.4.
        let mut card = minimal_card();
        card.documentation_url = Some("https://example.com/docs".into());
        card.icon_url = Some("https://example.com/icon.png".into());
        card.default_input_modes = vec!["text/plain".into()];
        card.default_output_modes = vec!["application/json".into()];
        card.supported_interfaces[0].protocol_version = "1.0".into();

        let s = serde_json::to_string(&card).unwrap();
        assert!(s.contains("documentationUrl"), "missing camelCase: {s}");
        assert!(s.contains("iconUrl"), "missing camelCase: {s}");
        assert!(s.contains("defaultInputModes"), "missing camelCase: {s}");
        assert!(s.contains("defaultOutputModes"), "missing camelCase: {s}");
        assert!(s.contains("protocolBinding"), "missing camelCase: {s}");
        assert!(s.contains("protocolVersion"), "missing camelCase: {s}");
        assert!(s.contains("supportedInterfaces"), "missing camelCase: {s}");
        // Snake case must NOT appear.
        assert!(!s.contains("documentation_url"));
        assert!(!s.contains("icon_url"));
        assert!(!s.contains("default_input_modes"));
        assert!(!s.contains("protocol_binding"));
    }
}
