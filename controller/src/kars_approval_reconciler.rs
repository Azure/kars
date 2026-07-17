// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsApproval` reconciler — the HITL approval lifecycle (kars Bridge Inc 4).
//!
//! For each `KarsApproval` the controller:
//!
//! 1. Ensures a cleanup finalizer.
//! 2. **Binds** the approval to the gated task's authority: on first
//!    observation where the task is governance-`Ready`, it copies the task's
//!    `status.envelopeDigest` into `status.boundEnvelopeDigest` and never
//!    changes it. The controller is the sole writer of this binding.
//! 3. Evaluates the pure decision function ([`crate::kars_approval::evaluate`])
//!    over the recorded human decision, the bound digest, the live task digest,
//!    and TTL expiry, and stamps the resulting `phase` + `Decided` condition.
//! 4. Preserves `requestedAt`, `expiresAt`, and `decidedAt` immutably across
//!    re-reconciles, so the timeline a Governance Receipt records cannot be
//!    rewritten.
//!
//! The reconciler never executes the approved action — it records the human
//! decision. Acting on it (a tier raise, an egress widen) is the consuming
//! reconciler's job; this primitive is the verifiable decision record.

use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::StreamExt;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use crate::egress_approval_reconciler::parse_iso8601_duration_secs;
use crate::kars_approval::{ApprovalOutcome, KarsApproval, KarsApprovalStatus, evaluate};
use crate::kars_task::KarsTask;
use crate::status::conditions::{self, reason as cond_reason, status as cond_status};

const FIELD_MANAGER: &str = "kars-controller/karsapproval";
const FINALIZER: &str = "kars.azure.com/karsapproval-cleanup";

/// The `Decided` condition type — `True` when terminal, `False` while pending.
const TYPE_DECIDED: &str = "Decided";

/// Default TTL when `spec.ttl` is omitted.
const DEFAULT_TTL: &str = "PT1H";
/// Hard ceiling on an approval TTL (7 days) — a pending decision should not
/// linger indefinitely.
const MAX_TTL_SECS: u64 = 7 * 24 * 3600;

/// Re-reconcile a still-pending approval periodically so TTL expiry is
/// observed even without an external event.
const REQUEUE_PENDING: Duration = Duration::from_secs(30);
/// Terminal approvals rarely change; re-check infrequently.
const REQUEUE_TERMINAL: Duration = Duration::from_secs(300);

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

struct Ctx {
    client: Client,
}

async fn reconcile(approval: Arc<KarsApproval>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = approval.name_any();
    let ns = approval.namespace().unwrap_or_else(|| "default".into());
    let approvals: Api<KarsApproval> = Api::namespaced(ctx.client.clone(), &ns);

    // Deletion: drop the finalizer; nothing cluster-side to clean up.
    if approval.metadata.deletion_timestamp.is_some() {
        if has_finalizer(&approval) {
            let patch = json!({
                "apiVersion": "kars.azure.com/v1alpha1",
                "kind": "KarsApproval",
                "metadata": { "finalizers": drop_finalizer(&approval) },
            });
            approvals
                .patch(
                    &name,
                    &PatchParams::apply(FIELD_MANAGER).force(),
                    &Patch::Apply(patch),
                )
                .await?;
        }
        return Ok(Action::await_change());
    }

    if !has_finalizer(&approval) {
        let mut finalizers = approval.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(FINALIZER.to_string());
        let patch = json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsApproval",
            "metadata": { "finalizers": finalizers },
        });
        approvals
            .patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(patch),
            )
            .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let generation = approval.metadata.generation;
    let prior = approval.status.clone().unwrap_or_default();

    // Resolve the gated task's live envelope digest (None unless it is
    // governance-Ready and has a digest).
    let tasks: Api<KarsTask> = Api::namespaced(ctx.client.clone(), &ns);
    let live_task = tasks.get_opt(&approval.spec.task_ref.name).await?;
    let live_task_digest = live_task
        .as_ref()
        .and_then(|task| task.status.as_ref())
        .and_then(|status| status.envelope_digest.clone());
    let task_completed = live_task.as_ref().is_some_and(task_is_completed);

    // Bind on first observation where the task is Ready. The controller owns
    // this; once set it is immutable.
    let bound_digest = prior
        .bound_envelope_digest
        .clone()
        .or_else(|| live_task_digest.clone());

    let now = Utc::now();
    let requested_at = prior
        .requested_at
        .as_ref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now);

    let ttl_secs = resolve_ttl_secs(approval.spec.ttl.as_deref());
    let expires_at = requested_at + ChronoDuration::seconds(ttl_secs as i64);
    // A pending decision cannot affect a task that has already delivered and
    // retired. Expire it immediately instead of leaving a success-shaped,
    // actionable Inbox card whose grant can no longer be consumed.
    let expired = now >= expires_at || (approval.spec.decision.is_none() && task_completed);

    let outcome = evaluate(
        approval.spec.decision.as_ref(),
        bound_digest.as_deref(),
        live_task_digest.as_deref(),
        expired,
    );

    let new_status = build_status(
        &prior,
        generation,
        &outcome,
        requested_at,
        expires_at,
        bound_digest,
        now,
    );

    let terminal = outcome.is_terminal();

    let status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsApproval",
        "status": new_status,
    });
    approvals
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(status_patch),
        )
        .await?;

    tracing::debug!(karsapproval = %name, ns = %ns, phase = outcome.phase(), "KarsApproval reconciled");

    Ok(Action::requeue(if terminal {
        REQUEUE_TERMINAL
    } else {
        REQUEUE_PENDING
    }))
}

