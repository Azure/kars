// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! CRD status helpers shared across reconcilers.
//!
//! Everything here is pure logic — no K8s client calls. Reconcilers call
//! these helpers to produce status-patch payloads; the reconciler owns the
//! `patch_status` call. Isolating status construction here keeps reconciler
//! bodies short and gives a single place to audit the wire format of
//! everything we write into `.status`.

pub mod conditions;
pub mod phase;
pub mod router_confirmation;
pub mod router_confirmation_io;

use crate::crd::KarsSandbox;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::ResourceExt;
use serde_json::{Value, json};

// Merge patches replace the conditions array. Keep other writers' conditions,
// but retire our own obsolete outcomes (e.g. Degraded on recovery).
fn retain_unrelated_conditions(prior: &[Condition], desired: &mut Vec<Condition>) {
    use conditions::*;
    for condition in prior {
        if !matches!(
            condition.type_.as_str(),
            TYPE_READY
                | TYPE_PROGRESSING
                | TYPE_RUNTIME_READY
                | TYPE_DEGRADED
                | TYPE_SUSPENDED
                | TYPE_ALLOWLIST_VERIFIED
                | TYPE_ALLOWLIST_AUTHORITATIVE
                | TYPE_ALLOWLIST_DRIFT
                | "CredentialsReady"
        ) && !desired.iter().any(|c| c.type_ == condition.type_)
        {
            desired.push(condition.clone());
        }
    }
}

/// Build the `status` patch for a `KarsSandbox` that has reached the
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
/// Build the `status` patch for a `KarsSandbox` that has reached the
/// Running phase. See [`build_running_status_patch_with_extras`] for
/// the additive variant; this convenience wrapper passes no extras and
/// matches the pre-S12.b call-site shape used in unit tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_running_status_patch(
    sandbox: &KarsSandbox,
    sandbox_ns: &str,
    runtime_kind: &str,
) -> Value {
    build_running_status_patch_with_extras(sandbox, sandbox_ns, runtime_kind, &[])
}

