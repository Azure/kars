//! CRD status helpers shared across reconcilers.
//!
//! Everything here is pure logic — no K8s client calls. Reconcilers call
//! these helpers to produce status-patch payloads; the reconciler owns the
//! `patch_status` call. Isolating status construction here keeps reconciler
//! bodies short and gives a single place to audit the wire format of
//! everything we write into `.status`.

pub mod conditions;

use crate::crd::ClawSandbox;
use kube::ResourceExt;
use serde_json::{Value, json};

/// Build the `status` patch for a `ClawSandbox` that has reached the
/// Running phase. Includes `observedGeneration` (per KEP-1623 status
/// semantics) and a Ready=True condition whose `lastTransitionTime` is
/// preserved across same-status reconciles.
///
/// **Why here, not inline in the reconciler:** status construction has
/// rules (condition timestamps, observedGeneration propagation,
/// foundryAgentId preservation) that are easy to get subtly wrong.
/// Centralising the logic keeps reconcile bodies focused on side-effects
/// and gives us one place to unit-test the wire shape.
pub fn build_running_status_patch(sandbox: &ClawSandbox, sandbox_ns: &str) -> Value {
    let name = sandbox.name_any();
    let generation = sandbox.metadata.generation;
    let prior_conditions = sandbox
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);
    let ready = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_READY),
        conditions::TYPE_READY,
        conditions::status::TRUE,
        conditions::reason::RECONCILED,
        "sandbox reconciled",
        generation,
    );

    let mut status_obj = json!({
        "status": {
            "phase": "Running",
            "namespace": sandbox_ns,
            "sandboxPod": format!("{name}-*"),
            "inferenceEndpoint": "https://azureclaw-inference-router.azureclaw-system.svc.cluster.local:8443",
            "pendingApprovals": 0,
            "observedGeneration": generation,
            "conditions": [ready],
        }
    });
    if let Some(existing) = sandbox.status.as_ref()
        && let Some(agent_id) = existing.foundry_agent_id.as_ref()
    {
        status_obj["status"]["foundryAgentId"] = json!(agent_id);
    }
    status_obj
}

/// Returns `true` when the existing CR status already encodes the same
/// "Running" reconciliation outcome that [`build_running_status_patch`]
/// would produce — meaning a `patch_status` call would be a no-op
/// semantically but would still bump `metadata.resourceVersion` and
/// re-trigger the watch.
///
/// **Why this matters:** kube-apiserver bumps `resourceVersion` on every
/// PATCH against the `.status` subresource regardless of whether the
/// patch changes any bytes. Without an idempotency guard the reconciler
/// observes its own status writes, re-runs reconcile, patches status
/// again, and so on. We have observed 7 reconciles in 12 seconds at
/// startup with concomitant Graph API throttling on the federated
/// credential creation path. Skipping the write when the desired status
/// already matches reality breaks that loop.
///
/// We intentionally only check the fields that
/// [`build_running_status_patch`] writes (plus the `Ready` condition
/// status), and accept that fields owned by other writers (e.g. a future
/// `tokensUsed` updater) may differ — those would not be touched by our
/// merge patch anyway.
pub fn running_status_matches(sandbox: &ClawSandbox, sandbox_ns: &str) -> bool {
    use crate::status::conditions::{TYPE_READY, status::TRUE as STATUS_TRUE};

    let Some(status) = sandbox.status.as_ref() else {
        return false;
    };
    if status.phase.as_deref() != Some("Running") {
        return false;
    }
    if status.namespace.as_deref() != Some(sandbox_ns) {
        return false;
    }
    if status.observed_generation != sandbox.metadata.generation {
        return false;
    }
    let ready_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_READY)
        .is_some_and(|c| c.status == STATUS_TRUE);
    if !ready_ok {
        return false;
    }
    true
}

