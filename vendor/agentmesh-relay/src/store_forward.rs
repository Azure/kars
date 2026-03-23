use std::collections::VecDeque;
use dashmap::DashMap;
use chrono::Utc;
use tracing::{info, debug};

use crate::types::{Amid, StoredMessage};

/// Store-and-forward system for offline message delivery
pub struct StoreForward {
    /// Messages waiting for offline agents: AMID -> queue of messages
    messages: DashMap<Amid, VecDeque<StoredMessage>>,
    /// Maximum messages per agent
    max_per_agent: usize,
    /// Maximum total messages
    max_total: usize,
}

impl StoreForward {
    pub fn new() -> Self {
        Self {
            messages: DashMap::new(),
            max_per_agent: 100,
            max_total: 10000,
        }
    }

    /// Store a message for later delivery
    /// Returns true if stored, false if at capacity
    pub fn store(&self, msg: StoredMessage) -> bool {
        let to = msg.to.clone();

        // Check total capacity
        let total: usize = self.messages.iter().map(|e| e.value().len()).sum();
        if total >= self.max_total {
            return false;
        }

        let mut queue = self.messages.entry(to).or_insert_with(VecDeque::new);

        // Check per-agent capacity
        if queue.len() >= self.max_per_agent {
            // Remove oldest message to make room
            queue.pop_front();
        }

        queue.push_back(msg);
        true
    }

    /// Retrieve and remove all pending messages for an agent
    pub fn retrieve(&self, amid: &Amid) -> Vec<StoredMessage> {
        self.messages
            .remove(amid)
            .map(|(_, queue)| queue.into_iter().collect())
            .unwrap_or_default()
    }

    /// Get count of pending messages for an agent
    pub fn get_pending_count(&self, amid: &Amid) -> usize {
        self.messages
            .get(amid)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Remove expired messages
    pub fn cleanup_expired(&self) -> usize {
        let now = Utc::now();
        let mut removed = 0;

        self.messages.retain(|_, queue| {
            let before = queue.len();
            queue.retain(|msg| msg.expires_at > now);
            removed += before - queue.len();
            !queue.is_empty()
        });

        removed
    }

    /// Background task to periodically clean up expired messages
    pub async fn cleanup_expired_loop(&self) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 5 minutes

        loop {
            interval.tick().await;
            let removed = self.cleanup_expired();
            if removed > 0 {
                info!("Cleaned up {} expired stored messages", removed);
            }
        }
    }

    /// Get statistics
    pub fn stats(&self) -> StoreForwardStats {
        let total_messages: usize = self.messages.iter().map(|e| e.value().len()).sum();
        let agents_with_pending = self.messages.len();

        StoreForwardStats {
            total_messages,
            agents_with_pending,
            max_per_agent: self.max_per_agent,
            max_total: self.max_total,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoreForwardStats {
    pub total_messages: usize,
    pub agents_with_pending: usize,
    pub max_per_agent: usize,
    pub max_total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use crate::types::MessageType;
    use chrono::Duration;

    fn make_message(to: &str) -> StoredMessage {
        StoredMessage {
            id: Uuid::new_v4(),
            from: "sender".to_string(),
            to: to.to_string(),
            encrypted_payload: "encrypted_data".to_string(),
            message_type: MessageType::Message,
            timestamp: Utc::now(),
            expires_at: Utc::now() + Duration::hours(72),
        }
    }

    #[test]
    fn test_store_and_retrieve() {
        let sf = StoreForward::new();

        let msg1 = make_message("agent1");
        let msg2 = make_message("agent1");
        let msg3 = make_message("agent2");

        assert!(sf.store(msg1));
        assert!(sf.store(msg2));
        assert!(sf.store(msg3));

        assert_eq!(sf.get_pending_count(&"agent1".to_string()), 2);
        assert_eq!(sf.get_pending_count(&"agent2".to_string()), 1);

        let retrieved = sf.retrieve(&"agent1".to_string());
        assert_eq!(retrieved.len(), 2);
        assert_eq!(sf.get_pending_count(&"agent1".to_string()), 0);
    }

    #[test]
    fn test_cleanup_expired() {
        let sf = StoreForward::new();

        // Add a message that's already expired
        let mut expired_msg = make_message("agent1");
        expired_msg.expires_at = Utc::now() - Duration::hours(1);
        sf.store(expired_msg);

        // Add a valid message
        sf.store(make_message("agent1"));

        let removed = sf.cleanup_expired();
        assert_eq!(removed, 1);
        assert_eq!(sf.get_pending_count(&"agent1".to_string()), 1);
    }
}
