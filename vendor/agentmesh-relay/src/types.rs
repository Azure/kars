use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// AgentMesh ID - derived from signing public key
pub type Amid = String;

/// Protocol message types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayMessage {
    /// Client connects to relay
    Connect {
        protocol: String,
        amid: Amid,
        /// Base64-encoded Ed25519 public key (required for signature verification)
        public_key: String,
        signature: String,
        /// Raw ISO timestamp string — kept as String so signature verification
        /// uses the exact bytes the SDK signed (avoids chrono re-serialization mismatch).
        timestamp: String,
        #[serde(default)]
        p2p_capable: bool,
    },

    /// Server confirms connection
    Connected {
        session_id: Uuid,
        pending_messages: u32,
    },

    /// Client sends message to another agent
    Send {
        to: Amid,
        encrypted_payload: String,
        message_type: MessageType,
        #[serde(skip_serializing_if = "Option::is_none")]
        ice_candidates: Option<Vec<IceCandidate>>,
    },

    /// Server delivers message to client
    Receive {
        from: Amid,
        encrypted_payload: String,
        message_type: MessageType,
        timestamp: DateTime<Utc>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ice_candidates: Option<Vec<IceCandidate>>,
    },

    /// Presence update
    Presence {
        status: PresenceStatus,
    },

    /// Query presence of another agent
    PresenceQuery {
        amid: Amid,
    },

    /// Response to presence query
    PresenceResponse {
        amid: Amid,
        status: PresenceStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_seen: Option<DateTime<Utc>>,
    },

    /// ICE negotiation for P2P upgrade
    IceOffer {
        to: Amid,
        sdp: String,
        candidates: Vec<IceCandidate>,
    },

    /// ICE answer
    IceAnswer {
        to: Amid,
        sdp: String,
        candidates: Vec<IceCandidate>,
    },

    /// P2P upgrade successful
    P2PEstablished {
        peer: Amid,
    },

    /// Error message
    Error {
        code: ErrorCode,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_seconds: Option<u32>,
    },

    /// Ping for keepalive
    Ping {
        timestamp: DateTime<Utc>,
    },

    /// Pong response
    Pong {
        timestamp: DateTime<Utc>,
    },

    /// Disconnect gracefully
    Disconnect {
        reason: String,
    },
}

/// Message types that flow through the relay
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Knock,
    Accept,
    Reject,
    Message,
    Close,
    Status,
    /// Optimistic message sent to allowlisted contacts (skip KNOCK, use cached session)
    OptimisticMessage,
}

/// Presence status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Online,
    Away,
    Offline,
    Dnd,
}

/// ICE candidate for P2P negotiation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceCandidate {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_mline_index: Option<u32>,
}

/// Error codes
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidSignature,
    UnknownRecipient,
    RecipientOffline,
    RateLimited,
    MessageTooLarge,
    InvalidMessage,
    InternalError,
    ProtocolMismatch,
    Unauthorized,
}

/// Stored message for offline delivery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: Uuid,
    pub from: Amid,
    pub to: Amid,
    pub encrypted_payload: String,
    pub message_type: MessageType,
    pub timestamp: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Agent connection state
#[derive(Debug, Clone)]
pub struct AgentConnection {
    pub amid: Amid,
    pub session_id: Uuid,
    pub status: PresenceStatus,
    pub connected_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub p2p_capable: bool,
}

/// Rate limiter state per agent
#[derive(Debug, Clone)]
pub struct RateLimitState {
    pub messages_this_minute: u32,
    pub knocks_this_minute: u32,
    pub minute_start: DateTime<Utc>,
}

impl Default for RateLimitState {
    fn default() -> Self {
        Self {
            messages_this_minute: 0,
            knocks_this_minute: 0,
            minute_start: Utc::now(),
        }
    }
}

/// Configuration
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub max_message_size: usize,
    pub max_pending_messages: usize,
    pub message_ttl_hours: u32,
    pub rate_limit_messages_per_minute: u32,
    pub rate_limit_knocks_per_minute: u32,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_message_size: 1_048_576, // 1MB — handoff snapshots can be large
            max_pending_messages: 100,
            message_ttl_hours: 72,
            rate_limit_messages_per_minute: 100,
            rate_limit_knocks_per_minute: 30,
        }
    }
}
