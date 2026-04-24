//! Provider contracts for AzureClaw.
//!
//! ## Router-side seams (three real ones)
//!
//! Three contracts have real router-side implementations and are wired
//! through `AppState`:
//!
//! * [`PolicyDecisionProvider`]  — `decide(request) -> verdict`.
//! * [`AuditSink`]               — `append(event) -> ReceiptId`.
//! * [`SigningProvider`]         — `sign(key_ref, payload) -> Signature`.
//!
//! Each is implemented in-tree as `impl <Trait> for Governance`; the
//! same `Arc<Governance>` coerces into `Arc<dyn Trait>` for each, so
//! `policy_provider`, `audit_sink`, and `signing_provider` on `AppState`
//! are three views of the same instance, not three separate pieces of
//! state. AGT-SDK-backed alternates land in `providers/agt/*` and are
//! tenant-flag selected.
//!
//! ## The fourth seam ([`MeshProvider`]) lives in the plugin, not here
//!
//! The router is a proxy for mesh traffic — it forwards the relay
//! WebSocket and registry HTTPS calls, and applies policy/audit hooks
//! around them, but it never sees cleartext and holds no keys. Signal /
//! X3DH / Double-Ratchet runs in the **agent** (TypeScript `mesh-plugin/`
//! plus vendored `@agentmesh/sdk` today). The `MeshProvider` trait file
//! ships here as documentation of the cross-language contract and as the
//! shape the conformance corpus targets, but it has **no router-side
//! `impl` and should not get one**. See `providers/mesh.rs` for the
//! full rationale.
//!
//! ## Implementation phases
//!
//! Each contract has up to three concrete backends (Phase 1):
//!
//! * `In-tree` — implemented on `Governance`, today's behaviour.
//! * `Agt*`    — AGT SDK backed (policy/audit/signing now, mesh later).
//! * `Null*`   — dev-only; admission rejects in prod unless the manifest
//!   carries `azureclaw.azure.com/dev-only: "true"`.
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
