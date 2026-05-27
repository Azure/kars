// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standardised K8s Condition helpers for Kars CRD status subresources.
//!
//! **Why a helper module, not inline `json!({...})` in the reconciler.**
//! K8s Conditions have strict semantics:
//!
//! * `type` must be a `PascalCase` well-known or domain-specific name
//!   ([KEP-1623][1]).
//! * `status` is the literal string `"True"` / `"False"` / `"Unknown"`.
//! * `lastTransitionTime` must only change when `status` transitions —
//!   updating it on every reconcile defeats the purpose of tracking
//!   transitions and creates unnecessary watch churn.
//! * `observedGeneration` points at the CR `metadata.generation` that
//!   produced this condition; consumers compare against
//!   `metadata.generation` to detect "controller has not yet seen the
//!   latest spec".
//! * `reason` is `PascalCase`, short, machine-readable.
//! * `message` is human-readable and may change freely.
//!
//! Getting any of these wrong makes `kubectl wait --for=condition=Ready`
//! misbehave and makes status unreliable as a signal to operators. This
//! module centralises the rules so every reconciler produces spec-compliant
//! conditions.
//!
//! We use [`k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition`]
//! directly rather than redefining a local type — per `docs/implementation-
//! plan.md` §0.2 #8 we consume published crates for standard wire types and
//! do not hand-roll them.
//!
//! [1]: https://github.com/kubernetes/enhancements/tree/master/keps/sig-api-machinery/1623-standardize-conditions

// The constants and helpers here define shared vocabulary for every
// reconciler that writes conditions. `Ready` + `Reconciled` are consumed
// today by the KarsSandbox reconciler; `Progressing`, `Degraded`, and the
// remaining `reason::*` values are the pinned vocabulary for upcoming
// reconcilers (McpServer, ToolPolicy, etc., per plan §7). Locking them in
// now prevents each future PR from reinventing slightly-different names.
// Unit tests below exercise every item so the surface is still verified.
#![allow(dead_code)]

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use k8s_openapi::jiff::Timestamp;

/// Well-known condition type: the CR has reached its desired state and is
/// serving traffic. Absence or `status=False` means not ready.
pub const TYPE_READY: &str = "Ready";

/// Well-known condition type: the controller is actively working toward the
/// desired state. `status=True` means in-progress; `status=False` with
/// `Ready=True` means reconcile has settled.
pub const TYPE_PROGRESSING: &str = "Progressing";

/// Well-known condition type: something prevents reconciliation (bad spec,
/// missing dependency, quota, etc.). `status=True` means degraded.
pub const TYPE_DEGRADED: &str = "Degraded";

/// Well-known condition type (Phase 2 S8): controller has *intentionally
/// stopped driving* a sub-resource — e.g. `OverlayMode` skips Pod
/// creation because an upstream `Sandbox` CR owns the Pod. `status=True`
/// means suspended; `status=False` means actively reconciling.
pub const TYPE_SUSPENDED: &str = "Suspended";

/// Well-known condition type (Phase 2 S10): the runtime declared in
/// `spec.runtime.kind` is implemented by the controller and its adapter
/// is wired up. `status=True/Reconciled` means the runtime adapter is
/// present and the Pod was templated for that runtime. `status=False`
/// with reason `AdapterMissing` means the controller parsed the
/// runtime kind but no adapter is wired (e.g. `OpenAIAgents` /
/// `MicrosoftAgentFramework` before S10.A3/A4 land); the controller
/// will *not* create a Deployment in that case to avoid silently
/// running the wrong runtime image.
pub const TYPE_RUNTIME_READY: &str = "RuntimeReady";

/// Phase 2 S12.e — `AllowlistVerified`: the controller fetched the
/// signed OCI artifact referenced by
/// `spec.networkPolicy.allowlistRef`, verified its cosign signature
/// against the cluster `SignerPolicy` (S12.d), and re-validated the
/// canonical-form rules from `docs/internal/policy-canonical-format.md`.
///
/// Emitted whenever `allowlistRef` is set on the CR. The S12.b
/// `KARS_FEATURE_SIGNED_ALLOWLIST` env gate was lifted in S12.e —
/// the verification path is always-on once an operator opts in by
/// populating `allowlistRef`.
///
/// `status=True/Verified` means the artifact is current and trusted.
/// `status=False/<reason>` carries the failure category — see
/// [`super::super::policy_fetcher::reason_for_error`] for the mapping.
pub const TYPE_ALLOWLIST_VERIFIED: &str = "AllowlistVerified";

