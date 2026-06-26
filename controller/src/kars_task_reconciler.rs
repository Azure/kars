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

use crate::kars_task::{KarsTask, KarsTaskStatus, TIER_MAX, TIER_MIN};
use crate::status::conditions::{self, TYPE_READY, reason as cond_reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_PENDING, PHASE_READY};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_TASK;
const FINALIZER: &str = "kars.azure.com/karstask-cleanup";
/// Server-Side Apply field manager for Governance Receipt writes.
const RECEIPT_FIELD_MANAGER: &str = "kars-controller/receipt";

const REQUEUE_OK: Duration = Duration::from_secs(300);

/// A child waiting on its parent requeues quickly so it converges to `Ready`
/// promptly once the parent reconciles, rather than waiting a full cycle.
const REQUEUE_PENDING: Duration = Duration::from_secs(10);

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
            let patch = json!({
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "KarsTask",
                "metadata": { "finalizers": drop_finalizer(&task) },
            });
            tasks
                .patch(
                    &name,
                    &PatchParams::apply(FIELD_MANAGER).force(),
                    &Patch::Apply(patch),
                )
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
            "metadata": { "finalizers": finalizers },
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

    // A child still waiting on its parent requeues quickly to converge.
    let requeue = if new_status.phase.as_deref() == Some(PHASE_PENDING) {
        REQUEUE_PENDING
    } else {
        REQUEUE_OK
    };
    Ok(Action::requeue(requeue))
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
    let digest_ok = status.envelope_digest.as_ref().is_some_and(|d| !d.is_empty());
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
    use crate::kars_receipt::{KarsReceipt, approval_facts, build_spec, build_statement, canonical_json};

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
    let completeness = gather_completeness(client).await;

    let Some(statement) = build_statement(task, status, &signer.key_id, &facts, completeness) else {
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
        Ok(entry) => {
            // Publish a fresh signed checkpoint (signed tree head) over the log
            // so clients / an external witness can pin the log's size + head
            // without the full chain. Best-effort; never blocks the receipt.
            match crate::kars_receipt_log::read_chain(client).await {
                Ok(chain) => {
                    if let Err(e) =
                        crate::kars_receipt_log::publish_checkpoint(client, signer, &chain).await
                    {
                        tracing::warn!(karstask = %name, ns = %ns, error = %e, "failed to publish receipt checkpoint");
                    }
                }
                Err(e) => {
                    tracing::debug!(karstask = %name, ns = %ns, error = %e, "could not read chain for checkpoint");
                }
            }
            Some(entry)
        }
        Err(e) => {
            tracing::warn!(karstask = %name, ns = %ns, error = %e, "failed to enter receipt in inclusion log");
            None
        }
    };

    // Informational status echo (unsigned). Stamp issuance time on first write;
    // observedTaskGeneration tracks freshness; inclusion fields bind to the log.
    let mut status_obj = json!({
        "issuedAt": chrono::Utc::now().to_rfc3339(),
        "observedTaskGeneration": task.metadata.generation,
    });
    if let Some(entry) = &inclusion {
        status_obj["inclusionSeq"] = json!(entry.seq as i64);
        status_obj["inclusionEntryHash"] = json!(entry.entry_hash);
    }
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

/// Observe which completeness-floor controls (design note §24b) are enforced
/// on the cluster, for binding into the receipt. Best-effort: any read error
/// yields a conservative `false` (we never claim a control is enforced unless
/// we positively observed it). The runtime egress-guard iptables hash and the
/// eBPF witness are intentionally NOT gathered here — they are V1/V2.
async fn gather_completeness(client: &kube::Client) -> crate::kars_receipt::PredicateCompleteness {
    use k8s_openapi::api::admissionregistration::v1::ValidatingAdmissionPolicy;
    use k8s_openapi::api::networking::v1::NetworkPolicy;

    let vaps: Api<ValidatingAdmissionPolicy> = Api::all(client.clone());
    let vap_present = |name: &str, list: &[ValidatingAdmissionPolicy]| -> bool {
        list.iter().any(|p| p.metadata.name.as_deref() == Some(name))
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

    crate::kars_receipt::PredicateCompleteness {
        task_namespace_floor_vap: vap_present("kars-task-namespace-floor", &vap_list),
        exec_ban_vap: vap_present("kars-sandbox-exec-ban", &vap_list),
        posture_lock_vap: vap_present("kars-sandbox-posture-lock", &vap_list),
        default_deny_egress,
        floor_enforced: false,
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

pub async fn run(client: Client) -> Result<()> {
    let tasks: Api<KarsTask> = Api::all(client.clone());
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

// ─────────────────────────────────────────────────────────────────────
// Unit tests — pure helpers only. K8s-API-touching paths are exercised
// by the kind-based integration harness.
// ─────────────────────────────────────────────────────────────────────
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
                execution: None,
                blueprint: None,
                display_name: None,
            },
        );
        t.metadata.namespace = Some("default".into());
        t
    }

    #[test]
    fn valid_envelope_passes() {
        let t = task_with(3, 3, 2);
        assert!(matches!(check_envelope(&t), EnvelopeCheck::Valid));
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
