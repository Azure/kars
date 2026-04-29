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
/// `runtime_kind` is the value of `spec.runtime.kind` that was successfully
/// reconciled (e.g. `"OpenClaw"`); it is mirrored to `status.runtimeKind`
/// (printer-column source of truth) and is the `RuntimeReady` Condition's
/// implicit subject. Stamping `runtimeKind` and the `RuntimeReady`
/// Condition *inside* this patch (rather than via a separate
/// `patch_status` call) is deliberate: a follow-up patch with
/// `conditions: [Ready]` would *replace* the `conditions` array under
/// merge semantics and erase a freshly-stamped `RuntimeReady`. See plan
/// §S10.A1 rubber-duck #1.
///
/// **Why here, not inline in the reconciler:** status construction has
/// rules (condition timestamps, observedGeneration propagation,
/// foundryAgentId preservation) that are easy to get subtly wrong.
/// Centralising the logic keeps reconcile bodies focused on side-effects
/// and gives us one place to unit-test the wire shape.
pub fn build_running_status_patch(
    sandbox: &ClawSandbox,
    sandbox_ns: &str,
    runtime_kind: &str,
) -> Value {
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
    let runtime_ready = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_RUNTIME_READY),
        conditions::TYPE_RUNTIME_READY,
        conditions::status::TRUE,
        conditions::reason::RECONCILED,
        &format!("runtime adapter `{runtime_kind}` reconciled"),
        generation,
    );
    // Phase 2 S7.B: complete the Conditions matrix. Pre-S7.B the
    // Running status patch only stamped Ready + RuntimeReady, leaving
    // `Progressing` to be inferred from `Ready=True`. KEP-1623 §C and
    // operator UX expect every Condition type the controller writes
    // to be present on every reconcile so dashboards / kubectl wait
    // queries (`--for=condition=Progressing=False`) work consistently
    // across success / overlay / degraded / adapter-missing paths.
    let progressing = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_PROGRESSING),
        conditions::TYPE_PROGRESSING,
        conditions::status::FALSE,
        conditions::reason::RECONCILED,
        "sandbox reconciled; no further controller work pending",
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
            "runtimeKind": runtime_kind,
            "conditions": [ready, progressing, runtime_ready],
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
pub fn running_status_matches(sandbox: &ClawSandbox, sandbox_ns: &str, runtime_kind: &str) -> bool {
    use crate::status::conditions::{
        TYPE_PROGRESSING, TYPE_READY, TYPE_RUNTIME_READY,
        status::{FALSE as STATUS_FALSE, TRUE as STATUS_TRUE},
    };

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
    if status.runtime_kind.as_deref() != Some(runtime_kind) {
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
    // Phase 2 S7.B: the running shape now stamps Progressing=False
    // alongside Ready=True; verifying it here prevents an upgrade-time
    // status flap where a pre-S7.B controller's Ready-only status would
    // otherwise be considered a no-op match and the Progressing field
    // would never get back-filled.
    let progressing_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_PROGRESSING)
        .is_some_and(|c| c.status == STATUS_FALSE);
    if !progressing_ok {
        return false;
    }
    let runtime_ready_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_RUNTIME_READY)
        .is_some_and(|c| c.status == STATUS_TRUE);
    if !runtime_ready_ok {
        return false;
    }
    true
}