/// Phase 2 S12.e — `AllowlistAuthoritative`: the controller derived the
/// live NetworkPolicy egress endpoints from the verified canonical
/// artifact (`status=True`) rather than from inline `allowedEndpoints`
/// or a stale last-known-good cache (`status=False`).
///
/// Reasons:
/// - `Verified` (status=True) — current reconcile used freshly verified
///   artifact bytes.
/// - `Inline` (status=False) — no `allowlistRef` set; controller fell
///   back to inline `allowedEndpoints` (the S12.b legacy path).
/// - `StaleLKG` (status=False) — `allowlistRef` set but verify failed;
///   controller is preserving the last-known-good endpoints from a
///   prior successful reconcile. Pair with `AllowlistVerified=False`
///   for the verify-failure reason.
/// - `FailedClosed` (status=False) — `allowlistRef` set, verify failed,
///   no LKG available (first reconcile after CR create or controller
///   restart). The controller did **not** write user-defined egress;
///   the sandbox is restricted to the always-allowed defaults.
pub const TYPE_ALLOWLIST_AUTHORITATIVE: &str = "AllowlistAuthoritative";

/// Phase 2 S12.e — `AllowlistDrift`: the CR has both `allowlistRef` and
/// non-empty inline `allowedEndpoints`, and the inline list differs from
/// the artifact-derived list. Informational — the artifact wins
/// (authoritative); operators should clean up the inline field.
///
/// Reasons:
/// - `InlineDiffersFromArtifact` (status=True) — drift observed.
/// - `InlineCleared` (status=False) — inline was just emptied;
///   condition kept visible briefly (≤2 reconciles) so operators see
///   the resolution before it disappears from status.
pub const TYPE_ALLOWLIST_DRIFT: &str = "AllowlistDrift";

/// `status` canonical values.
pub mod status {
    pub const TRUE: &str = "True";
    pub const FALSE: &str = "False";
    pub const UNKNOWN: &str = "Unknown";
}

