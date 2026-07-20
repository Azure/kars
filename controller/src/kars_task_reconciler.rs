// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsTask` reconciler — kars Bridge V0, slice 1.
//!
//! Watches `KarsTask` CRs and, for each:
//!
//! 1. Ensures the cleanup finalizer.
//! 2. Validates the trust envelope (defence-in-depth behind CEL admission)
//!    and computes its stable `envelopeDigest`.
//! 3. Stamps `status.phase`, `status.observedGeneration`, the `Ready`
//!    condition, and `status.envelopeDigest`, preserving any `lineage`
//!    written by the delegation-minting path (next slice).
//!
//! This reconciler is intentionally side-effect-free on the cluster for V0:
//! it materializes verifiable *status* (the digest a Governance Receipt binds
//! to), not yet a governed sandbox. Sandbox materialization and
//! capability-attenuating child minting build on this in the following slices.

use anyhow::Result;
use futures::StreamExt;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use crate::crd::KarsSandbox;
use crate::kars_task::{KarsTask, KarsTaskStatus, TIER_MAX, TIER_MIN};
use crate::kars_team::KarsTeam;
use crate::status::conditions::{self, TYPE_READY, reason as cond_reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_PENDING, PHASE_READY};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_TASK;
const FINALIZER: &str = "kars.azure.com/karstask-cleanup";
/// Server-Side Apply field manager for Governance Receipt writes.
const RECEIPT_FIELD_MANAGER: &str = "kars-controller/receipt";

/// Cluster-wide retention default ConfigMap (namespace = KARS_NAMESPACE /
/// kars-system) and the key on it holding the default TTL in seconds. Read via
/// the Bridge's GET/PUT /api/operator/retention-policy. Absent or `0` means
/// "never auto-delete" — the safe, backward-compatible default.
const RETENTION_POLICY_CM: &str = "kars-retention-policy";
const RETENTION_POLICY_KEY: &str = "defaultTtlSeconds";

const REQUEUE_OK: Duration = Duration::from_secs(300);

/// A child waiting on its parent requeues quickly so it converges to `Ready`
/// promptly once the parent reconciles, rather than waiting a full cycle.
const REQUEUE_PENDING: Duration = Duration::from_secs(10);
/// A launched task must observe the sandbox transition to Running promptly.
const REQUEUE_LAUNCHING: Duration = Duration::from_secs(2);

/// A launched, executing task polls its sandbox router on a tight loop so
/// in-flight capability requests surface in the inbox within seconds and
/// approved grants take effect promptly.
const REQUEUE_RUNNING: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl ReconcileError {
    fn class(&self) -> &'static str {
        match self {
            ReconcileError::Kube(_) => "kube_api",
            ReconcileError::SerdeJson(_) => "serde",
        }
    }
}

/// Result of validating an envelope: either valid, or a human-readable
/// reason the task is `Degraded`. Kept pure so it is unit-testable without
/// a cluster.
enum EnvelopeCheck {
    Valid,
    Invalid(String),
}

/// Validate the trust-envelope invariants. This mirrors the CEL admission
/// rules as a second line of defence — a CR that somehow reached the
/// reconciler with a bad envelope is surfaced as `Degraded` rather than
/// silently digested.
fn check_envelope(task: &KarsTask) -> EnvelopeCheck {
    let e = &task.spec.envelope;
    if e.tier < TIER_MIN || e.tier > TIER_MAX {
        return EnvelopeCheck::Invalid(format!("tier {} out of range 1..5", e.tier));
    }
    if e.authority_ceiling < TIER_MIN || e.authority_ceiling > TIER_MAX {
        return EnvelopeCheck::Invalid(format!(
            "authorityCeiling {} out of range 1..5",
            e.authority_ceiling
        ));
    }
    if e.authority_ceiling > e.tier {
        return EnvelopeCheck::Invalid(format!(
            "authorityCeiling {} exceeds tier {} (a task cannot grant a child more authority than it holds)",
            e.authority_ceiling, e.tier
        ));
    }
    if e.delegation_depth < 0 {
        return EnvelopeCheck::Invalid(format!(
            "delegationDepth {} must be >= 0",
            e.delegation_depth
        ));
    }
    EnvelopeCheck::Valid
}

struct Ctx {
    client: Client,
    /// Receipt-signing identity, loaded once at startup. Used to emit a signed
    /// Governance Receipt for each governance-`Ready` task.
    signer: crate::providers::signing::ReceiptSigner,
}

async fn reconcile(task: Arc<KarsTask>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = task.name_any();
    let ns = task.namespace().unwrap_or_else(|| "default".into());
    let tasks: Api<KarsTask> = Api::namespaced(ctx.client.clone(), &ns);

    // Deletion: drop the finalizer and let the API server reap the object.
    // There is nothing cluster-side to clean up in V0.
    if task.metadata.deletion_timestamp.is_some() {
        if has_finalizer(&task) {
            // Sweep the mission ConfigMaps the mesh peer wrote in the controller
            // namespace (output / artifacts / trace / review). They carry no
            // ownerReference (they live cross-namespace from the KarsTask), so
            // without this they orphan on delete — for EVERY delete path (kubectl,
            // GC, force-delete), not only the Bridge's own delete-mission sweep.
            {
                use k8s_openapi::api::core::v1::ConfigMap;
                let sys = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
                let cms: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &sys);
                for cm in [
                    format!("kars-mission-output-{name}"),
                    format!("kars-mission-artifacts-{name}"),
                    format!("kars-mission-trace-{name}"),
                    format!("kars-mission-review-{name}"),
                ] {
                    let _ = cms.delete(&cm, &kube::api::DeleteParams::default()).await;
                }
            }
            // Drop our finalizer with a merge patch. A server-side *apply* that
            // sets `finalizers: []` does not reliably remove a finalizer the
            // apiserver no longer attributes to this manager (it 400s with
            // "name must be provided"), which would strand the object in
            // Terminating forever and leak its sandbox. A merge patch replaces
            // the array deterministically.
            let patch = json!({ "metadata": { "finalizers": drop_finalizer(&task) } });
            tasks
                .patch(&name, &PatchParams::default(), &Patch::Merge(patch))
                .await?;
        }
        return Ok(Action::await_change());
    }

    // Ensure the finalizer before doing any work, so deletion is observable.
    if !has_finalizer(&task) {
        let mut finalizers = task.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(FINALIZER.to_string());
        let patch = json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsTask",
            "metadata": { "name": name, "finalizers": finalizers },
        });
        tasks
            .patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(patch),
            )
            .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    // Retention TTL (mirrors Kubernetes Job.spec.ttlSecondsAfterFinished): once
    // a task's deliverable has landed, an operator may want it auto-cleaned
    // after a window instead of accumulating forever. Two steps, each cheap and
    // idempotent:
    //   1. Stamp status.deliveredAt ONCE, the first reconcile that observes the
    //      mission-output ConfigMap (the harness-neutral "this task produced a
    //      terminal result" signal) — never touched again.
    //   2. Once stamped, if the effective TTL (this task's own override, else
    //      the cluster-wide default) has elapsed, delete the task. Deletion
    //      re-enters this same function's deletion-timestamp branch above,
    //      which already sweeps the mission-output/artifacts/trace/review
    //      ConfigMaps — so retention reuses the exact same cleanup path a
    //      human "Delete mission" click takes.
    if let Some(action) = reconcile_retention(&task, &tasks, &ctx, &name, &ns).await? {
        return Ok(action);
    }

    let generation = task.metadata.generation;
    let prior_conditions = task
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let prior_ready = conditions::find(&prior_conditions, TYPE_READY);

    // Resolve delegation: a task with `spec.parentRef` is a child whose
    // envelope must attenuate its parent's, and whose lineage the controller
    // mints from the parent's ancestry. A root task has no parent and empty
    // lineage. The controller is the *sole* writer of lineage.
    let delegation = resolve_delegation(&tasks, &task).await?;

    let mut new_status = match check_envelope(&task) {
        EnvelopeCheck::Invalid(why) => degraded_status(
            prior_ready,
            generation,
            &format!("invalid trust envelope: {why}"),
            delegation.lineage(),
        ),
        EnvelopeCheck::Valid => match delegation {
            Delegation::Root => ready_status(
                prior_ready,
                generation,
                task.spec.envelope.digest(),
                Vec::new(),
            ),
            Delegation::ParentMissing { parent } => {
                tracing::warn!(karstask = %name, ns = %ns, %parent, "KarsTask parent not found");
                degraded_status(
                    prior_ready,
                    generation,
                    &format!("parentRef `{parent}` not found in namespace"),
                    Vec::new(),
                )
            }
            Delegation::ParentNotReady { parent } => {
                tracing::info!(karstask = %name, ns = %ns, %parent, "KarsTask parent not yet ready — waiting");
                pending_status(
                    prior_ready,
                    generation,
                    &format!("waiting for parent `{parent}` to become ready"),
                )
            }
            Delegation::Child {
                lineage,
                violations,
            } if violations.is_empty() => {
                tracing::info!(karstask = %name, ns = %ns, depth = lineage.len(), "KarsTask delegated child ready");
                ready_status(
                    prior_ready,
                    generation,
                    task.spec.envelope.digest(),
                    lineage,
                )
            }
            Delegation::Child {
                lineage,
                violations,
            } => {
                let why = violations
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                tracing::warn!(karstask = %name, ns = %ns, %why, "KarsTask delegation amplifies authority — rejected");
                degraded_status(
                    prior_ready,
                    generation,
                    &format!("delegation amplifies parent authority: {why}"),
                    lineage,
                )
            }
        },
    };
    // `deliveredAt` is write-once retention state owned by this reconciler.
    // Carry it across normal status refreshes so SSA does not erase it and make
    // the retention path stamp it again on every reconcile.
    preserve_delivered_at(&task, &mut new_status);

    // Execution bridge (§20 launch gate). Only a governance-Ready task may
    // execute. Launch materializes a governed sandbox; un-launch tears it down.
    // Any execution error is surfaced (Degraded) but never fails the whole
    // reconcile — the governance status is already durable.
    reconcile_execution(&ctx.client, &ns, &task, &mut new_status).await;

    let status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "status": new_status,
    });
    tasks
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(status_patch),
        )
        .await?;

    // Governance Receipt (Inc 3). A governance-`Ready` task — one whose
    // envelope validated and (if delegated) attenuated its parent — gets a
    // signed, independently-verifiable receipt. A `Degraded` task never does:
    // there is no validated authority to attest. The receipt is deterministic,
    // so this is idempotent across requeues.
    reconcile_receipt(&ctx.client, &ns, &task, &new_status, &ctx.signer).await;

    // Governed self-promotion (§12): when the mission requests a higher tier,
    // open a human `KarsApproval` and — only once approved — widen its envelope.
    // Widening is controller-only; a mission cannot self-escalate.
    process_task_promotion(&ctx.client, &ns, &task).await;

    // In-flight capability gaps (§14): always apply any grants a human has
    // already approved (cheap + idempotent), and — while the agent is live —
    // poll the sandbox router for new blocked hosts / capability requests and
    // surface each as a Pending `KarsApproval`. A request never grants anything
    // by itself; only a human decision creates the EgressApproval grant.
    let executing =
        new_status.execution_phase.as_deref() == Some(crate::status::phase::PHASE_SANDBOX_RUNNING);
    process_access_requests(&ctx.client, &ns, &task, executing).await;

    Ok(Action::requeue(requeue_for_status(&new_status)))
}