/// Build the `status` patch for a `ClawSandbox` running in
/// **`OverlayMode`** (Phase 2 S8). In this mode the operator's upstream
/// `Sandbox` CR owns the Pod lifecycle; AzureClaw skipped Deployment +
/// Service creation and only laid down the overlay (namespace, SA with
/// Workload-Identity binding, NetworkPolicy, governance ConfigMaps).
///
/// Status shape:
/// - `phase: "Overlay"` — distinct from `Running` so dashboards can
///   surface "this CR is intentionally not driving a Pod".
/// - `Ready=True, Reason=OverlayMode` — overlay reconciled cleanly
///   from AzureClaw's perspective; `kubectl wait --for=condition=Ready`
///   still works.
/// - `Progressing=False, Reason=OverlayMode` — no further AzureClaw work
///   pending.
/// - `Suspended=True, Reason=OverlayMode` — explicit signal that we are
///   not driving a Pod here (operators querying `Suspended` see why).
///
/// `sandbox_pod` is set to the upstream CR name (with a `upstream/`
/// prefix so it's unambiguous in `kubectl get clawsandbox`) so operators
/// can pivot from `kubectl describe clawsandbox` to the upstream object.
pub fn build_overlay_status_patch(
    sandbox: &ClawSandbox,
    sandbox_ns: &str,
    upstream_ref: &str,
    runtime_kind: &str,
) -> Value {
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
        conditions::reason::OVERLAY_MODE,
        "overlay reconciled; upstream Sandbox CR owns the Pod",
        generation,
    );
    let progressing = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_PROGRESSING),
        conditions::TYPE_PROGRESSING,
        conditions::status::FALSE,
        conditions::reason::OVERLAY_MODE,
        "no further controller work pending in overlay mode",
        generation,
    );
    let suspended = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_SUSPENDED),
        conditions::TYPE_SUSPENDED,
        conditions::status::TRUE,
        conditions::reason::OVERLAY_MODE,
        &format!("Pod owned by upstream Sandbox CR `{upstream_ref}`"),
        generation,
    );
    // In overlay mode, AzureClaw doesn't drive the Pod; the runtime
    // adapter is therefore not stamped True (we have not deployed it),
    // but we still record `runtimeKind` so the printer column matches the
    // user's intent and we surface a `RuntimeReady=False/OverlayMode`
    // Condition so consumers can distinguish "no Pod because overlay"
    // from "no Pod because adapter missing".
    let runtime_ready = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_RUNTIME_READY),
        conditions::TYPE_RUNTIME_READY,
        conditions::status::FALSE,
        conditions::reason::OVERLAY_MODE,
        &format!("runtime `{runtime_kind}` not driven by AzureClaw in overlay mode"),
        generation,
    );
    json!({
        "status": {
            "phase": "Overlay",
            "namespace": sandbox_ns,
            "sandboxPod": format!("upstream/{upstream_ref}"),
            "pendingApprovals": 0,
            "observedGeneration": generation,
            "runtimeKind": runtime_kind,
            "conditions": [ready, progressing, suspended, runtime_ready],
        }
    })
}

