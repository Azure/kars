use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Trust tier levels
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "trust_tier", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    Anonymous,    // Tier 2
    Verified,     // Tier 1 - human verified
    Organization, // Tier 1.5 - org verified
}

impl TrustTier {
    pub fn level(&self) -> u8 {
        match self {
            TrustTier::Anonymous => 2,
            TrustTier::Verified => 1,
            TrustTier::Organization => 1,
        }
    }
}

/// Presence status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "presence_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Online,
    Away,
    Offline,
    Dnd,
}

/// Agent record in the registry
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Agent {
    pub id: Uuid,
    pub amid: String,
    pub signing_public_key: String,
    pub exchange_public_key: String,
    pub tier: TrustTier,
    pub display_name: Option<String>,
    pub organization_id: Option<Uuid>,
    pub capabilities: Vec<String>,
    pub relay_endpoint: String,
    pub direct_endpoint: Option<String>,
    pub status: PresenceStatus,
    pub reputation_score: f32,
    pub last_seen: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to register a new agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub amid: String,
    pub signing_public_key: String,
    pub exchange_public_key: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default = "default_relay")]
    pub relay_endpoint: String,
    #[serde(default)]
    pub direct_endpoint: Option<String>,
    /// For Tier 1 verification
    #[serde(default)]
    pub verification_token: Option<String>,
    /// Signature over the request timestamp (ISO format string)
    pub timestamp: String,
    pub signature: String,
}

fn default_relay() -> String {
    "wss://relay.agentmesh.online/v1/connect".to_string()
}

/// Response to registration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub success: bool,
    pub amid: String,
    pub tier: TrustTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Agent lookup response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLookup {
    pub amid: String,
    pub tier: TrustTier,
    pub display_name: Option<String>,
    pub organization: Option<String>,
    pub signing_public_key: String,
    pub exchange_public_key: String,
    pub capabilities: Vec<String>,
    pub relay_endpoint: String,
    pub direct_endpoint: Option<String>,
    pub status: PresenceStatus,
    pub reputation_score: f32,
    pub last_seen: DateTime<Utc>,
    /// PEM-encoded agent certificate (for verified tier)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
    /// Reputation flags (e.g., "rapid_reputation_increase")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<Vec<String>>,
    /// Number of ratings received
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratings_count: Option<u32>,
    /// Reputation status: "rated" or "unrated"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reputation_status: Option<String>,
}

/// Capability search request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySearchRequest {
    pub capability: String,
    #[serde(default)]
    pub tier_min: Option<u8>,
    #[serde(default)]
    pub reputation_min: Option<f32>,
    #[serde(default)]
    pub status: Option<PresenceStatus>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    20
}

/// Capability search response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySearchResponse {
    pub results: Vec<AgentLookup>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

/// Capability definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_time_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pricing {
    pub model: String,
    pub currency: String,
    pub amount: f64,
}

/// Update agent status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusUpdateRequest {
    pub amid: String,
    pub status: PresenceStatus,
    pub timestamp: String,
    pub signature: String,
}

/// Update agent capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesUpdateRequest {
    pub amid: String,
    pub capabilities: Vec<String>,
    pub timestamp: String,
    pub signature: String,
}

/// Organization record
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub verified: bool,
    pub root_certificate: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Reputation update
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationUpdate {
    pub target_amid: String,
    pub from_amid: String,
    pub score: f32, // 0.0 - 1.0
    pub session_id: Uuid,
    pub tags: Option<Vec<String>>,
    pub timestamp: String,
    pub signature: String,
}

/// Health check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub agents_registered: u64,
    pub agents_online: u64,
}

/// X3DH Prekey bundle for offline key exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrekeyBundle {
    pub identity_key: String,
    pub signed_prekey: String,
    pub signed_prekey_signature: String,
    pub signed_prekey_id: i32,
    pub one_time_prekeys: Vec<OneTimePrekey>,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneTimePrekey {
    pub id: i32,
    pub key: String,
}

/// Request to upload prekeys
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadPrekeysRequest {
    pub amid: String,
    pub signed_prekey: String,
    pub signed_prekey_signature: String,
    pub signed_prekey_id: i32,
    pub one_time_prekeys: Vec<OneTimePrekey>,
    pub timestamp: String,
    pub signature: String,
}

/// Response when fetching prekeys (one-time prekey is consumed)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrekeyResponse {
    pub identity_key: String,
    pub signed_prekey: String,
    pub signed_prekey_signature: String,
    pub signed_prekey_id: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_time_prekey: Option<OneTimePrekey>,
}
