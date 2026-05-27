// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `MeshProvider` contract — **plugin-side**. No Rust implementation
//! exists in the router and **none should**.
//!
//! ## Why this trait is here, but never `impl`'d in the router
//!
//! The router is a **proxy** for mesh traffic, not a Signal-protocol
//! participant. End-to-end encryption between agents is performed in the
//! sandbox by the **agent** (today: OpenClaw + the TypeScript
//! `mesh-plugin/` + the Microsoft AGT SDK via `@kars/mesh`). Keys
//! live with the agent; the router only sees opaque ciphertext over the
//! relay WebSocket.
//!
//! Concretely, the router's mesh role is:
//! 1. Forward the relay WebSocket (`forward_proxy` + `routes/mesh.rs`).
//! 2. Forward registry HTTPS calls (prekey upload, peer lookup).
//! 3. Apply policy + audit + trust hooks around (1) and (2).
//!
//! Steps (1)–(2) are pure transport; step (3) is delivered by the
//! existing `PolicyDecisionProvider` + `AuditSink` seams already wired
//! into `AppState`. There is no Signal/X3DH/Double-Ratchet code in the
//! router, and adding any would be a category error: it would force the
//! router to handle key material it has no business holding.
//!
//! ## Why we still ship the trait file
//!
//! 1. **Documentation of the contract** that *some* mesh participant
//!    must satisfy. This is the surface a future native-Rust agent (if
//!    one ever lands) would have to implement; it is also the surface
//!    that the current TypeScript SDK satisfies in spirit.
//! 2. **Conformance corpus shape** (internal Phase 1 plan
//!    §Conformance) — the libsignal-derived test vectors that exercise
//!    KNOCK / X3DH / Double-Ratchet are organised against this trait so
//!    the same fixtures can be re-run against a future Rust impl.
//! 3. **Cross-language parity check** — when the TS SDK or AGT's mesh
//!    layer ships a Rust binding, this trait is what we'll verify the
//!    binding against.
//!
//! ## Router-side seams (the real four)
//!
//! In the router there are **three** four-seam contracts that have real
//! impls:
//!
//! * [`crate::providers::PolicyDecisionProvider`] — `decide(request)`.
//! * [`crate::providers::AuditSink`]              — `append(event)`.
//! * [`crate::providers::SigningProvider`]        — `sign(key_ref, payload)`.
//!
//! Mesh is the fourth contract conceptually, but it lives **outside** the
//! router. The plan's "four-seam" language refers to the system as a
//! whole; the router owns three of those four seams.
//!
//! ## Implementations (theoretical, plugin-side)
//!
//! - `VendoredAgentMeshProvider` (TypeScript today, in `mesh-plugin/`).
//! - `AgtMeshProvider` lands when AGT delivers their AgentMesh relay/
//!   registry; will also be plugin-side.
//! - `NullMeshProvider` exists only as a conformance fixture target.
//!
//! Key custody: peer identity keys and ratchet state live inside the
//! plugin. The router cannot satisfy this contract because by design it
//! has no key material.

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
