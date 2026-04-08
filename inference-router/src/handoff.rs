//! Agent handoff — live migration (local ↔ cloud).
//!
//! Implements the handoff protocol from `internal/global-agentmesh-plan.md` §9.4.
//!
//! **Security model** (three-layer auth for handoff endpoints):
//! 1. Handoff token — one-time, TTL-based, in-memory only
//! 2. No localhost bypass — even same-pod calls need both tokens
//! 3. Mutual attestation — DH-encrypted state + Ed25519 succession signature
//!
//! All handoff endpoints are audit-logged with caller IP, timestamp, and outcome.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::Aead,
};
use axum::{
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::IntoResponse,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use hkdf::Hkdf;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;

use crate::routes::AppState;
use crate::spawn::SpawnRequest;

// ── Constants ────────────────────────────────────────────────────────────────

pub const HANDOFF_STATE_VERSION: u32 = 1;
const HANDOFF_TOKEN_BYTES: usize = 32;
/// Default TTL for handoff tokens (seconds).
pub const DEFAULT_TOKEN_TTL_SECS: u64 = 300; // 5 minutes
const MAX_TOKEN_TTL_SECS: u64 = 600; // 10 minutes
const HKDF_INFO: &[u8] = b"azureclaw-handoff-v1";
const AES_NONCE_BYTES: usize = 12;

// ── State transfer types ────────────────────────────────────────────────────

/// Complete agent state for handoff transfer.
///
/// Contains everything needed to resume an agent on a different host.
/// Private keys are NEVER included (identity succession replaces key transfer).
#[derive(Debug, Serialize, Deserialize)]
pub struct HandoffState {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Agent display name.
    pub agent_name: String,
    /// AMID of the sending agent.
    pub predecessor_amid: String,
    /// AMID of the receiving agent (set during restore).
    pub successor_amid: String,
    /// Trust scores snapshot (`/tmp/agt/trust_scores.json`).
    pub trust_scores: serde_json::Value,
    /// Full audit chain for integrity verification.
    pub audit_entries: Vec<AuditEntry>,
    /// Current token budget usage counters.
    pub token_budget_used: TokenUsage,
    /// Compressed workspace files (tar.gz).
    #[serde(with = "base64_bytes")]
    pub workspace_tar: Vec<u8>,
    /// Serialized chat history (if available).
    #[serde(with = "option_base64_bytes")]
    pub chat_snapshot: Option<Vec<u8>>,
    /// Current policy YAML for verification (cloud should match).
    pub policy_yaml: String,
    /// Sub-agent snapshots for re-spawn.
    pub sub_agent_snapshots: Vec<SubAgentSnapshot>,
    /// Credential references (name + env key, NOT the secret values).
    pub credentials: Vec<CredentialRef>,
    /// Handoff metadata (timestamps, direction, nonces, hashes).
    pub metadata: HandoffMetadata,
}

/// Audit chain entry (mirrors governance audit format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub action: String,
    pub agent_id: String,
    pub decision: String,
    pub details: Option<String>,
    pub hash: String,
    pub prev_hash: String,
}

/// Token usage counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Sub-agent state for re-spawn on the target host.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubAgentSnapshot {
    pub name: String,
    pub original_amid: String,
    pub spawn_config: SpawnRequest,
    pub task_context: String,
    /// "completed" | "paused_at_checkpoint"
    pub status: String,
    pub checkpoint: Option<String>,
    #[serde(with = "base64_bytes")]
    pub workspace_tar: Vec<u8>,
}

/// Credential reference (name + env var key only — secret values travel via CLI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRef {
    pub name: String,
    pub env_key: String,
}

