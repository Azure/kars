// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pending-handoff store — the two-step "request → confirm" gate that
//! defends against LLM self-initiated handoff (§9.9.9).
//!
//! Protections enforced here:
//!  1. An agent that asks to hand off cannot also confirm the same
//!     request within [`CONFIRMATION_MIN_DELAY_SECS`]. The window is
//!     long enough that any self-confirm is a deliberate LLM decision
//!     rather than a conversational artefact, and short enough that a
//!     human operator can still act within it.
//!  2. Rate limiting via [`HANDOFF_REQUEST_COOLDOWN_SECS`] caps the
//!     number of pending requests that can be created back-to-back.
//!  3. The confirmation token is server-generated (not produced by the
//!     LLM) and compared via [`super::constant_time_eq`].
//!  4. Pending requests expire after [`PENDING_HANDOFF_TTL_SECS`].
//!
//! Extracted from `handoff.rs` as the second step of the §4.1 hotspot
//! split. Uses shared constants and [`super::constant_time_eq`] from
//! the parent module; metrics via [`crate::metrics`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use serde::Serialize;
use tokio::sync::RwLock;

use super::constant_time_eq;
use super::{
    CONFIRMATION_MIN_DELAY_SECS, HANDOFF_REQUEST_COOLDOWN_SECS, HandoffDirection,
    PENDING_HANDOFF_TTL_SECS,
};

/// Confirmation token length in bytes (rendered as 2× hex chars).
const CONFIRMATION_TOKEN_HEX_LEN: usize = 4;

/// Gate state for handoff operations.
///
/// Enforces the two-step request/confirm protocol:
/// 1. The LLM cannot self-confirm ([`CONFIRMATION_MIN_DELAY_SECS`] minimum delay between request and confirm)
/// 2. Rate limited (max 1 request per [`HANDOFF_REQUEST_COOLDOWN_SECS`])
/// 3. Confirmation token is generated server-side (not by LLM)
#[derive(Clone)]
pub struct PendingHandoffStore {
    inner: Arc<RwLock<PendingHandoffInner>>,
}

struct PendingHandoffInner {
    /// Current pending request (only one at a time).
    pending: Option<PendingHandoff>,
    /// Timestamp of last request (for rate limiting).
    last_request_at: Option<Instant>,
}

struct PendingHandoff {
    /// The confirmation token (hex string, e.g. "7a3f1b2c").
    confirmation_token: String,
    /// Target direction.
    direction: HandoffDirection,
    /// Reason provided by the agent.
    reason: String,
    /// When the pending request was created.
    created_at: Instant,
    /// TTL for this pending request.
    ttl: Duration,
}

impl Default for PendingHandoffStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum PendingHandoffError {
    /// Rate limited — too soon after last request.
    RateLimited { retry_after_secs: u64 },
    /// No pending request to confirm.
    NoPending,
    /// Pending request expired.
    Expired,
    /// Wrong confirmation token.
    InvalidToken,
    /// Minimum delay not elapsed (LLM tried to self-confirm).
    TooFast { elapsed_ms: u64, min_delay_ms: u64 },
}

impl std::fmt::Display for PendingHandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { retry_after_secs } => {
                write!(f, "Rate limited — retry after {retry_after_secs}s")
            }
            Self::NoPending => write!(f, "No pending handoff request"),
            Self::Expired => write!(f, "Pending handoff request expired"),
            Self::InvalidToken => write!(f, "Invalid confirmation token"),
            Self::TooFast {
                elapsed_ms,
                min_delay_ms,
            } => write!(
                f,
                "Confirmed too quickly ({elapsed_ms}ms < {min_delay_ms}ms minimum) — human confirmation required"
            ),
        }
    }
}