/// As [`build_running_status_patch`], but appends `extra_conditions` to
/// the emitted `conditions` array. Used by the reconciler to surface the
/// `AllowlistVerified` Condition (S12.b) without forking the patch
/// builder.
///
/// Each extra is upserted by `type_` (last-writer wins), so a caller can
/// safely pass the freshly-computed condition without de-duplicating
/// against the standard set above.
pub fn build_running_status_patch_with_extras(
    sandbox: &KarsSandbox,
    sandbox_ns: &str,
    runtime_kind: &str,
    extra_conditions: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
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

    let mut conditions_vec = vec![ready, progressing, runtime_ready];
    for extra in extra_conditions {
        let mut extra = extra.clone();
        if let Some(prior) = conditions::find(prior_conditions, &extra.type_)
            && prior.status == extra.status
        {
            extra.last_transition_time = prior.last_transition_time.clone();
        }
        if let Some(slot) = conditions_vec.iter_mut().find(|c| c.type_ == extra.type_) {
            *slot = extra;
        } else {
            conditions_vec.push(extra);
        }
    }
    retain_unrelated_conditions(prior_conditions, &mut conditions_vec);

    let mut status_obj = json!({
        "status": {
            "phase": "Running",
            "namespace": sandbox_ns,
            "sandboxPod": format!("{name}-*"),
            "inferenceEndpoint": "https://kars-inference-router.kars-system.svc.cluster.local:8443",
            "observedGeneration": generation,
            "runtimeKind": runtime_kind,
            "conditions": conditions_vec,
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
#[cfg_attr(not(test), allow(dead_code))]
pub fn running_status_matches(sandbox: &KarsSandbox, sandbox_ns: &str, runtime_kind: &str) -> bool {
    running_status_matches_with_extras(sandbox, sandbox_ns, runtime_kind, &[])
}

/// As [`running_status_matches`], but additionally requires that the
/// existing CR status carries every condition in `extra_conditions`
/// with the same `type_`/`status`/`reason`/`observedGeneration` (message changes alone do
/// **not** force a re-patch — they ride along on the next genuine
/// transition). Used by the reconciler to keep the `AllowlistVerified`
/// Condition stable across same-result reconciles without churning
/// `resourceVersion`.
pub fn running_status_matches_with_extras(
    sandbox: &KarsSandbox,
    sandbox_ns: &str,
    runtime_kind: &str,
    extra_conditions: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition],
) -> bool {
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
    // Phase 2 S7.B: the running shape now stamps Progressing=False
    // alongside Ready=True; verifying it here prevents an upgrade-time
    // status flap where a pre-S7.B controller's Ready-only status would
    // otherwise be considered a no-op match and the Progressing field
    // would never get back-filled.
    for (type_, expected) in [
        (TYPE_READY, STATUS_TRUE),
        (TYPE_PROGRESSING, STATUS_FALSE),
        (TYPE_RUNTIME_READY, STATUS_TRUE),
    ] {
        // The builder upserts caller overrides last; compare those below
        // rather than also demanding the superseded default outcome.
        if extra_conditions.iter().any(|c| c.type_ == type_) {
            continue;
        }
        if !conditions::find(&status.conditions, type_).is_some_and(|c| {
            c.status == expected && c.observed_generation == sandbox.metadata.generation
        }) {
            return false;
        }
    }
    // S12.b: `AllowlistVerified` must match in (type,status,reason,generation) so
    // a transient → verified flip triggers a re-patch. We deliberately
    // ignore `message` because the verifier rewrites the digest /
    // generation summary on every successful pass and we don't want
    // that to defeat the idempotency guard.
    for (index, extra) in extra_conditions.iter().enumerate() {
        if extra_conditions[index + 1..]
            .iter()
            .any(|c| c.type_ == extra.type_)
        {
            continue;
        }
        let matched = status.conditions.iter().any(|c| {
            c.type_ == extra.type_
                && c.status == extra.status
                && c.reason == extra.reason
                && c.observed_generation == extra.observed_generation
        });
        if !matched {
            return false;
        }
    }
    true
}

/// Build the `status` patch for a `KarsSandbox` running in
/// **`OverlayMode`** (Phase 2 S8). In this mode the operator's upstream
/// `Sandbox` CR owns the Pod lifecycle; kars skipped Deployment +
/// Service creation and only laid down the overlay (namespace, SA with
/// Workload-Identity binding, NetworkPolicy, governance ConfigMaps).
///
/// Status shape:
/// - `phase: "Overlay"` — distinct from `Running` so dashboards can
///   surface "this CR is intentionally not driving a Pod".
/// - `Ready=True, Reason=OverlayMode` — overlay reconciled cleanly
///   from kars's perspective; `kubectl wait --for=condition=Ready`
///   still works.
/// - `Progressing=False, Reason=OverlayMode` — no further kars work
///   pending.
/// - `Suspended=True, Reason=OverlayMode` — explicit signal that we are
///   not driving a Pod here (operators querying `Suspended` see why).
///
/// `sandbox_pod` is set to the upstream CR name (with a `upstream/`
/// prefix so it's unambiguous in `kubectl get karssandbox`) so operators
/// can pivot from `kubectl describe karssandbox` to the upstream object.
pub fn build_overlay_status_patch(
    sandbox: &KarsSandbox,
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
    // In overlay mode, kars doesn't drive the Pod; the runtime
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
        &format!("runtime `{runtime_kind}` not driven by kars in overlay mode"),
        generation,
    );
    let mut conditions_vec = vec![ready, progressing, suspended, runtime_ready];
    retain_unrelated_conditions(prior_conditions, &mut conditions_vec);
    json!({
        "status": {
            "phase": "Overlay",
            "namespace": sandbox_ns,
            "sandboxPod": format!("upstream/{upstream_ref}"),
            "observedGeneration": generation,
            "runtimeKind": runtime_kind,
            "conditions": conditions_vec,
        }
    })
}

/// Returns `true` when the existing CR status already encodes the same
/// "Overlay" reconciliation outcome that [`build_overlay_status_patch`]
/// would produce. Same idempotency guard as
/// [`running_status_matches`].
#[must_use]
pub fn overlay_status_matches(
    sandbox: &KarsSandbox,
    sandbox_ns: &str,
    upstream_ref: &str,
    runtime_kind: &str,
) -> bool {
    use crate::status::conditions::{
        TYPE_PROGRESSING, TYPE_READY, TYPE_RUNTIME_READY, TYPE_SUSPENDED,
        status::{FALSE as STATUS_FALSE, TRUE as STATUS_TRUE},
    };

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
    [
        (TYPE_READY, STATUS_TRUE),
        (TYPE_PROGRESSING, STATUS_FALSE),
        (TYPE_SUSPENDED, STATUS_TRUE),
        (TYPE_RUNTIME_READY, STATUS_FALSE),
    ]
    .iter()
    .all(|(type_, expected)| {
        conditions::find(&status.conditions, type_).is_some_and(|c| {
            c.status == *expected && c.observed_generation == sandbox.metadata.generation
        })
    })
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
    sandbox: &KarsSandbox,
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
    let mut conditions_vec = vec![degraded, not_ready, not_progressing];
    retain_unrelated_conditions(prior_conditions, &mut conditions_vec);
    json!({
        "status": {
            "phase": "Degraded",
            "observedGeneration": generation,
            "conditions": conditions_vec,
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
    sandbox: &KarsSandbox,
    name: &str,
    reason_value: &'static str,
    message: &str,
) {
    use kube::{
        Api, ResourceExt,
        api::{Patch, PatchParams},
    };
    let sandbox_api: Api<KarsSandbox> =
        Api::namespaced(client.clone(), &sandbox.namespace().unwrap_or_default());
    let patch = build_degraded_status_patch(sandbox, reason_value, message);
    if let Err(e) = sandbox_api
        .patch_status(name, &PatchParams::default(), &Patch::Merge(patch))
        .await
    {
        tracing::warn!(sandbox = %name, error = %e, "failed to stamp Degraded status");
    }
}

/// Build the `status` patch for a `KarsSandbox` whose `spec.runtime.kind`
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
    sandbox: &KarsSandbox,
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
    let mut conditions_vec = vec![degraded, not_ready, runtime_not_ready, not_progressing];
    retain_unrelated_conditions(prior_conditions, &mut conditions_vec);
    json!({
        "status": {
            "phase": "Degraded",
            "observedGeneration": generation,
            "runtimeKind": runtime_kind,
            "conditions": conditions_vec,
        }
    })
}

/// Idempotency guard for [`build_runtime_unsupported_status_patch`].
#[must_use]
pub fn runtime_unsupported_status_matches(sandbox: &KarsSandbox, runtime_kind: &str) -> bool {
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
        .is_some_and(|c| {
            c.status == STATUS_TRUE
                && c.reason == ADAPTER_MISSING
                && c.observed_generation == sandbox.metadata.generation
        });
    let ready_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_READY)
        .is_some_and(|c| {
            c.status == STATUS_FALSE
                && c.reason == ADAPTER_MISSING
                && c.observed_generation == sandbox.metadata.generation
        });
    let runtime_ready_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_RUNTIME_READY)
        .is_some_and(|c| {
            c.status == STATUS_FALSE
                && c.reason == ADAPTER_MISSING
                && c.observed_generation == sandbox.metadata.generation
        });
    // Phase 2 S7.B: also verify Progressing=False so a pre-S7.B
    // status (no Progressing field) is treated as stale and gets
    // back-filled on the next reconcile rather than masked.
    let progressing_ok = status
        .conditions
        .iter()
        .find(|c| c.type_ == TYPE_PROGRESSING)
        .is_some_and(|c| {
            c.status == STATUS_FALSE
                && c.reason == ADAPTER_MISSING
                && c.observed_generation == sandbox.metadata.generation
        });
    degraded_ok && ready_ok && runtime_ready_ok && progressing_ok
}

/// Patch `.status` with the `AdapterMissing` Degraded shape (see
/// [`build_runtime_unsupported_status_patch`]). Patch errors are logged
/// but non-fatal so the reconciler can still return the requeue action.
pub async fn stamp_runtime_unsupported(
    client: &kube::Client,
    sandbox: &KarsSandbox,
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
    let sandbox_api: Api<KarsSandbox> =
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
mod tests;

#[cfg(test)]
mod convergence_tests;