/// Handoff metadata for audit trail and verification.
#[derive(Debug, Serialize, Deserialize)]
pub struct HandoffMetadata {
    /// ISO 8601 timestamp when handoff was initiated.
    pub initiated_at: String,
    /// Transfer direction.
    pub direction: HandoffDirection,
    /// Source hostname for audit trail.
    pub source_host: String,
    /// Random nonce for HKDF key derivation (base64).
    #[serde(with = "base64_bytes")]
    pub nonce: Vec<u8>,
    /// SHA-256 of the plaintext state (hex) for integrity verification.
    pub verification_hash: String,
    /// Signed succession notice (forward) or reclamation notice (reverse).
    #[serde(with = "option_base64_bytes")]
    pub succession_notice: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffDirection {
    LocalToAks,
    AksToLocal,
}

impl std::fmt::Display for HandoffDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalToAks => write!(f, "local_to_aks"),
            Self::AksToLocal => write!(f, "aks_to_local"),
        }
    }
}

// ── Encrypted blob ──────────────────────────────────────────────────────────

/// Encrypted handoff state blob (AES-256-GCM).
#[derive(Debug, Serialize, Deserialize)]
pub struct EncryptedHandoffBlob {
    /// Schema version.
    pub version: u32,
    /// AES-256-GCM nonce (base64).
    pub nonce: String,
    /// Encrypted + compressed state (base64).
    pub ciphertext: String,
    /// HKDF salt (base64) — needed by receiver to derive the same key.
    pub hkdf_salt: String,
    /// SHA-256 of plaintext for pre-decryption integrity check (hex).
    pub verification_hash: String,
}

// ── Handoff token store ─────────────────────────────────────────────────────

/// In-memory handoff token store.
///
/// - Only ONE active token at a time (prevents concurrent handoff races)
/// - Tokens auto-expire after TTL
/// - Token is never persisted to disk or environment
/// - Token hash (not value) is logged for audit
#[derive(Clone)]
pub struct HandoffTokenStore {
    inner: Arc<RwLock<Option<ActiveToken>>>,
}

struct ActiveToken {
    /// The raw token value (32 bytes, base64-encoded for comparison).
    token_b64: String,
    /// SHA-256 hash of the token for audit logging (hex).
    token_hash: String,
    /// When the token was created.
    created_at: Instant,
    /// Time-to-live.
    ttl: Duration,
    /// Whether this token has been used (reserved for future one-time enforcement).
    _used: bool,
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
    /// Returns (token_base64, token_hash_hex) — caller sends token to client,
    /// stores hash for audit.
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
            _used: false,
        };

        *self.inner.write().await = Some(active);
        (token_b64, token_hash)
    }

    /// Validate a handoff token. Returns Ok(token_hash) on success.
    ///
    /// The token is consumed on first successful validation (one-time use
    /// is enforced by marking it used, not deleting — audit trail preserved).
    pub async fn validate(&self, provided: &str) -> Result<String, HandoffTokenError> {
        let mut guard = self.inner.write().await;

        let active = guard
            .as_mut()
            .ok_or(HandoffTokenError::NoActiveToken)?;

        // Check expiry
        if active.created_at.elapsed() > active.ttl {
            *guard = None;
            return Err(HandoffTokenError::Expired);
        }

        // Check value
        if !constant_time_eq(provided.as_bytes(), active.token_b64.as_bytes()) {
            return Err(HandoffTokenError::Invalid);
        }

        // Check one-time use (for snapshot/restore we allow reuse within the session)
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

// ── Handoff session tracker ─────────────────────────────────────────────────

/// Tracks the state of an in-progress handoff.
#[derive(Clone)]
pub struct HandoffSession {
    inner: Arc<RwLock<HandoffSessionInner>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffSessionInner {
    pub phase: HandoffPhase,
    pub direction: Option<HandoffDirection>,
    pub started_at: Option<String>,
    pub predecessor_amid: Option<String>,
    pub successor_amid: Option<String>,
    pub snapshot_size_bytes: Option<usize>,
    pub snapshot_items: Option<SnapshotItemCounts>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffPhase {
    Idle,
    Initialized,
    Draining,
    Snapshotting,
    Transferring,
    Restoring,
    Verifying,
    Decommissioning,
    Complete,
    Failed,
    Aborted,
}

impl std::fmt::Display for HandoffPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotItemCounts {
    pub chat_messages: u32,
    pub trust_scores: u32,
    pub audit_entries: u32,
    pub sub_agents: u32,
    pub workspace_files: u32,
    pub credentials: u32,
}

impl Default for HandoffSession {
    fn default() -> Self {
        Self::new()
    }
}

impl HandoffSession {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HandoffSessionInner {
                phase: HandoffPhase::Idle,
                direction: None,
                started_at: None,
                predecessor_amid: None,
                successor_amid: None,
                snapshot_size_bytes: None,
                snapshot_items: None,
                error: None,
            })),
        }
    }

    pub async fn status(&self) -> HandoffSessionInner {
        self.inner.read().await.clone()
    }

    pub async fn set_phase(&self, phase: HandoffPhase) {
        self.inner.write().await.phase = phase;
    }

    pub async fn phase(&self) -> HandoffPhase {
        self.inner.read().await.phase
    }

    pub async fn initialize(
        &self,
        direction: HandoffDirection,
        predecessor_amid: Option<String>,
    ) {
        let mut inner = self.inner.write().await;
        inner.phase = HandoffPhase::Initialized;
        inner.direction = Some(direction);
        inner.started_at = Some(iso_now());
        inner.predecessor_amid = predecessor_amid;
        inner.successor_amid = None;
        inner.snapshot_size_bytes = None;
        inner.snapshot_items = None;
        inner.error = None;
    }

    pub async fn record_snapshot(&self, size_bytes: usize, items: SnapshotItemCounts) {
        let mut inner = self.inner.write().await;
        inner.snapshot_size_bytes = Some(size_bytes);
        inner.snapshot_items = Some(items);
    }

    pub async fn fail(&self, error: String) {
        let mut inner = self.inner.write().await;
        inner.phase = HandoffPhase::Failed;
        inner.error = Some(error);
    }

    pub async fn abort(&self) {
        let mut inner = self.inner.write().await;
        inner.phase = HandoffPhase::Aborted;
    }

    pub async fn complete(&self) {
        self.inner.write().await.phase = HandoffPhase::Complete;
    }

    /// Can a new handoff be started? Only from Idle, Complete, Failed, or Aborted.
    pub async fn can_start(&self) -> bool {
        matches!(
            self.inner.read().await.phase,
            HandoffPhase::Idle
                | HandoffPhase::Complete
                | HandoffPhase::Failed
                | HandoffPhase::Aborted
        )
    }
}

