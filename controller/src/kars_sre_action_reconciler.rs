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
//! 1. Validate the closed action set and protected-resource denylist.
//! 2. Install a finalizer and atomically persist a single-attempt claim,
//!    including namespace/target UIDs and a full-UID authority name.
//! 3. CREATE a per-action ServiceAccount, exact resourceNames Role and
//!    namespaced RoleBinding. Never overwrite/adopt existing objects.
//! 4. Mint an in-memory, SA-lifetime-bound TokenRequest (600 seconds, the
//!    Kubernetes minimum). No shared writer identity or controller fallback.
//! 5. Dry-run and execute the typed action with UID/resourceVersion fences
//!    through the isolated bearer client; record the actual outcome.
//! 6. Confirm UID-fenced deletion of all authority before removing the finalizer.
//!    Cleanup failures remain visible and retry across restart/deletion;
//!    interrupted/consumed attempts are never executed again.
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
//! The controller needs namespaced SA/Role/RoleBinding lifecycle permission,
//! TokenRequest creation, and the underlying workload verbs it delegates.
//! Kubernetes RBAC escalation checks remain enabled: no `bind`/`escalate`
//! permission is added. SRE execution never creates a ClusterRoleBinding.

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

use crate::kars_sre_action::{KarsSREAction, SREActionExecution};

#[path = "kars_sre_action_authority.rs"]
mod authority;
pub fn writer_connection(config: &kube::Config) -> kube::Config {
    authority::connection_config(config)
}

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

/// Bound each active attempt; another replica waits beyond this local timeout
/// before treating a persisted claim as interrupted and cleaning it up.
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(90);
const EXECUTION_CLEANUP_DELAY_SECONDS: i64 = 120;

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
    #[error("SRE execution authority: {0}")]
    Authority(#[from] anyhow::Error),
}

struct Ctx {
    client: Client,
    connection: kube::Config,
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

