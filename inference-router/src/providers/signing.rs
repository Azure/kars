// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `SigningProvider` contract.
//!
//! Responsibility: `sign(key_ref, payload) -> Signature`;
//! `verify(key_ref, payload, sig) -> bool`. **Key material never crosses
//! the boundary.** Callers pass an opaque `KeyRef`; the provider resolves
//! it to an internal handle.
//!
//! Implementations (Phase 1):
//! - `InTreeSigningProvider` — today's Ed25519 path using agent-local keys.
//! - `AgtSigningProvider` — shipped AGT Rust SDK.
//! - `NullSigningProvider` — dev-only; always returns a deterministic
//!   non-verifying signature labeled `ci:stub-ok`; admission rejects in
//!   prod.
//!
//! **No hand-rolled crypto.** `ci/no-custom-crypto.sh` enforces this file,
//! `providers/mesh.rs`, `vendor/`, and a short allowlist are the only places
//! where crypto primitives may be imported. See internal Phase 1 plan
//! §0.2 #8.
//!
//! See internal Phase 1 plan §1.2.

/// Opaque reference to a signing key. The interpretation is
/// provider-specific. Examples:
/// - In-tree: `"agent:default"` resolves to the agent-local keypair.
/// - AGT: `"agt://tenant/agent#ed25519/<fingerprint>"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyRef(pub String);

/// Raw signature bytes. Format is provider-specific; opaque to callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature(pub Vec<u8>);

/// Full SHA-256 for wire authorization and identity bindings.
pub fn sha256_hex(payload: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(payload))
}

#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error("unknown key ref: {0:?}")]
    UnknownKey(KeyRef),
    #[error("signing backend unreachable: {0}")]
    Unreachable(String),
    #[error("internal provider error: {0}")]
    Internal(String),
}

#[async_trait::async_trait]
pub trait SigningProvider: Send + Sync {
    /// Produce a signature over `payload` using the key identified by
    /// `key_ref`. The caller is responsible for any canonicalisation of
    /// `payload` before invoking.
    async fn sign(&self, key_ref: &KeyRef, payload: &[u8]) -> Result<Signature, SigningError>;

    /// Verify `sig` against `payload` for the key identified by `key_ref`.
    /// Returns `Ok(true)` on a valid signature, `Ok(false)` on a valid
    /// format but wrong signature, and `Err(_)` only when the provider
    /// itself failed (key unknown, backend down).
    async fn verify(
        &self,
        key_ref: &KeyRef,
        payload: &[u8],
        sig: &Signature,
    ) -> Result<bool, SigningError>;
}

#[cfg(test)]
mod tests {
    #[test]
    fn composed_credential_and_budget_authority_uses_full_sha256_with_leading_zeroes() {
        assert_eq!(
            super::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            super::sha256_hex(&[3]),
            "084fed08b978af4d7d196a7446a86b58009e636b611db16211b65a9aadff29c5"
        );
    }
}