fn requeue_for_status(status: &KarsTaskStatus) -> Duration {
    if status.phase.as_deref() == Some(PHASE_PENDING) {
        return REQUEUE_PENDING;
    }
    match status.execution_phase.as_deref() {
        Some(crate::status::phase::PHASE_SANDBOX_LAUNCHING) => REQUEUE_LAUNCHING,
        Some(crate::status::phase::PHASE_SANDBOX_RUNNING) => REQUEUE_RUNNING,
        _ => REQUEUE_OK,
    }
}

/// Retention TTL check/enforcement, run at the top of every reconcile (after
/// the finalizer is ensured). Returns `Some(action)` only when this reconcile
/// must stop (a fresh deliveredAt stamp needs one prompt reread, or the task was
/// deleted). A delivered task that is not yet TTL-expired still proceeds through
/// normal execution reconciliation so `launch=false` tears down its sandbox.
async fn reconcile_retention(
    task: &KarsTask,
    tasks: &Api<KarsTask>,
    ctx: &Ctx,
    name: &str,
    ns: &str,
) -> Result<Option<Action>, ReconcileError> {
    use k8s_openapi::api::core::v1::ConfigMap;

    let already_delivered_at = task.status.as_ref().and_then(|s| s.delivered_at.clone());

    if already_delivered_at.is_none() {
        // Not yet stamped — check whether a deliverable has landed. The
        // mission-output ConfigMap is the harness-neutral "this task produced a
        // terminal result" signal (written on success AND on a genuine
        // terminal error/timeout alike — either way the task is done running).
        let cms: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), ns);
        let output = cms
            .get_opt(&format!("kars-mission-output-{name}"))
            .await?
            .and_then(|cm| cm.data);
        let Some(data) = output else {
            // Still running (or never launched) — nothing to do.
            return Ok(None);
        };
        let delivered_at = data
            .get("finishedAt")
            .cloned()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let status_patch = json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsTask",
            "status": { "deliveredAt": delivered_at },
        });
        tasks
            .patch_status(
                name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(status_patch),
            )
            .await?;
        tracing::debug!(karstask = %name, ns = %ns, "retention: stamped deliveredAt");
        // Requeue promptly so the TTL check (below, on the NEXT reconcile) can
        // run against the now-stamped timestamp without waiting a full cycle.
        return Ok(Some(Action::requeue(Duration::from_secs(5))));
    }

    // Already delivered — check the effective TTL.
    let delivered_at = already_delivered_at.expect("checked above");
    let Ok(delivered_ts) = chrono::DateTime::parse_from_rfc3339(&delivered_at) else {
        return Ok(None);
    };
    let effective_ttl = effective_retention_ttl_seconds(ctx, task).await;
    if effective_ttl <= 0 {
        return Ok(None); // retention disabled for this task
    }
    let age = chrono::Utc::now().signed_duration_since(delivered_ts.with_timezone(&chrono::Utc));
    if age.num_seconds() < effective_ttl {
        return Ok(None);
    }
    tracing::info!(
        karstask = %name,
        ns = %ns,
        delivered_at = %delivered_at,
        ttl_seconds = effective_ttl,
        "retention: TTL elapsed — deleting delivered task"
    );
    tasks
        .delete(name, &kube::api::DeleteParams::default())
        .await?;
    Ok(Some(Action::await_change()))
}

/// This task's own retention override, else the cluster-wide default read
/// from the `kars-retention-policy` ConfigMap (key `defaultTtlSeconds`).
/// Absent/unparseable/`<= 0` on both means retention is disabled (never
/// auto-delete) — the safe default that preserves pre-retention behavior.
async fn effective_retention_ttl_seconds(ctx: &Ctx, task: &KarsTask) -> i64 {
    if let Some(ttl) = task.spec.retention_ttl_seconds {
        return ttl;
    }
    use k8s_openapi::api::core::v1::ConfigMap;
    let sys = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let cms: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &sys);
    cms.get_opt(RETENTION_POLICY_CM)
        .await
        .ok()
        .flatten()
        .and_then(|cm| cm.data)
        .and_then(|d| {
            d.get(RETENTION_POLICY_KEY)
                .and_then(|v| v.parse::<i64>().ok())
        })
        .unwrap_or(0)
}

/// Outcome of resolving a task's `parentRef`.
enum Delegation {
    /// No `parentRef` — this is a root task.
    Root,
    /// `parentRef` set but the parent does not exist.
    ParentMissing { parent: String },
    /// `parentRef` resolved but the parent is not yet governance-`Ready` (no
    /// validated envelope digest). A child must not be granted authority
    /// against a parent whose own authority isn't established — it waits.
    ParentNotReady { parent: String },
    /// `parentRef` resolved; carries the minted lineage and any attenuation
    /// violations (empty = valid subset).
    Child {
        lineage: Vec<String>,
        violations: Vec<crate::kars_task::EnvelopeViolation>,
    },
}

impl Delegation {
    /// The lineage to persist for this outcome (empty unless a child resolved).
    fn lineage(&self) -> Vec<String> {
        match self {
            Delegation::Child { lineage, .. } => lineage.clone(),
            _ => Vec::new(),
        }
    }
}

/// Resolve `spec.parentRef`: fetch the parent, mint lineage from its ancestry,
/// and compute whether this task's envelope attenuates the parent's.
async fn resolve_delegation(
    tasks: &Api<KarsTask>,
    task: &KarsTask,
) -> Result<Delegation, ReconcileError> {
    let Some(parent_ref) = task.spec.parent_ref.as_ref() else {
        return Ok(Delegation::Root);
    };
    let parent = match tasks.get_opt(&parent_ref.name).await? {
        Some(p) => p,
        None => {
            return Ok(Delegation::ParentMissing {
                parent: parent_ref.name.clone(),
            });
        }
    };

    // Parent-readiness gate: a child may only be granted authority once the
    // parent's own authority is established (governance-`Ready` with a stamped
    // envelope digest). Otherwise the subset relation would be checked against
    // an unvalidated — possibly degraded or in-flux — parent envelope.
    if !task_is_ready(&parent) {
        return Ok(Delegation::ParentNotReady {
            parent: parent_ref.name.clone(),
        });
    }

    // Minted lineage = parent's ancestry + the parent itself. The controller
    // owns this; a client-supplied lineage is ignored.
    let mut lineage = parent
        .status
        .as_ref()
        .map(|s| s.lineage.clone())
        .unwrap_or_default();
    lineage.push(parent.name_any());

    // Full attenuation over the effective authority the sandbox enforces
    // (envelope numeric/ref axes + effective tool policy + effective egress).
    let violations = crate::kars_task::spec_attenuation_violations(&task.spec, &parent.spec);
    Ok(Delegation::Child {
        lineage,
        violations,
    })
}

