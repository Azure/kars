// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Replay-protection cache for inbound A2A traffic.
//!
//! `[GAP-V1]` — this module currently provides only the
//! [`ReplayCache`]. Wrapping [`azureclaw_a2a_core::verify_inbound_card`]
//! as an axum layer that runs inside this binary is a v1.1 task;
//! today the verified-caller subject is consumed from the
//! `X-A2A-Agent-Subject` header populated by the upstream Gateway
//! API mTLS handshake (see `docs/architecture/a2a-gateway.md`).
//! [`VerifyError`] enumerates the failure surface that wiring will
//! emit; nothing in this module raises `MissingSignature`,
//! `Invalid`, or `UnknownIssuer` yet.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("missing signature header")]
    MissingSignature,
    #[error("invalid signature: {0}")]
    Invalid(String),
    #[error("issuer not in allow-list: {0}")]
    UnknownIssuer(String),
    #[error("replayed nonce: {0}")]
    Replay(String),
}

/// In-memory replay cache: nonce → expiry. Cap-bounded; evicts
/// expired entries on every `check_and_insert` call.
pub struct ReplayCache {
    inner: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
    cap: usize,
}

impl ReplayCache {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
            cap,
        }
    }

    /// Returns `Err(Replay)` if `nonce` has been seen within `ttl`.
    /// Otherwise records it and returns `Ok(())`.
    pub fn check_and_insert(&self, nonce: &str) -> Result<(), VerifyError> {
        let now = Instant::now();
        let mut g = self.inner.lock().expect("replay cache poisoned");
        g.retain(|_, exp| *exp > now);

        if g.contains_key(nonce) {
            return Err(VerifyError::Replay(nonce.to_string()));
        }
        if g.len() >= self.cap {
            // Hard cap — evict the oldest. This bounds memory at the
            // cost of allowing a *very old* replay through. With the
            // default TTL (5 min) and cap (100k) the eviction is only
            // possible at sustained > 333 rps from distinct subjects.
            if let Some((k, _)) = g
                .iter()
                .min_by_key(|(_, e)| **e)
                .map(|(k, e)| (k.clone(), *e))
            {
                g.remove(&k);
            }
        }
        g.insert(nonce.to_string(), now + self.ttl);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_nonce_accepted() {
        let c = ReplayCache::new(Duration::from_secs(60), 16);
        c.check_and_insert("nonce-a").unwrap();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn duplicate_nonce_rejected() {
        let c = ReplayCache::new(Duration::from_secs(60), 16);
        c.check_and_insert("dup").unwrap();
        let err = c.check_and_insert("dup").unwrap_err();
        assert!(matches!(err, VerifyError::Replay(_)));
    }

    #[test]
    fn distinct_nonces_coexist() {
        let c = ReplayCache::new(Duration::from_secs(60), 16);
        c.check_and_insert("a").unwrap();
        c.check_and_insert("b").unwrap();
        c.check_and_insert("c").unwrap();
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn expired_entry_swept_on_next_check() {
        let c = ReplayCache::new(Duration::from_millis(20), 16);
        c.check_and_insert("ephemeral").unwrap();
        std::thread::sleep(Duration::from_millis(40));
        // After expiry, the same nonce becomes acceptable again
        // (the cache is for replay protection within the TTL window —
        // outside it, the JWS `exp` claim is the authority).
        c.check_and_insert("ephemeral").unwrap();
    }

    #[test]
    fn cap_eviction_bounds_memory() {
        let c = ReplayCache::new(Duration::from_secs(600), 4);
        for i in 0..10 {
            let _ = c.check_and_insert(&format!("n-{i}"));
        }
        assert!(c.len() <= 4);
    }
}
