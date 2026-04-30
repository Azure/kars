// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `PolicyDecisionProvider` contract.
//!
//! Responsibility: `decide(request) -> verdict`. A single synchronous call
//! evaluates whether a given principal may perform a given tool call on a
//! given payload in a given context.
//!
//! Implementations (Phase 1):
//! - `VendoredPolicyDecisionProvider` — today's `governance.rs` code path.
//!   `PolicyEngine` + `TrustManager` + `RateLimiter` + `BehaviorMonitor`.
//! - `AgtPolicyDecisionProvider` — wraps the shipped AGT Rust SDK.
//! - `NullPolicyDecisionProvider` — dev-only; defaults to fail-closed
//!   (admission-policy rejects in prod).
//!
//! See internal Phase 1 plan §1.2 and §1.4.

use std::time::Duration;

/// Canonical policy request. The router builds this from the live HTTP
/// request context. Payload is a sha256 digest, not the raw body.
#[derive(Debug, Clone)]
pub struct PolicyRequest {
    /// Principal making the request — typically `agent://…` or `user://…`.
    pub principal: String,
    /// Tool name (e.g., `foundry.chat`, `mcp.fs.read`, `a2a.invoke`).
    pub tool: String,
    /// Sha256 of the request body, hex-encoded.
    pub payload_digest_hex: String,
    /// Free-form contextual labels; implementations may consult or ignore.
    pub context: Vec<(String, String)>,
}

/// Verdict emitted by a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// Allowed unconditionally.
    Allow,
    /// Allowed but caller must attach the given labels to the response
    /// (e.g., content-safety warnings, trust-tier annotations).
    AllowWithLabels(Vec<(String, String)>),
    /// Denied; implementations SHOULD provide a short human-readable reason.
    Deny { reason: String },
    /// Needs out-of-band approval; `ttl` is how long to hold the request.
    NeedsApproval { approver: String, ttl: Duration },
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("policy backend unreachable: {0}")]
    Unreachable(String),
    #[error("malformed request: {0}")]
    Malformed(String),
    #[error("internal provider error: {0}")]
    Internal(String),
}

#[async_trait::async_trait]
pub trait PolicyDecisionProvider: Send + Sync {
    /// Evaluate a single request. Implementations MUST be side-effect-free
    /// on the caller's behalf; side effects (audit append, counter increment)
    /// belong in downstream contracts.
    ///
    /// Under `OutageMode::Strict` an `Unreachable` error MUST translate to
    /// `Deny` at the call site — the provider itself returns the error so
    /// the policy layer has a chance to log and react.
    async fn decide(&self, request: PolicyRequest) -> Result<PolicyVerdict, PolicyError>;
}