/// A task is governance-`Ready` when its `Ready` condition is `True` and it
/// carries a stamped envelope digest — the proof its authority was validated.
fn task_is_ready(task: &KarsTask) -> bool {
    let Some(status) = task.status.as_ref() else {
        return false;
    };
    let digest_ok = status
        .envelope_digest
        .as_ref()
        .is_some_and(|d| !d.is_empty());
    let ready_ok = status
        .conditions
        .iter()
        .flatten()
        .any(|c| c.type_ == TYPE_READY && c.status == cond_status::TRUE);
    digest_ok && ready_ok
}

/// Build a `Ready` status with the given digest + lineage.
fn ready_status(
    prior_ready: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>,
    generation: Option<i64>,
    digest: String,
    lineage: Vec<String>,
) -> KarsTaskStatus {
    let ready = conditions::preserve_transition_time(
        prior_ready,
        TYPE_READY,
        cond_status::TRUE,
        cond_reason::RECONCILED,
        "trust envelope validated and digested",
        generation,
    );
    KarsTaskStatus {
        phase: Some(PHASE_READY.to_string()),
        observed_generation: generation,
        conditions: Some(vec![ready]),
        envelope_digest: Some(digest),
        lineage,
        ..Default::default()
    }
}

fn preserve_delivered_at(task: &KarsTask, status: &mut KarsTaskStatus) {
    status.delivered_at = task.status.as_ref().and_then(|s| s.delivered_at.clone());
}

/// Build a `Degraded` status with no digest — the receipt must never bind to
/// authority that didn't validate or that amplified its parent.
/// Build a `Pending` status for a child whose parent is not yet ready — a
/// transient, non-degraded waiting state (no digest, no execution) that
/// converges once the parent reconciles to `Ready`.
fn pending_status(
    prior_ready: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>,
    generation: Option<i64>,
    message: &str,
) -> KarsTaskStatus {
    let ready = conditions::preserve_transition_time(
        prior_ready,
        TYPE_READY,
        cond_status::FALSE,
        cond_reason::DEPENDENCY_MISSING,
        message,
        generation,
    );
    KarsTaskStatus {
        phase: Some(PHASE_PENDING.to_string()),
        observed_generation: generation,
        conditions: Some(vec![ready]),
        envelope_digest: None,
        lineage: Vec::new(),
        ..Default::default()
    }
}

fn degraded_status(
    prior_ready: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition>,
    generation: Option<i64>,
    message: &str,
    lineage: Vec<String>,
) -> KarsTaskStatus {
    let ready = conditions::preserve_transition_time(
        prior_ready,
        TYPE_READY,
        cond_status::FALSE,
        cond_reason::SPEC_INVALID,
        message,
        generation,
    );
    KarsTaskStatus {
        phase: Some(PHASE_DEGRADED.to_string()),
        observed_generation: generation,
        conditions: Some(vec![ready]),
        envelope_digest: None,
        lineage,
        ..Default::default()
    }
}

/// Reconcile the execution bridge (§20 launch gate) and fold the result into
/// `status`. Rules:
/// - Only a governance-`Ready` task may execute.
/// - `execution.launch == true` → materialize a governed `KarsSandbox` and
///   reflect its phase as `executionPhase` (Launching/Running/Degraded).
/// - Otherwise → ensure any prior sandbox is torn down; `executionPhase=Idle`.
///
/// Execution errors degrade *execution* only; the governance status stands.
async fn reconcile_execution(
    client: &kube::Client,
    ns: &str,
    task: &KarsTask,
    status: &mut KarsTaskStatus,
) {
    let launched = task
        .spec
        .execution
        .as_ref()
        .map(|e| e.launch)
        .unwrap_or(false);
    let governance_ready = status.phase.as_deref() == Some(PHASE_READY);

    if launched && governance_ready {
        match crate::kars_task_execution::materialize(client, ns, task).await {
            Ok(outcome) => {
                status.execution_phase = Some(outcome.phase);
                status.sandbox_ref = Some(crate::mcp_server::LocalObjectRef {
                    name: outcome.sandbox_name,
                });
                status.execution_detail = Some(outcome.detail);
            }
            Err(e) => {
                tracing::warn!(karstask = %task.name_any(), ns = %ns, error = %e, "KarsTask execution materialize failed");
                status.execution_phase = Some(PHASE_DEGRADED.to_string());
                status.execution_detail = Some(format!("failed to materialize sandbox: {e}"));
            }
        }
    } else {
        // Not launched (or not Ready): ensure no sandbox lingers from a prior
        // launch, and report Idle.
        if task
            .status
            .as_ref()
            .and_then(|s| s.sandbox_ref.as_ref())
            .is_some()
            && let Err(e) = crate::kars_task_execution::teardown(client, ns, task).await
        {
            tracing::warn!(karstask = %task.name_any(), ns = %ns, error = %e, "KarsTask execution teardown failed");
        }
        status.execution_phase = Some("Idle".to_string());
        status.sandbox_ref = None;
        status.execution_detail = None;
    }
}

/// Emit (or retract) the Governance Receipt for a task.
///
/// - Governance-`Ready` (an `envelopeDigest` is present) → build the in-toto
///   Statement, sign it with DSSE/Ed25519, and Server-Side-Apply a
///   `KarsReceipt` owned by the task. Deterministic ⇒ idempotent.
/// - Otherwise → ensure no stale receipt remains; a `Degraded` task has no
///   validated authority to attest.
///
/// Receipt errors are surfaced in logs but never fail the reconcile — the
/// governance status is already durable.
async fn reconcile_receipt(
    client: &kube::Client,
    ns: &str,
    task: &KarsTask,
    status: &KarsTaskStatus,
    signer: &crate::providers::signing::ReceiptSigner,
) {
    use crate::kars_approval::KarsApproval;
    use crate::kars_receipt::{
        KarsReceipt, approval_facts, build_spec, build_statement, canonical_json,
    };

    let name = task.name_any();
    let receipts: Api<KarsReceipt> = Api::namespaced(client.clone(), ns);

    // Gather the human decisions (HITL approvals) bound to this task, so every
    // steer is recorded in the signed receipt. Best-effort: a list failure
    // must not block the receipt (it just omits approvals this pass).
    let approvals: Api<KarsApproval> = Api::namespaced(client.clone(), ns);
    let task_approvals = match approvals.list(&ListParams::default()).await {
        Ok(list) => list
            .items
            .into_iter()
            .filter(|a| a.spec.task_ref.name == name)
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::debug!(karstask = %name, ns = %ns, error = %e, "could not list KarsApprovals for receipt");
            Vec::new()
        }
    };
    let facts = approval_facts(&task_approvals);

    // Gather the completeness-floor posture from cluster state (best-effort —
    // a read failure yields a conservative "not enforced" observation, never a
    // false positive). This is what makes the receipt's completeness claim
    // concrete and re-derivable by an auditor.
    // A sandbox is materialized only when the task was launched AND a sandbox
    // ref exists — the honest precondition for claiming the egress-guard datapath
    // ruleset is bound (an un-launched governance-only receipt must not claim it).
    let sandbox_materialized = task
        .spec
        .execution
        .as_ref()
        .map(|e| e.launch)
        .unwrap_or(false)
        && status.sandbox_ref.is_some();
    let completeness = gather_completeness(client, ns, &name, sandbox_materialized).await;

    let Some(statement) = build_statement(task, status, &signer.key_id, &facts, completeness)
    else {
        // No digest → no receipt. Retract any prior one.
        match receipts
            .delete(&name, &kube::api::DeleteParams::default())
            .await
        {
            Ok(_) => {}
            Err(kube::Error::Api(ae)) if ae.code == 404 => {}
            Err(e) => {
                tracing::warn!(karstask = %name, ns = %ns, error = %e, "failed to retract stale KarsReceipt");
            }
        }
        return;
    };

    let digest = status
        .envelope_digest
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let payload = canonical_json(&statement);
    let dsse = signer.sign_statement(&payload);
    let claims = statement.predicate.claims.clone();
    let spec = build_spec(&name, &digest, &signer.key_id, dsse, claims);

    // Owner reference to the task so the receipt is GC'd with it.
    let owner = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "name": name,
        "uid": task.metadata.uid.clone().unwrap_or_default(),
        "controller": true,
        "blockOwnerDeletion": true,
    });
    let receipt = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsReceipt",
        "metadata": {
            "name": name,
            "namespace": ns,
            "ownerReferences": [owner],
        },
        "spec": spec,
    });

    if let Err(e) = receipts
        .patch(
            &name,
            &PatchParams::apply(RECEIPT_FIELD_MANAGER).force(),
            &Patch::Apply(&receipt),
        )
        .await
    {
        tracing::warn!(karstask = %name, ns = %ns, error = %e, "failed to emit KarsReceipt");
        return;
    }

    // Enter the receipt in the hash-chained inclusion log (cross-receipt
    // tamper-evidence). Best-effort: a log failure must not block the receipt,
    // which is already durable and individually signed.
    let payload_sha = crate::kars_receipt_log::sha256_hex(&payload);
    let log_ref = format!("{ns}/{name}");
    let inclusion = match crate::kars_receipt_log::append(client, &log_ref, &payload_sha).await {
        Ok((entry, appended)) => {
            // Only when a NEW entry was actually written do we re-publish the
            // signed checkpoint and witness it, and only then do we echo the
            // receipt status. On the idempotent no-op path (the common case for
            // a stable task requeued every few minutes) we skip ALL of these
            // writes — otherwise every requeue of every task rewrites the shared
            // checkpoint/witness ConfigMaps and the receipt status, flooding
            // etcd with revisions until it hits its NOSPACE quota.
            if appended {
                match crate::kars_receipt_log::read_chain(client).await {
                    Ok(chain) => {
                        match crate::kars_receipt_log::publish_checkpoint(client, signer, &chain)
                            .await
                        {
                            Ok(checkpoint) => {
                                // Independent transparency witness co-signs the head.
                                if let Ok(witness) =
                                    crate::providers::signing::load_or_create_witness(client).await
                                    && let Err(e) = crate::kars_receipt_log::witness_checkpoint(
                                        client,
                                        &witness,
                                        &chain,
                                        &checkpoint,
                                    )
                                    .await
                                {
                                    tracing::warn!(karstask = %name, ns = %ns, error = %e, "failed to witness receipt checkpoint");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(karstask = %name, ns = %ns, error = %e, "failed to publish receipt checkpoint");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!(karstask = %name, ns = %ns, error = %e, "could not read chain for checkpoint");
                    }
                }
            }
            Some((entry, appended))
        }
        Err(e) => {
            tracing::warn!(karstask = %name, ns = %ns, error = %e, "failed to enter receipt in inclusion log");
            None
        }
    };

    // Informational status echo (unsigned). Only written when the receipt's
    // inclusion actually changed (a new log entry was appended) — on the
    // idempotent no-op path we leave the prior echo (and its `issuedAt`) intact
    // so we never churn the object. `issuedAt` is therefore the time of the
    // last *material* receipt change, not of every requeue.
    if let Some((entry, true)) = &inclusion {
        let status_obj = json!({
            "issuedAt": chrono::Utc::now().to_rfc3339(),
            "observedTaskGeneration": task.metadata.generation,
            "inclusionSeq": entry.seq as i64,
            "inclusionEntryHash": entry.entry_hash,
        });
        let status_patch = json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsReceipt",
            "status": status_obj,
        });
        if let Err(e) = receipts
            .patch_status(
                &name,
                &PatchParams::apply(RECEIPT_FIELD_MANAGER).force(),
                &Patch::Apply(&status_patch),
            )
            .await
        {
            tracing::debug!(karstask = %name, ns = %ns, error = %e, "KarsReceipt status echo failed (non-fatal)");
        }
        tracing::info!(karstask = %name, ns = %ns, key_id = %signer.key_id, "Governance Receipt emitted");
    }
}

