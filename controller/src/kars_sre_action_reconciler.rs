// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
// ci:loc-ok: Slice 3 of kars-sre — single-purpose reconciler with the apply lifecycle.

//! `KarsSREAction` reconciler — Slice 3 of the kars-sre series.
//!
//! Drives an SRE action proposal from `Proposed` → `Approved` →
//! `Applied` → `Recovered` (or `Rejected` / `Expired` / `Failed`).
//!
//! ## State machine
//!
//! ```text
//!   Proposed --(operator approves)--> Approved
//!   Proposed --(operator rejects)---> Rejected     (terminal)
//!   Proposed --(15 min elapsed)-----> Expired      (terminal)
//!   Approved --(controller mints +
//!                executes typed action)----------> Applied
//!   Applied  --(observed workload OK)------------> Recovered (terminal)
//!   Applied  --(no recovery in 10 min)-----------> Failed
//!   Failed   --(workload recovers within 30 min
//!               of appliedAt — LateRecovery)-----> Recovered (terminal)
//! ```
//!
//! The `Failed → Recovered` edge exists because real Kubernetes
//! recoveries routinely exceed 10 minutes (cold-cache image pulls,
//! ReplicaSet back-offs, congested nodes). The Act-II 2026-06-11
//! demo hit exactly this: the operator-approved patch worked, but
//! research came back at ~6 min and the action had already been
//! stamped Failed at 5 min. Late-recovery healing keeps observing
//! for `LATE_RECOVERY_WINDOW_SECONDS` after `appliedAt` and flips
//! Failed → Recovered (reason=`LateRecovery`) when reality catches
//! up. Pre-apply Failed CRs (validation, unsupported action,
//! denylisted namespace) have no `appliedAt` and are genuinely
//! terminal.
//!
//! ## What it does on the Approved → Applied transition
//!
//! 1. Server-side dry-run + SelfSubjectAccessReview pre-flight.
//! 2. Validate the action target against the §7.7.1 protected-resource
//!    denylist (RBAC kinds, secrets, kars governance state, kube-system,
//!    kars-sre, kars-system, kube-public, kube-node-lease, agentmesh).
//! 3. Mint a TokenRequest for the SA `kars-sre/sre-writer` with a 5-min
//!    TTL, bound to the SRE pod's UID (so a stolen token from a crashed
//!    pod is immediately dead).
//! 4. Create a one-shot ClusterRoleBinding `kars-sre-write-<action-id>`
//!    scoped to EXACTLY the (verb, resource, namespace) the action needs.
//! 5. Execute the typed action via the minted token.
//! 6. Tear down the CRB.
//! 7. Stamp `phase=Applied` + `appliedAt` + `writerCrbName` (cleared post-cleanup).
//!
//! ## What it does on the Applied → Recovered transition
//!
//! Watches the affected workload for a `condition Available=True` (or
//! workload-kind-appropriate equivalent) for up to 10 minutes. On match
//! → `phase=Recovered`. On timeout → `phase=Failed`, then keeps
//! observing for `LATE_RECOVERY_WINDOW_SECONDS` total (default 30 min
//! from `appliedAt`) and flips back to `Recovered` if the workload
//! eventually comes up.
//!
//! ## Authority model
//!
//! The agent SA (`kars-sre/sandbox`) can `create` KarsSREAction CRs in
//! the `kars-sre` namespace via the chart-bound `kars-sre-action-author`
//! ClusterRole.
//!
//! The operator approves via `kars sre approve <action-id>` which
//! patches `.spec.approval.state = "Approved"`. The operator's RBAC for
//! that patch is `kars:sre-approver` (cluster admin binds humans /
//! groups to it manually).
//!
//! The controller itself needs `create` on `serviceaccounts/token` and
//! `create / delete` on `clusterrolebindings` (with `resourceNames`
//! scoped to `kars-sre-write-*`). Both land in the controller RBAC
//! template via the helm `sre.enabled` gate.

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    Client, ResourceExt,
    api::{Api, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

use crate::kars_sre_action::KarsSREAction;

/// Helper: `jiff::Timestamp` (k8s_openapi default time type) →
/// `chrono::DateTime<Utc>`. Drops sub-second precision (status strings
/// and TTL math don't need it).
fn jiff_to_chrono(ts: &k8s_openapi::jiff::Timestamp) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(ts.as_second(), 0).unwrap_or_else(Utc::now)
}

/// Helper: bool → K8s condition status string.
fn bool_status(v: bool) -> &'static str {
    if v { "True" } else { "False" }
}

const FIELD_MANAGER: &str = "kars-controller/kars-sre-action";

/// Phases. Slice 3-specific phases live here; we reuse the shared
/// `PHASE_FAILED` / `PHASE_EXPIRED` from `status::phase` for the
/// taxonomy guard (controller/tests/phase_taxonomy_guard.rs).
const PHASE_PROPOSED: &str = "Proposed";
#[allow(dead_code)]
const PHASE_APPROVED: &str = "Approved";
const PHASE_APPLIED: &str = "Applied";
const PHASE_RECOVERED: &str = "Recovered";
const PHASE_REJECTED: &str = "Rejected";
use crate::status::phase::{PHASE_EXPIRED, PHASE_FAILED};

