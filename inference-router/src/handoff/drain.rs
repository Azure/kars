//! Router drain-state — "stop accepting new work, complete in-flight".
//!
//! Extracted from `handoff.rs` as the first step of the Phase 1 split
//! (internal Phase 1 plan §4.2). The drain machinery is self-contained:
//! no crypto, no auth, no interaction with [`super::HandoffState`]. It guards
//! the transition from "active" to "quiescent" during a live migration.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Drain state for the router — stops accepting new work, completes in-flight.
#[derive(Clone)]
pub struct DrainState {
    inner: Arc<RwLock<DrainInner>>,
}

struct DrainInner {
    draining: bool,
    drain_started: Option<Instant>,
}

impl Default for DrainState {
    fn default() -> Self {
        Self::new()
    }
}

impl DrainState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(DrainInner {
                draining: false,
                drain_started: None,
            })),
        }
    }

    pub async fn start_drain(&self) {
        let mut inner = self.inner.write().await;
        inner.draining = true;
        inner.drain_started = Some(Instant::now());
    }

    pub async fn stop_drain(&self) {
        let mut inner = self.inner.write().await;
        inner.draining = false;
        inner.drain_started = None;
    }

    pub async fn is_draining(&self) -> bool {
        self.inner.read().await.draining
    }

    pub async fn drain_duration(&self) -> Option<Duration> {
        self.inner.read().await.drain_started.map(|s| s.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_transitions() {
        let drain = DrainState::new();
        assert!(!drain.is_draining().await);
        assert!(drain.drain_duration().await.is_none());

        drain.start_drain().await;
        assert!(drain.is_draining().await);
        assert!(drain.drain_duration().await.is_some());

        drain.stop_drain().await;
        assert!(!drain.is_draining().await);
        assert!(drain.drain_duration().await.is_none());
    }

    #[tokio::test]
    async fn drain_default_is_not_draining() {
        let drain = DrainState::default();
        assert!(!drain.is_draining().await);
    }

    #[tokio::test]
    async fn drain_idempotent_start() {
        let drain = DrainState::new();
        drain.start_drain().await;
        let first = drain.drain_duration().await;
        drain.start_drain().await;
        let second = drain.drain_duration().await;
        // Second start_drain resets the timer; both measurements exist.
        assert!(first.is_some());
        assert!(second.is_some());
    }
}