impl PendingHandoffStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(PendingHandoffInner {
                pending: None,
                last_request_at: None,
            })),
        }
    }

    /// Create a new pending handoff request. Returns the confirmation token.
    ///
    /// Enforces rate limiting: max 1 request per [`HANDOFF_REQUEST_COOLDOWN_SECS`].
    pub async fn create_pending(
        &self,
        direction: HandoffDirection,
        reason: String,
    ) -> Result<String, PendingHandoffError> {
        let mut guard = self.inner.write().await;

        if let Some(last) = guard.last_request_at {
            let elapsed = last.elapsed().as_secs();
            if elapsed < HANDOFF_REQUEST_COOLDOWN_SECS {
                crate::metrics::HANDOFF_PENDING_EVENTS
                    .with_label_values(&["rate_limited"])
                    .inc();
                return Err(PendingHandoffError::RateLimited {
                    retry_after_secs: HANDOFF_REQUEST_COOLDOWN_SECS - elapsed,
                });
            }
        }

        // Generate random confirmation token (4 bytes → 8 hex chars).
        let token = {
            let mut rng = rand::rng();
            let mut bytes = [0u8; CONFIRMATION_TOKEN_HEX_LEN];
            rng.fill(&mut bytes);
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };

        guard.pending = Some(PendingHandoff {
            confirmation_token: token.clone(),
            direction,
            reason,
            created_at: Instant::now(),
            ttl: Duration::from_secs(PENDING_HANDOFF_TTL_SECS),
        });
        guard.last_request_at = Some(Instant::now());

        crate::metrics::HANDOFF_PENDING_EVENTS
            .with_label_values(&["created"])
            .inc();
        Ok(token)
    }

    /// Confirm a pending handoff request with the confirmation token.
    ///
    /// Enforces:
    /// 1. Token must match (constant-time comparison)
    /// 2. Minimum delay of [`CONFIRMATION_MIN_DELAY_SECS`] since request
    ///    (prevents LLM self-confirm)
    /// 3. Request must not be expired
    ///
    /// On success, returns the direction and consumes the pending request.
    pub async fn confirm(
        &self,
        token: &str,
    ) -> Result<(HandoffDirection, String), PendingHandoffError> {
        let mut guard = self.inner.write().await;

        let pending = match guard.pending.as_ref() {
            Some(p) => p,
            None => {
                crate::metrics::HANDOFF_PENDING_EVENTS
                    .with_label_values(&["no_pending"])
                    .inc();
                return Err(PendingHandoffError::NoPending);
            }
        };

        if pending.created_at.elapsed() > pending.ttl {
            guard.pending = None;
            crate::metrics::HANDOFF_PENDING_EVENTS
                .with_label_values(&["expired"])
                .inc();
            return Err(PendingHandoffError::Expired);
        }

        let elapsed_ms = pending.created_at.elapsed().as_millis() as u64;
        let min_delay_ms = CONFIRMATION_MIN_DELAY_SECS * 1000;
        if elapsed_ms < min_delay_ms {
            crate::metrics::HANDOFF_PENDING_EVENTS
                .with_label_values(&["too_fast"])
                .inc();
            return Err(PendingHandoffError::TooFast {
                elapsed_ms,
                min_delay_ms,
            });
        }

        if !constant_time_eq(token.as_bytes(), pending.confirmation_token.as_bytes()) {
            crate::metrics::HANDOFF_PENDING_EVENTS
                .with_label_values(&["invalid_token"])
                .inc();
            return Err(PendingHandoffError::InvalidToken);
        }

        let direction = pending.direction;
        let reason = pending.reason.clone();
        guard.pending = None;

        crate::metrics::HANDOFF_PENDING_EVENTS
            .with_label_values(&["confirmed"])
            .inc();
        Ok((direction, reason))
    }

    /// Get status of any pending request (for display).
    pub async fn status(&self) -> Option<PendingHandoffStatus> {
        let guard = self.inner.read().await;
        let pending = guard.pending.as_ref()?;

        if pending.created_at.elapsed() > pending.ttl {
            return None;
        }

        Some(PendingHandoffStatus {
            direction: pending.direction,
            reason: pending.reason.clone(),
            confirmation_token: pending.confirmation_token.clone(),
            created_at_secs_ago: pending.created_at.elapsed().as_secs(),
            expires_in_secs: pending
                .ttl
                .checked_sub(pending.created_at.elapsed())
                .map(|d| d.as_secs())
                .unwrap_or(0),
        })
    }

    /// Cancel any pending request.
    pub async fn cancel(&self) {
        let had_pending = {
            let mut guard = self.inner.write().await;
            let had = guard.pending.is_some();
            guard.pending = None;
            had
        };
        if had_pending {
            crate::metrics::HANDOFF_PENDING_EVENTS
                .with_label_values(&["cancelled"])
                .inc();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingHandoffStatus {
    pub direction: HandoffDirection,
    pub reason: String,
    pub confirmation_token: String,
    pub created_at_secs_ago: u64,
    pub expires_in_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pending_handoff_create_and_confirm() {
        let store = PendingHandoffStore::new();

        let token = store
            .create_pending(HandoffDirection::LocalToAks, "going to meeting".into())
            .await
            .unwrap();
        assert_eq!(token.len(), 8);

        let status = store.status().await;
        assert!(status.is_some());
        assert_eq!(status.unwrap().confirmation_token, token);

        // Confirm too fast — should fail (min delay).
        let result = store.confirm(&token).await;
        assert!(matches!(
            result.unwrap_err(),
            PendingHandoffError::TooFast { .. }
        ));
    }

    #[tokio::test]
    async fn test_pending_handoff_wrong_token() {
        let store = PendingHandoffStore::new();
        let _token = store
            .create_pending(HandoffDirection::LocalToAks, "test".into())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(8100)).await;

        let result = store.confirm("wrong_token").await;
        assert!(matches!(
            result.unwrap_err(),
            PendingHandoffError::InvalidToken
        ));
    }

    #[tokio::test]
    async fn test_pending_handoff_confirm_after_delay() {
        let store = PendingHandoffStore::new();
        let token = store
            .create_pending(HandoffDirection::LocalToAks, "heading out".into())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(8100)).await;

        let (direction, reason) = store.confirm(&token).await.unwrap();
        assert_eq!(direction, HandoffDirection::LocalToAks);
        assert_eq!(reason, "heading out");

        assert!(store.status().await.is_none());
    }

    #[tokio::test]
    async fn test_pending_handoff_rate_limit() {
        let store = PendingHandoffStore::new();

        let _token = store
            .create_pending(HandoffDirection::LocalToAks, "first".into())
            .await
            .unwrap();

        let result = store
            .create_pending(HandoffDirection::LocalToAks, "second".into())
            .await;
        assert!(matches!(
            result.unwrap_err(),
            PendingHandoffError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn test_pending_handoff_cancel() {
        let store = PendingHandoffStore::new();
        let _token = store
            .create_pending(HandoffDirection::LocalToAks, "test".into())
            .await
            .unwrap();

        assert!(store.status().await.is_some());
        store.cancel().await;
        assert!(store.status().await.is_none());

        let result = store.confirm("anything").await;
        assert!(matches!(
            result.unwrap_err(),
            PendingHandoffError::NoPending
        ));
    }

    #[tokio::test]
    async fn test_pending_handoff_no_pending() {
        let store = PendingHandoffStore::new();
        let result = store.confirm("anything").await;
        assert!(matches!(
            result.unwrap_err(),
            PendingHandoffError::NoPending
        ));
    }
}