/// Build the `status` patch for a `ClawSandbox` that has failed spec
/// validation or an early reconcile check. Stamps `observedGeneration`
/// and a `Degraded=True` / `Ready=False` condition pair so `kubectl wait
/// --for=condition=Ready` and `--for=condition=Degraded` both behave
/// correctly, and so operators see *why* we stopped reconciling.
///
/// **Why this exists:** without it, a CR that fails validation (empty
/// model, invalid isolation, bad name) sits at `status.phase = ""` for
/// 300s with no condition at all — indistinguishable from a controller
/// that hasn't seen the CR yet. KEP-1623 §Conditions explicitly calls
/// this out as a bug.
pub fn build_degraded_status_patch(
    sandbox: &ClawSandbox,
    reason_value: &'static str,
    message: &str,
) -> Value {
    let generation = sandbox.metadata.generation;
    let prior_conditions = sandbox
        .status
        .as_ref()
        .map(|s| s.conditions.as_slice())
        .unwrap_or(&[]);
    let degraded = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_DEGRADED),
        conditions::TYPE_DEGRADED,
        conditions::status::TRUE,
        reason_value,
        message,
        generation,
    );
    let not_ready = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_READY),
        conditions::TYPE_READY,
        conditions::status::FALSE,
        reason_value,
        message,
        generation,
    );
    json!({
        "status": {
            "phase": "Degraded",
            "observedGeneration": generation,
            "conditions": [degraded, not_ready],
        }
    })
}