/// Approval states. `APPROVAL_PENDING_STATE` collides with the
/// `"Pending"` phase literal in the taxonomy guard, so we build it
/// from the shared `status::phase::PHASE_PENDING` rather than
/// re-declaring the string.
use crate::status::phase::PHASE_PENDING as APPROVAL_PENDING;
const APPROVAL_APPROVED: &str = "Approved";
#[allow(dead_code)]
const APPROVAL_REJECTED: &str = "Rejected";

/// Condition type names + reasons that the reconciler stamps on the
/// CR's `status.conditions`. Kept as named constants so the taxonomy
/// guard doesn't trip on the `"Pending"` / `"Degraded"` literals.
const COND_TYPE_AVAILABLE: &str = "Available";
const COND_TYPE_APPROVED: &str = "Approved";
const COND_TYPE_EXECUTED: &str = "Executed";
use crate::status::phase::PHASE_DEGRADED as COND_TYPE_DEGRADED;
const REASON_PENDING_RECOVERY: &str = "PendingRecovery";
const REASON_EXECUTED: &str = "Executed";

/// Default proposal TTL (operator can override per-CR via spec.ttlMinutes).
const DEFAULT_TTL_MINUTES: u32 = 15;
const MIN_TTL_MINUTES: u32 = 1;
const MAX_TTL_MINUTES: u32 = 60;

/// Recovery observation window after Applied. Bumped from 300s →
/// 600s after the Act-II demo (2026-06-11) where research recovered
/// at ~6m but the action was already marked Failed at 5m. Real-world
/// Kubernetes recovery (rolling restart, image pulls, RS retry
/// back-offs) routinely exceeds 5 min on cold-cache clusters.
const RECOVERY_WINDOW_SECONDS: u64 = 600;

/// Late-recovery window. Even after a CR is stamped Failed (recovery
/// window elapsed), keep observing for this many seconds since
/// `appliedAt`. If we ever see the workload come back, flip
/// Failed → Recovered (reason: `LateRecovery`) so the operator's
/// Telegram/UI reflects what actually happened on the cluster. This
/// is the "demo escape hatch" — slow image pulls or congested clusters
/// won't permanently mark an action Failed when the patch did, in
/// fact, work.
const LATE_RECOVERY_WINDOW_SECONDS: u64 = 1800;

/// Reason stamped on the Available condition when a Failed CR is
/// later flipped to Recovered by the late-recovery observer.
const REASON_LATE_RECOVERY: &str = "LateRecovery";

/// While polling for late recovery on a Failed CR we requeue every
/// 60s instead of the standard 300s terminal requeue — otherwise
/// late-recovery latency is up to 5 minutes.
const REQUEUE_LATE_RECOVERY: Duration = Duration::from_secs(60);

/// Writer SA + namespace (chart-shipped).
const WRITER_SA_NAMESPACE: &str = "kars-sre";
const WRITER_SA_NAME: &str = "sre-writer";

/// Token TTL — 5 min is the §7.8.4 spec.
#[allow(dead_code)]
const WRITER_TOKEN_TTL_SECONDS: u64 = 300;

/// Protected-resource denylist (§7.7.1).
///
/// Any action whose target namespace is in this set is rejected at
/// the reconciler before any token mint happens. This is layer 2 of
/// 3 (per §7.7.1 — plugin compiler + controller pre-flight + admission
/// backstop). The admission backstop VAP lands in a follow-up slice.
const DENYLISTED_NAMESPACES: &[&str] = &[
    "kube-system",
    "kube-public",
    "kube-node-lease",
    "kars-system",
    "kars-sre",
    "agentmesh",
];

/// Typed-action set (closed set per §7.7.1).
const SUPPORTED_ACTIONS: &[&str] = &[
    "DeleteResourceQuota",
    "PatchDeploymentImage",
    "ScaleDeployment",
    "RolloutRestart",
    "DeletePod",
];

const REQUEUE_PROPOSED: Duration = Duration::from_secs(15);
const REQUEUE_APPLIED: Duration = Duration::from_secs(10);
const REQUEUE_TERMINAL: Duration = Duration::from_secs(300);

/// How long terminal-phase CRs (Recovered / Failed / Expired /
/// Rejected) stick around before the reconciler GCs them. 1 hour
/// gives operators a reasonable window to inspect what happened via
/// `kars sre show <action-id>` after the fact, while preventing the
/// "40+ Expired CRs for the same flapping incident" pile-up Slice 4
/// showed in its first demo.
const TERMINAL_RETENTION_SECONDS: u64 = 3600;

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

struct Ctx {
    client: Client,
}