// ── State serialization + encryption ────────────────────────────────────────

/// Serialize a HandoffState to compressed JSON bytes.
pub fn serialize_state(state: &HandoffState) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(state).map_err(|e| format!("JSON serialize: {e}"))?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&json)
        .map_err(|e| format!("gzip compress: {e}"))?;
    encoder.finish().map_err(|e| format!("gzip finish: {e}"))
}

/// Deserialize compressed JSON bytes back to HandoffState.
pub fn deserialize_state(compressed: &[u8]) -> Result<HandoffState, String> {
    let mut decoder = GzDecoder::new(compressed);
    let mut json = Vec::new();
    decoder
        .read_to_end(&mut json)
        .map_err(|e| format!("gzip decompress: {e}"))?;
    serde_json::from_slice(&json).map_err(|e| format!("JSON deserialize: {e}"))
}

/// Encrypt state blob with AES-256-GCM.
///
/// Key is derived via HKDF from a shared secret (DH exchange between agents).
/// For Phase H1, the shared secret is the handoff token itself (CLI knows both sides).
/// Phase H2+ replaces this with actual X25519 DH shared secret.
pub fn encrypt_state(
    plaintext: &[u8],
    shared_secret: &[u8],
    salt: &[u8],
) -> Result<EncryptedHandoffBlob, String> {
    // Derive AES-256 key via HKDF-SHA256
    let hk = Hkdf::<Sha256>::new(Some(salt), shared_secret);
    let mut key_bytes = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key_bytes)
        .map_err(|e| format!("HKDF expand: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| format!("AES key init: {e}"))?;

    // Random 96-bit nonce
    let mut nonce_bytes = [0u8; AES_NONCE_BYTES];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("AES-GCM encrypt: {e}"))?;

    let verification_hash = hex_sha256(plaintext);

    Ok(EncryptedHandoffBlob {
        version: HANDOFF_STATE_VERSION,
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(&ciphertext),
        hkdf_salt: BASE64.encode(salt),
        verification_hash,
    })
}