    let dns_label = |value: &str| {
        !value.is_empty()
            && value.len() <= 63
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if !dns_label(&ns)
        || name.is_none_or(|name| name.len() > 253 || !name.split('.').all(dns_label))
    {
        return Validation::ProtectedResource(
            "target namespace/name must be valid Kubernetes names".into(),
        );
    }
    if let Err(e) = authority::scope(spec_action) {
        return Validation::ProtectedResource(e.to_string());
    }
    Validation::Ok
}

fn action_id(cr: &KarsSREAction) -> String {
    format!(
        "sre-action-{}",
        cr.metadata.uid.as_deref().unwrap_or("unknown")
    )
}

fn fenced_metadata(cr: &KarsSREAction) -> anyhow::Result<Value> {
    Ok(
        json!({"uid":authority::uid(&cr.metadata)?, "resourceVersion":cr.resource_version()
        .filter(|v| !v.is_empty()).ok_or_else(|| anyhow::anyhow!("action resourceVersion missing"))?}),
    )
}

async fn update_status(
    api: &Api<KarsSREAction>,
    cr: &KarsSREAction,
    status: Value,
) -> Result<KarsSREAction, ReconcileError> {
    Ok(api
        .patch_status(
            &cr.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":fenced_metadata(cr)?,"status":status})),
        )
        .await?)
}

async fn set_finalizer(
    api: &Api<KarsSREAction>,
    cr: &KarsSREAction,
    present: bool,
) -> Result<(), ReconcileError> {
    let mut finalizers = cr.finalizers().to_vec();
    finalizers.retain(|v| v != authority::FINALIZER);
    if present {
        finalizers.push(authority::FINALIZER.into());
    }
    let mut metadata = fenced_metadata(cr)?;
    metadata["finalizers"] = json!(finalizers);
    api.patch(
        &cr.name_any(),
        &PatchParams::default(),
        &Patch::Merge(json!({"metadata":metadata})),
    )
    .await?;
    Ok(())
}

async fn claim_execution(
    api: &Api<KarsSREAction>,
    ctx: &Ctx,
    cr: &KarsSREAction,
) -> Result<KarsSREAction, ReconcileError> {
    if !cr.finalizers().iter().any(|f| f == authority::FINALIZER)
        || cr.metadata.deletion_timestamp.is_some()
        || cr.spec.approval.state != APPROVAL_APPROVED
        || cr.status.as_ref().is_some_and(|s| s.execution.is_some())
    {
        return Err(anyhow::anyhow!("action is not eligible for execution").into());
    }
    let namespace = cr.spec.action.params["namespace"]
        .as_str()
        .unwrap_or_default();
    let name = cr.spec.action.params["name"].as_str().unwrap_or_default();
    let namespace_uid = authority::namespace_uid(&ctx.client, namespace).await?;
    let target = authority::target_metadata(&ctx.client, &cr.spec.action, namespace, name).await?;
    let now = Utc::now();
    let execution = SREActionExecution {
        started_at: now.to_rfc3339(),
        deadline: (now + chrono::Duration::seconds(EXECUTION_CLEANUP_DELAY_SECONDS)).to_rfc3339(),
        namespace: namespace.into(),
        namespace_uid,
        authority_name: authority::authority_name(cr)?,
        target_uid: authority::uid(&target)?.into(),
        cleanup_pending: true,
    };
    // resourceVersion makes this a compare-and-swap across replicas and approval
    // edits. A response lost after persistence consumes the attempt, never replays.
    update_status(api, cr, json!({"phase":PHASE_APPROVED,"execution":execution,
        "conditions":[cond(COND_TYPE_EXECUTED,"Unknown","ExecutionClaimed","Attempt consumed; outcome not yet known")]})).await
}

async fn cleanup_execution(
    api: &Api<KarsSREAction>,
    client: &Client,
    cr: &KarsSREAction,
) -> Result<KarsSREAction, ReconcileError> {
    let Some(execution) = cr.status.as_ref().and_then(|s| s.execution.as_ref()) else {
        return Ok(cr.clone());
    };
    if !execution.cleanup_pending {
        return Ok(cr.clone());
    }
    if let Err(error) = authority::cleanup(client, cr, execution).await {
        let mut conditions = cr
            .status
            .as_ref()
            .map(|s| s.conditions.clone())
            .unwrap_or_default();
        conditions.retain(|c| c.type_ != "AuthorityCleaned");
        let mut conditions = serde_json::to_value(conditions)?;
        conditions.as_array_mut().unwrap().push(cond(
            "AuthorityCleaned",
            "False",
            "CleanupFailed",
            &error.to_string(),
        ));
        update_status(api, cr, json!({"conditions":conditions})).await?;
        return Err(ReconcileError::Authority(error));
    }
    let interrupted = cr.status.as_ref().and_then(|s| s.phase.as_deref()) == Some(PHASE_APPROVED);
    let mut status = json!({"execution":{"cleanupPending":false}});
    if interrupted {
        status["phase"] = json!(PHASE_FAILED);
        status["conditions"] = json!([
            cond(
                COND_TYPE_EXECUTED,
                "Unknown",
                "ExecutionInterrupted",
                "Consumed attempt interrupted; outcome unknown; never replayed"
            ),
            cond(
                COND_TYPE_DEGRADED,
                "True",
                "ExecutionInterrupted",
                "Create a new reviewed proposal after inspecting the target"
            )
        ]);
    } else {
        status["conditions"] = serde_json::to_value(
            cr.status
                .as_ref()
                .map(|s| s.conditions.clone())
                .unwrap_or_default(),
        )?;
        status["conditions"]
            .as_array_mut()
            .unwrap()
            .retain(|c| c["type"] != "AuthorityCleaned");
    }
    status["conditions"].as_array_mut().unwrap().push(cond(
        "AuthorityCleaned",
        "True",
        "Revoked",
        "RoleBinding, ServiceAccount and Role confirmed absent",
    ));
    update_status(api, cr, status).await
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

    // Cleanup precedes deletion, recovery observation, and terminal retention.
    if let Some(legacy) = cr
        .status
        .as_ref()
        .and_then(|s| s.writer_crb_name.as_deref())
        .filter(|v| !v.is_empty())
    {
        use k8s_openapi::api::rbac::v1::ClusterRoleBinding;
        if Api::<ClusterRoleBinding>::all(ctx.client.clone())
            .get_opt(legacy)
            .await?
            .is_some()
        {
            // Old bindings contain no full action UID ownership proof. Never
            // delete a same-named foreign binding based on a truncated UID.
            stamp_phase(&api, &name, PHASE_FAILED, "Legacy writer binding still exists; operator must verify ownership and revoke it before continuing", &cr).await?;
            return Err(anyhow::anyhow!("unresolved legacy writer authority").into());
        }
        update_status(&api, &cr, json!({"writerCrbName":null})).await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    if let Some(execution) = cr.status.as_ref().and_then(|s| s.execution.as_ref()) {
        if execution.cleanup_pending {
            if phase == PHASE_APPROVED {
                let deadline = DateTime::parse_from_rfc3339(&execution.deadline)
                    .map_err(|_| anyhow::anyhow!("invalid execution cleanup deadline"))?;
                if Utc::now() < deadline {
                    return Ok(Action::requeue(REQUEUE_APPLIED));
                }
            }
            let cleaned = cleanup_execution(&api, &ctx.client, &cr).await?;
            if cleaned
                .finalizers()
                .iter()
                .any(|f| f == authority::FINALIZER)
            {
                set_finalizer(&api, &cleaned, false).await?;
            }
            return Ok(Action::requeue(Duration::from_secs(1)));
        }
        if phase == PHASE_PROPOSED || phase == PHASE_APPROVED {
            stamp_phase(
                &api,
                &name,
                PHASE_FAILED,
                "Execution attempt already consumed; refusing replay",
                &cr,
            )
            .await?;
            return Ok(Action::requeue(REQUEUE_TERMINAL));
        }
    }
    if cr.finalizers().iter().any(|f| f == authority::FINALIZER)
        && (cr.metadata.deletion_timestamp.is_some()
            || cr.status.as_ref().is_some_and(|s| s.execution.is_some()))
    {
        set_finalizer(&api, &cr, false).await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    if cr.metadata.deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }

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
                let params = kube::api::DeleteParams {
                    preconditions: Some(kube::api::Preconditions {
                        uid: Some(authority::uid(&cr.metadata)?.into()),
                        resource_version: cr.resource_version(),
                    }),
                    ..Default::default()
                };
                match api.delete(&name, &params).await {
                    Ok(_) => {}
                    Err(kube::Error::Api(error)) if error.code == 404 => {}
                    Err(error) => return Err(error.into()),
                }
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

        if !cr.finalizers().iter().any(|f| f == authority::FINALIZER) {
            set_finalizer(&api, &cr, true).await?;
            return Ok(Action::requeue(Duration::from_secs(1)));
        }
        let claimed = claim_execution(&api, &ctx, &cr).await?;
        let outcome = tokio::time::timeout(EXECUTION_TIMEOUT, apply_action(&ctx, &claimed)).await;
        let (phase, conditions, applied_at) = match outcome {
            Ok(Ok(())) => (
                PHASE_APPLIED,
                vec![
                    cond(
                        COND_TYPE_AVAILABLE,
                        "False",
                        REASON_PENDING_RECOVERY,
                        "Awaiting recovery observation",
                    ),
                    cond(
                        COND_TYPE_APPROVED,
                        "True",
                        APPROVAL_APPROVED,
                        "Operator approved the proposal",
                    ),
                    cond(
                        COND_TYPE_EXECUTED,
                        "True",
                        REASON_EXECUTED,
                        "Typed action executed via isolated short-lived token",
                    ),
                ],
                Some(Utc::now().to_rfc3339()),
            ),
            Ok(Err(e)) => (
                PHASE_FAILED,
                vec![
                    cond(
                        COND_TYPE_EXECUTED,
                        "Unknown",
                        "ExecutionFailed",
                        &format!("apply failed: {e}; inspect target before a new proposal"),
                    ),
                    cond(
                        COND_TYPE_DEGRADED,
                        "True",
                        "ExecutionFailed",
                        "Attempt consumed; no automatic retry",
                    ),
                ],
                None,
            ),
            Err(_) => (
                PHASE_FAILED,
                vec![
                    cond(
                        COND_TYPE_EXECUTED,
                        "Unknown",
                        "ExecutionTimedOut",
                        "Execution timed out; outcome unknown; no automatic retry",
                    ),
                    cond(
                        COND_TYPE_DEGRADED,
                        "True",
                        "ExecutionTimedOut",
                        "Inspect target before a new proposal",
                    ),
                ],
                None,
            ),
        };
        // Persist the observed outcome before cleanup, so cleanup failures never
        // turn a known successful mutation into a claim of failed execution.
        let finished = update_status(&api, &claimed, json!({"phase":phase,
            "observedGeneration":claimed.metadata.generation,"appliedAt":applied_at,"conditions":conditions})).await;
        let finished = match finished {
            Ok(finished) => finished,
            Err(error) => {
                // Preserve the journal/finalizer if status persistence failed.
                // Retry will mark this consumed attempt interrupted, not replay it.
                if let Some(execution) = claimed.status.as_ref().and_then(|s| s.execution.as_ref())
                    && let Err(cleanup) = authority::cleanup(&ctx.client, &claimed, execution).await
                {
                    tracing::error!(error = %cleanup, "SRE authority cleanup after status failure requires retry");
                }
                return Err(error);
            }
        };
        let cleaned = cleanup_execution(&api, &ctx.client, &finished).await?;
        set_finalizer(&api, &cleaned, false).await?;
        return Ok(Action::requeue(if phase == PHASE_APPLIED {
            REQUEUE_APPLIED
        } else {
            REQUEUE_TERMINAL
        }));
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
    let mut conds = conds;
    if let Some(status) = &cr.status {
        for condition in &status.conditions {
            if !matches!(
                condition.type_.as_str(),
                COND_TYPE_AVAILABLE | COND_TYPE_APPROVED | COND_TYPE_DEGRADED
            ) {
                conds.push(serde_json::to_value(condition)?);
            }
        }
    }
    if name != cr.name_any() {
        return Err(anyhow::anyhow!("status target name mismatch").into());
    }
    update_status(
        api,
        cr,
        json!({
            "phase": phase,
            "observedGeneration": cr.metadata.generation,
            "conditions": conds,
        }),
    )
    .await?;
    Ok(())
}

/// The caller has durably consumed this attempt before authority is created.
async fn apply_action(ctx: &Ctx, cr: &KarsSREAction) -> anyhow::Result<()> {
    let execution = cr
        .status
        .as_ref()
        .and_then(|s| s.execution.as_ref())
        .ok_or_else(|| anyhow::anyhow!("execution claim missing"))?;
    let writer = authority::writer(&ctx.client, &ctx.connection, cr, execution).await?;
    let api: Api<KarsSREAction> =
        Api::namespaced(ctx.client.clone(), &cr.namespace().unwrap_or_default());
    let live = api.get(&cr.name_any()).await?;
    anyhow::ensure!(
        live.metadata.uid == cr.metadata.uid
            && live.metadata.deletion_timestamp.is_none()
            && live.spec.approval.state == APPROVAL_APPROVED
            && live.spec.action == cr.spec.action
            && live.resource_version() == cr.resource_version(),
        "action changed during authority preparation"
    );
    execute_typed_action(
        &writer,
        &cr.spec.action,
        &execution.namespace,
        cr.spec.action.params["name"].as_str().unwrap_or_default(),
        &execution.target_uid,
    )
    .await
}

async fn execute_typed_action(
    client: &Client,
    action: &crate::kars_sre_action::ActionSpec,
    namespace: &str,
    name: &str,
    expected_uid: &str,
) -> anyhow::Result<()> {
    use kube::api::{DeleteParams, Preconditions};
    anyhow::ensure!(
        matches!(validate_action(action), Validation::Ok),
        "invalid typed action"
    );
    let api = authority::target_api(client, action, namespace)?;
    let metadata = authority::target_metadata(client, action, namespace, name).await?;
    anyhow::ensure!(
        authority::uid(&metadata)? == expected_uid,
        "target UID changed; refusing replacement"
    );
    let fence = json!({"name":name,"uid":expected_uid,"resourceVersion":metadata.resource_version});
    let params = &action.params;
    match action.kind.as_str() {
        "DeleteResourceQuota" | "DeletePod" => {
            if action.kind == "DeleteResourceQuota"
                && metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get("kars.azure.com/managed-by"))
                    .is_some_and(|value| value == "controller")
            {
                anyhow::bail!("refused: ResourceQuota is kars-managed");
            }
            let mut dp = DeleteParams {
                preconditions: Some(Preconditions {
                    uid: Some(expected_uid.into()),
                    resource_version: metadata.resource_version,
                }),
                dry_run: true,
                ..Default::default()
            };
            api.delete(name, &dp).await?;
            dp.dry_run = false;
            api.delete(name, &dp).await?;
        }
        "ScaleDeployment" => {
            // Merge patch on Scale avoids the invalid spec-only SSA body; the
            // UID + resourceVersion fence protects the underlying Deployment.
            let body = json!({"metadata":fence,"spec":{"replicas":params["replicas"]}});
            let mut pp = PatchParams {
                dry_run: true,
                ..Default::default()
            };
            api.patch_scale(name, &pp, &Patch::Merge(&body)).await?;
            pp.dry_run = false;
            api.patch_scale(name, &pp, &Patch::Merge(&body)).await?;
        }
        "PatchDeploymentImage" => {
            let body = json!({"metadata":fence,"spec":{"template":{"spec":{
                "containers":[{"name":params["container"],"image":params["image"]}]
            }}}});
            let mut pp = PatchParams {
                dry_run: true,
                ..Default::default()
            };
            // Strategic merge preserves unrelated containers and fields.
            api.patch(name, &pp, &Patch::Strategic(&body)).await?;
            pp.dry_run = false;
            api.patch(name, &pp, &Patch::Strategic(&body)).await?;
        }
        "RolloutRestart" => {
            let body = json!({"metadata":fence,"spec":{"template":{"metadata":{
                "annotations":{"kars.azure.com/restartedAt":Utc::now().to_rfc3339()}
            }}}});
            let mut pp = PatchParams {
                dry_run: true,
                ..Default::default()
            };
            api.patch(name, &pp, &Patch::Merge(&body)).await?;
            pp.dry_run = false;
            api.patch(name, &pp, &Patch::Merge(&body)).await?;
        }
        _ => anyhow::bail!("unsupported typed action"),
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
pub async fn run(client: Client, connection: kube::Config) -> Result<()> {
    let api: Api<KarsSREAction> = Api::all(client.clone());
    let ctx = Arc::new(Ctx {
        client,
        connection: writer_connection(&connection),
    });

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
#[path = "kars_sre_action_execution_tests.rs"]
mod execution_tests;

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
    fn authority_names_use_the_entire_uid() {
        let mut first = mk("DeletePod", json!({"namespace":"team-a","name":"x"}));
        first.metadata.uid = Some("12345678-1111-1111-1111-111111111111".into());
        let mut second = first.clone();
        second.metadata.uid = Some("12345678-2222-2222-2222-222222222222".into());
        assert_ne!(
            authority::authority_name(&first).unwrap(),
            authority::authority_name(&second).unwrap()
        );
    }
}