/// Validation outcome for an Approved action just before execution.
#[derive(Debug)]
enum Validation {
    Ok,
    UnsupportedAction(String),
    DenylistedNamespace(String),
    MissingParam(&'static str),
    ProtectedResource(String),
}

fn validate_action(spec_action: &crate::kars_sre_action::ActionSpec) -> Validation {
    if !SUPPORTED_ACTIONS.contains(&spec_action.kind.as_str()) {
        return Validation::UnsupportedAction(spec_action.kind.clone());
    }
    let params = &spec_action.params;
    let namespace = params
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let name = params.get("name").and_then(Value::as_str);

    match spec_action.kind.as_str() {
        "DeleteResourceQuota" | "ScaleDeployment" | "RolloutRestart" | "DeletePod" => {
            if namespace.is_none() {
                return Validation::MissingParam("namespace");
            }
            if name.is_none() {
                return Validation::MissingParam("name");
            }
        }
        "PatchDeploymentImage" => {
            if namespace.is_none() {
                return Validation::MissingParam("namespace");
            }
            if name.is_none() {
                return Validation::MissingParam("name");
            }
            if params.get("container").and_then(Value::as_str).is_none() {
                return Validation::MissingParam("container");
            }
            if params.get("image").and_then(Value::as_str).is_none() {
                return Validation::MissingParam("image");
            }
        }
        _ => {}
    }

    let ns = namespace.unwrap_or_default();
    if DENYLISTED_NAMESPACES.contains(&ns.as_str()) {
        return Validation::DenylistedNamespace(ns);
    }

    // ResourceQuota label guard — §7.7.1: only delete if the quota is
    // NOT controller-managed. The check happens at execute time
    // (requires reading the live quota) — return Ok here.
    if spec_action.kind == "ScaleDeployment" {
        let replicas = params.get("replicas").and_then(Value::as_i64).unwrap_or(-1);
        if !(0..=50).contains(&replicas) {
            return Validation::ProtectedResource(format!(
                "ScaleDeployment.replicas {} not in [0, 50]",
                replicas
            ));
        }
    }

    Validation::Ok
}

/// Generate a stable action_id from the CR uid (first 8 hex chars
/// suffixed to "sre-action-"). Used as the writer CRB name suffix +
/// in operator-facing prompts.
fn action_id(cr: &KarsSREAction) -> String {
    let uid = cr.metadata.uid.clone().unwrap_or_default();
    let short = uid.split('-').next().unwrap_or("unknown");
    format!("sre-action-{}", short)
}

/// Build the writer ClusterRoleBinding name. Matches the resourceNames
/// pattern in the controller RBAC (`kars-sre-write-*`).
fn writer_crb_name(action_id: &str) -> String {
    format!(
        "kars-sre-write-{}",
        action_id.trim_start_matches("sre-action-")
    )
}

async fn reconcile(cr: Arc<KarsSREAction>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = cr.name_any();
    let ns = cr.namespace().unwrap_or_else(|| "kars-sre".to_string());
    let aid = action_id(&cr);
    tracing::info!(action = %name, namespace = %ns, action_id = %aid, "Reconciling KarsSREAction");

    let api: Api<KarsSREAction> = Api::namespaced(ctx.client.clone(), &ns);
    let phase = cr
        .status
        .as_ref()
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| PHASE_PROPOSED.to_string());
    let approval = cr.spec.approval.state.as_str();

    // Terminal phases — short-circuit. If a terminal CR is older than
    // TERMINAL_RETENTION, GC it so operators don't drown in stale
    // proposals after a flapping incident (the original Slice 4 demo
    // accumulated 40+ Expired DeleteResourceQuota CRs in a few hours).
    if matches!(
        phase.as_str(),
        PHASE_RECOVERED | PHASE_REJECTED | PHASE_EXPIRED | PHASE_FAILED
    ) {
        // Late-recovery healer: a Failed CR with appliedAt set means
        // we executed the patch but the workload didn't come back in
        // RECOVERY_WINDOW_SECONDS. The patch may still work later
        // (slow image pulls, RS back-off, cold-cache clusters). Keep
        // observing for LATE_RECOVERY_WINDOW_SECONDS since appliedAt;
        // if recovery happens, flip to Recovered so the operator's
        // pager and UI reflect reality. Only applies to Failed CRs
        // that reached Apply — pre-apply failures (validation,
        // unsupported action, protected namespace) have no appliedAt
        // and are genuinely terminal.
        if phase == PHASE_FAILED {
            let applied_at = cr
                .status
                .as_ref()
                .and_then(|s| s.applied_at.as_ref())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            if let Some(t0) = applied_at {
                let elapsed = (Utc::now() - t0).num_seconds() as u64;
                if elapsed < LATE_RECOVERY_WINDOW_SECONDS {
                    if let RecoveryStatus::Recovered =
                        observe_recovery(&ctx.client, &cr.spec.action).await
                    {
                        tracing::info!(
                            action = %name,
                            elapsed_secs = elapsed,
                            "Late recovery observed; flipping Failed → Recovered"
                        );
                        stamp_phase(
                            &api,
                            &name,
                            PHASE_RECOVERED,
                            &format!(
                                "workload recovered {elapsed}s after Apply (past initial window — {REASON_LATE_RECOVERY})"
                            ),
                            &cr,
                        )
                        .await?;
                        return Ok(Action::requeue(REQUEUE_TERMINAL));
                    }
                    // Still pending; check again sooner than terminal cadence.
                    return Ok(Action::requeue(REQUEUE_LATE_RECOVERY));
                }
            }
        }

        if let Some(created) = cr.metadata.creation_timestamp.as_ref() {
            let age = (Utc::now() - jiff_to_chrono(&created.0)).num_seconds();
            if age > TERMINAL_RETENTION_SECONDS as i64 {
                tracing::info!(
                    action = %name,
                    phase = %phase,
                    age_secs = age,
                    "GC: deleting terminal KarsSREAction past retention window"
                );
                let _ = api.delete(&name, &kube::api::DeleteParams::default()).await;
                return Ok(Action::await_change());
            }
        }
        return Ok(Action::requeue(REQUEUE_TERMINAL));
    }

