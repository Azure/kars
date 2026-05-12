// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Canonical `.status.phase` strings + helpers to emit the matching
//! Warning Event when a reconciler stamps a decorative phase.
//!
//! ## Why this module exists
//!
//! Per `docs/internal/crd-well-oiled-machine/principles.md` §3 the
//! contract is:
//!
//! - **`Ready`** — the data plane (router or sandbox-side informer) has
//!   confirmed it is enforcing the spec.
//! - **`Compiled`** — the controller parsed the spec and wrote a
//!   ConfigMap, but the router has not echoed the digest yet (or, for
//!   today, the router does not yet consume this CRD kind at all).
//!
//! Three reconcilers historically stamped `Ready` while the data plane
//! ignored the body (`InferencePolicy`'s `tokenBudget` /
//! `contentSafety` / `modelPreference`, `ClawMemory`'s binding) or
//! stamped `Pending` forever with a TODO comment. Each was a lie to the
//! user: `kubectl wait --for=condition=Ready` returned green for a
//! policy that was doing nothing.
//!
//! This module is the single place that owns:
//!
//! 1. The phase-string vocabulary the rest of the operator must use.
//! 2. The `PolicyNotEnforced` Warning Event format. Whenever a
//!    reconciler stamps `Compiled` because the router does not yet
//!    consume the spec, it also publishes a Warning Event so the
//!    explanation is visible in `kubectl describe`.
//!
//! ## Why a `Reporter` wrapper, not raw `kube::runtime::events`
//!
//! Three reasons:
//!
//! - Reconcilers already take a `Ctx { client }`; threading a
//!   `Recorder` per reconciler avoids constructing one per reconcile
//!   call (which would mean a new HTTP client + UUID each time).
//! - Tests can pass a no-op reporter without faking the K8s API.
//! - The `controller` / `instance` fields are uniform across every
//!   reconciler — Phase 2 §10.4 #1 (field-manager-per-reconciler)
//!   gives us provenance, but the Event reporter must still identify
//!   the operator pod as the source.

use k8s_openapi::api::core::v1::ObjectReference;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::{Client, Resource};

/// `.status.phase = "Pending"` — controller has accepted the CR but
/// has not yet produced a compiled artifact (e.g. waiting on an admit
/// step, or finalizer cleanup in progress).
///
/// No reconciler in Slice 0 stamps this value (every CR goes
/// straight from "no status" → `Compiled` or `Failed`), but the
/// constant is part of the public phase vocabulary documented in
/// `docs/api/lifecycle.md` and consumed by the Headlamp plugin's
/// `phaseToStatus()`. Future reconcilers (e.g. async finalizer flows)
/// will need to emit it.
#[allow(dead_code)]
pub const PHASE_PENDING: &str = "Pending";

/// `.status.phase = "Compiled"` — the controller parsed the spec,
/// wrote the ConfigMap, but the router has not yet confirmed it is
/// enforcing the compiled artifact. The user's `kubectl wait
/// --for=condition=Ready` must block here.
///
/// Introduced in Slice 0 of `crd-well-oiled-machine`. Replaces the
/// historical practice of stamping `Ready` for unwired CRDs and the
/// `Pending`-forever apology in `ClawMemory`.
pub const PHASE_COMPILED: &str = "Compiled";

/// `.status.phase = "Ready"` — the data plane has confirmed it is
/// enforcing the compiled artifact. For Slice 0 only `ToolPolicy` and
/// `McpServer` (singular) and `A2AAgent` may stamp this. The remaining
/// reconcilers stamp `Compiled` until their slice lands.
pub const PHASE_READY: &str = "Ready";

/// `.status.phase = "Degraded"` — the spec is valid but some
/// dependency is failing. Reconciler should still attempt progress on
/// the next requeue.
pub const PHASE_DEGRADED: &str = "Degraded";

/// `.status.phase = "Failed"` — the spec itself or a hard prerequisite
/// is wrong; reconciler will not converge without a spec change.
pub const PHASE_FAILED: &str = "Failed";