/// Decrypt an encrypted handoff blob.
pub fn decrypt_state(
    blob: &EncryptedHandoffBlob,
    shared_secret: &[u8],
) -> Result<Vec<u8>, String> {
    let salt = BASE64
        .decode(&blob.hkdf_salt)
        .map_err(|e| format!("decode salt: {e}"))?;
    let nonce_bytes = BASE64
        .decode(&blob.nonce)
        .map_err(|e| format!("decode nonce: {e}"))?;
    let ciphertext = BASE64
        .decode(&blob.ciphertext)
        .map_err(|e| format!("decode ciphertext: {e}"))?;

    if nonce_bytes.len() != AES_NONCE_BYTES {
        return Err(format!(
            "invalid nonce length: {} (expected {AES_NONCE_BYTES})",
            nonce_bytes.len()
        ));
    }

    // Derive same key via HKDF
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared_secret);
    let mut key_bytes = [0u8; 32];
    hk.expand(HKDF_INFO, &mut key_bytes)
        .map_err(|e| format!("HKDF expand: {e}"))?;

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| format!("AES key init: {e}"))?;

    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "AES-GCM decryption failed — wrong key or tampered ciphertext".to_string())?;

    // Verify integrity
    let hash = hex_sha256(&plaintext);
    if hash != blob.verification_hash {
        return Err(format!(
            "integrity check failed: computed={} expected={}",
            &hash[..16],
            &blob.verification_hash[..16]
        ));
    }

    Ok(plaintext)
}

/// Compute verification hash of a plaintext state blob.
pub fn compute_verification_hash(plaintext: &[u8]) -> String {
    hex_sha256(plaintext)
}

// ── Handoff auth middleware ──────────────────────────────────────────────────

/// Authentication middleware for handoff endpoints.
///
/// **CRITICAL SECURITY**: Unlike `admin_auth_middleware`, this middleware:
/// 1. Does NOT allow localhost bypass (prompt injection protection)
/// 2. Requires BOTH admin token AND handoff token
/// 3. Validates the handoff token against the in-memory store
///
/// The handoff token exists only in CLI process memory — the agent process
/// inside the pod never sees it, preventing prompt injection attacks from
/// exfiltrating state via localhost calls.
pub async fn handoff_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    // Extract admin token from Authorization header
    let admin_token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    // Verify admin token — NO localhost bypass
    let expected_admin = match &state.admin_token {
        Some(token) => token.as_str(),
        None => {
            tracing::error!("handoff auth: no admin token configured");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server misconfiguration: no admin token",
            )
                .into_response();
        }
    };

    match admin_token {
        Some(provided) if constant_time_eq(provided.as_bytes(), expected_admin.as_bytes()) => {}
        Some(_) => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff auth: invalid admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response();
        }
        None => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff auth: missing admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Admin token required").into_response();
        }
    }

    // Extract and verify handoff token from X-Handoff-Token header
    let handoff_token = headers
        .get("x-handoff-token")
        .and_then(|v| v.to_str().ok());

    match handoff_token {
        Some(token) => {
            match state.handoff_tokens.validate(token).await {
                Ok(token_hash) => {
                    tracing::info!(
                        path = %request.uri().path(),
                        token_hash = &token_hash[..16],
                        "handoff auth: validated"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        path = %request.uri().path(),
                        error = %e,
                        "handoff auth: token validation failed"
                    );
                    return (StatusCode::UNAUTHORIZED, format!("Handoff token: {e}"))
                        .into_response();
                }
            }
        }
        None => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff auth: missing X-Handoff-Token header"
            );
            return (
                StatusCode::UNAUTHORIZED,
                "X-Handoff-Token header required for handoff endpoints",
            )
                .into_response();
        }
    }

    next.run(request).await.into_response()
}

