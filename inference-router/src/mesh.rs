//! Local mesh state — inbox buffer and metrics counters.
//!
//! These are the only pieces of governance.rs that survive: simple data
//! structures for mesh communication that the sidecar doesn't own.

use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicU64;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMessage {
    pub id: String,
    pub from_agent: String,
    pub to_agent: String,
    pub content: String,
    pub message_type: String,
    pub timestamp: String,
    pub signature: String,
}

/// Inbox for received mesh messages (fallback HTTP path, not E2E relay).
pub struct MeshInbox {
    messages: RwLock<Vec<MeshMessage>>,
}

impl MeshInbox {
    pub fn new() -> Self {
        Self {
            messages: RwLock::new(Vec::new()),
        }
    }

    pub async fn receive(&self, msg: MeshMessage) {
        self.messages.write().await.push(msg);
    }

    pub async fn peek(&self) -> Vec<MeshMessage> {
        self.messages.read().await.clone()
    }
}

/// Counters for AGT mesh activity (atomic, lock-free).
pub struct MeshMetrics {
    pub sessions: AtomicU64,
    pub messages_sent: AtomicU64,
    pub messages_received: AtomicU64,
    pub trust_updates: AtomicU64,
}

impl MeshMetrics {
    pub fn new() -> Self {
        Self {
            sessions: AtomicU64::new(0),
            messages_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            trust_updates: AtomicU64::new(0),
        }
    }
}