/// Observe which completeness-floor controls (design note §24b) are enforced
/// on the cluster, for binding into the receipt. Best-effort: any read error
/// yields a conservative `false` (we never claim a control is enforced unless
/// we positively observed it). The runtime egress-guard iptables hash and the
/// eBPF witness are intentionally NOT gathered here — they are V1/V2.
async fn gather_completeness(
    client: &kube::Client,
    ns: &str,
    task_name: &str,
    sandbox_materialized: bool,
) -> crate::kars_receipt::PredicateCompleteness {
    use k8s_openapi::api::admissionregistration::v1::ValidatingAdmissionPolicy;
    use k8s_openapi::api::core::v1::ConfigMap;
    use k8s_openapi::api::networking::v1::NetworkPolicy;

    // Witness ConfigMaps live in the controller namespace.
    fn cms_system(client: &kube::Client) -> Api<ConfigMap> {
        let sys = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
        Api::namespaced(client.clone(), &sys)
    }

    let vaps: Api<ValidatingAdmissionPolicy> = Api::all(client.clone());
    let vap_present = |name: &str, list: &[ValidatingAdmissionPolicy]| -> bool {
        list.iter()
            .any(|p| p.metadata.name.as_deref() == Some(name))
    };
    let vap_list = vaps
        .list(&ListParams::default())
        .await
        .map(|l| l.items)
        .unwrap_or_default();

    // A cluster-wide default-deny egress NetworkPolicy is installed by the
    // operator chart in kars-system; treat its presence there as the floor.
    let nps: Api<NetworkPolicy> = Api::namespaced(client.clone(), "kars-system");
    let default_deny_egress = nps
        .list(&ListParams::default())
        .await
        .map(|l| {
            l.items.iter().any(|np| {
                np.spec
                    .as_ref()
                    .and_then(|s| s.policy_types.as_ref())
                    .is_some_and(|t| t.iter().any(|pt| pt == "Egress"))
            })
        })
        .unwrap_or(false);

    // V1 token/cost-audit binding: read the durable run records for this task.
    // The router-metered token totals land on `kars-mission-output-<task>` and
    // the per-round/per-tool execution trace on `kars-mission-trace-<task>`.
    // Their presence (with a real total) is the re-derivable audit chain; their
    // absence simply means the task hasn't run yet (the binding is honestly
    // unset, never faked).
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    let run_total_tokens = cms
        .get_opt(&format!("kars-mission-output-{task_name}"))
        .await
        .ok()
        .flatten()
        .and_then(|cm| cm.data)
        .and_then(|d| d.get("totalTokens").and_then(|t| t.parse::<u64>().ok()))
        .filter(|t| *t > 0);
    let trace_event_count = cms
        .get_opt(&format!("kars-mission-trace-{task_name}"))
        .await
        .ok()
        .flatten()
        .and_then(|cm| cm.data)
        .and_then(|d| d.get("eventCount").and_then(|c| c.parse::<u64>().ok()));
    let token_cost_audit_bound = run_total_tokens.is_some();

    // V1 egress-guard ruleset binding: hash the authored iptables ruleset the
    // egress-guard enforces for a task-materialized (non-SRE) sandbox. Only
    // meaningful once a sandbox was actually materialized (launched + a sandbox
    // ref exists) — an un-launched task has no datapath to bind, so we must not
    // pin a hash or claim it is bound. (The node-level eBPF witness that the
    // kernel applied it is V2.)
    let egress_guard_ruleset_hash =
        sandbox_materialized.then(|| crate::reconciler::egress_guard_ruleset_hash(false));

    // V1 transparency witness: an independent witness co-signs the receipt-log
    // checkpoint (kars-receipt-witness ConfigMap). Presence of a verified witness
    // co-signature binds "the log isn't forked" into the receipt.
    let witness_cm = cms_system(client)
        .get_opt("kars-receipt-witness")
        .await
        .ok()
        .flatten();
    let witness_key_id = witness_cm
        .as_ref()
        .and_then(|cm| cm.data.as_ref())
        .and_then(|d| d.get("witnessKeyId").cloned())
        .filter(|s| !s.is_empty());
    let transparency_witnessed = witness_key_id.is_some();

    // V2 kernel-datapath witness: the eBPF/datapath witness aggregator
    // (deploy/ebpf-witness) cross-checks kernel-observed egress (Inspektor
    // Gadget DNS/TCP traces) against each sandbox's declared allowlist and
    // publishes one JSON document (`witness.json`) with a per-sandbox verdict
    // in `kars-datapath-witness`: COMPLIANT (Strict mode, every observed host
    // was declared — the kernel actually enforced the authored posture),
    // BEYOND-DECLARED (Strict mode, but the kernel observed an undeclared
    // host — a genuine completeness gap), or LEARN (the sandbox isn't in
    // Strict egress mode yet, so enforcement isn't active — not proof of
    // anything). Only COMPLIANT binds this axis; the sandbox being absent
    // from the witness (not yet observed) or any other verdict leaves it
    // honestly unset, never faked.
    let kernel_datapath_witnessed = cms_system(client)
        .get_opt("kars-datapath-witness")
        .await
        .ok()
        .flatten()
        .and_then(|cm| cm.data)
        .and_then(|d| d.get("witness.json").cloned())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|doc| doc.get("sandboxes").cloned())
        .and_then(|sbs| sbs.as_array().cloned())
        .map(|sbs| {
            sbs.iter().any(|s| {
                s.get("sandbox").and_then(|v| v.as_str()) == Some(task_name)
                    && s.get("verdict").and_then(|v| v.as_str()) == Some("COMPLIANT")
            })
        })
        .unwrap_or(false);

    crate::kars_receipt::PredicateCompleteness {
        task_namespace_floor_vap: vap_present("kars-task-namespace-floor", &vap_list),
        exec_ban_vap: vap_present("kars-sandbox-exec-ban", &vap_list),
        posture_lock_vap: vap_present("kars-sandbox-posture-lock", &vap_list),
        default_deny_egress,
        floor_enforced: false,
        token_cost_audit_bound,
        run_total_tokens,
        trace_event_count,
        egress_guard_ruleset_bound: sandbox_materialized,
        egress_guard_ruleset_hash,
        transparency_witnessed,
        witness_key_id,
        kernel_datapath_witnessed,
    }
    .with_rollup()
}

