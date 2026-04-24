//! A2A 1.0.0 agent-card scaffold (Agent2Agent protocol).
//!
//! Spec: <https://a2a-protocol.org/v1.0.0/specification>
//!
//! ## Module isolation (ADR-0001 D4)
//!
//! This module is structurally prohibited from importing concrete
//! credential-bearing types (`auth::ImdsToken`, `auth::FoundryCredentials`,
//! etc.). All policy / signing / audit calls go through traits in
//! `crate::providers::*`. The `forbid(unsafe_code)` attribute below
//! prevents any `unsafe` block from sneaking into the parser path.
//! `ci/a2a-module-isolation.sh` enforces the import constraint
//! mechanically.
//!
//! ## Status: scaffold (`phase1/a2a-1.0.0-scaffold`)
//!
//! This PR lands type-level + signature-envelope primitives only. **No
//! router routes are wired yet.** Subsequent branches add:
//!
//! - `phase1/a2a-1.0.0-routes` — `/.well-known/agent.json` discovery
//!   endpoint serving a per-sandbox card signed via `SigningProvider`.
//! - `phase1/a2a-1.0.0-verify` — inbound A2A request verification:
//!   pin the caller card by key thumbprint and reject mismatched JWS
//!   signatures.
//! - `phase1/a2a-1.0.0-jsonrpc-binding` — JSON-RPC 2.0 binding
//!   handlers for `message/send`, `tasks/get`, `tasks/cancel`.
//!
//! ## Submodules (this PR)
//!
//! - [`agent_card`] — Canonical [`AgentCard`] data model per spec
//!   §4.4. Serde-derived `serialize` produces the exact wire shape
//!   required by `/.well-known/agent.json`. All optional fields use
//!   `skip_serializing_if = "Option::is_none"` to match spec
//!   "field required: No → omit on absent" semantics.
//! - [`signature`] — JWS detached-content signature envelope per RFC
//!   7515. Builds the signing input (`protected || '.' || payload`),
//!   computes the EdDSA signature using the project's existing
//!   `ed25519-dalek` workspace dep, and base64url-encodes the result
//!   per spec §4.4.7. Verification is symmetric.
//! - [`error`] — A2A-specific error catalogue per spec §3.3.2 with
//!   structured details (`Code`, `Message`, optional `Details`).
//!
//! ## Security posture
//!
//! Per §0.2 #8 (no rolling our own crypto / framing / wire format):
//!
//! - Ed25519 signing → `ed25519-dalek` workspace dep (existing).
//! - Base64url → `base64` crate `URL_SAFE_NO_PAD` engine (existing).
//! - JSON serialisation → `serde_json` (no hand-rolled tokenizer).
//! - JWS framing → done by hand against RFC 7515, but the only
//!   bytes-level work is `protected || '.' || payload` concatenation.
//!   Every signed/verified path has round-trip + tampering tests.
//!
//! The wire-format newtype pattern from `mcp::streamable_http::SessionId`
//! is reused: structured construction is fallible, raw bytes never bypass
//! validation.
//!
//! ## Spec citation
//!
//! - A2A 1.0.0 specification:
//!   <https://a2a-protocol.org/v1.0.0/specification>
//! - RFC 7515 (JWS): <https://www.rfc-editor.org/rfc/rfc7515>
//! - RFC 8037 (JOSE EdDSA): <https://www.rfc-editor.org/rfc/rfc8037>

#![forbid(unsafe_code)]

pub mod agent_card;
pub mod error;
pub mod signature;

pub use agent_card::{
    A2A_PROTOCOL_VERSION, AgentCapabilities, AgentCard, AgentCardSignature, AgentExtension,
    AgentInterface, AgentProvider, AgentSkill, ProtocolBinding,
};
pub use error::{A2aError, A2aErrorCode};
pub use signature::{
    SignatureError, SignatureInput, base64url_decode, base64url_encode, build_signing_input,
};