/// Canonical Warning Event reason for "controller compiled the spec
/// but the router does not yet consume it." Whatever slice eventually
/// wires the consumer must delete the corresponding
/// [`PhaseEventReporter::warn_policy_not_enforced`] call site as part
/// of its PR (principles.md §5: delete on contact).
pub const REASON_POLICY_NOT_ENFORCED: &str = "PolicyNotEnforced";

/// Canonical Warning Event reason for "this CRD kind ships as singular
/// today; plural support arrives in a later slice." Distinct from
/// `PolicyNotEnforced` because `McpServer` *is* enforced — there is
/// just a sandbox-side capacity cap of one.
pub const REASON_LIMITED_SUPPORT: &str = "LimitedSupport";

/// Default reporter identity. The pod name is filled from
/// `CONTROLLER_POD_NAME` (set in the controller Deployment) when
/// available; otherwise we fall back to a static identifier so events
/// still publish during local `cargo test` runs.
const REPORTER_CONTROLLER: &str = "azureclaw-controller";

/// Thin wrapper around [`kube::runtime::events::Recorder`].
///
/// Constructed once per reconciler at start-up (placed in each
/// reconciler's `Ctx`) and cloned cheaply per reconcile call.
#[derive(Clone)]
pub struct PhaseEventReporter {
    recorder: Recorder,
}

impl PhaseEventReporter {
    /// Build a reporter for a named reconciler. The `reconciler_name`
    /// shows up in the Event's `reportingController` so operators can
    /// disambiguate which reconciler stamped the event.
    pub fn new(client: Client, reconciler_name: &str) -> Self {
        let instance = std::env::var("CONTROLLER_POD_NAME").ok();
        let reporter = Reporter {
            controller: format!("{REPORTER_CONTROLLER}/{reconciler_name}"),
            instance,
        };
        Self {
            recorder: Recorder::new(client, reporter),
        }
    }

    /// Publish a `Warning` Event explaining that the CR is in the
    /// `Compiled` phase because the router does not yet consume the
    /// spec.
    ///
    /// `note` is the human-readable message — keep it ≤1 KiB; we
    /// truncate defensively below the kube-runtime limit. `action`
    /// shows up in event JSON (not `kubectl describe`) and identifies
    /// the operation that produced the outcome.
    ///
    /// Returns the underlying `kube::Error` on publish failure;
    /// reconcilers should `tracing::warn!` and continue rather than
    /// fail the reconcile — the status is the source of truth, the
    /// event is just user-visible context.
    pub async fn warn_policy_not_enforced<R>(
        &self,
        cr: &R,
        action: &str,
        note: impl Into<String>,
    ) -> Result<(), kube::Error>
    where
        R: Resource<DynamicType = ()>,
    {
        self.publish_warning(cr, REASON_POLICY_NOT_ENFORCED, action, note.into())
            .await
    }

    /// Publish a `Warning` Event explaining that the CR is supported
    /// only in a limited fashion today (e.g. singular `McpServer`
    /// where the user might expect plural support). Distinct from
    /// `warn_policy_not_enforced` so operators can grep events by
    /// reason.
    pub async fn warn_limited_support<R>(
        &self,
        cr: &R,
        action: &str,
        note: impl Into<String>,
    ) -> Result<(), kube::Error>
    where
        R: Resource<DynamicType = ()>,
    {
        self.publish_warning(cr, REASON_LIMITED_SUPPORT, action, note.into())
            .await
    }

    async fn publish_warning<R>(
        &self,
        cr: &R,
        reason: &str,
        action: &str,
        mut note: String,
    ) -> Result<(), kube::Error>
    where
        R: Resource<DynamicType = ()>,
    {
        // kube-runtime caps `note` at 1 KiB; truncate defensively so
        // we never emit a stricter-than-doc'd error.
        if note.len() > 1024 {
            note.truncate(1024);
        }
        let event = Event {
            type_: EventType::Warning,
            reason: reason.to_string(),
            note: Some(note),
            action: action.to_string(),
            secondary: None,
        };
        let reference = object_ref_for(cr);
        self.recorder.publish(&event, &reference).await
    }
}

