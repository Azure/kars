// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One-shot handoff token — auth for the second half of the live
//! migration handshake.
//!
//! The token is issued by the drain-side router when the agent asks to
//! hand off (`POST /agt/handoff/init`). The restore-side router — on
//! the same or a different kars deployment — validates the token
//! against the known hash before accepting the encrypted state blob.
//!
//! Properties enforced by [`HandoffTokenStore`]:
//!  * **Single active token at a time.** Creating a new token replaces
//!    the old one, preventing concurrent handoff races.
//!  * **TTL bound** clamped to [`MAX_TOKEN_TTL_SECS`]. The caller
//!    cannot request an indefinitely-live token.
//!  * **Constant-time comparison** via [`super::constant_time_eq`].
//!  * **Audit-hash-only logging.** The raw token never reaches logs;
//!    [`active_token_hash`] returns the SHA-256 hex which is what
//!    downstream middleware stamps into audit entries.
//!
//! Extracted from `handoff.rs` as the third step of the §4.1 hotspot
//! split. Shares `hex_sha256` and `constant_time_eq` with the parent
//! module; all crypto primitives flow through `sha2`/`base64`/`rand`
//! which are already on the no-custom-crypto allowlist for
//! `handoff/mod.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rand::Rng;
use tokio::sync::RwLock;

use super::{constant_time_eq, hex_sha256};

/// Token length in random bytes (base64-encoded on the wire).
const HANDOFF_TOKEN_BYTES: usize = 32;

/// Default TTL for handoff tokens (seconds). Re-exported from the
/// parent module for back-compat with `crate::handoff::DEFAULT_TOKEN_TTL_SECS`.
pub const DEFAULT_TOKEN_TTL_SECS: u64 = 300;

/// Upper bound the caller may request; larger values are silently
/// clamped to this ceiling.
const MAX_TOKEN_TTL_SECS: u64 = 600;

/// Single-slot, auto-expiring handoff-token store.
///
/// Security model (§9.9.2):
/// - Only ONE active token at a time (prevents concurrent handoff races)
/// - Tokens auto-expire after TTL
/// - Token is never persisted to disk or environment
/// - Token hash (not value) is logged for audit
#[derive(Clone)]
pub struct HandoffTokenStore {
    inner: Arc<RwLock<Option<ActiveToken>>>,
}

struct ActiveToken {
    /// The raw token value (32 random bytes, base64-encoded for comparison).
    token_b64: String,
    /// SHA-256 hash of the token for audit logging (hex).
    token_hash: String,
    /// When the token was created.
    created_at: Instant,
    /// Time-to-live.
    ttl: Duration,
}

impl Default for HandoffTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl HandoffTokenStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a new handoff token. Replaces any existing token.
    ///
    /// Returns (token_base64, token_hash_hex) — caller sends token to
    /// the client, stores hash for audit.
    pub async fn create_token(&self, ttl_secs: u64) -> (String, String) {
        let ttl_secs = ttl_secs.min(MAX_TOKEN_TTL_SECS);

        // Generate random bytes BEFORE any await (ThreadRng is !Send).
        let token_b64 = {
            let mut rng = rand::rng();
            let mut token_bytes = [0u8; HANDOFF_TOKEN_BYTES];
            rng.fill(&mut token_bytes);
            BASE64.encode(token_bytes)
        };

        let token_hash = hex_sha256(token_b64.as_bytes());

        let active = ActiveToken {
            token_b64: token_b64.clone(),
            token_hash: token_hash.clone(),
            created_at: Instant::now(),
            ttl: Duration::from_secs(ttl_secs),
        };

        *self.inner.write().await = Some(active);
        (token_b64, token_hash)
    }

    /// Validate a handoff token. Returns Ok(token_hash) on success.
    ///
    /// Tokens are validated against the store but not consumed — reuse
    /// within a session is allowed (e.g. for snapshot/restore retries).
    pub async fn validate(&self, provided: &str) -> Result<String, HandoffTokenError> {
        let mut guard = self.inner.write().await;

        let active = guard.as_mut().ok_or(HandoffTokenError::NoActiveToken)?;

        if active.created_at.elapsed() > active.ttl {
            *guard = None;
            return Err(HandoffTokenError::Expired);
        }

        if !constant_time_eq(provided.as_bytes(), active.token_b64.as_bytes()) {
            return Err(HandoffTokenError::Invalid);
        }

        let hash = active.token_hash.clone();
        Ok(hash)
    }

    /// Revoke the current token (on abort or decommission).
    pub async fn revoke(&self) {
        *self.inner.write().await = None;
    }

    /// Check if there's an active (non-expired) token.
    pub async fn is_active(&self) -> bool {
        let guard = self.inner.read().await;
        match guard.as_ref() {
            Some(t) => t.created_at.elapsed() <= t.ttl,
            None => false,
        }
    }

    /// Get the hash of the active token (for audit logging).
    pub async fn active_token_hash(&self) -> Option<String> {
        let guard = self.inner.read().await;
        guard
            .as_ref()
            .filter(|t| t.created_at.elapsed() <= t.ttl)
            .map(|t| t.token_hash.clone())
    }
}

#[derive(Debug)]
pub enum HandoffTokenError {
    NoActiveToken,
    Expired,
    Invalid,
}

impl std::fmt::Display for HandoffTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoActiveToken => write!(f, "No active handoff token"),
            Self::Expired => write!(f, "Handoff token expired"),
            Self::Invalid => write!(f, "Invalid handoff token"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Most functional coverage (create/validate/expire/revoke/replace/max-ttl)
    // lives alongside the broader handoff-mod tests in `handoff/mod.rs`.
    // This file only hosts the small checks specific to the extracted surface.

    #[tokio::test]
    async fn test_active_token_hash_tracks_revoke() {
        let store = HandoffTokenStore::new();
        let (_token, hash) = store.create_token(60).await;
        assert_eq!(
            store.active_token_hash().await.as_deref(),
            Some(hash.as_str())
        );

        store.revoke().await;
        assert!(store.active_token_hash().await.is_none());
    }

    #[tokio::test]
    async fn test_active_token_hash_expires() {
        let store = HandoffTokenStore::new();
        let (_token, _hash) = store.create_token(0).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        // `is_active` must agree with `active_token_hash` on expiry.
        assert!(!store.is_active().await);
        assert!(store.active_token_hash().await.is_none());
    }

    #[tokio::test]
    async fn test_create_token_is_high_entropy() {
        // Two consecutive creates must produce distinct tokens with
        // non-trivial base64 length (>= 40 chars for 32 random bytes).
        let store = HandoffTokenStore::new();
        let (token_a, _) = store.create_token(60).await;
        let (token_b, _) = store.create_token(60).await;
        assert_ne!(token_a, token_b);
        assert!(token_a.len() >= 40);
        assert!(token_b.len() >= 40);
    }
}
