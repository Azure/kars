// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! A2A 1.0.0 — Agent2Agent protocol implementation.
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
//! ## Status: implemented
//!
//! The PR series `phase1/a2a-*` and `phase1/ap2-*` landed the full
//! card-discovery + JSON-RPC + AP2 mandate pipeline:
//!
//! - [`agent_card`] / [`signature`] / [`card_signing`] — RFC 7515 JWS +
//!   RFC 8037 EdDSA over AgentCards (spec §4.4.7).
//! - [`card_server`] — `/.well-known/agent.json` builder.
//! - [`card_verifier`] — inbound caller-card pin-by-thumbprint verifier.
//! - [`trust_store`] — A2A peer-card public-key store with hot reload.
//! - [`jsonrpc_dispatch`] — JSON-RPC 2.0 binding for `message/send`,
//!   `tasks/get`, `tasks/cancel`, with [`InMemoryTaskStore`] and
//!   [`OsRngTaskIdMinter`].
//! - [`ap2`] — AP2 IntentMandate / CartMandate / PaymentMandate
//!   evaluation against a [`MandateLedger`].
//! - [`mandate_signing`] / [`mandate_trust_store`] — Ed25519 sign +
//!   verify for AP2 mandates.
//! - [`message_send_ap2`] — `message/send` glue that consults AP2
//!   trust + ledger before forwarding to the task store.
//! - [`agent_projection`] — `A2aAgent` CRD → trust-anchor projection.
//! - [`snapshot_rebuild`] — trust-store snapshot rebuild orchestrator.
//!
//! Production routes live in [`crate::routes::a2a`] (`GET
//! /.well-known/agent.json`, `POST /a2a`).
//!
//! ## Security posture
//!
//! Per §0.2 #8 (no rolling our own crypto / framing / wire format):
//!
//! - Ed25519 signing → `ed25519-dalek` workspace dep (existing,
//!   allow-listed in `ci/no-custom-crypto.sh`).
//! - Base64url → `base64` crate `URL_SAFE_NO_PAD` engine.
//! - JSON serialisation → `serde_json` (no hand-rolled tokenizer).
//! - JWS framing → done by hand against RFC 7515, but the only
//!   bytes-level work is `protected || '.' || payload` concatenation.
//!   Every signed/verified path has round-trip + tampering tests.
//!
//! The wire-format newtype pattern from `mcp::streamable_http::SessionId`
//! is reused: structured construction is fallible, raw bytes never bypass
//! validation.
//!
//! ## Spec citations
//!
//! - A2A 1.0.0 specification:
//!   <https://a2a-protocol.org/v1.0.0/specification>
//! - AP2 specification: <https://a2a-protocol.org/ap2/v1.0.0>
//! - RFC 7515 (JWS): <https://www.rfc-editor.org/rfc/rfc7515>
//! - RFC 8037 (JOSE EdDSA): <https://www.rfc-editor.org/rfc/rfc8037>

#![forbid(unsafe_code)]

// Lifted to the shared `azureclaw-a2a-core` crate in Phase 2 S3.5
// (ADR-0001 #4). Re-exported here under the original module paths so
// every existing `crate::a2a::signature::*` / `crate::a2a::card_signing::*`
// call site keeps compiling unchanged. The router and the new
// public-edge `a2a-gateway` now share a single verifier implementation.
pub use azureclaw_a2a_core::{agent_card, card_signing, card_verifier, error, signature};

pub mod agent_projection;
pub mod ap2;
pub mod card_server;
pub mod jsonrpc_dispatch;
pub mod mandate_signing;
pub mod mandate_trust_store;
pub mod message_send_ap2;
pub mod snapshot_rebuild;
pub mod trust_store;

pub use agent_card::{
    A2A_PROTOCOL_VERSION, AgentCapabilities, AgentCard, AgentCardSignature, AgentExtension,
    AgentInterface, AgentProvider, AgentSkill, ProtocolBinding,
};
pub use agent_projection::{
    A2aAgentSigningKeySpec, A2aAgentSpec, ProjectionError, project_anchors,
};
pub use ap2::{
    Ap2Denial, COUNTERPARTY_WILDCARD, DAILY_WINDOW_SECS, InMemoryMandateLedger, IntentMandate,
    MONTHLY_WINDOW_SECS, MandateLedger, MandateLedgerMut, PaymentAttempt, PaymentRecord,
    validate_payment_attempt, validate_payment_attempt_signed,
};
pub use card_server::{AgentCardConfig, CardServerError, build_card, build_signed_card};
pub use card_signing::{CardSignError, TrustedKeys, sign_card, verify_card};
pub use card_verifier::{
    CardVerifierConfig, CardVerifyError, VerifiedCallerIdentity, verify_inbound_card,
};
pub use error::{A2aError, A2aErrorCode};
pub use jsonrpc_dispatch::{
    CounterTaskIdMinter, InMemoryTaskStore, Message, MessageSendParams, OsRngTaskIdMinter,
    StoreError, Task, TaskIdMinter, TaskState, TaskStore, TasksCancelParams, TasksGetParams,
    handle_message_send, handle_tasks_cancel, handle_tasks_get,
};
pub use mandate_signing::{
    MandateSignError, TrustedKeys as MandateTrustedKeys, sign_mandate, verify_mandate,
};
pub use mandate_trust_store::{
    MandateTrustStore, MandateTrustStoreSnapshot, MandateTrustStoreSnapshotView,
};
pub use signature::{
    SignatureError, SignatureInput, base64url_decode, base64url_encode, build_signing_input,
};
pub use snapshot_rebuild::{RebuildIssue, RebuildOutcome, rebuild_snapshot};
pub use trust_store::{
    AnchorSource, TrustAnchor, TrustStore, TrustStoreBuildError, TrustStoreBuilder,
    TrustStoreSnapshot,
};