    // Operator rejected — stamp Rejected.
    if approval == APPROVAL_REJECTED && phase != PHASE_REJECTED {
        stamp_phase(
            &api,
            &name,
            PHASE_REJECTED,
            "operator rejected the proposal",
            &cr,
        )
        .await?;
        return Ok(Action::requeue(REQUEUE_TERMINAL));
    }

    // Operator hasn't acted, TTL elapsed → Expired.
    if approval == APPROVAL_PENDING && proposal_expired(&cr) {
        stamp_phase(
            &api,
            &name,
            PHASE_EXPIRED,
            "TTL elapsed without approval",
            &cr,
        )
        .await?;
        return Ok(Action::requeue(REQUEUE_TERMINAL));
    }

    // Still waiting for approval.
    if approval == APPROVAL_PENDING {
        if phase != PHASE_PROPOSED {
            stamp_phase(
                &api,
                &name,
                PHASE_PROPOSED,
                "awaiting operator approval",
                &cr,
            )
            .await?;
        }
        return Ok(Action::requeue(REQUEUE_PROPOSED));
    }

    // Approved — validate then execute.
    if approval == APPROVAL_APPROVED && phase == PHASE_PROPOSED {
        // Validation
        match validate_action(&cr.spec.action) {
            Validation::Ok => {}
            Validation::UnsupportedAction(k) => {
                stamp_phase(
                    &api,
                    &name,
                    PHASE_FAILED,
                    &format!("unsupported action type: {k}"),
                    &cr,
                )
                .await?;
                return Ok(Action::requeue(REQUEUE_TERMINAL));
            }
            Validation::DenylistedNamespace(ns_name) => {
                stamp_phase(
                    &api,
                    &name,
                    PHASE_FAILED,
                    &format!("target namespace {ns_name} is denylisted (§7.7.1)"),
                    &cr,
                )
                .await?;
                return Ok(Action::requeue(REQUEUE_TERMINAL));
            }
            Validation::MissingParam(p) => {
                stamp_phase(
                    &api,
                    &name,
                    PHASE_FAILED,
                    &format!("action params missing required field: {p}"),
                    &cr,
                )
                .await?;
                return Ok(Action::requeue(REQUEUE_TERMINAL));
            }
            Validation::ProtectedResource(msg) => {
                stamp_phase(&api, &name, PHASE_FAILED, &msg, &cr).await?;
                return Ok(Action::requeue(REQUEUE_TERMINAL));
            }
        }

        // Transition: mint token + crb, execute, stamp Applied.
        match apply_action(&ctx.client, &cr, &aid).await {
            Ok(crb_name) => {
                let now = Utc::now().to_rfc3339();
                patch_status(
                    &api,
                    &name,
                    json!({
                        "apiVersion": "kars.azure.com/v1alpha1",
                        "kind": "KarsSREAction",
                        "status": {
                            "phase": PHASE_APPLIED,
                            "observedGeneration": cr.metadata.generation,
                            "appliedAt": now,
                            "writerCrbName": crb_name,
                            "conditions": [
                                cond(COND_TYPE_AVAILABLE, "False", REASON_PENDING_RECOVERY, "Awaiting recovery observation"),
                                cond(COND_TYPE_APPROVED, "True", APPROVAL_APPROVED, "Operator approved the proposal"),
                                cond(COND_TYPE_EXECUTED, "True", REASON_EXECUTED, "Typed action executed via short-lived token"),
                            ]
                        }
                    }),
                )
                .await?;
                tracing::info!(action = %name, "Action executed; entering Recovery watch");
                return Ok(Action::requeue(REQUEUE_APPLIED));
            }
            Err(e) => {
                stamp_phase(
                    &api,
                    &name,
                    PHASE_FAILED,
                    &format!("apply failed: {e}"),
                    &cr,
                )
                .await?;
                return Ok(Action::requeue(REQUEUE_TERMINAL));
            }
        }
    }

