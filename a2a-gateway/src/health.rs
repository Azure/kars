// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `/healthz` and `/readyz` endpoints.
//!
//! - `/healthz`: liveness — process is alive and event loop is
//!   serving. Returns 200 unconditionally.
//! - `/readyz`: readiness — confirms the upstream router mTLS port
//!   is reachable. Returns 503 until the gateway has successfully
//!   completed at least one TLS handshake to the upstream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Default)]
pub struct ReadyState {
    inner: Arc<AtomicBool>,
}

impl ReadyState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn mark_ready(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }
    pub fn mark_not_ready(&self) {
        self.inner.store(false, Ordering::SeqCst);
    }
    pub fn is_ready(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_state_starts_not_ready() {
        let r = ReadyState::new();
        assert!(!r.is_ready());
    }

    #[test]
    fn mark_ready_flips_state() {
        let r = ReadyState::new();
        r.mark_ready();
        assert!(r.is_ready());
        r.mark_not_ready();
        assert!(!r.is_ready());
    }

    #[test]
    fn ready_state_clones_share_storage() {
        let a = ReadyState::new();
        let b = a.clone();
        a.mark_ready();
        assert!(b.is_ready());
    }
}