/// Returns `true` when the existing CR status already encodes the same
/// "Overlay" reconciliation outcome that [`build_overlay_status_patch`]
/// would produce. Same idempotency guard as
/// [`running_status_matches`].
#[must_use]
pub fn overlay_status_matches(
    sandbox: &ClawSandbox,
    sandbox_ns: &str,
    upstream_ref: &str,
    runtime_kind: &str,
) -> bool {
    use crate::status::conditions::{TYPE_READY, status::TRUE as STATUS_TRUE};

    let Some(status) = sandbox.status.as_ref() else {
        return false;
    };
    if status.phase.as_deref() != Some("Overlay") {
        return false;
    }
    if status.namespace.as_deref() != Some(sandbox_ns) {
        return false;
    }
    if status.observed_generation != sandbox.metadata.generation {
        return false;
    }
    if status.runtime_kind.as_deref() != Some(runtime_kind) {
        return false;
    }
    let expected_pod = format!("upstream/{upstream_ref}");
    if status.sandbox_pod.as_deref() != Some(expected_pod.as_str()) {
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
    // Phase 2 S7.B: Degraded path stamps `Progressing=False` so
    // `kubectl wait --for=condition=Progressing=False` resolves
    // identically across the success / overlay / degraded / adapter-
    // missing paths. The reason mirrors the degraded reason so
    // operators see the same "why" on both conditions.
    let not_progressing = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_PROGRESSING),
        conditions::TYPE_PROGRESSING,
        conditions::status::FALSE,
        reason_value,
        message,
        generation,
    );
    json!({
        "status": {
            "phase": "Degraded",
            "observedGeneration": generation,
            "conditions": [degraded, not_ready, not_progressing],
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

/// Build the `status` patch for a `ClawSandbox` whose `spec.runtime.kind`
/// has no controller-side adapter wired (S10.A1: `OpenAIAgents` /
/// `MicrosoftAgentFramework` / `BYO`). Stamps:
/// - `phase: Degraded`
/// - `runtimeKind: <kind>` so the printer column reflects user intent
/// - `Ready=False / Reason=AdapterMissing`
/// - `Degraded=True / Reason=AdapterMissing`
/// - `RuntimeReady=False / Reason=AdapterMissing`
///
/// Per plan §S10.A1 rubber-duck #2, falling through to
/// `ctx.sandbox_image` (the OpenClaw image) for these kinds would
/// silently run the wrong runtime; the controller refuses instead.
pub fn build_runtime_unsupported_status_patch(
    sandbox: &ClawSandbox,
    runtime_kind: &str,
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
        conditions::reason::ADAPTER_MISSING,
        message,
        generation,
    );
    let not_ready = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_READY),
        conditions::TYPE_READY,
        conditions::status::FALSE,
        conditions::reason::ADAPTER_MISSING,
        message,
        generation,
    );
    let runtime_not_ready = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_RUNTIME_READY),
        conditions::TYPE_RUNTIME_READY,
        conditions::status::FALSE,
        conditions::reason::ADAPTER_MISSING,
        message,
        generation,
    );
    // Phase 2 S7.B: complete the Conditions matrix on the adapter-
    // missing path too — see `build_running_status_patch` rationale.
    let not_progressing = conditions::preserve_transition_time(
        conditions::find(prior_conditions, conditions::TYPE_PROGRESSING),
        conditions::TYPE_PROGRESSING,
        conditions::status::FALSE,
        conditions::reason::ADAPTER_MISSING,
        message,
        generation,
    );
    json!({
        "status": {
            "phase": "Degraded",
            "observedGeneration": generation,
            "runtimeKind": runtime_kind,
            "conditions": [degraded, not_ready, runtime_not_ready, not_progressing],
        }
    })
}

/// Idempotency guard for [`build_runtime_unsupported_status_patch`].
#[must_use]
pub fn runtime_unsupported_status_matches(sandbox: &ClawSandbox, runtime_kind: &str) -> bool {
    use crate::status::conditions::{
        TYPE_DEGRADED, TYPE_PROGRESSING, TYPE_READY, TYPE_RUNTIME_READY,
        reason::ADAPTER_MISSING,
        status::{FALSE as STATUS_FALSE, TRUE as STATUS_TRUE},
    };

    let Some(status) = sandbox.status.as_ref() else {
        return false;
    };
    if status.phase.as_deref() != Some("Degraded") {
        return false;
    }
    if status.observed_generation != sandbox.metadata.generation {
        return false;
    }
    if status.runtime_kind.as_deref() != Some(runtime_kind) {
        return false;
    }
    let degraded_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_DEGRADED)
        .is_some_and(|c| c.status == STATUS_TRUE && c.reason == ADAPTER_MISSING);
    let ready_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_READY)
        .is_some_and(|c| c.status == STATUS_FALSE && c.reason == ADAPTER_MISSING);
    let runtime_ready_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_RUNTIME_READY)
        .is_some_and(|c| c.status == STATUS_FALSE && c.reason == ADAPTER_MISSING);
    // Phase 2 S7.B: also verify Progressing=False so a pre-S7.B
    // status (no Progressing field) is treated as stale and gets
    // back-filled on the next reconcile rather than masked.
    let progressing_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_PROGRESSING)
        .is_some_and(|c| c.status == STATUS_FALSE && c.reason == ADAPTER_MISSING);
    degraded_ok && ready_ok && runtime_ready_ok && progressing_ok
}