/// Patch `.status` on `name` with a `Degraded=True` / `Ready=False`
/// condition pair. Used by early-exit validation failures so operators
/// see *why* we stopped reconciling instead of an empty status. Failures
/// to patch are logged but non-fatal — the reconciler still returns the
/// originally-intended `Action`.
pub async fn stamp_degraded(
    client: &kube::Client,
    sandbox: &ClawSandbox,
    name: &str,
    reason_value: &'static str,
    message: &str,
) {
    use kube::{
        Api, ResourceExt,
        api::{Patch, PatchParams},
    };
    let sandbox_api: Api<ClawSandbox> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let patch = build_degraded_status_patch(sandbox, reason_value, message);
    if let Err(e) = sandbox_api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(patch))
        .await
    {
        tracing::warn!(sandbox = %name, error = %e, "failed to stamp Degraded status");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{ClawSandbox, ClawSandboxSpec, ClawSandboxStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn new_sandbox(generation: Option<i64>, status: Option<ClawSandboxStatus>) -> ClawSandbox {
        ClawSandbox {
            metadata: ObjectMeta {
                name: Some("demo".into()),
                namespace: Some("azureclaw-demo".into()),
                generation,
                ..Default::default()
            },
            spec: ClawSandboxSpec::default(),
            status,
        }
    }

    #[test]
    fn running_patch_emits_generation_and_ready_condition() {
        let sb = new_sandbox(Some(7), None);
        let patch = build_running_status_patch(&sb, "azureclaw-demo");
        let st = &patch["status"];
        assert_eq!(st["phase"], "Running");
        assert_eq!(st["observedGeneration"], 7);
        let conds = st["conditions"].as_array().expect("conditions array");
        assert_eq!(conds.len(), 1);
        assert_eq!(conds[0]["type"], "Ready");
        assert_eq!(conds[0]["status"], "True");
        assert_eq!(conds[0]["reason"], "Reconciled");
        assert_eq!(conds[0]["observedGeneration"], 7);
    }

    #[test]
    fn running_patch_preserves_foundry_agent_id() {
        let prior = ClawSandboxStatus {
            foundry_agent_id: Some("asst-abc".into()),
            ..Default::default()
        };
        let sb = new_sandbox(Some(3), Some(prior));
        let patch = build_running_status_patch(&sb, "azureclaw-demo");
        assert_eq!(patch["status"]["foundryAgentId"], "asst-abc");
    }

    #[test]
    fn running_patch_reuses_ready_transition_time() {
        let existing_ready = conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::RECONCILED,
            "ok",
            Some(1),
        );
        let prior_ts = existing_ready.last_transition_time.clone();
        let prior = ClawSandboxStatus {
            conditions: vec![existing_ready],
            ..Default::default()
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        let sb = new_sandbox(Some(2), Some(prior));
        let patch = build_running_status_patch(&sb, "azureclaw-demo");
        let emitted_ts = patch["status"]["conditions"][0]["lastTransitionTime"]
            .as_str()
            .expect("timestamp must be stringified");
        // Timestamps serialize as RFC3339; ensure format unchanged == preserved.
        let prior_ts_str = serde_json::to_value(&prior_ts).unwrap();
        assert_eq!(emitted_ts, prior_ts_str.as_str().unwrap());
    }

    #[test]
    fn running_patch_emits_null_observed_generation_when_metadata_missing() {
        let sb = new_sandbox(None, None);
        let patch = build_running_status_patch(&sb, "azureclaw-demo");
        assert!(patch["status"]["observedGeneration"].is_null());
    }

    #[test]
    fn running_status_matches_returns_false_when_status_missing() {
        let sb = new_sandbox(Some(1), None);
        assert!(!running_status_matches(&sb, "azureclaw-demo"));
    }

    #[test]
    fn running_status_matches_returns_false_when_phase_differs() {
        let prior = ClawSandboxStatus {
            phase: Some("Pending".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(!running_status_matches(&sb, "azureclaw-demo"));
    }

    #[test]
    fn running_status_matches_returns_false_when_namespace_differs() {
        let prior = ClawSandboxStatus {
            phase: Some("Running".into()),
            namespace: Some("azureclaw-other".into()),
            observed_generation: Some(1),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(!running_status_matches(&sb, "azureclaw-demo"));
    }

    #[test]
    fn running_status_matches_returns_false_when_generation_stale() {
        let prior = ClawSandboxStatus {
            phase: Some("Running".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(2), Some(prior));
        assert!(!running_status_matches(&sb, "azureclaw-demo"));
    }

    #[test]
    fn running_status_matches_returns_false_when_ready_false() {
        let prior = ClawSandboxStatus {
            phase: Some("Running".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::FALSE,
                conditions::reason::FAILED,
                "boom",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(!running_status_matches(&sb, "azureclaw-demo"));
    }

    #[test]
    fn running_status_matches_returns_true_for_settled_status() {
        let prior = ClawSandboxStatus {
            phase: Some("Running".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::RECONCILED,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(running_status_matches(&sb, "azureclaw-demo"));
    }

    #[test]
    fn degraded_patch_stamps_degraded_true_and_ready_false() {
        let sb = new_sandbox(Some(9), None);
        let patch = build_degraded_status_patch(
            &sb,
            conditions::reason::SPEC_INVALID,
            "empty inference.model",
        );
        let st = &patch["status"];
        assert_eq!(st["phase"], "Degraded");
        assert_eq!(st["observedGeneration"], 9);
        let conds = st["conditions"].as_array().expect("conditions array");
        assert_eq!(conds.len(), 2);
        let degraded = conds
            .iter()
            .find(|c| c["type"] == "Degraded")
            .expect("Degraded cond");
        assert_eq!(degraded["status"], "True");
        assert_eq!(degraded["reason"], "SpecInvalid");
        assert_eq!(degraded["observedGeneration"], 9);
        let ready = conds
            .iter()
            .find(|c| c["type"] == "Ready")
            .expect("Ready cond");
        assert_eq!(ready["status"], "False");
        assert_eq!(ready["reason"], "SpecInvalid");
        assert_eq!(ready["observedGeneration"], 9);
    }

    #[test]
    fn degraded_patch_preserves_transition_time_on_repeat() {
        let prior_degraded = conditions::new_condition(
            conditions::TYPE_DEGRADED,
            conditions::status::TRUE,
            conditions::reason::SPEC_INVALID,
            "bad spec",
            Some(1),
        );
        let prior_ts = prior_degraded.last_transition_time.clone();
        let prior = ClawSandboxStatus {
            conditions: vec![prior_degraded],
            ..Default::default()
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        let sb = new_sandbox(Some(2), Some(prior));
        let patch =
            build_degraded_status_patch(&sb, conditions::reason::SPEC_INVALID, "still bad spec");
        let degraded_ts = patch["status"]["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "Degraded")
            .unwrap()["lastTransitionTime"]
            .as_str()
            .unwrap()
            .to_string();
        let prior_ts_str = serde_json::to_value(&prior_ts).unwrap();
        assert_eq!(degraded_ts, prior_ts_str.as_str().unwrap());
    }

    #[test]
    fn degraded_patch_handles_missing_generation() {
        let sb = new_sandbox(None, None);
        let patch =
            build_degraded_status_patch(&sb, conditions::reason::SPEC_INVALID, "no generation");
        assert!(patch["status"]["observedGeneration"].is_null());
        let degraded = patch["status"]["conditions"][0].clone();
        assert!(degraded["observedGeneration"].is_null());
    }
}