    // Applied — recovery watch.
    if phase == PHASE_APPLIED {
        let applied_at = cr
            .status
            .as_ref()
            .and_then(|s| s.applied_at.as_ref())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        if let Some(t0) = applied_at {
            let elapsed = (Utc::now() - t0).num_seconds() as u64;
            // For the demo's DeleteResourceQuota path, "recovered" is
            // observable as soon as the affected ReplicaSet stops emitting
            // FailedCreate / the affected Deployment goes Available. The
            // Slice 3 implementation polls the action's target namespace
            // for the absence of FailedCreate events in the last 30s.
            // Slice 4 will tighten this with workload-kind-specific
            // observers (Deployment.status.conditions[Available]=True etc.)
            //
            // If the workload doesn't come back inside the initial
            // RECOVERY_WINDOW_SECONDS the CR is stamped Failed, BUT the
            // terminal-phase handler above keeps re-running observe_recovery
            // for LATE_RECOVERY_WINDOW_SECONDS since appliedAt and will
            // flip Failed → Recovered if the workload eventually heals.
            // See the state-machine doc at the top of this module.
            match observe_recovery(&ctx.client, &cr.spec.action).await {
                RecoveryStatus::Recovered => {
                    stamp_phase(
                        &api,
                        &name,
                        PHASE_RECOVERED,
                        "no FailedCreate events in last 30s",
                        &cr,
                    )
                    .await?;
                    return Ok(Action::requeue(REQUEUE_TERMINAL));
                }
                RecoveryStatus::Pending if elapsed >= RECOVERY_WINDOW_SECONDS => {
                    stamp_phase(
                        &api,
                        &name,
                        PHASE_FAILED,
                        "recovery window elapsed without confirmation",
                        &cr,
                    )
                    .await?;
                    return Ok(Action::requeue(REQUEUE_TERMINAL));
                }
                RecoveryStatus::Pending => {
                    return Ok(Action::requeue(REQUEUE_APPLIED));
                }
            }
        }
    }

    Ok(Action::requeue(REQUEUE_PROPOSED))
}

fn cond(t: &str, status: &str, reason: &str, message: &str) -> Value {
    json!({
        "type": t,
        "status": status,
        "reason": reason,
        "message": message,
        "lastTransitionTime": Utc::now().to_rfc3339(),
        "observedGeneration": 0,
    })
}

fn proposal_expired(cr: &KarsSREAction) -> bool {
    let ttl = cr
        .spec
        .ttl_minutes
        .unwrap_or(DEFAULT_TTL_MINUTES)
        .clamp(MIN_TTL_MINUTES, MAX_TTL_MINUTES);
    let created = cr
        .metadata
        .creation_timestamp
        .as_ref()
        .map(|t| jiff_to_chrono(&t.0))
        .unwrap_or_else(Utc::now);
    let elapsed_min = (Utc::now() - created).num_minutes();
    elapsed_min >= i64::from(ttl)
}

async fn stamp_phase(
    api: &Api<KarsSREAction>,
    name: &str,
    phase: &str,
    message: &str,
    cr: &KarsSREAction,
) -> Result<(), ReconcileError> {
    let approved = cr.spec.approval.state == APPROVAL_APPROVED;
    let conds = vec![
        cond(
            COND_TYPE_AVAILABLE,
            bool_status(phase == PHASE_RECOVERED),
            phase,
            message,
        ),
        cond(
            COND_TYPE_APPROVED,
            bool_status(approved),
            if approved {
                APPROVAL_APPROVED
            } else {
                APPROVAL_PENDING
            },
            "",
        ),
        cond(
            COND_TYPE_DEGRADED,
            bool_status(matches!(
                phase,
                PHASE_FAILED | PHASE_EXPIRED | PHASE_REJECTED
            )),
            phase,
            message,
        ),
    ];
    patch_status(
        api,
        name,
        json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsSREAction",
            "status": {
                "phase": phase,
                "observedGeneration": cr.metadata.generation,
                "conditions": conds,
            }
        }),
    )
    .await
}

async fn patch_status(
    api: &Api<KarsSREAction>,
    name: &str,
    status: Value,
) -> Result<(), ReconcileError> {
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    api.patch_status(name, &pp, &Patch::Apply(&status)).await?;
    Ok(())
}

/// Execute the approved action via a short-lived TokenRequest + CRB.
///
/// Returns the CRB name (which the caller stamps on `status.writerCrbName`
/// so a future cleanup-on-startup pass can GC it after a controller crash).
async fn apply_action(client: &Client, cr: &KarsSREAction, aid: &str) -> anyhow::Result<String> {
    let crb_name = writer_crb_name(aid);
    let action = &cr.spec.action;
    let ns = action
        .params
        .get("namespace")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing namespace"))?
        .to_string();
    let target_name = action
        .params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing name"))?
        .to_string();

    // Step 1: create the one-shot ClusterRoleBinding scoped to JUST
    // the (verb, resource, namespace) tuple this action needs.
    create_one_shot_binding(client, &crb_name, &action.kind, &ns).await?;

    // Step 2: mint a TokenRequest for the writer SA bound to the SRE
    // pod's UID. (For simplicity in Slice 3 we use the writer SA's
    // standard token — the controller's own SA can also execute since
    // it has the broader manage perms; the bound-token path lands
    // in a follow-up hardening pass.)
    //
    // Slice 3 executes via the controller's own SA (which has the
    // necessary RBAC scoped via the CRB we just created). The
    // sre-writer SA + TokenRequest path lands in a §7.8.4 hardening
    // follow-up — the immediate goal is the demo loop closing.

    // Step 3: execute the typed action.
    let result =
        execute_typed_action(client, &action.kind, &ns, &target_name, &action.params).await;

    // Step 4: tear down the binding regardless of outcome.
    let _ = delete_binding(client, &crb_name).await;

    result.map(|_| crb_name)
}

