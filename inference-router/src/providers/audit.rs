//! `AuditSink` contract.
//!
//! Responsibility: `append(event) -> ReceiptId`. The router appends a
//! tamper-evident entry for every policy-relevant operation; the only thing
//! we persist in CR status is the opaque `ReceiptId`. The Merkle chain and
//! proof retrieval live inside the implementation.
//!
//! Implementations (Phase 1):
//! - `VendoredAuditSink` — current `audit.rs` in router (hash-chained log).
//! - `AgtAuditSink` — shipped AGT Rust SDK.
//! - `NullAuditSink` — dev-only; admission rejects in prod.
//!
//! See `docs/implementation-plan.md` §1.2.

/// Opaque receipt. `ReceiptId` is the only thing the router persists to CR
/// status — it's treated as a black box outside this module.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReceiptId(pub String);

/// Minimal event shape. Implementations MAY add their own fields; callers
/// should treat the serialised form as opaque.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    /// Monotonic millisecond epoch captured by the caller.
    pub timestamp_ms: u64,
    /// Principal — same format as `PolicyRequest.principal`.
    pub principal: String,
    /// Action identifier (e.g., `tool.invoke`, `policy.deny`).
    pub action: String,
    /// Sha256 of the request/response body.
    pub payload_digest_hex: String,
    /// Verdict encoded as a compact enum string (`allow` / `deny:reason` / …).
    pub verdict: String,
    /// Free-form key-value labels.
    pub labels: Vec<(String, String)>,
}

/// Returned alongside the receipt. Implementations that chain events
/// provide the previous and current hashes so observers can rebuild the
/// chain without re-calling the provider.
#[derive(Debug, Clone)]
pub struct AuditReceipt {
    pub id: ReceiptId,
    /// Optional: previous entry's hash. `None` for the genesis entry.
    pub prev_hash_hex: Option<String>,
    /// Sha256 of the canonical serialisation of this entry.
    pub entry_hash_hex: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit backend unreachable: {0}")]
    Unreachable(String),
    #[error("queue full (backpressure)")]
    QueueFull,
    #[error("internal provider error: {0}")]
    Internal(String),
}

#[async_trait::async_trait]
pub trait AuditSink: Send + Sync {
    /// Append a single event. Implementations MUST be idempotent on
    /// duplicate calls with identical `(timestamp_ms, principal, action,
    /// payload_digest_hex)` tuples — the caller may retry on `Unreachable`.
    ///
    /// Under `OutageMode::Strict` the caller fails the request on any error
    /// from this method. Under `CachedRead` / `DegradedDev` the caller may
    /// continue and retry out-of-band.
    async fn append(&self, event: AuditEvent) -> Result<AuditReceipt, AuditError>;

    /// Fetch an entry back by receipt id. Optional: implementations that
    /// don't support lookup (e.g., fire-and-forget remote sinks) return
    /// `Ok(None)`.
    async fn get(&self, id: &ReceiptId) -> Result<Option<AuditEvent>, AuditError>;
}
