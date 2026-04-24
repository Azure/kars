//! In-tree `SigningProvider` implementation on [`Governance`].
//!
//! Mirrors the pattern used by `policy_impl.rs` and `audit_impl.rs`:
//! no wrapper type, `impl SigningProvider for Governance` directly,
//! `Arc<Governance>` coerces to `Arc<dyn SigningProvider>`.
//!
//! The only key material the router holds is the agent's own Ed25519
//! keypair, owned by `Governance.identity` (an [`agentmesh::identity::AgentIdentity`]).
//! So `key_ref` must be `"agent:default"` or the agent's DID. Any other
//! `KeyRef` returns [`SigningError::UnknownKey`]. Multi-key stores
//! (signed-prekey rotation, delegate-child keypairs) will land in a
//! follow-up branch behind the same trait.
//!
//! **No hand-rolled crypto.** All bytes go through `AgentIdentity::sign`
//! / `AgentIdentity::verify`, which are thin wrappers over `ed25519-dalek`.
//! `ci/no-custom-crypto.sh` enforces that no `ed25519`/`sha2`/`aes` imports
//! appear in this file.

use async_trait::async_trait;

use super::signing::{KeyRef, Signature, SigningError, SigningProvider};
use crate::governance::Governance;

/// The one `KeyRef` every router understands out of the box: the agent's
/// own Ed25519 keypair. DID-form is also accepted so callers that already
/// have `governance.identity.did` on hand don't need a separate constant.
pub(crate) const DEFAULT_KEY_REF: &str = "agent:default";

fn accepts(gov: &Governance, key_ref: &KeyRef) -> bool {
    key_ref.0 == DEFAULT_KEY_REF || key_ref.0 == gov.identity.did
}

#[async_trait]
impl SigningProvider for Governance {
    async fn sign(&self, key_ref: &KeyRef, payload: &[u8]) -> Result<Signature, SigningError> {
        if !accepts(self, key_ref) {
            return Err(SigningError::UnknownKey(key_ref.clone()));
        }
        Ok(Signature(self.identity.sign(payload)))
    }

    async fn verify(
        &self,
        key_ref: &KeyRef,
        payload: &[u8],
        sig: &Signature,
    ) -> Result<bool, SigningError> {
        if !accepts(self, key_ref) {
            return Err(SigningError::UnknownKey(key_ref.clone()));
        }
        Ok(self.identity.verify(payload, &sig.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::SigningProvider;
    use std::sync::Arc;

    fn gov() -> Arc<Governance> {
        Arc::new(Governance::new("signing-impl-test"))
    }

    #[tokio::test]
    async fn sign_and_verify_round_trip_default_ref() {
        let g = gov();
        let kref = KeyRef(DEFAULT_KEY_REF.to_string());
        let payload = b"succession:alice:bob:1700000000";

        let sig = g.sign(&kref, payload).await.expect("sign");
        assert_eq!(sig.0.len(), 64, "Ed25519 signature is 64 bytes");

        let ok = g.verify(&kref, payload, &sig).await.expect("verify");
        assert!(ok);
    }

    #[tokio::test]
    async fn did_is_also_accepted_as_key_ref() {
        let g = gov();
        let kref = KeyRef(g.identity.did.clone());
        let sig = g.sign(&kref, b"payload").await.expect("sign");
        assert!(g.verify(&kref, b"payload", &sig).await.expect("verify"));
    }

    #[tokio::test]
    async fn unknown_key_ref_fails_both_sign_and_verify() {
        let g = gov();
        let bad = KeyRef("agent:alien".to_string());
        let err = g.sign(&bad, b"x").await.unwrap_err();
        assert!(matches!(err, SigningError::UnknownKey(_)));
        let err = g
            .verify(&bad, b"x", &Signature(vec![0u8; 64]))
            .await
            .unwrap_err();
        assert!(matches!(err, SigningError::UnknownKey(_)));
    }

    #[tokio::test]
    async fn verify_rejects_tampered_payload() {
        let g = gov();
        let kref = KeyRef(DEFAULT_KEY_REF.to_string());
        let sig = g.sign(&kref, b"original").await.expect("sign");
        let ok = g.verify(&kref, b"tampered", &sig).await.expect("verify");
        assert!(!ok);
    }

    #[tokio::test]
    async fn verify_rejects_wrong_length_signature() {
        let g = gov();
        let kref = KeyRef(DEFAULT_KEY_REF.to_string());
        let ok = g
            .verify(&kref, b"payload", &Signature(vec![0u8; 32]))
            .await
            .expect("verify");
        assert!(
            !ok,
            "agentmesh::AgentIdentity::verify returns false on wrong-length sigs"
        );
    }

    #[tokio::test]
    async fn verify_rejects_signature_from_different_identity() {
        let g1 = gov();
        let g2 = Arc::new(Governance::new("other-agent"));
        let kref = KeyRef(DEFAULT_KEY_REF.to_string());
        let sig_from_g2 = g2.sign(&kref, b"payload").await.expect("sign");
        let ok = g1
            .verify(&kref, b"payload", &sig_from_g2)
            .await
            .expect("verify");
        assert!(!ok);
    }

    #[tokio::test]
    async fn arc_dyn_signing_provider_coercion_works() {
        let g: Arc<Governance> = gov();
        let as_trait: Arc<dyn SigningProvider> = Arc::clone(&g) as Arc<dyn SigningProvider>;
        let kref = KeyRef(DEFAULT_KEY_REF.to_string());
        let sig = as_trait.sign(&kref, b"hello").await.expect("sign");
        assert!(
            as_trait
                .verify(&kref, b"hello", &sig)
                .await
                .expect("verify")
        );
    }
}
