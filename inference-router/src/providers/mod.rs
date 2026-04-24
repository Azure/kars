//! Provider contracts for AzureClaw.
//!
//! Everything that crosses the AGT boundary goes through exactly four
//! contracts defined in this module:
//!
//! * [`MeshProvider`]            — session establishment, E2E send/receive.
//! * [`PolicyDecisionProvider`]  — `decide(request) -> verdict`.
//! * [`AuditSink`]               — `append(event) -> ReceiptId`.
//! * [`SigningProvider`]         — `sign(key_ref, payload) -> Signature`.
//!
//! Each contract will have three implementations (Phase 1):
//!
//! * `Vendored*` — current vendored-AgentMesh behaviour.
//! * `Agt*`      — AGT SDK backed (policy/audit/signing now, mesh later).
//! * `Null*`     — dev-only; admission rejects in prod unless the manifest
//!   carries `azureclaw.azure.com/dev-only: "true"`.
//!
//! **Phase 0 status:** contracts only. No implementations and no call-site
//! migrations land here. Provider construction, dispatch, and feature-flag
//! plumbing are Phase 1 per `docs/implementation-plan.md` §7.
//!
//! **Outage semantics** (§1.3):
//! * `Strict` (prod default) — fail-closed on AGT/Mesh down.
//! * `CachedRead` — allow cached < TTL else fail-closed.
//! * `DegradedDev` (`azureclaw dev` only) — fail-open with warning label.
//!
//! Every new implementation of any contract below MUST land with a
//! `docs/security-audits/YYYY-MM-DD-<slug>.md` covering the §0.2 #9 scope.

// Scaffolding for Phase 1 — see docs/implementation-plan.md §7. Dead-code
// lints are silenced at the module level until call-sites land.
#![allow(dead_code)]

pub mod audit;
pub mod audit_impl;
pub mod mesh;
pub mod outage;
pub mod policy;
pub mod policy_impl;
pub mod signing;
pub mod signing_impl;

pub use audit::{AuditError, AuditEvent, AuditReceipt, AuditSink, ReceiptId};
pub(crate) use audit_impl::now_ms as audit_now_ms;
pub use mesh::{MeshProvider, PeerId, SendResult, SessionId};
pub use outage::{
    CachedDecision, DEFAULT_CACHED_TTL, MAX_CACHED_TTL, OutageAction, OutageConfig,
    OutageConfigError, OutageMode, OutageParseError, decide_outage,
};
pub use policy::{PolicyDecisionProvider, PolicyRequest, PolicyVerdict};
pub use policy_impl::verdict_to_legacy_json;
pub use signing::{KeyRef, Signature, SigningError, SigningProvider};
pub(crate) use signing_impl::DEFAULT_KEY_REF;
// The in-tree `PolicyDecisionProvider` implementation lives in
// `policy_impl.rs` (`impl PolicyDecisionProvider for Governance`). No
// wrapper type exists for it. The word "vendored" is reserved for
// `/vendor/` (patched upstream forks). AGT-SDK-backed concrete impls
// will land in siblings under `providers/agt/` in a follow-up branch
// and do carry their own types because they have state of their own.

/// Selects which implementation of a contract a tenant uses.
/// See `docs/implementation-plan.md` §1.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Vendored,
    Agt,
    Null,
}