fn task_is_completed(task: &KarsTask) -> bool {
    if task
        .status
        .as_ref()
        .and_then(|status| status.delivered_at.as_ref())
        .is_some()
    {
        return true;
    }
    let annotations = task.annotations();
    matches!(
        (
            annotations.get("kars.azure.com/run-requested"),
            annotations.get("kars.azure.com/run-completed"),
        ),
        (Some(requested), Some(completed)) if requested == completed
    )
}

/// Resolve the effective TTL in seconds, clamped to [`MAX_TTL_SECS`], falling
/// back to [`DEFAULT_TTL`] on absence or a parse failure.
fn resolve_ttl_secs(ttl: Option<&str>) -> u64 {
    let raw = ttl.unwrap_or(DEFAULT_TTL);
    let secs = parse_iso8601_duration_secs(raw)
        .or_else(|_| parse_iso8601_duration_secs(DEFAULT_TTL))
        .unwrap_or(3600);
    secs.min(MAX_TTL_SECS)
}

/// Build the new status, preserving immutable timestamps across re-reconciles.
fn build_status(
    prior: &KarsApprovalStatus,
    generation: Option<i64>,
    outcome: &ApprovalOutcome,
    requested_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    bound_digest: Option<String>,
    now: DateTime<Utc>,
) -> KarsApprovalStatus {
    let terminal = outcome.is_terminal();
    let decided = matches!(
        outcome,
        ApprovalOutcome::Approved { .. } | ApprovalOutcome::Denied { .. }
    );

    let (cond_status_value, message) = match outcome {
        ApprovalOutcome::Pending(why) => (cond_status::FALSE, why.to_string()),
        ApprovalOutcome::Approved { decider } => {
            (cond_status::TRUE, format!("approved by {decider}"))
        }
        ApprovalOutcome::Denied { decider } => (cond_status::TRUE, format!("denied by {decider}")),
        ApprovalOutcome::Expired => (cond_status::TRUE, "expired before a decision".to_string()),
        ApprovalOutcome::Stale(why) => (cond_status::TRUE, why.clone()),
    };

    let reason_value = match outcome {
        ApprovalOutcome::Pending(_) => cond_reason::RECONCILING,
        ApprovalOutcome::Approved { .. } | ApprovalOutcome::Denied { .. } => {
            cond_reason::RECONCILED
        }
        ApprovalOutcome::Expired => cond_reason::TIMED_OUT,
        ApprovalOutcome::Stale(_) => cond_reason::DEPENDENCY_MISSING,
    };

    let prior_decided = prior
        .conditions
        .as_ref()
        .and_then(|cs| conditions::find(cs, TYPE_DECIDED));
    let condition = conditions::preserve_transition_time(
        prior_decided,
        TYPE_DECIDED,
        cond_status_value,
        reason_value,
        &message,
        generation,
    );

    // decidedAt + decider are immutable once first recorded.
    let decider = match outcome {
        ApprovalOutcome::Approved { decider } | ApprovalOutcome::Denied { decider } => {
            Some(decider.clone())
        }
        _ => prior.decider.clone(),
    };
    let decided_at = if decided {
        prior.decided_at.clone().or_else(|| Some(now.to_rfc3339()))
    } else {
        prior.decided_at.clone()
    };

    KarsApprovalStatus {
        phase: Some(outcome.phase().to_string()),
        observed_generation: generation,
        requested_at: Some(
            prior
                .requested_at
                .clone()
                .unwrap_or_else(|| requested_at.to_rfc3339()),
        ),
        decided_at,
        // Once terminal, freeze expiresAt as last computed; while pending it
        // tracks the (stable) requested_at + ttl.
        expires_at: Some(
            prior
                .expires_at
                .clone()
                .filter(|_| terminal)
                .unwrap_or_else(|| expires_at.to_rfc3339()),
        ),
        bound_envelope_digest: bound_digest.or_else(|| prior.bound_envelope_digest.clone()),
        decider,
        conditions: Some(vec![condition]),
    }
}

fn has_finalizer(a: &KarsApproval) -> bool {
    a.metadata
        .finalizers
        .as_ref()
        .is_some_and(|f| f.iter().any(|s| s == FINALIZER))
}

