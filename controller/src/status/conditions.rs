//! Standardised K8s Condition helpers for AzureClaw CRD status subresources.
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
// today by the ClawSandbox reconciler; `Progressing`, `Degraded`, and the
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
}

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
        _ => new_condition(type_, status_value, reason_value, message, observed_generation),
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
        let prior = new_condition(TYPE_READY, status::FALSE, reason::CREATING, "booting", Some(1));
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
            new_condition(TYPE_READY, status::FALSE, reason::CREATING, "start", Some(1)),
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