/// True iff the task carries our cleanup finalizer.
fn has_finalizer(task: &KarsTask) -> bool {
    task.metadata
        .finalizers
        .as_ref()
        .is_some_and(|f| f.iter().any(|s| s == FINALIZER))
}

/// Return the finalizer list with our finalizer removed.
fn drop_finalizer(task: &KarsTask) -> Vec<String> {
    task.metadata
        .finalizers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s != FINALIZER)
        .collect()
}

fn error_policy(task: Arc<KarsTask>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsTask", error.class());
    tracing::warn!(
        karstask = %task.name_any(),
        error_class = error.class(),
        error = %error,
        "KarsTask reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

/// Process a governed per-mission promotion (§12), mirroring the standing-team
/// promotion but scoped to a single `KarsTask`. When `spec.requested_tier`
/// exceeds the task's current envelope tier, ensure a human `KarsApproval`
/// (`tierRaise`) owned by this task exists; once that approval is `Approved`,
/// widen the task's envelope to the requested tier. Widening is controller-only
/// (enforced by the envelope-write VAP), and only an approval THIS task owns is
/// honored — closing the "request + self-approve via the BFF" escalation.
async fn process_task_promotion(client: &Client, ns: &str, task: &KarsTask) {
    use crate::kars_approval::{ApprovalAction, KarsApproval};

    let Some(target) = task.spec.requested_tier else {
        return;
    };
    let current = task.spec.envelope.tier;
    if target <= current
        || !(crate::kars_task::TIER_MIN..=crate::kars_task::TIER_MAX).contains(&target)
    {
        return; // nothing to promote (or out of range)
    }

    let task_name = task.name_any();
    let approval_name = format!("{task_name}-promote-t{target}");
    let approvals: Api<KarsApproval> = Api::namespaced(client.clone(), ns);

    // If the approval exists and is Approved (and owned by this task), widen.
    if let Ok(Some(appr)) = approvals.get_opt(&approval_name).await {
        ensure_task_approval_owner(&approvals, &approval_name, task).await;
        let controller_owned = appr.metadata.owner_references.as_ref().is_some_and(|refs| {
            refs.iter()
                .any(|r| r.kind == "KarsTask" && r.name == task_name && r.controller == Some(true))
        });
        if !controller_owned {
            tracing::warn!(karstask = %task_name, "ignoring promote approval not owned by this task (forgery guard)");
            return;
        }
        let approved = appr
            .status
            .as_ref()
            .and_then(|s| s.phase.as_deref())
            .map(|p| p == "Approved")
            .unwrap_or(false);
        if approved && target > task.spec.envelope.tier {
            let tasks: Api<KarsTask> = Api::namespaced(client.clone(), ns);
            // Merge-patch only the two envelope fields so the other envelope
            // settings are preserved (an SSA apply would drop unmanaged siblings).
            let patch = json!({
                "spec": {
                    "envelope": { "tier": target, "authorityCeiling": target },
                    "requestedTier": null,
                }
            });
            let _ = tasks
                .patch(&task_name, &PatchParams::default(), &Patch::Merge(patch))
                .await;
            tracing::info!(karstask = %task_name, tier = target, "mission promotion approved — envelope widened (requestedTier cleared)");
        }
        return;
    }

    // Otherwise open the human approval (idempotent create).
    let owner = json!([{
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "name": task_name,
        "uid": task.uid().unwrap_or_default(),
        "controller": true,
        "blockOwnerDeletion": true,
    }]);
    let appr = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsApproval",
        "metadata": {
            "name": approval_name,
            "ownerReferences": owner,
            "labels": { "kars.azure.com/promote-task": task_name },
            "annotations": task_owner_annotations(task),
        },
        "spec": {
            "taskRef": { "name": task_name },
            "action": ApprovalAction {
                kind: "tierRaise".into(),
                summary: format!("Promote mission '{task_name}' from Tier {current} to Tier {target}"),
                detail: Some(format!(
                    "This mission is requesting a wider authority envelope (Tier {target}). \
                     Approving grants it up to Tier {target} authority for the rest of its run."
                )),
                requested_tier: Some(target),
            },
        },
    });
    let _ = approvals
        .patch(
            &approval_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(appr),
        )
        .await;
}

/// In-flight capability requests (§14): poll the task's router and every spawned
/// child router for
/// (a) hosts the forward-proxy blocked and (b) capabilities the agent explicitly
/// requested via `POST /v1/access-request`; surface each novel one as a Pending
/// `KarsApproval` owned by this task; and — only once a human approves an
/// egress request — create the `EgressApproval` grant that actually widens the
/// allowlist. Nothing here grants without a human decision.
async fn process_access_requests(client: &Client, ns: &str, task: &KarsTask, executing: bool) {
    let Some(root_sandbox) = task
        .status
        .as_ref()
        .and_then(|s| s.sandbox_ref.as_ref())
        .map(|r| r.name.clone())
    else {
        return; // not launched yet — no router to poll
    };

    // Consume side always runs: apply any egress requests a human has approved,
    // even after the agent has gone Idle, so a grant still lands.
    consume_approved_egress(client, ns, task, &root_sandbox).await;

    // Request side only while the agent is live — a finished run raises nothing.
    if !executing {
        return;
    }

    let run_started_unix = run_started_unix(task);
    let sandboxes = access_request_sandboxes(client, ns, &root_sandbox).await;
    push_decisions_to_routers(client, ns, task, &root_sandbox, &sandboxes).await;
    futures::future::join_all(
        sandboxes.iter().map(|sandbox| {
            poll_sandbox_access_requests(client, ns, task, sandbox, run_started_unix)
        }),
    )
    .await;
}

async fn access_request_sandboxes(client: &Client, ns: &str, root: &str) -> Vec<String> {
    let mut names = vec![root.to_string()];
    let sandboxes: Api<KarsSandbox> = Api::namespaced(client.clone(), ns);
    if let Ok(list) = sandboxes.list(&ListParams::default()).await {
        loop {
            let mut added = false;
            for child in &list.items {
                if child.metadata.deletion_timestamp.is_some() {
                    continue;
                }
                let Some(parent) = child.labels().get("kars.azure.com/parent") else {
                    continue;
                };
                let name = child.name_any();
                if names.contains(parent) && !names.contains(&name) {
                    names.push(name);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
    }
    names
}

async fn poll_sandbox_access_requests(
    client: &Client,
    ns: &str,
    task: &KarsTask,
    sandbox: &str,
    run_started_unix: Option<u64>,
) {
    let token = match crate::status::router_confirmation_io::read_admin_token(client, sandbox).await
    {
        Ok(Some(token)) => token,
        _ => return,
    };
    let base = crate::status::router_confirmation::router_admin_url(sandbox);
    let Ok(http) = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return;
    };

    // (a) Blocked egress attempts → egress-kind approvals.
    if let Some(entries) =
        fetch_json_entries(&http, &base, "/internal/egress/blocked", &token).await
    {
        for e in entries {
            let last_seen_unix = e.get("last_seen_unix").and_then(serde_json::Value::as_u64);
            if run_started_unix
                .is_some_and(|started| last_seen_unix.is_some_and(|last_seen| last_seen < started))
            {
                continue;
            }
            let host = e.get("host").and_then(|v| v.as_str()).unwrap_or("").trim();
            let port = e.get("port").and_then(|v| v.as_u64()).unwrap_or(443) as u16;
            if host.is_empty() || is_runtime_bootstrap_host(host) {
                continue;
            }
            ensure_egress_approval(
                client,
                ns,
                task,
                sandbox,
                host,
                port,
                "The agent was blocked from reaching this host while working on the mission.",
            )
            .await;
        }
    }

    // (b) Explicit capability requests → mapped approvals.
    if let Some(entries) =
        fetch_json_entries(&http, &base, "/internal/access-requests", &token).await
    {
        for r in entries {
            let last_seen_unix = r.get("last_seen_unix").and_then(serde_json::Value::as_u64);
            if run_started_unix
                .is_some_and(|started| last_seen_unix.is_some_and(|last_seen| last_seen < started))
            {
                continue;
            }
            let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("").trim();
            let target = r
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let reason = r
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if kind.is_empty() {
                continue;
            }
            if kind == "egress" {
                let port = r.get("port").and_then(|v| v.as_u64()).unwrap_or(443) as u16;
                if !target.is_empty() {
                    let why = if reason.is_empty() {
                        "The agent requested egress to this host to complete the mission."
                    } else {
                        reason
                    };
                    ensure_egress_approval(client, ns, task, sandbox, target, port, why).await;
                }
            } else {
                let tier = r.get("tier").and_then(|v| v.as_i64()).map(|t| t as i32);
                ensure_capability_approval(client, ns, task, sandbox, kind, target, reason, tier)
                    .await;
            }
        }
    }
}

fn run_started_unix(task: &KarsTask) -> Option<u64> {
    let nonce = task.annotations().get("kars.azure.com/run-requested")?;
    if let Some(raw_nanos) = nonce.strip_prefix("run-") {
        let nanos = raw_nanos.parse::<u128>().ok()?;
        return u64::try_from(nanos / 1_000_000_000).ok();
    }
    let seconds = nonce.rsplit('-').next()?.parse::<u64>().ok()?;
    (1_000_000_000..10_000_000_000)
        .contains(&seconds)
        .then_some(seconds)
}

fn is_runtime_bootstrap_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("registry.npmjs.org")
}

/// GET a `/internal/*` router surface and return its `entries` array, if any.
async fn fetch_json_entries(
    http: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
) -> Option<Vec<serde_json::Value>> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let resp = http.get(&url).bearer_auth(token).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("entries").and_then(|v| v.as_array()).cloned()
}

