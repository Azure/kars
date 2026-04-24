//! `SigningProvider` contract.
//!
//! Responsibility: `sign(key_ref, payload) -> Signature`;
//! `verify(key_ref, payload, sig) -> bool`. **Key material never crosses
//! the boundary.** Callers pass an opaque `KeyRef`; the provider resolves
//! it to an internal handle.
//!
//! Implementations (Phase 1):
//! - `VendoredSigningProvider` — today's Ed25519 path using the keys
//!   emitted by `vendor/agentmesh-sdk`.
//! - `AgtSigningProvider` — shipped AGT Rust SDK.
//! - `NullSigningProvider` — dev-only; always returns a deterministic
//!   non-verifying signature labeled `ci:stub-ok`; admission rejects in
//!   prod.
//!
//! **No hand-rolled crypto.** `ci/no-custom-crypto.sh` enforces this file,
//! `providers/mesh.rs`, `vendor/`, and a short allowlist are the only places
//! where crypto primitives may be imported. See `docs/implementation-plan.md`
//! §0.2 #8.
//!
//! See `docs/implementation-plan.md` §1.2.

/// Opaque reference to a signing key. The interpretation is
/// provider-specific. Examples:
/// - Vendored: `"agent:default"` resolves to the SDK-generated keypair.
/// - AGT: `"agt://tenant/agent#ed25519/<fingerprint>"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyRef(pub String);

/// Raw signature bytes. Format is provider-specific; opaque to callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature(pub Vec<u8>);

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