/// Common `reason` values. `PascalCase`, short.
pub mod reason {
    pub const RECONCILING: &str = "Reconciling";
    pub const RECONCILED: &str = "Reconciled";
    pub const CREATING: &str = "Creating";
    pub const CREATED: &str = "Created";
    pub const FAILED: &str = "Failed";
    pub const SPEC_INVALID: &str = "SpecInvalid";
    pub const DEPENDENCY_MISSING: &str = "DependencyMissing";
    pub const TIMED_OUT: &str = "TimedOut";
    /// Phase 2 S8 — `OverlayMode`: operator's upstream `Sandbox` CR
    /// owns the Pod; Kars provides the governance overlay only.
    pub const OVERLAY_MODE: &str = "OverlayMode";
    /// Phase 2 S10 — `AdapterMissing`: the runtime declared in
    /// `spec.runtime.kind` is recognised by the CRD but the controller
    /// has no adapter wired (e.g. `OpenAIAgents` before S10.A3,
    /// `MicrosoftAgentFramework` before S10.A4). The controller refuses
    /// to create a Deployment rather than silently fall through to the
    /// OpenClaw image.
    pub const ADAPTER_MISSING: &str = "AdapterMissing";
    /// Phase G P1 #4 — `SuspendedBySpec`: the operator set
    /// `spec.suspended: true`. Reconciler scales the Deployment to
    /// `replicas: 0` while preserving namespace + governance overlay.
    pub const SUSPENDED_BY_SPEC: &str = "SuspendedBySpec";
    /// Phase G P1 #4 — `Active`: paired with `Suspended=False`. Used
    /// only to clear a prior `Suspended=True/SuspendedBySpec` so
    /// operators see the un-suspend transition; never stamped on CRs
    /// that have not previously been suspended.
    pub const ACTIVE: &str = "Active";
    /// Phase 2 S12.b — `Verified`: signed allowlist artifact fetched,
    /// cosign signature passed, signer identity matched cluster
    /// SignerPolicy, canonical form re-validated.
    pub const VERIFIED: &str = "Verified";
    /// Phase 2 S13 — `InferencePolicyNotFound`: the
    /// `KarsSandbox.spec.inferenceRef.name` did not resolve to an
    /// `InferencePolicy` CR in the sandbox's namespace. Same-namespace
    /// constraint is enforced; cross-namespace lookups are not allowed.
    pub const INFERENCE_POLICY_NOT_FOUND: &str = "InferencePolicyNotFound";
    /// Phase 2 S13 — `ToolPolicyNotFound`: governance is enabled and
    /// `spec.governance.toolPolicyRef.name` did not resolve to a
    /// `ToolPolicy` CR in the sandbox's namespace.
    pub const TOOL_POLICY_NOT_FOUND: &str = "ToolPolicyNotFound";
    /// Phase 2 S12.e — `Inline`: `AllowlistAuthoritative=False` reason
    /// for sandboxes without `allowlistRef`. Inline `allowedEndpoints`
    /// is the (legacy) source of truth.
    pub const INLINE: &str = "Inline";
    /// Phase 2 S12.e — `StaleLKG`: verify failed but the controller is
    /// preserving the last-known-good endpoints from a prior reconcile.
    pub const STALE_LKG: &str = "StaleLKG";
    /// Phase 2 S12.e — `FailedClosed`: verify failed and no LKG was
    /// cached; the controller refused to write user-defined egress.
    pub const FAILED_CLOSED: &str = "FailedClosed";
    /// Phase 2 S12.e — `InlineDiffersFromArtifact`: drift detected
    /// between inline `allowedEndpoints` and the verified artifact.
    pub const INLINE_DIFFERS_FROM_ARTIFACT: &str = "InlineDiffersFromArtifact";
    /// Phase 2 S12.e — `InlineCleared`: drift just resolved; condition
    /// kept visible briefly so operators see the cleanup before it is
    /// dropped from status.
    pub const INLINE_CLEARED: &str = "InlineCleared";
    /// Phase 2 S5 — `AwaitingFoundryProvisioning`: the controller has
    /// successfully compiled and published the binding ConfigMap, but
    /// the upstream Azure AI Foundry Memory Store is created by the
    /// runtime path (CLI plugin / router proxy) on first use, not by
    /// the controller. Until the runtime confirms the upstream store
    /// exists, we cannot honestly report `Ready=True`.
    pub const AWAITING_FOUNDRY_PROVISIONING: &str = "AwaitingFoundryProvisioning";
    /// `crd-well-oiled-machine` Slice 0 — `AwaitingRouterEnforcement`:
    /// the controller has compiled the spec and published the
    /// artifact ConfigMap, but the router has not yet echoed back the
    /// loaded digest (or, for today, the router does not yet consume
    /// this CRD kind at all — InferencePolicy in Slice 0, McpServer
    /// plural in Slice 4, etc.). Until the data-plane confirmation
    /// closure of principles.md §3 is wired, the controller emits
    /// `Ready=False` / reason=`AwaitingRouterEnforcement` and stamps
    /// `phase=Compiled`. Each slice that wires its router-side
    /// informer deletes the corresponding call site (§5 "delete on
    /// contact").
    pub const AWAITING_ROUTER_ENFORCEMENT: &str = "AwaitingRouterEnforcement";

    /// The data-plane router has confirmed it loaded the exact policy
    /// digest the controller published. This closes the principles.md
    /// §3 invariant ("Ready ⇔ router echo") for ToolPolicy's AGT
    /// profile. Slice 1c is the first user; later slices reuse it for
    /// InferencePolicy, KarsMemory, and McpServer plural.
    pub const ROUTER_ENFORCING: &str = "RouterEnforcing";

    /// A ToolPolicy with `spec.agtProfile.inline` set has no
    /// referencing `KarsSandbox` — no router exists to confirm
    /// enforcement. The controller stamps `phase=Compiled` with
    /// this reason rather than `Ready` because there is no consumer
    /// to honor the policy yet. As soon as a sandbox references the
    /// policy, the reconciler retries and (on success) promotes to
    /// `Ready` / `RouterEnforcing`.
    pub const NO_SANDBOXES_REFERENCING: &str = "NoSandboxesReferencing";