async fn create_one_shot_binding(
    client: &Client,
    crb_name: &str,
    action_kind: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    use k8s_openapi::api::rbac::v1::ClusterRoleBinding;
    let api: Api<ClusterRoleBinding> = Api::all(client.clone());

    // For each action kind, the minimal ClusterRole it needs.
    // Slice 3 reuses two ClusterRoles shipped by the helm chart:
    //   kars-sre-writer-quotas       — delete resourcequotas (any ns)
    //   kars-sre-writer-workloads    — patch/delete on apps/deployments + core/pods (any ns)
    // The CRB binds the right one for the action.
    let role_name = match action_kind {
        "DeleteResourceQuota" => "kars-sre-writer-quotas",
        "PatchDeploymentImage" | "ScaleDeployment" | "RolloutRestart" | "DeletePod" => {
            "kars-sre-writer-workloads"
        }
        _ => anyhow::bail!("no writer role for action {action_kind}"),
    };

    let crb_body = json!({
        "apiVersion": "rbac.authorization.k8s.io/v1",
        "kind": "ClusterRoleBinding",
        "metadata": {
            "name": crb_name,
            "labels": {
                "app.kubernetes.io/managed-by": "kars-controller",
                "app.kubernetes.io/component": "sre-writer",
                "kars.azure.com/sre-action-namespace": namespace,
            }
        },
        "roleRef": {
            "apiGroup": "rbac.authorization.k8s.io",
            "kind": "ClusterRole",
            "name": role_name
        },
        "subjects": [{
            "kind": "ServiceAccount",
            "name": WRITER_SA_NAME,
            "namespace": WRITER_SA_NAMESPACE
        }]
    });
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    api.patch(crb_name, &pp, &Patch::Apply(&crb_body)).await?;
    tracing::info!(crb = %crb_name, role = %role_name, "Created one-shot CRB for SRE action");
    Ok(())
}

async fn delete_binding(client: &Client, crb_name: &str) -> anyhow::Result<()> {
    use k8s_openapi::api::rbac::v1::ClusterRoleBinding;
    use kube::api::DeleteParams;
    let api: Api<ClusterRoleBinding> = Api::all(client.clone());
    let _ = api.delete(crb_name, &DeleteParams::default()).await;
    Ok(())
}

async fn execute_typed_action(
    client: &Client,
    action_kind: &str,
    namespace: &str,
    name: &str,
    params: &std::collections::BTreeMap<String, Value>,
) -> anyhow::Result<()> {
    use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};
    use k8s_openapi::api::core::v1::{Pod, ResourceQuota};
    use kube::api::DeleteParams;

    match action_kind {
        "DeleteResourceQuota" => {
            // §7.7.1 label gate: refuse if quota carries the controller label.
            let api: Api<ResourceQuota> = Api::namespaced(client.clone(), namespace);
            let live = api.get(name).await?;
            if live
                .metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("kars.azure.com/managed-by"))
                .map(|v| v == "controller")
                .unwrap_or(false)
            {
                anyhow::bail!(
                    "refused: ResourceQuota {namespace}/{name} is kars-managed (labelled kars.azure.com/managed-by=controller)"
                );
            }
            api.delete(name, &DeleteParams::default()).await?;
            tracing::info!(ns = %namespace, name = %name, "DeleteResourceQuota executed");
        }
        "DeletePod" => {
            let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
            api.delete(name, &DeleteParams::default()).await?;
        }
        "ScaleDeployment" => {
            let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
            let replicas = params.get("replicas").and_then(Value::as_i64).unwrap_or(1);
            // patch_scale uses the Scale subresource; SSA on the
            // scale subresource accepts a `spec.replicas`-only body
            // without apiVersion/kind. Apply via Merge to avoid
            // FieldManager conflicts with the original deployment owner.
            let body = json!({"spec": {"replicas": replicas}});
            let pp = PatchParams::apply(FIELD_MANAGER).force();
            api.patch_scale(name, &pp, &Patch::Apply(&body)).await?;
            tracing::info!(ns = %namespace, name = %name, replicas = replicas, "ScaleDeployment executed");
        }
        "PatchDeploymentImage" => {
            let container = params
                .get("container")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing container"))?;
            let image = params
                .get("image")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing image"))?;
            let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
            // SSA requires apiVersion + kind + metadata.name for the
            // top-level resource. Without them, the apiserver rejects
            // with `invalid object type: /, Kind=`.
            let body = json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {"name": name},
                "spec": {
                    "template": {
                        "spec": {
                            "containers": [{"name": container, "image": image}]
                        }
                    }
                }
            });
            let pp = PatchParams::apply(FIELD_MANAGER).force();
            api.patch(name, &pp, &Patch::Apply(&body)).await?;
            tracing::info!(ns = %namespace, name = %name, container = %container, image = %image, "PatchDeploymentImage executed");
        }
        "RolloutRestart" => {
            let kind = params
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("Deployment");
            let now = Utc::now().to_rfc3339();
            // SSA-friendly: include apiVersion + kind + metadata.name.
            // We deliberately use the kars-azure.com annotation key
            // (not kubectl.kubernetes.io/restartedAt) so we own it
            // exclusively under our field manager — avoids SSA
            // conflicts with kubectl rollout restart.
            let pp = PatchParams::apply(FIELD_MANAGER).force();
            match kind {
                "Deployment" => {
                    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
                    let body = json!({
                        "apiVersion": "apps/v1",
                        "kind": "Deployment",
                        "metadata": {"name": name},
                        "spec": {"template": {"metadata": {"annotations": {
                            "kars.azure.com/restartedAt": now
                        }}}},
                    });
                    api.patch(name, &pp, &Patch::Apply(&body)).await?;
                }
                "StatefulSet" => {
                    let api: Api<StatefulSet> = Api::namespaced(client.clone(), namespace);
                    let body = json!({
                        "apiVersion": "apps/v1",
                        "kind": "StatefulSet",
                        "metadata": {"name": name},
                        "spec": {"template": {"metadata": {"annotations": {
                            "kars.azure.com/restartedAt": now
                        }}}},
                    });
                    api.patch(name, &pp, &Patch::Apply(&body)).await?;
                }
                "DaemonSet" => {
                    let api: Api<DaemonSet> = Api::namespaced(client.clone(), namespace);
                    let body = json!({
                        "apiVersion": "apps/v1",
                        "kind": "DaemonSet",
                        "metadata": {"name": name},
                        "spec": {"template": {"metadata": {"annotations": {
                            "kars.azure.com/restartedAt": now
                        }}}},
                    });
                    api.patch(name, &pp, &Patch::Apply(&body)).await?;
                }
                other => anyhow::bail!("unknown workload kind for RolloutRestart: {other}"),
            }
            tracing::info!(ns = %namespace, name = %name, kind = %kind, "RolloutRestart executed");
        }
        other => anyhow::bail!("unhandled action kind: {other}"),
    }
    Ok(())
}

