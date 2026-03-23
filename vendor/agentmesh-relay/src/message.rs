use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::{Amid, MessageType};

/// Structured message format for AgentMesh communications
/// This is the decrypted payload format that agents use

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub session_id: Uuid,
    pub sequence: u32,
    pub from: Amid,
    pub to: Amid,
    #[serde(flatten)]
    pub payload: MessagePayload,
    pub timestamp: DateTime<Utc>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePayload {
    /// KNOCK - Initial handshake request
    Knock {
        protocol_version: String,
        from_info: AgentInfo,
        intent: Intent,
        session_request: SessionRequest,
    },

    /// Accept a KNOCK
    Accept {
        session_key: String,
        capabilities: Vec<String>,
        constraints: SessionConstraints,
    },

    /// Reject a KNOCK
    Reject {
        reason: String,
        human_readable: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_seconds: Option<u32>,
    },

    /// Request within a session
    Request {
        intent: Intent,
        parameters: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        budget: Option<Budget>,
        #[serde(skip_serializing_if = "Option::is_none")]
        response_format: Option<ResponseFormat>,
    },

    /// Response to a request
    Response {
        status: ResponseStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        results: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<ErrorInfo>,
    },

    /// Status update during processing
    Status {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        progress: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        estimated_completion_seconds: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Error
    Error {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after_seconds: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fallback_amid: Option<Amid>,
    },

    /// Close session
    Close {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reputation_feedback: Option<ReputationFeedback>,
    },

    /// Capability negotiation
    CapabilityNegotiation {
        i_need: Vec<String>,
        i_offer: Vec<String>,
        preferred_schemas: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        languages: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        payment_methods: Option<Vec<String>>,
    },

    /// Capability negotiation response
    CapabilityNegotiationResponse {
        matched: Vec<String>,
        unavailable: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        suggested_alternatives: Option<serde_json::Value>,
        agreed_schema: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agreed_language: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub amid: Amid,
    pub tier: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reputation_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urgency: Option<Urgency>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRequest {
    #[serde(rename = "type")]
    pub session_type: SessionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_messages: Option<u32>,
    pub ttl_seconds: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    RequestResponse,
    Conversation,
    Stream,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConstraints {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_message_size_bytes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub format_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Success,
    PartialSuccess,
    Error,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationFeedback {
    pub score: f32, // 0.0 - 1.0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Standard capability categories
pub mod capabilities {
    pub const TRAVEL: &str = "travel";
    pub const COMMERCE: &str = "commerce";
    pub const FINANCE: &str = "finance";
    pub const PRODUCTIVITY: &str = "productivity";
    pub const CREATIVE: &str = "creative";
    pub const RESEARCH: &str = "research";
    pub const DEVELOPMENT: &str = "development";
    pub const COMMUNICATION: &str = "communication";
    pub const MARKETPLACE: &str = "marketplace";
}

/// Validate a message is well-formed
pub fn validate_message(msg: &AgentMessage) -> Result<(), &'static str> {
    // Basic validation
    if msg.from.is_empty() {
        return Err("Empty sender AMID");
    }

    if msg.to.is_empty() {
        return Err("Empty recipient AMID");
    }

    if msg.signature.is_empty() {
        return Err("Missing signature");
    }

    Ok(())
}