/// A short, stable, RFC1123-safe suffix for deterministic (deduplicated) object
/// names. djb2 over the input, hex-encoded.
fn stable_suffix(input: &str) -> String {
    let mut h: u64 = 5381;
    for b in input.as_bytes() {
        h = h.wrapping_mul(33) ^ u64::from(*b);
    }
    format!("{h:x}")
}

const REQ_KIND_ANN: &str = "kars.azure.com/req-kind";
const REQ_TARGET_ANN: &str = "kars.azure.com/req-target";
const REQ_PORT_ANN: &str = "kars.azure.com/req-port";
const REQ_TTL_ANN: &str = "kars.azure.com/req-ttl";
const REQ_SANDBOX_ANN: &str = "kars.azure.com/req-sandbox";
/// Marks an egress approval whose grant has already been materialised, so the
/// consumer is idempotent and never re-creates the EgressApproval.
const REQ_GRANTED_ANN: &str = "kars.azure.com/req-granted";
/// Marks an approval whose decision has already been mirrored to the router, so
/// the agent-facing poll reflects it exactly once.
const REQ_PUSHED_ANN: &str = "kars.azure.com/req-pushed";

fn task_owner_ref(task: &KarsTask) -> serde_json::Value {
    json!([{
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsTask",
        "name": task.name_any(),
        "uid": task.uid().unwrap_or_default(),
        "controller": true,
        "blockOwnerDeletion": true,
    }])
}

async fn control_request_owner_ref(
    client: &Client,
    ns: &str,
    task: &KarsTask,
) -> serde_json::Value {
    let team_name = task.annotations().get("kars.azure.com/team").cloned();
    if let Some(team_name) = team_name {
        let teams: Api<KarsTeam> = Api::namespaced(client.clone(), ns);
        if let Ok(Some(team)) = teams.get_opt(&team_name).await {
            return json!([{
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "KarsTeam",
                "name": team.name_any(),
                "uid": team.uid().unwrap_or_default(),
                "controller": true,
                "blockOwnerDeletion": true,
            }]);
        }
    }
    task_owner_ref(task)
}

async fn control_approval_owned_by_task_or_team(
    client: &Client,
    ns: &str,
    task: &KarsTask,
    approval: &crate::kars_approval::KarsApproval,
) -> bool {
    let Some(refs) = approval.metadata.owner_references.as_ref() else {
        return false;
    };
    let task_name = task.name_any();
    if refs.iter().any(|owner| {
        owner.kind == "KarsTask"
            && owner.name == task_name
            && owner.controller == Some(true)
            && task
                .metadata
                .uid
                .as_ref()
                .is_none_or(|uid| &owner.uid == uid)
    }) {
        return true;
    }

    let Some(team_name) = task.annotations().get("kars.azure.com/team") else {
        return false;
    };
    let teams: Api<KarsTeam> = Api::namespaced(client.clone(), ns);
    let Ok(Some(team)) = teams.get_opt(team_name).await else {
        return false;
    };
    refs.iter().any(|owner| {
        owner.kind == "KarsTeam"
            && owner.name == *team_name
            && owner.controller == Some(true)
            && team
                .metadata
                .uid
                .as_ref()
                .is_none_or(|uid| &owner.uid == uid)
    })
}

fn task_owner_annotations(task: &KarsTask) -> serde_json::Map<String, serde_json::Value> {
    let mut annotations = serde_json::Map::new();
    for key in ["kars.azure.com/owner-sub", "kars.azure.com/owner-name"] {
        if let Some(value) = task
            .annotations()
            .get(key)
            .filter(|value| !value.trim().is_empty())
        {
            annotations.insert(key.into(), json!(value));
        }
    }
    annotations
}

async fn ensure_task_approval_owner(
    approvals: &Api<crate::kars_approval::KarsApproval>,
    name: &str,
    task: &KarsTask,
) {
    let annotations = task_owner_annotations(task);
    if annotations.is_empty() {
        return;
    }
    let patch = json!({"metadata": {"annotations": annotations}});
    let _ = approvals
        .patch(name, &PatchParams::default(), &Patch::Merge(patch))
        .await;
}

/// Idempotently open a Pending `KarsApproval` (kind `egress`) for a host the
/// agent needs. Machine-readable host/port live in annotations so the consumer
/// can materialise the grant without parsing prose.
async fn ensure_egress_approval(
    client: &Client,
    ns: &str,
    task: &KarsTask,
    sandbox: &str,
    host: &str,
    port: u16,
    reason: &str,
) {
    use crate::kars_approval::{ApprovalAction, KarsApproval};
    let task_name = task.name_any();
    let name = format!(
        "{task_name}-eg-{}",
        stable_suffix(&format!("{sandbox}:{host}:{port}"))
    );
    let approvals: Api<KarsApproval> = Api::namespaced(client.clone(), ns);
    // Don't reopen an already-decided (or existing) request.
    if let Ok(Some(_)) = approvals.get_opt(&name).await {
        ensure_task_approval_owner(&approvals, &name, task).await;
        return;
    }
    let mut approval_annotations = task_owner_annotations(task);
    approval_annotations.insert(REQ_KIND_ANN.into(), json!("egress"));
    approval_annotations.insert(REQ_TARGET_ANN.into(), json!(host));
    approval_annotations.insert(REQ_PORT_ANN.into(), json!(port.to_string()));
    approval_annotations.insert(REQ_SANDBOX_ANN.into(), json!(sandbox));
    let owner_references = control_request_owner_ref(client, ns, task).await;
    let appr = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsApproval",
        "metadata": {
            "name": name,
            "ownerReferences": owner_references,
            "labels": { "kars.azure.com/req-task": task_name, "kars.azure.com/req-kind": "egress" },
            "annotations": approval_annotations,
        },
        "spec": {
            "taskRef": { "name": task_name },
            "action": ApprovalAction {
                kind: "egress".into(),
                summary: format!("Allow '{sandbox}' to reach {host}:{port}"),
                detail: Some(format!(
                    "{reason} Approving adds {host}:{port} to sandbox '{sandbox}'s egress \
                     allowlist for a limited window so the agent can proceed."
                )),
                requested_tier: None,
            },
        },
    });
    let _ = approvals
        .patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(appr),
        )
        .await;
}

enum TypedTierScope {
    Task(String),
    Team(String),
}

impl TypedTierScope {
    fn name(&self) -> &str {
        match self {
            Self::Task(name) | Self::Team(name) => name,
        }
    }
}

async fn typed_tier_scope(
    client: &Client,
    ns: &str,
    task: &KarsTask,
    target: i32,
) -> Option<TypedTierScope> {
    if !(crate::kars_task::TIER_MIN..=crate::kars_task::TIER_MAX).contains(&target) {
        tracing::warn!(
            karstask = %task.name_any(),
            tier = target,
            "ignoring typed tier request outside the supported range"
        );
        return None;
    }

    if let Some(team_name) = task.annotations().get("kars.azure.com/team") {
        let teams: Api<KarsTeam> = Api::namespaced(client.clone(), ns);
        match teams.get_opt(team_name).await {
            Ok(Some(_)) => return Some(TypedTierScope::Team(team_name.clone())),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    karstask = %task.name_any(),
                    team = %team_name,
                    tier = target,
                    %error,
                    "failed to resolve typed tier request owner"
                );
                return None;
            }
        }
    }

    Some(TypedTierScope::Task(task.name_any()))
}

