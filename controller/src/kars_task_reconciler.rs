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
use crate::status::phase::{PHASE_DEGRADED, PHASE_READY};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_TASK;
const FINALIZER: &str = "kars.azure.com/karstask-cleanup";

const REQUEUE_OK: Duration = Duration::from_secs(300);

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

    let new_status = match check_envelope(&task) {
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

    Ok(Action::requeue(REQUEUE_OK))
}

/// Outcome of resolving a task's `parentRef`.
enum Delegation {
    /// No `parentRef` — this is a root task.
    Root,
    /// `parentRef` set but the parent does not exist.
    ParentMissing { parent: String },
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
    // Minted lineage = parent's ancestry + the parent itself. The controller
    // owns this; a client-supplied lineage is ignored.
    let mut lineage = parent
        .status
        .as_ref()
        .map(|s| s.lineage.clone())
        .unwrap_or_default();
    lineage.push(parent.name_any());

    let violations = task
        .spec
        .envelope
        .attenuation_violations(&parent.spec.envelope);
    Ok(Delegation::Child {
        lineage,
        violations,
    })
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
    }
}

/// Build a `Degraded` status with no digest — the receipt must never bind to
/// authority that didn't validate or that amplified its parent.
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
    }
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
    let ctx = Arc::new(Ctx { client });
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