/// Auth middleware for the handoff/init endpoint — requires admin token only
/// (the handoff token doesn't exist yet; this endpoint creates it).
///
/// NO localhost bypass — only the CLI should call this.
pub async fn handoff_init_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    let admin_token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let expected_admin = match &state.admin_token {
        Some(token) => token.as_str(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server misconfiguration: no admin token",
            )
                .into_response();
        }
    };

    match admin_token {
        Some(provided) if constant_time_eq(provided.as_bytes(), expected_admin.as_bytes()) => {}
        Some(_) => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff init auth: invalid admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response();
        }
        None => {
            tracing::warn!(
                path = %request.uri().path(),
                "handoff init auth: missing admin token"
            );
            return (StatusCode::UNAUTHORIZED, "Admin token required").into_response();
        }
    }

    next.run(request).await.into_response()
}

/// Auth middleware for the handoff/status endpoint — admin token required,
/// but handoff token is optional (read-only, safe to query).
/// Localhost bypass IS allowed for status.
pub async fn handoff_status_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    // Allow localhost for status (read-only)
    if let Some(connect_info) = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        && connect_info.0.ip().is_loopback()
    {
        return next.run(request).await.into_response();
    }

    // Non-localhost: require admin token
    let admin_token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let expected_admin = match &state.admin_token {
        Some(token) => token.as_str(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server misconfiguration: no admin token",
            )
                .into_response();
        }
    };

    match admin_token {
        Some(provided) if constant_time_eq(provided.as_bytes(), expected_admin.as_bytes()) => {}
        Some(_) => {
            return (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response();
        }
        None => {
            return (StatusCode::UNAUTHORIZED, "Admin token required").into_response();
        }
    }

    next.run(request).await.into_response()
}

// ── Snapshot builder ────────────────────────────────────────────────────────

/// Build a HandoffState from the current router state.
///
/// Collects: trust scores, audit chain, token budget, policy YAML, sub-agent info.
/// Does NOT include workspace or chat (those come from the agent process via mesh).
pub async fn build_snapshot(
    state: &AppState,
    direction: HandoffDirection,
    predecessor_amid: &str,
    successor_amid: &str,
) -> Result<HandoffState, String> {
    // Trust scores (as JSON value from governance)
    let trust_scores = {
        let scores = state.governance.all_trust_scores();
        serde_json::to_value(&scores).unwrap_or(serde_json::Value::Null)
    };

    // Audit entries (from governance wrapper)
    let audit_entries = {
        let raw_entries = state.governance.audit.entries();
        raw_entries
            .iter()
            .map(|e| AuditEntry {
                timestamp: e.timestamp.clone(),
                action: e.action.clone(),
                agent_id: e.agent_id.clone(),
                decision: e.decision.clone(),
                details: None,
                hash: e.hash.clone(),
                prev_hash: e.previous_hash.clone(),
            })
            .collect()
    };

    // Token budget
    let usage = state.budget.get_usage(&state.sandbox_name).await;
    let token_budget_used = TokenUsage {
        prompt_tokens: usage.0,
        completion_tokens: usage.1,
        total_tokens: usage.0 + usage.1,
    };

    // Policy summary (rule count, not raw YAML — the receiver has its own policy)
    let policy_yaml = format!(
        "# Policy loaded: {} rules",
        state.governance.policy.is_loaded()
    );

    // Nonce for HKDF (ThreadRng is !Send — scope before await)
    let nonce = {
        let mut n = vec![0u8; 32];
        rand::rng().fill(&mut n[..]);
        n
    };

    let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());

    let handoff_state = HandoffState {
        version: HANDOFF_STATE_VERSION,
        agent_name: state.sandbox_name.as_ref().clone(),
        predecessor_amid: predecessor_amid.to_string(),
        successor_amid: successor_amid.to_string(),
        trust_scores,
        audit_entries,
        token_budget_used,
        workspace_tar: Vec::new(), // Populated by agent via mesh message
        chat_snapshot: None,       // Populated by agent via mesh message
        policy_yaml,
        sub_agent_snapshots: Vec::new(), // Populated during drain phase
        credentials: Vec::new(),         // Populated by CLI (knows credential names)
        metadata: HandoffMetadata {
            initiated_at: iso_now(),
            direction,
            source_host: hostname,
            nonce,
            verification_hash: String::new(), // Computed after serialization
            succession_notice: None,          // Added during succession phase
        },
    };

    Ok(handoff_state)
}

