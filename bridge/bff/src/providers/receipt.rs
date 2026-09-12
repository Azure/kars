// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standard Ed25519 verification for existing receipt and checkpoint bytes.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

pub(crate) fn verify_ed25519(public_key: &[u8; 32], message: &[u8], signature: &str) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    let Some(bytes) = STANDARD
        .decode(signature.as_bytes())
        .ok()
        .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
    else {
        return false;
    };
    key.verify(message, &Signature::from_bytes(&bytes)).is_ok()
}

#[cfg(test)]
pub(crate) struct ReceiptTestSigner(ed25519_dalek::SigningKey);

#[cfg(test)]
impl ReceiptTestSigner {
    pub(crate) fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(bytes))
    }

    pub(crate) fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.0.sign(message).to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc8032_known_answer_and_malformed_signatures_keep_exact_verification_semantics() {
        let key: [u8; 32] =
            hex::decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
                .unwrap()
                .try_into()
                .unwrap();
        let signature = hex::decode(concat!(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ))
        .unwrap();
        let encoded = STANDARD.encode(&signature);
        assert!(verify_ed25519(&key, b"", &encoded));
        assert!(!verify_ed25519(&key, b"changed", &encoded));
        assert!(!verify_ed25519(&[0; 32], b"", &encoded));
        for invalid in [
            "".into(),
            "not base64".into(),
            format!("{encoded}\n"),
            STANDARD.encode([0; 63]),
            STANDARD.encode([0; 65]),
        ] {
            assert!(!verify_ed25519(&key, b"", &invalid));
        }
        let mut changed = signature;
        changed[0] ^= 1;
        assert!(!verify_ed25519(&key, b"", &STANDARD.encode(changed)));
    }
}