async fn record_typed_tier_promotion(
    client: &Client,
    ns: &str,
    task: &KarsTask,
    scope: &TypedTierScope,
    target: i32,
) -> bool {
    let patch = json!({ "spec": { "requestedTier": target } });
    let result = match scope {
        TypedTierScope::Task(task_name) => {
            let tasks: Api<KarsTask> = Api::namespaced(client.clone(), ns);
            tasks
                .patch(task_name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map(|_| ())
        }
        TypedTierScope::Team(team_name) => {
            let teams: Api<KarsTeam> = Api::namespaced(client.clone(), ns);
            teams
                .patch(team_name, &PatchParams::default(), &Patch::Merge(patch))
                .await
                .map(|_| ())
        }
    };
    if let Err(error) = result {
        tracing::warn!(
            karstask = %task.name_any(),
            scope = %scope.name(),
            tier = target,
            %error,
            "failed to record typed tier request"
        );
        return false;
    }
    true
}

/// Idempotently open a Pending `KarsApproval` for a non-egress capability
/// (tool/skill/mcp/command/permission/tier). Tier requests enter the existing
/// task/team promotion state machine; other capability decisions are mirrored
/// to the active run without pretending the controller materialized a grant.
async fn ensure_capability_approval(
    client: &Client,
    ns: &str,
    task: &KarsTask,
    sandbox: &str,
    kind: &str,
    target: &str,
    reason: &str,
    tier: Option<i32>,
) {
    use crate::kars_approval::{ApprovalAction, KarsApproval};
    let task_name = task.name_any();
    let promotion_scope = if kind == "tier" {
        let Some(target_tier) = tier else {
            tracing::warn!(karstask = %task_name, "ignoring typed tier request without a tier");
            return;
        };
        typed_tier_scope(client, ns, task, target_tier).await
    } else {
        None
    };
    if kind == "tier" && promotion_scope.is_none() {
        return;
    }
    let name = match (kind, tier, promotion_scope.as_ref()) {
        ("tier", Some(target_tier), Some(scope)) => {
            format!("{}-promote-t{target_tier}", scope.name())
        }
        _ => {
            let key = format!("{sandbox}:{kind}:{target}");
            format!("{task_name}-cap-{}", stable_suffix(&key))
        }
    };
    let approvals: Api<KarsApproval> = Api::namespaced(client.clone(), ns);
    let (approval_kind, summary) = match kind {
        "tier" => (
            "tierRaise".to_string(),
            match tier {
                Some(t) => format!("Raise the mission's autonomy to Tier {t}"),
                None => "Raise the mission's autonomy tier".to_string(),
            },
        ),
        "tool" => ("toolCall".to_string(), format!("Grant the tool '{target}'")),
        "clarification" => ("clarification".to_string(), target.to_string()),
        other => (
            "custom".to_string(),
            format!("Grant {other} access: '{target}'"),
        ),
    };
    let detail = if reason.is_empty() {
        format!("The agent requested {kind} '{target}' to complete the mission.")
    } else {
        format!("{reason} (requested {kind}: '{target}')")
    };
    let mut approval_annotations = task_owner_annotations(task);
    approval_annotations.insert(REQ_KIND_ANN.into(), json!(kind));
    approval_annotations.insert(REQ_TARGET_ANN.into(), json!(target));
    approval_annotations.insert(REQ_SANDBOX_ANN.into(), json!(sandbox));
    let owner_references = control_request_owner_ref(client, ns, task).await;
    let appr = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsApproval",
        "metadata": {
            "name": name,
            "ownerReferences": owner_references,
            "labels": { "kars.azure.com/req-task": task_name, "kars.azure.com/req-kind": kind },
            "annotations": approval_annotations,
        },
        "spec": {
            "taskRef": { "name": task_name },
            "action": ApprovalAction {
                kind: approval_kind,
                summary,
                detail: Some(detail),
                requested_tier: tier,
            },
        },
    });
    if let Err(error) = approvals
        .patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(appr),
        )
        .await
    {
        tracing::warn!(
            karstask = %task_name,
            request_kind = %kind,
            approval = %name,
            %error,
            "failed to create typed capability approval"
        );
        return;
    }
    if let (Some(scope), Some(target_tier)) = (promotion_scope.as_ref(), tier) {
        record_typed_tier_promotion(client, ns, task, scope, target_tier).await;
    }
}

/// For every `Approved` egress `KarsApproval` owned by this task that hasn't yet
/// been materialised, create the `EgressApproval` grant that widens the sandbox
/// allowlist, then annotate the approval so we never re-create the grant.
async fn consume_approved_egress(client: &Client, ns: &str, task: &KarsTask, sandbox: &str) {
    use crate::egress_approval::EgressApproval;
    use crate::kars_approval::{KarsApproval, PHASE_APPROVED};
    let task_name = task.name_any();
    let approvals: Api<KarsApproval> = Api::namespaced(client.clone(), ns);
    let lp = ListParams::default().labels(&format!("kars.azure.com/req-task={task_name}"));
    let Ok(list) = approvals.list(&lp).await else {
        return;
    };
    for appr in list.items {
        // Only egress requests that a human has approved.
        let is_egress = appr
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(REQ_KIND_ANN))
            .map(|k| k == "egress")
            .unwrap_or(false);
        if !is_egress {
            continue;
        }
        let approved = appr
            .status
            .as_ref()
            .and_then(|s| s.phase.as_deref())
            .map(|p| p == PHASE_APPROVED)
            .unwrap_or(false);
        if !approved {
            continue;
        }
        if !control_approval_owned_by_task_or_team(client, ns, task, &appr).await {
            continue;
        }
        // Already materialised?
        let anns = appr.metadata.annotations.clone().unwrap_or_default();
        if anns.get(REQ_GRANTED_ANN).is_some() {
            continue;
        }
        let host = match anns.get(REQ_TARGET_ANN) {
            Some(h) if !h.is_empty() => h.clone(),
            _ => continue,
        };
        let port: u16 = anns
            .get(REQ_PORT_ANN)
            .and_then(|p| p.parse().ok())
            .unwrap_or(443);
        let ttl = anns
            .get(REQ_TTL_ANN)
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "PT8H".into());
        let grant_sandbox = anns
            .get(REQ_SANDBOX_ANN)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| sandbox.to_string());
        let appr_name = appr.name_any();
        let grant_name = format!(
            "{task_name}-egg-{}",
            stable_suffix(&format!("{grant_sandbox}:{host}:{port}"))
        );
        let egress: Api<EgressApproval> = Api::namespaced(client.clone(), ns);
        let grant = json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "EgressApproval",
            "metadata": {
                "name": grant_name,
                "ownerReferences": task_owner_ref(task),
                "labels": { "kars.azure.com/req-task": task_name },
            },
            "spec": {
                "sandbox": grant_sandbox,
                "hosts": [ { "host": host, "port": port } ],
                "reason": format!("Approved via Bridge inbox for mission '{task_name}'"),
                "ttl": ttl,
            },
        });
        if egress
            .patch(
                &grant_name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(grant),
            )
            .await
            .is_ok()
        {
            // Stamp the approval so we don't re-create the grant every requeue.
            let stamp = json!({ "metadata": { "annotations": { REQ_GRANTED_ANN: grant_name } } });
            let _ = approvals
                .patch(&appr_name, &PatchParams::default(), &Patch::Merge(stamp))
                .await;
            tracing::info!(
                karstask = %task_name, sandbox = %grant_sandbox, host = %host, port = port, grant = %grant_name,
                "egress request approved — allowlist grant created"
            );
        }
    }
}

/// Mirror decided (`Approved`/`Denied`) `KarsApproval`s owned by this task back
/// to the sandbox router, so the agent's `GET /v1/access-requests` poll shows
/// the outcome and it can proceed (or stop) instead of blindly retrying. Each
/// decision is pushed exactly once (stamped with `REQ_PUSHED_ANN`).
fn approved_request_is_materialized(
    kind: &str,
    requested_tier: Option<i32>,
    task_tier: i32,
    egress_granted: bool,
) -> bool {
    match kind {
        "egress" => egress_granted,
        "tier" => requested_tier.is_some_and(|target| task_tier >= target),
        _ => true,
    }
}