fn drop_finalizer(a: &KarsApproval) -> Vec<String> {
    a.metadata
        .finalizers
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s != FINALIZER)
        .collect()
}

fn error_policy(approval: Arc<KarsApproval>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsApproval", error.class());
    tracing::warn!(
        karsapproval = %approval.name_any(),
        error_class = error.class(),
        error = %error,
        "KarsApproval reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

pub async fn run(client: Client) -> Result<()> {
    let approvals: Api<KarsApproval> = Api::all(client.clone());
    match approvals.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsApproval CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("KarsApproval CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(approvals, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsApproval", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("KarsApproval reconciled {:?}", o),
                Err(e) => tracing::warn!("KarsApproval reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_approval::ApprovalDecision;

    fn approved(decider: &str) -> ApprovalOutcome {
        ApprovalOutcome::Approved {
            decider: decider.to_string(),
        }
    }

    #[test]
    fn resolve_ttl_defaults_and_clamps() {
        assert_eq!(resolve_ttl_secs(None), 3600);
        assert_eq!(resolve_ttl_secs(Some("PT15M")), 900);
        assert_eq!(resolve_ttl_secs(Some("garbage")), 3600);
        // 30d clamps to the 7d ceiling.
        assert_eq!(resolve_ttl_secs(Some("P30D")), MAX_TTL_SECS);
    }

    #[test]
    fn completed_tasks_make_pending_approvals_non_actionable() {
        let mut delivered = KarsTask::new("delivered", Default::default());
        delivered.status = Some(Default::default());
        delivered.status.as_mut().unwrap().delivered_at = Some(Utc::now().to_rfc3339());
        assert!(task_is_completed(&delivered));

        let mut acknowledged = KarsTask::new("acknowledged", Default::default());
        acknowledged.metadata.annotations = Some(
            [
                (
                    "kars.azure.com/run-requested".to_string(),
                    "nonce-1".to_string(),
                ),
                (
                    "kars.azure.com/run-completed".to_string(),
                    "nonce-1".to_string(),
                ),
            ]
            .into(),
        );
        assert!(task_is_completed(&acknowledged));

        let pending = KarsTask::new("pending", Default::default());
        assert!(!task_is_completed(&pending));
    }

    #[test]
    fn decided_at_is_set_once_and_preserved() {
        let now = Utc::now();
        let req = now - ChronoDuration::minutes(5);
        let exp = req + ChronoDuration::hours(1);

        // First terminal write stamps decidedAt.
        let s1 = build_status(
            &KarsApprovalStatus::default(),
            Some(1),
            &approved("alice"),
            req,
            exp,
            Some("sha256:aa".to_string()),
            now,
        );
        assert_eq!(s1.phase.as_deref(), Some("Approved"));
        let first_decided = s1.decided_at.clone().unwrap();
        assert_eq!(s1.decider.as_deref(), Some("alice"));

        // A later re-reconcile preserves the original decidedAt.
        let later = now + ChronoDuration::minutes(10);
        let s2 = build_status(
            &s1,
            Some(1),
            &approved("alice"),
            req,
            exp,
            Some("sha256:aa".to_string()),
            later,
        );
        assert_eq!(s2.decided_at, Some(first_decided));
    }

    #[test]
    fn pending_has_no_decided_at() {
        let now = Utc::now();
        let s = build_status(
            &KarsApprovalStatus::default(),
            Some(1),
            &ApprovalOutcome::Pending("awaiting a human decision"),
            now,
            now + ChronoDuration::hours(1),
            Some("sha256:aa".to_string()),
            now,
        );
        assert_eq!(s.phase.as_deref(), Some("Pending"));
        assert!(s.decided_at.is_none());
        // The Decided condition is False while pending.
        let c = &s.conditions.unwrap()[0];
        assert_eq!(c.status, "False");
    }

    #[test]
    fn requested_at_is_immutable() {
        let now = Utc::now();
        let prior = KarsApprovalStatus {
            requested_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            ..Default::default()
        };
        let s = build_status(
            &prior,
            Some(1),
            &ApprovalOutcome::Pending("awaiting a human decision"),
            now,
            now + ChronoDuration::hours(1),
            Some("sha256:aa".to_string()),
            now,
        );
        assert_eq!(s.requested_at.as_deref(), Some("2020-01-01T00:00:00+00:00"));
    }

    #[test]
    fn decision_records_decider_in_status() {
        let d = ApprovalDecision {
            verdict: "approve".to_string(),
            decider: "bob".to_string(),
            decider_subject: Some("subject-bob".into()),
            decider_roles: vec!["operator".into()],
            reason: Some("looks good".to_string()),
        };
        let out = evaluate(Some(&d), Some("sha256:aa"), Some("sha256:aa"), false);
        assert!(matches!(out, ApprovalOutcome::Approved { decider } if decider == "bob"));
    }
}