    /// `crd-well-oiled-machine` Slice 3b.4 — `AuthMisconfigured`: at
    /// least one referencing sandbox's router reported an upstream
    /// authentication failure while consuming the compiled policy
    /// (today: Foundry Memory Store returning 403 on the
    /// `foundry.memory` MCP tool). This is *not* a transient network
    /// error; it indicates a misconfigured project-MI or wrong
    /// `Azure AI User` role assignment (see the
    /// `kars-deployment` skill notes on the project-MI
    /// gotcha). The controller stamps `Ready=False` / `Degraded=True`
    /// with this reason so operators don't waste time chasing
    /// transient digest mismatches when the real problem is RBAC.
    ///
    /// Wire contract: the router records auth failures via
    /// `PolicyStatusRegistry::record_error` with an
    /// `AuthMisconfigured:` prefix on the message; the controller
    /// matches that exact prefix on the `last_error` returned in
    /// `/internal/policy-status` to elevate the condition.
    pub const AUTH_MISCONFIGURED: &str = "AuthMisconfigured";

    /// `crd-well-oiled-machine` Slice 3b.5 — `MemoryStoreMissing`: at
    /// least one referencing sandbox's router observed an HTTP 404
    /// from the upstream Foundry Memory Store on a
    /// `foundry.memory.{search,update,...}` call. The store the
    /// compiled `KarsMemory` binding points at does not exist (yet)
    /// on the Foundry side. Today the openclaw runtime lazily
    /// auto-creates stores via `ensureMemoryStore` on first sync, so
    /// 404 is operator-visible up to the first runtime sync. Slice
    /// 3c (router-side auto-provision at binding install) eliminates
    /// the 404 path entirely.
    ///
    /// Wire contract: the router records 404s via
    /// `PolicyStatusRegistry::record_error` with a
    /// `MemoryStoreMissing:` prefix on the message; the controller
    /// matches the exact prefix to elevate `Degraded=True`.
    ///
    /// Precedence: `AuthMisconfigured` outranks `MemoryStoreMissing`
    /// — a 403 means the operator can't even check whether the
    /// store exists, so RBAC dominates.
    pub const MEMORY_STORE_MISSING: &str = "MemoryStoreMissing";

    /// `crd-well-oiled-machine` Slice 4d.1 — `McpSingularDeprecated`:
    /// the KarsSandbox uses `spec.governance.mcpServerRef` (singular),
    /// which is superseded by `spec.governance.mcpServerRefs` (plural).
    /// The singular path keeps working in Slice 4d.1 (one-to-one
    /// alias), but operators should migrate. Emitted as a Warning
    /// event, not a Degraded condition — the sandbox still reconciles
    /// to Ready. Slice 4d.2 wires the per-server file scheme that the
    /// plural form unlocks; Slice 4-final removes the singular field.
    pub const MCP_SINGULAR_DEPRECATED: &str = "McpSingularDeprecated";

    /// `crd-well-oiled-machine` Slice 5c.2 — `Unsigned`:
    /// `AllowlistVerified=False` reason for sandboxes that use
    /// `spec.networkPolicy.allowedEndpoints` (inline) without a
    /// signed `spec.networkPolicy.allowlistRef`. Default behaviour
    /// is *allow with warning* — the sandbox still reconciles to
    /// `Ready=True` and the inline endpoints are programmed into the
    /// L4 NetworkPolicy and the L7 router allowlist mount. When the
    /// controller is configured with `REQUIRE_SIGNED_ALLOWLIST=true`
    /// (helm value `egress.requireSigned: true`), the resolver
    /// fail-closes instead: endpoints = None,
    /// `AllowlistAuthoritative=False/FailedClosed`,
    /// `fail_closed_no_lkg = true`, and the reconciler elevates
    /// `Degraded=True/Unsigned`.
    pub const UNSIGNED: &str = "Unsigned";
}

