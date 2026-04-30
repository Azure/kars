// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `azureclaw-a2a-core` — shared A2A 1.0.0 primitives.
//!
//! ## Why this crate exists (Phase 2 S3.5 — ADR-0001 #4)
//!
//! Originally these modules lived under
//! `azureclaw-inference-router/src/a2a/`. With the introduction of a
//! public-ingress edge (`a2a-gateway`), both the gateway *and* the
//! router need to verify inbound JWS-signed `AgentCard`s using
//! identical, byte-for-byte semantics. A second copy of the verifier
//! would be a security smell: divergence between the two parsers is
//! exactly the class of bug §0.2 #8 ("don't roll our own crypto /
//! framing") rules out.
//!
//! Therefore: the verifier, signer, AgentCard schema, the JWS
//! signing-input builder, and the A2A error catalogue have been
//! lifted into this library-only crate. The router re-exports the
//! same items under `crate::a2a::*` so existing call sites are
//! unaffected; the gateway depends on this crate directly.
//!
//! ## Surface
//!
//! - [`signature`] — RFC 7515 `protected.payload` signing-input
//!   builder + base64url helpers.
//! - [`agent_card`] — A2A 1.0.0 `AgentCard` schema (§5.5) +
//!   `AgentCardSignature`.
//! - [`card_signing`] — Ed25519 / EdDSA sign + verify over the
//!   AgentCard manifest (§4.4.7), with the `alg = "EdDSA"` allow-list.
//! - [`card_verifier`] — inbound caller-card pin-by-thumbprint
//!   verifier with replay protection.
//! - [`error`] — A2A application error codes (§3.3.2).
//!
//! ## Security invariant
//!
//! `forbid(unsafe_code)` is enforced at the crate root. No module
//! below may introduce `unsafe` blocks. The JWS path is the public
//! edge's only authentication boundary, so memory-safety of the
//! parser is non-negotiable.

#![forbid(unsafe_code)]
#![allow(
    clippy::collapsible_if,
    clippy::redundant_guards,
    clippy::needless_borrows_for_generic_args,
    clippy::match_like_matches_macro,
    clippy::await_holding_lock,
    clippy::unnecessary_unwrap
)]

pub mod agent_card;
pub mod card_signing;
pub mod card_verifier;
pub mod error;
pub mod signature;

pub use agent_card::{
    A2A_PROTOCOL_VERSION, AgentCapabilities, AgentCard, AgentCardSignature, AgentExtension,
    AgentInterface, AgentProvider, AgentSkill, ProtocolBinding,
};
pub use card_signing::{CardSignError, TrustedKeys, sign_card, verify_card};
pub use card_verifier::{
    CardVerifierConfig, CardVerifyError, VerifiedCallerIdentity, verify_inbound_card,
};
pub use error::{A2aError, A2aErrorCode};
pub use signature::{
    SignatureError, SignatureInput, base64url_decode, base64url_encode, build_signing_input,
};
