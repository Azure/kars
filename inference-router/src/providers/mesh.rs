//! `MeshProvider` contract.
//!
//! Responsibility: session establishment between two peers, E2E-encrypted
//! message send and receive, and relay/registry interaction. Hides the
//! Signal-protocol-vs-AGT-mesh choice from call sites.
//!
//! Implementations (Phase 1):
//! - `VendoredAgentMeshProvider` — wraps the current `vendor/agentmesh-sdk`
//!   client + `vendor/agentmesh-relay` + `vendor/agentmesh-registry` code path.
//! - `AgtMeshProvider` — lands when AGT's AgentMesh relay/registry ships
//!   (`docs/implementation-plan.md` §1.5).
//! - `NullMeshProvider` — dev/test; `open_session` always errors.
//!
//! Key custody: peer identity keys and ratchet state live inside the
//! implementation. Callers never see raw key material.
//!
//! Phase 0 note: this file defines the contract only. Call-sites in
//! `router/src/mesh.rs`, `router/src/handoff.rs`, and
//! `controller/src/mesh_peer.rs` continue to use the vendored path
//! directly until Phase 1.

use std::fmt;

/// Canonical agent identifier as emitted by the registry
/// (e.g., `agent://tenant/name@key-fingerprint`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Opaque session handle scoped to a `MeshProvider` instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

/// Errors that cross the provider boundary. Implementations translate
/// protocol-specific errors (libsodium / registry 401 / relay closed)
/// into these canonical variants.
#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("registry unreachable: {0}")]
    RegistryUnreachable(String),
    #[error("relay unreachable: {0}")]
    RelayUnreachable(String),
    #[error("peer not registered: {0}")]
    UnknownPeer(PeerId),
    #[error("session not established: {0:?}")]
    NoSession(SessionId),
    #[error("decryption failed (likely ratchet drift or key mismatch)")]
    DecryptionFailed,
    #[error("policy denied session establishment: {0}")]
    PolicyDenied(String),
    #[error("internal provider error: {0}")]
    Internal(String),
}

/// Result of [`MeshProvider::send`]. The relay acknowledges receipt, not
/// delivery to the peer — recipient-side acks are out of band.
#[derive(Debug, Clone)]
pub struct SendResult {
    pub session_id: SessionId,
    pub relay_message_id: String,
}

/// The full mesh contract. All methods are `async` and `Send` so they
/// can run inside the router's tokio runtime.
#[async_trait::async_trait]
pub trait MeshProvider: Send + Sync {
    /// Register `self` with the registry and upload prekeys.
    /// Idempotent — safe to call on reconnect.
    async fn register(&self) -> Result<PeerId, MeshError>;

    /// Resolve a peer by display name (namespace-scoped). Returns the
    /// canonical `PeerId`, or `UnknownPeer` if not registered.
    async fn resolve(&self, display_name: &str) -> Result<PeerId, MeshError>;

    /// Establish (or reuse) a Signal/X3DH session with `peer`. Idempotent:
    /// if a session already exists the implementation returns it.
    async fn open_session(&self, peer: &PeerId) -> Result<SessionId, MeshError>;

    /// Send an opaque byte payload over the E2E tunnel. The payload is
    /// whatever the caller put in (typically a JSON envelope). The
    /// provider handles ratchet state and relay transport internally.
    async fn send(&self, session: &SessionId, payload: &[u8]) -> Result<SendResult, MeshError>;

    /// Install a handler for inbound decrypted messages on any session
    /// this provider owns. Only one handler may be registered at a time;
    /// calling twice replaces the previous one.
    async fn on_message(
        &self,
        handler: Box<dyn Fn(SessionId, Vec<u8>) + Send + Sync + 'static>,
    ) -> Result<(), MeshError>;

    /// Tear down a session and forget its ratchet state.
    async fn close_session(&self, session: &SessionId) -> Result<(), MeshError>;
}