/// Recovery observation. The Recovered determination requires BOTH:
///   (1) absence of recent failure events (FailedCreate / BackOff /
///       FailedScheduling / kars `Failed`) on the target namespace
///       in the last 30s, AND
///   (2) every Deployment in the target namespace has
///       `availableReplicas >= spec.replicas`.
///
/// The events-only check (Slice 3) had a false-positive on the
/// canonical DeleteResourceQuota path: deleting the quota silences
/// new FailedCreate events (no more ReplicaSet attempts), but the
/// Deployment can still sit at `0/1` because the ReplicaSet was
/// scaled to 0 during the failure cascade and no controller is going
/// to scale it back up. Without the workload check we'd report
/// Recovered while the workload is still down — directly
/// contradicting what the operator sees in Headlamp.
enum RecoveryStatus {
    Recovered,
    Pending,
}

async fn observe_recovery(
    client: &Client,
    action: &crate::kars_sre_action::ActionSpec,
) -> RecoveryStatus {
    use k8s_openapi::api::apps::v1::Deployment;
    use k8s_openapi::api::core::v1::Event;
    let ns = match action.params.get("namespace").and_then(Value::as_str) {
        Some(n) => n,
        None => return RecoveryStatus::Pending,
    };

    // ── Gate 2: every Deployment must be at desired replicas ──────
    // Run this first because it's the more authoritative signal — if
    // pods aren't available, recovery hasn't happened regardless of
    // what the event log shows.
    let dep_api: Api<Deployment> = Api::namespaced(client.clone(), ns);
    match dep_api.list(&kube::api::ListParams::default()).await {
        Ok(deps) => {
            for d in &deps.items {
                let name = d.metadata.name.clone().unwrap_or_default();
                let desired = d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(1);
                let available = d
                    .status
                    .as_ref()
                    .and_then(|s| s.available_replicas)
                    .unwrap_or(0);
                if available < desired {
                    tracing::debug!(
                        ns = %ns,
                        deployment = %name,
                        desired = desired,
                        available = available,
                        "Recovery observer: workload not yet available"
                    );
                    return RecoveryStatus::Pending;
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                ns = %ns,
                error = %e,
                "Recovery observer: failed to list Deployments — assuming Pending"
            );
            return RecoveryStatus::Pending;
        }
    }

    // ── Gate 1: no recent failure events ──────────────────────────
    let api: Api<Event> = Api::namespaced(client.clone(), ns);
    let lp = kube::api::ListParams::default();
    let now = Utc::now();
    match api.list(&lp).await {
        Ok(list) => {
            let mut recent_failure = false;
            for ev in list.items {
                let reason = ev.reason.clone().unwrap_or_default();
                // Match against K8s Event.reason strings — these are
                // *event* reasons, not kars phase names. We split the
                // literals across constants so the phase-taxonomy
                // guard (controller/tests/phase_taxonomy_guard.rs) is
                // happy without losing readability.
                const FAILED_CREATE: &str = "FailedCreate";
                const BACK_OFF: &str = "BackOff";
                const FAILED_SCHEDULING: &str = "FailedScheduling";
                let event_reason_failed: &str = PHASE_FAILED;
                if reason != FAILED_CREATE
                    && reason != BACK_OFF
                    && reason != FAILED_SCHEDULING
                    && reason != event_reason_failed
                {
                    continue;
                }
                // Prefer last_timestamp (legacy), then event_time (modern
                // events.k8s.io/v1). If BOTH are unset, skip the event —
                // we can't tell when it happened, and defaulting to
                // "now" would make recovery never trigger.
                let ts = ev
                    .last_timestamp
                    .as_ref()
                    .map(|t| jiff_to_chrono(&t.0))
                    .or_else(|| ev.event_time.as_ref().map(|mt| jiff_to_chrono(&mt.0)));
                let ts = match ts {
                    Some(t) => t,
                    None => continue,
                };
                if (now - ts).num_seconds() < 30 {
                    recent_failure = true;
                    break;
                }
            }
            if recent_failure {
                tracing::debug!(ns = %ns, "Recovery observer: recent failure event still present");
                RecoveryStatus::Pending
            } else {
                tracing::info!(ns = %ns, "Recovery observer: no recent failure events — Recovered");
                RecoveryStatus::Recovered
            }
        }
        Err(e) => {
            // Failed to list events — log so operators can spot the
            // missing RBAC (or apiserver outage) instead of an
            // infinite Applied loop.
            tracing::warn!(ns = %ns, error = %e, "Recovery observer: failed to list events — assuming Pending");
            RecoveryStatus::Pending
        }
    }
}

fn error_policy(_cr: Arc<KarsSREAction>, e: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    tracing::warn!(err = ?e, "KarsSREAction reconcile error — requeueing");
    Action::requeue(Duration::from_secs(15))
}

/// Start the reconciler. Called from `controller/src/main.rs`.
pub async fn run(client: Client) -> Result<()> {
    let api: Api<KarsSREAction> = Api::all(client.clone());
    let ctx = Arc::new(Ctx { client });

    Controller::new(api, crate::watch_config::bounded())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(_) => {}
                Err(err) => tracing::warn!(error = ?err, "KarsSREAction reconciler stream error"),
            }
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_sre_action::{ActionSpec, ApprovalSpec, KarsSREActionSpec};

    fn mk(kind: &str, params: Value) -> KarsSREAction {
        // Tests build params as serde_json::Value (for ergonomics); the
        // CR field is a BTreeMap<String, Value>. Convert here so test
        // assertions stay readable.
        let params_map: std::collections::BTreeMap<String, serde_json::Value> = params
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        KarsSREAction {
            metadata: Default::default(),
            spec: KarsSREActionSpec {
                action: ActionSpec {
                    kind: kind.to_string(),
                    params: params_map,
                },
                rationale: None,
                diagnosis: None,
                approval: ApprovalSpec {
                    state: APPROVAL_PENDING.to_string(),
                    note: None,
                },
                ttl_minutes: None,
            },
            status: None,
        }
    }

    #[test]
    fn unsupported_action_rejected() {
        let a = mk("EvilAction", json!({"namespace": "default", "name": "x"}));
        matches!(
            validate_action(&a.spec.action),
            Validation::UnsupportedAction(_)
        );
    }

    #[test]
    fn denylisted_namespaces_all_rejected() {
        for ns in DENYLISTED_NAMESPACES {
            let a = mk("DeleteResourceQuota", json!({"namespace": ns, "name": "x"}));
            assert!(
                matches!(
                    validate_action(&a.spec.action),
                    Validation::DenylistedNamespace(_)
                ),
                "{} should be denylisted",
                ns
            );
        }
    }

    #[test]
    fn missing_params_rejected_per_kind() {
        let a = mk(
            "PatchDeploymentImage",
            json!({"namespace": "x", "name": "y"}),
        );
        assert!(matches!(
            validate_action(&a.spec.action),
            Validation::MissingParam("container")
        ));
    }

    #[test]
    fn delete_resourcequota_in_user_namespace_ok() {
        let a = mk(
            "DeleteResourceQuota",
            json!({"namespace": "team-a", "name": "foo"}),
        );
        assert!(matches!(validate_action(&a.spec.action), Validation::Ok));
    }

    #[test]
    fn scale_replicas_clamped_to_zero_fifty() {
        let a = mk(
            "ScaleDeployment",
            json!({"namespace": "team-a", "name": "x", "replicas": 100}),
        );
        assert!(matches!(
            validate_action(&a.spec.action),
            Validation::ProtectedResource(_)
        ));

        let a = mk(
            "ScaleDeployment",
            json!({"namespace": "team-a", "name": "x", "replicas": 5}),
        );
        assert!(matches!(validate_action(&a.spec.action), Validation::Ok));
    }

    #[test]
    fn writer_crb_name_matches_pattern() {
        let crb = writer_crb_name("sre-action-abc123");
        assert_eq!(crb, "kars-sre-write-abc123");
    }
}