async fn push_decisions_to_routers(
    client: &Client,
    ns: &str,
    task: &KarsTask,
    root_sandbox: &str,
    request_sandboxes: &[String],
) {
    use crate::kars_approval::{KarsApproval, PHASE_APPROVED, PHASE_DENIED};
    let task_name = task.name_any();
    let approvals: Api<KarsApproval> = Api::namespaced(client.clone(), ns);
    let lp = ListParams::default().labels(&format!("kars.azure.com/req-task={task_name}"));
    let Ok(list) = approvals.list(&lp).await else {
        return;
    };
    let Ok(http) = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return;
    };
    for appr in list.items {
        if !control_approval_owned_by_task_or_team(client, ns, task, &appr).await {
            continue;
        }
        let anns = appr.metadata.annotations.clone().unwrap_or_default();
        if anns.get(REQ_PUSHED_ANN).is_some() {
            continue; // already mirrored
        }
        let Some(kind) = anns.get(REQ_KIND_ANN) else {
            continue;
        };
        let phase = appr.status.as_ref().and_then(|s| s.phase.as_deref());
        let verdict = match phase {
            Some(PHASE_APPROVED) => "approved",
            Some(PHASE_DENIED) => "denied",
            _ => continue, // still pending
        };
        if verdict == "approved"
            && !approved_request_is_materialized(
                kind,
                appr.spec.action.requested_tier,
                task.spec.envelope.tier,
                anns.get(REQ_GRANTED_ANN).is_some(),
            )
        {
            continue;
        }
        let target = anns.get(REQ_TARGET_ANN).cloned().unwrap_or_default();
        let request_sandbox = anns
            .get(REQ_SANDBOX_ANN)
            .filter(|value| !value.trim().is_empty())
            .map(String::as_str)
            .unwrap_or(root_sandbox);
        let reason = appr
            .spec
            .decision
            .as_ref()
            .and_then(|decision| decision.reason.clone());
        let destinations = if kind == "tier" {
            request_sandboxes.to_vec()
        } else {
            vec![request_sandbox.to_string()]
        };
        let mut updated = false;
        for sandbox in destinations {
            let token =
                match crate::status::router_confirmation_io::read_admin_token(client, &sandbox)
                    .await
                {
                    Ok(Some(token)) => token,
                    _ => continue,
                };
            let base = crate::status::router_confirmation::router_admin_url(&sandbox);
            let url = format!(
                "{}/internal/access-requests/decision",
                base.trim_end_matches('/')
            );
            let router_updated = match http
                .post(&url)
                .bearer_auth(&token)
                .json(&json!({
                    "kind": kind,
                    "target": target,
                    "verdict": verdict,
                    "reason": reason,
                }))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|body| body.get("updated").and_then(serde_json::Value::as_bool))
                    .unwrap_or(false),
                _ => false,
            };
            updated |= router_updated;
        }
        if updated {
            let appr_name = appr.name_any();
            let stamp = json!({ "metadata": { "annotations": { REQ_PUSHED_ANN: verdict } } });
            let _ = approvals
                .patch(&appr_name, &PatchParams::default(), &Patch::Merge(stamp))
                .await;
        }
    }
}

pub async fn run(client: Client) -> Result<()> {
    let tasks: Api<KarsTask> = Api::all(client.clone());
    let sandboxes: Api<KarsSandbox> = Api::all(client.clone());
    match tasks.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsTask CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("KarsTask CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let signer = match crate::providers::signing::load_or_create(&client).await {
        Ok(s) => {
            tracing::info!(key_id = %s.key_id, "Governance Receipt signer ready");
            s
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to initialise receipt signer — KarsTask reconciler disabled");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    };
    let ctx = Arc::new(Ctx { client, signer });
    Controller::new(tasks, crate::watch_config::bounded())
        .owns(sandboxes, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsTask", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("KarsTask reconciled {:?}", o),
                Err(e) => tracing::warn!("KarsTask reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}

/// Publish the controller-authored egress-guard ruleset hash into a ConfigMap.
/// The node-level datapath-witness DaemonSet reads this, compares the live
/// kernel iptables ruleset, and (on match) writes `kars-datapath-witness` so a
/// receipt's completeness predicate can bind the kernel datapath. Idempotent.
pub async fn publish_datapath_authored(client: &Client) -> Result<()> {
    use k8s_openapi::api::core::v1::ConfigMap;
    let sys = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let cms: Api<ConfigMap> = Api::namespaced(client.clone(), &sys);
    let authored = crate::reconciler::egress_guard_ruleset_hash(false);
    let cm: ConfigMap = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": "kars-datapath-authored",
            "namespace": sys,
            "labels": { "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "datapath-witness" },
        },
        "data": { "rulesetHash": authored, "redirectPort": "8444" },
    }))?;
    cms.patch(
        "kars-datapath-authored",
        &kube::api::PatchParams::apply("kars-controller/datapath-authored").force(),
        &kube::api::Patch::Apply(&cm),
    )
    .await?;
    tracing::info!(hash = %authored, "published datapath authored ruleset hash");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_task::{KarsTaskSpec, TaskEnvelope};

    fn task_with(tier: i32, authority_ceiling: i32, delegation_depth: i32) -> KarsTask {
        let mut t = KarsTask::new(
            "t",
            KarsTaskSpec {
                objective: "do the thing".into(),
                envelope: TaskEnvelope {
                    tier,
                    authority_ceiling,
                    delegation_depth,
                    ..TaskEnvelope::default()
                },
                parent_ref: None,
                requested_tier: None,
                execution: None,
                blueprint: None,
                display_name: None,
                retention_ttl_seconds: None,
            },
        );
        t.metadata.namespace = Some("default".into());
        t
    }

    #[test]
    fn approved_typed_controls_wait_for_materialized_authority() {
        assert!(!approved_request_is_materialized("egress", None, 1, false));
        assert!(approved_request_is_materialized("egress", None, 1, true));
        assert!(!approved_request_is_materialized("tier", Some(4), 3, false));
        assert!(approved_request_is_materialized("tier", Some(4), 4, false));
        assert!(approved_request_is_materialized("tool", None, 1, false));
    }

    #[test]
    fn launch_status_requeues_until_sandbox_state_converges() {
        let mut status = KarsTaskStatus::default();
        status.execution_phase = Some(crate::status::phase::PHASE_SANDBOX_LAUNCHING.into());
        assert_eq!(requeue_for_status(&status), REQUEUE_LAUNCHING);

        status.execution_phase = Some(crate::status::phase::PHASE_SANDBOX_RUNNING.into());
        assert_eq!(requeue_for_status(&status), REQUEUE_RUNNING);

        status.execution_phase = None;
        status.phase = Some(PHASE_PENDING.into());
        assert_eq!(requeue_for_status(&status), REQUEUE_PENDING);
    }

    #[test]
    fn run_start_time_is_derived_from_nonce_nanoseconds() {
        let mut task = task_with(3, 3, 2);
        task.annotations_mut().insert(
            "kars.azure.com/run-requested".into(),
            "run-1784504805123456789".into(),
        );
        assert_eq!(run_started_unix(&task), Some(1_784_504_805));
    }

    #[test]
    fn malformed_run_nonce_has_no_start_time() {
        let mut task = task_with(3, 3, 2);
        task.annotations_mut().insert(
            "kars.azure.com/run-requested".into(),
            "run-not-a-timestamp".into(),
        );
        assert_eq!(run_started_unix(&task), None);
    }

    #[test]
    fn team_run_start_time_uses_trailing_unix_seconds() {
        let mut task = task_with(3, 3, 2);
        task.annotations_mut().insert(
            "kars.azure.com/run-requested".into(),
            "cncf-release-watch-run-1784505483".into(),
        );
        assert_eq!(run_started_unix(&task), Some(1_784_505_483));
    }

    #[test]
    fn passive_npm_probe_is_not_a_mission_approval() {
        assert!(is_runtime_bootstrap_host("registry.npmjs.org"));
        assert!(is_runtime_bootstrap_host("REGISTRY.NPMJS.ORG"));
        assert!(!is_runtime_bootstrap_host("api.github.com"));
    }

    #[test]
    fn valid_envelope_passes() {
        let t = task_with(3, 3, 2);
        assert!(matches!(check_envelope(&t), EnvelopeCheck::Valid));
    }

    #[test]
    fn normal_status_refresh_preserves_delivered_at() {
        let mut task = task_with(3, 3, 2);
        task.status = Some(KarsTaskStatus {
            delivered_at: Some("2026-07-12T19:13:57Z".into()),
            ..Default::default()
        });
        let mut refreshed = ready_status(None, Some(1), "sha256:test".into(), Vec::new());

        preserve_delivered_at(&task, &mut refreshed);

        assert_eq!(
            refreshed.delivered_at.as_deref(),
            Some("2026-07-12T19:13:57Z")
        );
    }

    #[test]
    fn authority_ceiling_above_tier_is_rejected() {
        let t = task_with(2, 4, 1);
        match check_envelope(&t) {
            EnvelopeCheck::Invalid(why) => assert!(why.contains("authorityCeiling")),
            EnvelopeCheck::Valid => panic!("expected rejection"),
        }
    }

    #[test]
    fn tier_out_of_range_is_rejected() {
        let t = task_with(9, 5, 0);
        assert!(matches!(check_envelope(&t), EnvelopeCheck::Invalid(_)));
    }

    #[test]
    fn finalizer_roundtrip() {
        let mut t = task_with(1, 1, 0);
        assert!(!has_finalizer(&t));
        t.metadata.finalizers = Some(vec![FINALIZER.to_string(), "other/keep".to_string()]);
        assert!(has_finalizer(&t));
        let dropped = drop_finalizer(&t);
        assert_eq!(dropped, vec!["other/keep".to_string()]);
    }
}