/// Patch `.status` with the `AdapterMissing` Degraded shape (see
/// [`build_runtime_unsupported_status_patch`]). Patch errors are logged
/// but non-fatal so the reconciler can still return the requeue action.
pub async fn stamp_runtime_unsupported(
    client: &kube::Client,
    sandbox: &ClawSandbox,
    name: &str,
    runtime_kind: &str,
    message: &str,
) {
    use kube::{
        Api, ResourceExt,
        api::{Patch, PatchParams},
    };
    if runtime_unsupported_status_matches(sandbox, runtime_kind) {
        return;
    }
    let sandbox_api: Api<ClawSandbox> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let patch = build_runtime_unsupported_status_patch(sandbox, runtime_kind, message);
    if let Err(e) = sandbox_api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(patch))
        .await
    {
        tracing::warn!(sandbox = %name, error = %e, "failed to stamp AdapterMissing status");
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
        let patch = build_running_status_patch(&sb, "azureclaw-demo", "OpenClaw");
        let st = &patch["status"];
        assert_eq!(st["phase"], "Running");
        assert_eq!(st["observedGeneration"], 7);
        assert_eq!(st["runtimeKind"], "OpenClaw");
        let conds = st["conditions"].as_array().expect("conditions array");
        assert_eq!(
            conds.len(),
            3,
            "expected Ready + Progressing + RuntimeReady"
        );
        let ready = conds.iter().find(|c| c["type"] == "Ready").expect("Ready");
        assert_eq!(ready["status"], "True");
        assert_eq!(ready["reason"], "Reconciled");
        assert_eq!(ready["observedGeneration"], 7);
        let progressing = conds
            .iter()
            .find(|c| c["type"] == "Progressing")
            .expect("Progressing");
        assert_eq!(progressing["status"], "False");
        assert_eq!(progressing["reason"], "Reconciled");
        assert_eq!(progressing["observedGeneration"], 7);
        let runtime_ready = conds
            .iter()
            .find(|c| c["type"] == "RuntimeReady")
            .expect("RuntimeReady");
        assert_eq!(runtime_ready["status"], "True");
        assert_eq!(runtime_ready["reason"], "Reconciled");
        assert!(
            runtime_ready["message"]
                .as_str()
                .unwrap_or_default()
                .contains("OpenClaw"),
            "RuntimeReady message must reference the runtime kind"
        );
    }

    #[test]
    fn running_patch_preserves_foundry_agent_id() {
        let prior = ClawSandboxStatus {
            foundry_agent_id: Some("asst-abc".into()),
            ..Default::default()
        };
        let sb = new_sandbox(Some(3), Some(prior));
        let patch = build_running_status_patch(&sb, "azureclaw-demo", "OpenClaw");
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
        let patch = build_running_status_patch(&sb, "azureclaw-demo", "OpenClaw");
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
        let patch = build_running_status_patch(&sb, "azureclaw-demo", "OpenClaw");
        assert!(patch["status"]["observedGeneration"].is_null());
    }

    #[test]
    fn running_status_matches_returns_false_when_status_missing() {
        let sb = new_sandbox(Some(1), None);
        assert!(!running_status_matches(&sb, "azureclaw-demo", "OpenClaw"));
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
        assert!(!running_status_matches(&sb, "azureclaw-demo", "OpenClaw"));
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
        assert!(!running_status_matches(&sb, "azureclaw-demo", "OpenClaw"));
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
        assert!(!running_status_matches(&sb, "azureclaw-demo", "OpenClaw"));
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
        assert!(!running_status_matches(&sb, "azureclaw-demo", "OpenClaw"));
    }

    #[test]
    fn running_status_matches_returns_true_for_settled_status() {
        let prior = ClawSandboxStatus {
            phase: Some("Running".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            runtime_kind: Some("OpenClaw".into()),
            conditions: vec![
                conditions::new_condition(
                    conditions::TYPE_READY,
                    conditions::status::TRUE,
                    conditions::reason::RECONCILED,
                    "ok",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_PROGRESSING,
                    conditions::status::FALSE,
                    conditions::reason::RECONCILED,
                    "ok",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_RUNTIME_READY,
                    conditions::status::TRUE,
                    conditions::reason::RECONCILED,
                    "ok",
                    Some(1),
                ),
            ],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(running_status_matches(&sb, "azureclaw-demo", "OpenClaw"));
    }

    #[test]
    fn running_status_matches_returns_false_when_progressing_missing() {
        // Phase 2 S7.B regression: pre-S7.B controllers wrote
        // [Ready=True, RuntimeReady=True] without Progressing. After
        // upgrade, that prior shape must be considered stale so the
        // first reconcile back-fills the new Progressing condition
        // instead of being short-circuited as a no-op.
        let prior = ClawSandboxStatus {
            phase: Some("Running".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            runtime_kind: Some("OpenClaw".into()),
            conditions: vec![
                conditions::new_condition(
                    conditions::TYPE_READY,
                    conditions::status::TRUE,
                    conditions::reason::RECONCILED,
                    "ok",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_RUNTIME_READY,
                    conditions::status::TRUE,
                    conditions::reason::RECONCILED,
                    "ok",
                    Some(1),
                ),
            ],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(!running_status_matches(&sb, "azureclaw-demo", "OpenClaw"));
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
        assert_eq!(conds.len(), 3);
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
        let progressing = conds
            .iter()
            .find(|c| c["type"] == "Progressing")
            .expect("Progressing cond");
        assert_eq!(progressing["status"], "False");
        assert_eq!(progressing["reason"], "SpecInvalid");
        assert_eq!(progressing["observedGeneration"], 9);
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

    // ── OverlayMode (Phase 2 S8) status helpers ──

    #[test]
    fn overlay_patch_emits_overlay_phase_and_three_conditions() {
        let sb = new_sandbox(Some(4), None);
        let patch = build_overlay_status_patch(&sb, "azureclaw-demo", "upstream-1", "OpenClaw");
        let st = &patch["status"];
        assert_eq!(st["phase"], "Overlay");
        assert_eq!(st["namespace"], "azureclaw-demo");
        assert_eq!(st["sandboxPod"], "upstream/upstream-1");
        assert_eq!(st["observedGeneration"], 4);
        assert_eq!(st["runtimeKind"], "OpenClaw");
        let conds = st["conditions"].as_array().expect("conditions array");
        assert_eq!(
            conds.len(),
            4,
            "expected Ready+Progressing+Suspended+RuntimeReady"
        );
        let ready = conds.iter().find(|c| c["type"] == "Ready").expect("Ready");
        assert_eq!(ready["status"], "True");
        assert_eq!(ready["reason"], "OverlayMode");
        let progressing = conds
            .iter()
            .find(|c| c["type"] == "Progressing")
            .expect("Progressing");
        assert_eq!(progressing["status"], "False");
        assert_eq!(progressing["reason"], "OverlayMode");
        let suspended = conds
            .iter()
            .find(|c| c["type"] == "Suspended")
            .expect("Suspended");
        assert_eq!(suspended["status"], "True");
        assert_eq!(suspended["reason"], "OverlayMode");
        assert!(
            suspended["message"]
                .as_str()
                .unwrap_or_default()
                .contains("upstream-1"),
            "Suspended message must reference the upstream CR name"
        );
        let runtime_ready = conds
            .iter()
            .find(|c| c["type"] == "RuntimeReady")
            .expect("RuntimeReady");
        assert_eq!(runtime_ready["status"], "False");
        assert_eq!(runtime_ready["reason"], "OverlayMode");
    }

    #[test]
    fn overlay_status_matches_rejects_when_status_missing() {
        let sb = new_sandbox(Some(1), None);
        assert!(!overlay_status_matches(
            &sb,
            "azureclaw-demo",
            "u1",
            "OpenClaw"
        ));
    }

    #[test]
    fn overlay_status_matches_rejects_when_phase_is_running() {
        let prior = ClawSandboxStatus {
            phase: Some("Running".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            sandbox_pod: Some("upstream/u1".into()),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::OVERLAY_MODE,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(!overlay_status_matches(
            &sb,
            "azureclaw-demo",
            "u1",
            "OpenClaw"
        ));
    }

    #[test]
    fn overlay_status_matches_rejects_when_upstream_ref_differs() {
        let prior = ClawSandboxStatus {
            phase: Some("Overlay".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            sandbox_pod: Some("upstream/old-name".into()),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::OVERLAY_MODE,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(!overlay_status_matches(
            &sb,
            "azureclaw-demo",
            "new-name",
            "OpenClaw"
        ));
    }

    #[test]
    fn overlay_status_matches_rejects_when_generation_stale() {
        let prior = ClawSandboxStatus {
            phase: Some("Overlay".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            sandbox_pod: Some("upstream/u1".into()),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::OVERLAY_MODE,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(2), Some(prior));
        assert!(!overlay_status_matches(
            &sb,
            "azureclaw-demo",
            "u1",
            "OpenClaw"
        ));
    }

    #[test]
    fn overlay_status_matches_returns_true_for_settled_overlay_status() {
        let prior = ClawSandboxStatus {
            phase: Some("Overlay".into()),
            namespace: Some("azureclaw-demo".into()),
            observed_generation: Some(1),
            sandbox_pod: Some("upstream/u1".into()),
            runtime_kind: Some("OpenClaw".into()),
            conditions: vec![conditions::new_condition(
                conditions::TYPE_READY,
                conditions::status::TRUE,
                conditions::reason::OVERLAY_MODE,
                "ok",
                Some(1),
            )],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(overlay_status_matches(
            &sb,
            "azureclaw-demo",
            "u1",
            "OpenClaw"
        ));
    }

    #[test]
    fn overlay_patch_preserves_ready_transition_time_on_repeat() {
        let existing_ready = conditions::new_condition(
            conditions::TYPE_READY,
            conditions::status::TRUE,
            conditions::reason::OVERLAY_MODE,
            "overlay",
            Some(1),
        );
        let prior_ts = existing_ready.last_transition_time.clone();
        let prior = ClawSandboxStatus {
            conditions: vec![existing_ready],
            ..Default::default()
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        let sb = new_sandbox(Some(2), Some(prior));
        let patch = build_overlay_status_patch(&sb, "azureclaw-demo", "u1", "OpenClaw");
        let ready = patch["status"]["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "Ready")
            .unwrap()
            .clone();
        let prior_ts_str = serde_json::to_value(&prior_ts).unwrap();
        assert_eq!(
            ready["lastTransitionTime"].as_str().unwrap(),
            prior_ts_str.as_str().unwrap()
        );
    }

    // ── S10.A1: AdapterMissing (runtime unsupported) status helpers ──

    #[test]
    fn runtime_unsupported_patch_stamps_three_conditions_and_runtime_kind() {
        let sb = new_sandbox(Some(5), None);
        let patch = build_runtime_unsupported_status_patch(
            &sb,
            "OpenAIAgents",
            "no adapter wired in this build",
        );
        let st = &patch["status"];
        assert_eq!(st["phase"], "Degraded");
        assert_eq!(st["observedGeneration"], 5);
        assert_eq!(st["runtimeKind"], "OpenAIAgents");
        let conds = st["conditions"].as_array().expect("conditions array");
        assert_eq!(
            conds.len(),
            4,
            "expected Degraded+Ready+RuntimeReady+Progressing"
        );
        let degraded = conds
            .iter()
            .find(|c| c["type"] == "Degraded")
            .expect("Degraded");
        assert_eq!(degraded["status"], "True");
        assert_eq!(degraded["reason"], "AdapterMissing");
        let ready = conds.iter().find(|c| c["type"] == "Ready").expect("Ready");
        assert_eq!(ready["status"], "False");
        assert_eq!(ready["reason"], "AdapterMissing");
        let runtime_ready = conds
            .iter()
            .find(|c| c["type"] == "RuntimeReady")
            .expect("RuntimeReady");
        assert_eq!(runtime_ready["status"], "False");
        assert_eq!(runtime_ready["reason"], "AdapterMissing");
        let progressing = conds
            .iter()
            .find(|c| c["type"] == "Progressing")
            .expect("Progressing");
        assert_eq!(progressing["status"], "False");
        assert_eq!(progressing["reason"], "AdapterMissing");
    }

    #[test]
    fn runtime_unsupported_status_matches_rejects_when_status_missing() {
        let sb = new_sandbox(Some(1), None);
        assert!(!runtime_unsupported_status_matches(&sb, "OpenAIAgents"));
    }

    #[test]
    fn runtime_unsupported_status_matches_rejects_when_runtime_kind_differs() {
        let prior = ClawSandboxStatus {
            phase: Some("Degraded".into()),
            observed_generation: Some(1),
            runtime_kind: Some("MicrosoftAgentFramework".into()),
            conditions: vec![
                conditions::new_condition(
                    conditions::TYPE_DEGRADED,
                    conditions::status::TRUE,
                    conditions::reason::ADAPTER_MISSING,
                    "x",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_READY,
                    conditions::status::FALSE,
                    conditions::reason::ADAPTER_MISSING,
                    "x",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_RUNTIME_READY,
                    conditions::status::FALSE,
                    conditions::reason::ADAPTER_MISSING,
                    "x",
                    Some(1),
                ),
            ],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(!runtime_unsupported_status_matches(&sb, "OpenAIAgents"));
    }

    #[test]
    fn runtime_unsupported_status_matches_returns_true_for_settled_status() {
        let prior = ClawSandboxStatus {
            phase: Some("Degraded".into()),
            observed_generation: Some(1),
            runtime_kind: Some("OpenAIAgents".into()),
            conditions: vec![
                conditions::new_condition(
                    conditions::TYPE_DEGRADED,
                    conditions::status::TRUE,
                    conditions::reason::ADAPTER_MISSING,
                    "x",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_READY,
                    conditions::status::FALSE,
                    conditions::reason::ADAPTER_MISSING,
                    "x",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_RUNTIME_READY,
                    conditions::status::FALSE,
                    conditions::reason::ADAPTER_MISSING,
                    "x",
                    Some(1),
                ),
                conditions::new_condition(
                    conditions::TYPE_PROGRESSING,
                    conditions::status::FALSE,
                    conditions::reason::ADAPTER_MISSING,
                    "x",
                    Some(1),
                ),
            ],
            ..Default::default()
        };
        let sb = new_sandbox(Some(1), Some(prior));
        assert!(runtime_unsupported_status_matches(&sb, "OpenAIAgents"));
    }

    #[test]
    fn runtime_unsupported_patch_preserves_transition_time_on_repeat() {
        let prior_degraded = conditions::new_condition(
            conditions::TYPE_DEGRADED,
            conditions::status::TRUE,
            conditions::reason::ADAPTER_MISSING,
            "no adapter",
            Some(1),
        );
        let prior_ts = prior_degraded.last_transition_time.clone();
        let prior = ClawSandboxStatus {
            conditions: vec![prior_degraded],
            ..Default::default()
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        let sb = new_sandbox(Some(2), Some(prior));
        let patch = build_runtime_unsupported_status_patch(&sb, "OpenAIAgents", "still no adapter");
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
}