/// Slice 3b.4 wire-contract prefix routers attach to
/// `PolicyStatusRegistry::record_error` messages when the upstream
/// (Foundry Memory Store, today) rejected auth. The controller scans
/// for this prefix to elevate the Degraded condition with
/// `reason=AuthMisconfigured`. Kept here in `conditions` so producer
/// and consumer share one source of truth.
pub const AUTH_MISCONFIGURED_PREFIX: &str = "AuthMisconfigured:";

/// Slice 3b.5 wire-contract prefix routers attach to
/// `PolicyStatusRegistry::record_error` messages when the upstream
/// Foundry Memory Store returned HTTP 404 for the bound store. The
/// controller scans for this prefix to elevate the Degraded
/// condition with `reason=MemoryStoreMissing`. Lives next to
/// `AUTH_MISCONFIGURED_PREFIX` so producer and consumer share one
/// source of truth.
pub const MEMORY_STORE_MISSING_PREFIX: &str = "MemoryStoreMissing:";

/// Build a condition with a freshly-stamped `lastTransitionTime`.
///
/// **When to call:** on the *first* time a condition enters a given
/// `status` value, or when merging with a prior condition whose status
/// differs from the new one. If the prior condition already has the same
/// `status`, prefer [`preserve_transition_time`] to avoid churning the
/// timestamp on every reconcile.
pub fn new_condition(
    type_: &str,
    status_value: &str,
    reason_value: &str,
    message: &str,
    observed_generation: Option<i64>,
) -> Condition {
    Condition {
        type_: type_.to_string(),
        status: status_value.to_string(),
        reason: reason_value.to_string(),
        message: message.to_string(),
        last_transition_time: Time(Timestamp::now()),
        observed_generation,
    }
}

/// Return a condition of the given `type_`, reusing the prior condition's
/// `last_transition_time` iff the prior condition's `status` matches the
/// new `status_value`. If the status differs (or no prior exists) a fresh
/// timestamp is stamped.
///
/// This is the helper reconcilers should call on every pass — it
/// automatically distinguishes "I just transitioned" from "I'm still
/// Ready, nothing changed".
pub fn preserve_transition_time(
    prior: Option<&Condition>,
    type_: &str,
    status_value: &str,
    reason_value: &str,
    message: &str,
    observed_generation: Option<i64>,
) -> Condition {
    match prior {
        Some(p) if p.type_ == type_ && p.status == status_value => Condition {
            type_: type_.to_string(),
            status: status_value.to_string(),
            reason: reason_value.to_string(),
            message: message.to_string(),
            last_transition_time: p.last_transition_time.clone(),
            observed_generation,
        },
        _ => {
            // Status flipped (or no prior) — record the transition so
            // operators can alert on flap rate without parsing CR yaml.
            crate::metrics::record_condition_transition(type_, status_value);
            new_condition(
                type_,
                status_value,
                reason_value,
                message,
                observed_generation,
            )
        }
    }
}

/// Upsert `c` into `conditions` in-place, matched by `type_`.
///
/// K8s convention: at most one condition per `type_`. If a prior exists
/// it's replaced; otherwise appended.
pub fn set(conditions: &mut Vec<Condition>, c: Condition) {
    if let Some(slot) = conditions.iter_mut().find(|e| e.type_ == c.type_) {
        *slot = c;
    } else {
        conditions.push(c);
    }
}