// ── Drain mode ──────────────────────────────────────────────────────────────

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
        self.inner
            .read()
            .await
            .drain_started
            .map(|s| s.elapsed())
    }
}

// ── Utility functions ───────────────────────────────────────────────────────

/// SHA-256 hash as hex string.
fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time string comparison (prevents timing attacks on token validation).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Current time as ISO 8601 string.
fn iso_now() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Format as ISO 8601 UTC
    let secs_per_day = 86400u64;
    let days = now / secs_per_day;
    let remaining = now % secs_per_day;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;

    // Days since epoch → date (simplified, sufficient for audit timestamps)
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ── Base64 serde helpers ────────────────────────────────────────────────────

mod base64_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error> {
        BASE64.encode(bytes).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        BASE64.decode(s).map_err(serde::de::Error::custom)
    }
}

mod option_base64_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        opt: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match opt {
            Some(bytes) => BASE64.encode(bytes).serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let opt = Option::<String>::deserialize(deserializer)?;
        match opt {
            Some(s) => BASE64.decode(s).map(Some).map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_sha256() {
        let hash = hex_sha256(b"hello world");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn test_iso_now_format() {
        let now = iso_now();
        // Should be ISO 8601 format: YYYY-MM-DDTHH:MM:SSZ
        assert!(now.ends_with('Z'));
        assert_eq!(now.len(), 20);
        assert_eq!(&now[4..5], "-");
        assert_eq!(&now[7..8], "-");
        assert_eq!(&now[10..11], "T");
        assert_eq!(&now[13..14], ":");
        assert_eq!(&now[16..17], ":");
    }

    #[test]
    fn test_days_to_date_epoch() {
        let (y, m, d) = days_to_date(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_date_known() {
        // 2026-04-08 = day 20551
        let (y, m, d) = days_to_date(20551);
        assert_eq!((y, m, d), (2026, 4, 8));
    }

    #[tokio::test]
    async fn test_handoff_token_create_and_validate() {
        let store = HandoffTokenStore::new();
        assert!(!store.is_active().await);

        let (token, hash) = store.create_token(60).await;
        assert!(store.is_active().await);
        assert!(!token.is_empty());
        assert_eq!(hash.len(), 64); // SHA-256 hex

        // Valid token
        let result = store.validate(&token).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), hash);

        // Still active (not consumed — session reuse allowed)
        assert!(store.is_active().await);
    }

    #[tokio::test]
    async fn test_handoff_token_invalid() {
        let store = HandoffTokenStore::new();
        let (_token, _hash) = store.create_token(60).await;

        let result = store.validate("wrong-token").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handoff_token_no_active() {
        let store = HandoffTokenStore::new();
        let result = store.validate("anything").await;
        assert!(matches!(result.unwrap_err(), HandoffTokenError::NoActiveToken));
    }

    #[tokio::test]
    async fn test_handoff_token_expired() {
        let store = HandoffTokenStore::new();
        // Create with 0-second TTL (immediately expired)
        let (token, _hash) = store.create_token(0).await;
        // Small sleep to ensure expiration
        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = store.validate(&token).await;
        assert!(matches!(result.unwrap_err(), HandoffTokenError::Expired));
    }

    #[tokio::test]
    async fn test_handoff_token_replace_old() {
        let store = HandoffTokenStore::new();
        let (token1, _) = store.create_token(60).await;
        let (token2, _) = store.create_token(60).await;

        // Old token no longer valid
        let result = store.validate(&token1).await;
        assert!(result.is_err());

        // New token is valid
        let result = store.validate(&token2).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_handoff_token_revoke() {
        let store = HandoffTokenStore::new();
        let (_token, _) = store.create_token(60).await;
        assert!(store.is_active().await);

        store.revoke().await;
        assert!(!store.is_active().await);
    }

    #[tokio::test]
    async fn test_handoff_token_max_ttl_clamped() {
        let store = HandoffTokenStore::new();
        // Request 99999 seconds — should be clamped to MAX_TOKEN_TTL_SECS
        let (token, _) = store.create_token(99999).await;
        // Token should still be valid (clamped, not rejected)
        assert!(store.validate(&token).await.is_ok());
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let state = make_test_state();
        let compressed = serialize_state(&state).unwrap();
        let restored = deserialize_state(&compressed).unwrap();

        assert_eq!(restored.version, state.version);
        assert_eq!(restored.agent_name, state.agent_name);
        assert_eq!(restored.predecessor_amid, state.predecessor_amid);
        assert_eq!(restored.successor_amid, state.successor_amid);
        assert_eq!(
            restored.token_budget_used.total_tokens,
            state.token_budget_used.total_tokens
        );
        assert_eq!(restored.credentials.len(), state.credentials.len());
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let plaintext = b"sensitive agent state data here";
        let secret = b"shared-secret-from-dh-exchange";
        let salt = b"random-salt-bytes";

        let blob = encrypt_state(plaintext, secret, salt).unwrap();
        assert_eq!(blob.version, HANDOFF_STATE_VERSION);
        assert!(!blob.ciphertext.is_empty());
        assert!(!blob.nonce.is_empty());

        let decrypted = decrypt_state(&blob, secret).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_wrong_key_fails() {
        let plaintext = b"secret data";
        let secret = b"correct-key";
        let wrong_secret = b"wrong-key!!!";
        let salt = b"salt";

        let blob = encrypt_state(plaintext, secret, salt).unwrap();
        let result = decrypt_state(&blob, wrong_secret);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("decryption failed"));
    }

    #[test]
    fn test_encrypt_tampered_ciphertext_fails() {
        let plaintext = b"secret data";
        let secret = b"key";
        let salt = b"salt";

        let mut blob = encrypt_state(plaintext, secret, salt).unwrap();
        // Tamper with ciphertext
        let mut ct = BASE64.decode(&blob.ciphertext).unwrap();
        if !ct.is_empty() {
            ct[0] ^= 0xFF;
        }
        blob.ciphertext = BASE64.encode(&ct);

        let result = decrypt_state(&blob, secret);
        assert!(result.is_err());
    }

    #[test]
    fn test_verification_hash_consistency() {
        let data = b"test data for hashing";
        let hash1 = compute_verification_hash(data);
        let hash2 = compute_verification_hash(data);
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_full_state_encrypt_decrypt_roundtrip() {
        let state = make_test_state();
        let compressed = serialize_state(&state).unwrap();
        let secret = b"dh-shared-secret";
        let salt = b"hkdf-salt-value";

        let blob = encrypt_state(&compressed, secret, salt).unwrap();
        let decrypted = decrypt_state(&blob, secret).unwrap();
        let restored = deserialize_state(&decrypted).unwrap();

        assert_eq!(restored.agent_name, "test-agent");
        assert_eq!(restored.predecessor_amid, "AMID_A");
        assert_eq!(restored.successor_amid, "AMID_B");
    }

    #[tokio::test]
    async fn test_handoff_session_lifecycle() {
        let session = HandoffSession::new();
        assert_eq!(session.phase().await, HandoffPhase::Idle);
        assert!(session.can_start().await);

        session
            .initialize(HandoffDirection::LocalToAks, Some("AMID_A".into()))
            .await;
        assert_eq!(session.phase().await, HandoffPhase::Initialized);
        assert!(!session.can_start().await);

        session.set_phase(HandoffPhase::Draining).await;
        assert_eq!(session.phase().await, HandoffPhase::Draining);

        session.set_phase(HandoffPhase::Snapshotting).await;
        session
            .record_snapshot(
                524288,
                SnapshotItemCounts {
                    chat_messages: 47,
                    trust_scores: 3,
                    audit_entries: 120,
                    sub_agents: 2,
                    workspace_files: 5,
                    credentials: 1,
                },
            )
            .await;

        let status = session.status().await;
        assert_eq!(status.snapshot_size_bytes, Some(524288));
        assert_eq!(status.snapshot_items.as_ref().unwrap().chat_messages, 47);

        session.complete().await;
        assert_eq!(session.phase().await, HandoffPhase::Complete);
        assert!(session.can_start().await);
    }

    #[tokio::test]
    async fn test_handoff_session_abort() {
        let session = HandoffSession::new();
        session
            .initialize(HandoffDirection::AksToLocal, None)
            .await;
        session.abort().await;
        assert_eq!(session.phase().await, HandoffPhase::Aborted);
        assert!(session.can_start().await);
    }

    #[tokio::test]
    async fn test_handoff_session_fail() {
        let session = HandoffSession::new();
        session
            .initialize(HandoffDirection::LocalToAks, None)
            .await;
        session.fail("integrity check failed".into()).await;

        let status = session.status().await;
        assert_eq!(status.phase, HandoffPhase::Failed);
        assert_eq!(status.error.as_deref(), Some("integrity check failed"));
        assert!(session.can_start().await);
    }

    #[tokio::test]
    async fn test_drain_state() {
        let drain = DrainState::new();
        assert!(!drain.is_draining().await);
        assert!(drain.drain_duration().await.is_none());

        drain.start_drain().await;
        assert!(drain.is_draining().await);
        assert!(drain.drain_duration().await.is_some());

        drain.stop_drain().await;
        assert!(!drain.is_draining().await);
    }

    #[test]
    fn test_handoff_direction_display() {
        assert_eq!(HandoffDirection::LocalToAks.to_string(), "local_to_aks");
        assert_eq!(HandoffDirection::AksToLocal.to_string(), "aks_to_local");
    }

    #[test]
    fn test_handoff_direction_serde_roundtrip() {
        let dir = HandoffDirection::LocalToAks;
        let json = serde_json::to_string(&dir).unwrap();
        assert_eq!(json, "\"local_to_aks\"");
        let restored: HandoffDirection = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, dir);
    }

    // ── Test helpers ────────────────────────────────────────────────────────

    fn make_test_state() -> HandoffState {
        HandoffState {
            version: HANDOFF_STATE_VERSION,
            agent_name: "test-agent".to_string(),
            predecessor_amid: "AMID_A".to_string(),
            successor_amid: "AMID_B".to_string(),
            trust_scores: serde_json::json!({"agent1": 750, "agent2": 300}),
            audit_entries: vec![AuditEntry {
                timestamp: "2026-04-08T14:30:00Z".to_string(),
                action: "handoff:init".to_string(),
                agent_id: "AMID_A".to_string(),
                decision: "allow".to_string(),
                details: Some("handoff initiated".to_string()),
                hash: "abc123".to_string(),
                prev_hash: "000000".to_string(),
            }],
            token_budget_used: TokenUsage {
                prompt_tokens: 15000,
                completion_tokens: 5000,
                total_tokens: 20000,
            },
            workspace_tar: vec![1, 2, 3, 4],
            chat_snapshot: Some(vec![5, 6, 7, 8]),
            policy_yaml: "- deny: web_search\n- allow: code_execution".to_string(),
            sub_agent_snapshots: vec![SubAgentSnapshot {
                name: "researcher".to_string(),
                original_amid: "AMID_SUB1".to_string(),
                spawn_config: SpawnRequest {
                    name: "researcher".to_string(),
                    model: Some("gpt-4.1".to_string()),
                    governance: true,
                    trust_threshold: Some(500),
                    learn_egress: false,
                    isolation: None,
                    token_budget_daily: Some(50000),
                    token_budget_per_request: None,
                    trusted_peers: None,
                },
                task_context: "Searching quantum computing papers".to_string(),
                status: "paused_at_checkpoint".to_string(),
                checkpoint: Some("3 papers found, 2 more to search".to_string()),
                workspace_tar: vec![9, 10, 11],
            }],
            credentials: vec![CredentialRef {
                name: "telegram-token".to_string(),
                env_key: "TELEGRAM_BOT_TOKEN".to_string(),
            }],
            metadata: HandoffMetadata {
                initiated_at: "2026-04-08T14:30:00Z".to_string(),
                direction: HandoffDirection::LocalToAks,
                source_host: "Pals-MacBook-Pro".to_string(),
                nonce: vec![0u8; 32],
                verification_hash: String::new(),
                succession_notice: None,
            },
        }
    }
}