/// Build the [`ObjectReference`] consumed by
/// [`kube::runtime::events::Recorder::publish`].
///
/// Pulled out as a free function so unit tests can verify the shape
/// without a live Recorder.
pub fn object_ref_for<R>(cr: &R) -> ObjectReference
where
    R: Resource<DynamicType = ()>,
{
    use kube::ResourceExt;
    ObjectReference {
        api_version: Some(R::api_version(&()).into_owned()),
        kind: Some(R::kind(&()).into_owned()),
        name: Some(cr.name_any()),
        namespace: cr.namespace(),
        uid: cr.meta().uid.clone(),
        resource_version: cr.meta().resource_version.clone(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claw_memory::ClawMemory;
    use crate::inference_policy::InferencePolicy;
    use crate::mcp_server::McpServer;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    #[test]
    fn phase_constants_are_pascal_case() {
        for s in [
            PHASE_PENDING,
            PHASE_COMPILED,
            PHASE_READY,
            PHASE_DEGRADED,
            PHASE_FAILED,
        ] {
            assert!(
                s.chars().next().is_some_and(|c| c.is_uppercase()),
                "phase {s} must be PascalCase"
            );
            assert!(
                !s.contains('_') && !s.contains('-') && !s.contains(' '),
                "phase {s} must be one PascalCase word"
            );
        }
    }

    #[test]
    fn compiled_phase_is_distinct_from_ready_and_pending() {
        // Regression guard: a refactor that aliases these constants
        // would silently re-introduce the "Ready means nothing" bug.
        assert_ne!(PHASE_COMPILED, PHASE_READY);
        assert_ne!(PHASE_COMPILED, PHASE_PENDING);
    }

    #[test]
    fn reason_constants_match_documented_format() {
        // Reasons must be PascalCase per K8s Events convention.
        for s in [REASON_POLICY_NOT_ENFORCED, REASON_LIMITED_SUPPORT] {
            assert!(s.chars().next().is_some_and(|c| c.is_uppercase()));
            assert!(s.chars().all(|c| c.is_ascii_alphabetic()));
        }
    }

    fn cm_fixture() -> ClawMemory {
        ClawMemory {
            metadata: ObjectMeta {
                name: Some("mem-1".into()),
                namespace: Some("azureclaw-test".into()),
                uid: Some("00000000-0000-0000-0000-000000000001".into()),
                resource_version: Some("42".into()),
                ..Default::default()
            },
            spec: Default::default(),
            status: None,
        }
    }

    #[test]
    fn object_ref_for_clawmemory_carries_identifiers() {
        let r = object_ref_for(&cm_fixture());
        assert_eq!(r.kind.as_deref(), Some("ClawMemory"));
        assert_eq!(r.name.as_deref(), Some("mem-1"));
        assert_eq!(r.namespace.as_deref(), Some("azureclaw-test"));
        assert_eq!(
            r.uid.as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(r.resource_version.as_deref(), Some("42"));
        let av = r.api_version.unwrap();
        assert!(av.starts_with("azureclaw.azure.com/"), "got {av}");
    }

    #[test]
    fn object_ref_for_inferencepolicy_uses_correct_kind() {
        let p = InferencePolicy {
            metadata: ObjectMeta {
                name: Some("p-1".into()),
                namespace: Some("ns".into()),
                ..Default::default()
            },
            spec: Default::default(),
            status: None,
        };
        let r = object_ref_for(&p);
        assert_eq!(r.kind.as_deref(), Some("InferencePolicy"));
        assert_eq!(r.name.as_deref(), Some("p-1"));
        assert_eq!(r.namespace.as_deref(), Some("ns"));
    }

    #[test]
    fn object_ref_for_mcpserver_uses_correct_kind() {
        let m = McpServer {
            metadata: ObjectMeta {
                name: Some("m-1".into()),
                namespace: Some("ns".into()),
                ..Default::default()
            },
            spec: Default::default(),
            status: None,
        };
        let r = object_ref_for(&m);
        assert_eq!(r.kind.as_deref(), Some("McpServer"));
    }
}