/// Look up a condition by `type_`.
pub fn find<'a>(conditions: &'a [Condition], type_: &str) -> Option<&'a Condition> {
    conditions.iter().find(|c| c.type_ == type_)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_constants_are_pascal_case() {
        for t in [TYPE_READY, TYPE_PROGRESSING, TYPE_DEGRADED] {
            assert!(t.chars().next().unwrap().is_uppercase());
            assert!(!t.contains('_'));
            assert!(!t.contains('-'));
            assert!(!t.contains(' '));
        }
    }

    #[test]
    fn status_constants_match_k8s_canonical_values() {
        assert_eq!(status::TRUE, "True");
        assert_eq!(status::FALSE, "False");
        assert_eq!(status::UNKNOWN, "Unknown");
    }

    #[test]
    fn reason_constants_are_pascal_case() {
        for r in [
            reason::RECONCILING,
            reason::RECONCILED,
            reason::CREATING,
            reason::CREATED,
            reason::FAILED,
            reason::SPEC_INVALID,
            reason::DEPENDENCY_MISSING,
            reason::TIMED_OUT,
            reason::INFERENCE_POLICY_NOT_FOUND,
            reason::TOOL_POLICY_NOT_FOUND,
        ] {
            assert!(r.chars().next().unwrap().is_uppercase(), "{r}");
            assert!(!r.contains('_'), "{r}");
            assert!(!r.contains(' '), "{r}");
        }
    }

    #[test]
    fn new_condition_populates_all_fields() {
        let c = new_condition(
            TYPE_READY,
            status::TRUE,
            reason::RECONCILED,
            "sandbox is serving",
            Some(7),
        );
        assert_eq!(c.type_, "Ready");
        assert_eq!(c.status, "True");
        assert_eq!(c.reason, "Reconciled");
        assert_eq!(c.message, "sandbox is serving");
        assert_eq!(c.observed_generation, Some(7));
    }

    #[test]
    fn preserve_reuses_timestamp_when_status_unchanged() {
        let prior = new_condition(TYPE_READY, status::TRUE, reason::RECONCILED, "ok", Some(3));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let next = preserve_transition_time(
            Some(&prior),
            TYPE_READY,
            status::TRUE,
            reason::RECONCILED,
            "still ok",
            Some(4),
        );
        assert_eq!(next.last_transition_time, prior.last_transition_time);
        assert_eq!(next.message, "still ok");
        assert_eq!(next.observed_generation, Some(4));
    }

    #[test]
    fn preserve_stamps_new_timestamp_when_status_flips() {
        let prior = new_condition(
            TYPE_READY,
            status::FALSE,
            reason::CREATING,
            "booting",
            Some(1),
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        let next = preserve_transition_time(
            Some(&prior),
            TYPE_READY,
            status::TRUE,
            reason::RECONCILED,
            "ready",
            Some(1),
        );
        assert_ne!(next.last_transition_time, prior.last_transition_time);
        assert!(next.last_transition_time.0 > prior.last_transition_time.0);
    }

    #[test]
    fn preserve_stamps_timestamp_when_prior_is_none() {
        let next = preserve_transition_time(
            None,
            TYPE_PROGRESSING,
            status::TRUE,
            reason::RECONCILING,
            "starting",
            Some(2),
        );
        assert_eq!(next.type_, "Progressing");
        assert_eq!(next.status, "True");
    }

    #[test]
    fn preserve_stamps_timestamp_when_prior_type_differs() {
        let prior = new_condition(TYPE_DEGRADED, status::TRUE, reason::FAILED, "x", Some(1));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let next = preserve_transition_time(
            Some(&prior),
            TYPE_READY,
            status::TRUE,
            reason::RECONCILED,
            "ready",
            Some(1),
        );
        assert_ne!(next.last_transition_time, prior.last_transition_time);
    }

    #[test]
    fn set_upserts_by_type() {
        let mut v: Vec<Condition> = vec![];
        set(
            &mut v,
            new_condition(
                TYPE_READY,
                status::FALSE,
                reason::CREATING,
                "start",
                Some(1),
            ),
        );
        set(
            &mut v,
            new_condition(
                TYPE_PROGRESSING,
                status::TRUE,
                reason::RECONCILING,
                "go",
                Some(1),
            ),
        );
        assert_eq!(v.len(), 2);

        set(
            &mut v,
            new_condition(TYPE_READY, status::TRUE, reason::RECONCILED, "ok", Some(1)),
        );
        assert_eq!(v.len(), 2, "upsert must replace, not append");
        assert_eq!(find(&v, TYPE_READY).unwrap().status, "True");
    }

    #[test]
    fn find_returns_none_for_missing_type() {
        let v = vec![new_condition(
            TYPE_READY,
            status::TRUE,
            reason::RECONCILED,
            "ok",
            Some(1),
        )];
        assert!(find(&v, TYPE_DEGRADED).is_none());
        assert_eq!(find(&v, TYPE_READY).unwrap().message, "ok");
    }

    #[test]
    fn observed_generation_propagates_from_none() {
        let c = new_condition(TYPE_READY, status::TRUE, reason::RECONCILED, "ok", None);
        assert_eq!(c.observed_generation, None);
    }
}
