// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

pub(super) async fn patch_sandbox_drift(
    sandboxes: &Api<KarsSandbox>,
    sandbox_name: &str,
    expected_uid: Option<&str>,
    message: &str,
) -> Result<(), ReconcileError> {
    let current = sandboxes.get(sandbox_name).await?;
    let (uid, rv) = workloads::identity(&current.metadata)?;
    if Some(uid) != expected_uid || current.metadata.deletion_timestamp.is_some() {
        return Err(anyhow::anyhow!("evaluation target changed before drift publication").into());
    }
    let mut updated = current
        .status
        .as_ref()
        .map(|status| status.conditions.clone())
        .unwrap_or_default();
    let condition = conditions::preserve_transition_time(
        conditions::find(&updated, conditions::TYPE_DEGRADED),
        conditions::TYPE_DEGRADED,
        cond_status::TRUE,
        "ConformanceDrift",
        message,
        current.metadata.generation,
    );
    updated.retain(|condition| condition.type_ != conditions::TYPE_DEGRADED);
    conditions::set(&mut updated, condition);
    let body = json!({
        "metadata": {"uid":uid, "resourceVersion":rv},
        "status": {
            "conditions": updated,
        },
    });
    sandboxes
        .patch_status(sandbox_name, &PatchParams::default(), &Patch::Merge(body))
        .await?;
    Ok(())
}

pub(super) fn build_conditions(
    prior: &[Condition],
    observed_generation: Option<i64>,
    resolved: &ResolvedCorpus,
    spawned_run_now: Option<&str>,
    cron_job_name: Option<&str>,
    last_result: Option<&EvalResult>,
    (state, drift_detected): (&str, bool),
) -> Vec<Condition> {
    let state = if state != PHASE_PENDING && last_result.is_some_and(|r| r.schema_version == "v1") {
        "RunnerUpgradeRequired"
    } else if state == "AllPassed"
        && !last_result.is_some_and(|r| {
            r.schema_version == "v2"
                && r.total > 0
                && r.passed == r.total
                && r.failed == 0
                && r.errored == 0
        })
    {
        "Inconclusive"
    } else {
        state
    };
    let drift_detected =
        drift_detected && last_result.is_some_and(|r| r.schema_version == "v2" && r.failed > 0);
    let mut out: Vec<Condition> = Vec::with_capacity(4);

    let prior_ready = conditions::find(prior, conditions::TYPE_READY);
    let prior_progressing = conditions::find(prior, conditions::TYPE_PROGRESSING);
    let prior_degraded = conditions::find(prior, conditions::TYPE_DEGRADED);
    let prior_drift = conditions::find(prior, TYPE_CONFORMANCE_DRIFT);

    let ready_status = if state == "AllPassed" {
        cond_status::TRUE
    } else {
        cond_status::FALSE
    };
    let ready_reason = if state == PHASE_PENDING {
        if spawned_run_now.is_some() {
            reason::RUN_TRIGGERED
        } else if cron_job_name.is_some() {
            reason::SCHEDULED
        } else {
            cond_reason::RECONCILED
        }
    } else {
        state
    };
    let ready_msg = match (state, last_result) {
        (PHASE_PENDING, _) => format!(
            "awaiting current run — corpus {} ({})",
            resolved.label, resolved.digest
        ),
        ("AllPassed", Some(r)) => format!(
            "all {} cases passed against corpus {}",
            r.total, r.corpus_label
        ),
        ("DriftDetected", Some(r)) => format!(
            "{} of {} cases failed against corpus {}",
            r.failed, r.total, r.corpus_label
        ),
        ("RunnerUpgradeRequired", _) => "Legacy v1 history is retained but cannot establish fresh Ready; upgrade the runner to negotiated v2.".into(),
        _ => "Current evaluation is inconclusive; unavailable, incomplete or invalid evidence is not policy enforcement.".into(),
    };
    out.push(conditions::preserve_transition_time(
        prior_ready,
        conditions::TYPE_READY,
        ready_status,
        ready_reason,
        &ready_msg,
        observed_generation,
    ));

    let progressing_status = if state == PHASE_PENDING {
        cond_status::TRUE
    } else {
        cond_status::FALSE
    };
    let progressing_msg = match (spawned_run_now, cron_job_name) {
        (Some(j), _) => format!("spawned one-shot Job {j}"),
        (None, Some(cj)) => format!("scheduled via CronJob {cj}"),
        (None, None) => "no schedule and no run-now; awaiting external trigger".into(),
    };
    out.push(conditions::preserve_transition_time(
        prior_progressing,
        conditions::TYPE_PROGRESSING,
        progressing_status,
        if spawned_run_now.is_some() {
            reason::RUN_TRIGGERED
        } else if cron_job_name.is_some() {
            reason::SCHEDULED
        } else {
            cond_reason::RECONCILED
        },
        &progressing_msg,
        observed_generation,
    ));

    out.push(conditions::preserve_transition_time(
        prior_degraded,
        conditions::TYPE_DEGRADED,
        if !matches!(state, "AllPassed" | PHASE_PENDING) {
            cond_status::TRUE
        } else {
            cond_status::FALSE
        },
        ready_reason,
        &ready_msg,
        observed_generation,
    ));

    out.push(conditions::preserve_transition_time(
        prior_drift,
        TYPE_CONFORMANCE_DRIFT,
        if drift_detected {
            cond_status::TRUE
        } else {
            cond_status::FALSE
        },
        if drift_detected {
            reason::DRIFT_DETECTED
        } else if state == "AllPassed" {
            reason::ALL_PASSED
        } else {
            "NotEvaluated"
        },
        match last_result {
            Some(r) if drift_detected => format!(
                "{} failing cases out of {} (corpus {})",
                r.failed, r.total, r.corpus_label
            ),
            Some(r) if state == "AllPassed" => format!("all {} cases passed", r.total),
            _ => "no current conclusive policy verdict".into(),
        }
        .as_str(),
        observed_generation,
    ));

    out
}

/// Common path for "the corpus is unresolvable; just publish a
/// Degraded status and back off". Used from multiple early-return
/// branches above to keep their bodies skinny.
pub(super) async fn write_degraded(
    api: &Api<KarsEval>,
    eval: &KarsEval,
    prior_conditions: &[Condition],
    prior_status: &KarsEvalStatus,
    why_reason: &str,
    why_msg: &str,
) -> Result<Action, ReconcileError> {
    let current = api.get(&eval.name_any()).await?;
    workloads::same_eval(&current, eval)?;
    let observed_generation = current.metadata.generation;
    let (uid, rv) = workloads::identity(&current.metadata)?;
    let prior_ready = conditions::find(prior_conditions, conditions::TYPE_READY);
    let prior_progressing = conditions::find(prior_conditions, conditions::TYPE_PROGRESSING);
    let prior_degraded = conditions::find(prior_conditions, conditions::TYPE_DEGRADED);
    let prior_drift = conditions::find(prior_conditions, TYPE_CONFORMANCE_DRIFT);
    let new_conditions = vec![
        conditions::preserve_transition_time(
            prior_ready,
            conditions::TYPE_READY,
            cond_status::FALSE,
            why_reason,
            why_msg,
            observed_generation,
        ),
        conditions::preserve_transition_time(
            prior_progressing,
            conditions::TYPE_PROGRESSING,
            cond_status::FALSE,
            cond_reason::FAILED,
            why_msg,
            observed_generation,
        ),
        conditions::preserve_transition_time(
            prior_degraded,
            conditions::TYPE_DEGRADED,
            cond_status::TRUE,
            why_reason,
            why_msg,
            observed_generation,
        ),
        conditions::preserve_transition_time(
            prior_drift,
            TYPE_CONFORMANCE_DRIFT,
            cond_status::FALSE,
            cond_reason::RECONCILED,
            "no runs while corpus is unresolvable",
            observed_generation,
        ),
    ];
    let new_status = KarsEvalStatus {
        phase: Some(PHASE_DEGRADED.into()),
        observed_generation,
        conditions: Some(new_conditions),
        last_run_at: prior_status.last_run_at.clone(),
        last_result: prior_status.last_result.clone(),
        history: prior_status.history.clone(),
        corpus_config_map_ref: prior_status.corpus_config_map_ref.clone(),
        corpus_digest: prior_status.corpus_digest.clone(),
        cron_job_name: prior_status.cron_job_name.clone(),
        report_config_map_ref: None,
        report_config_map_uid: None,
        report_evidence_digest: None,
    };
    let patch = json!({
        "metadata":{"uid":uid,"resourceVersion":rv},
        "status": new_status,
    });
    let mut patch = patch;
    patch["status"]["reportConfigMapRef"] = serde_json::Value::Null;
    patch["status"]["reportConfigMapUid"] = serde_json::Value::Null;
    patch["status"]["reportEvidenceDigest"] = serde_json::Value::Null;
    api.patch_status(
        &eval.name_any(),
        &PatchParams::default(),
        &Patch::Merge(patch),
    )
    .await?;
    Ok(Action::requeue(REQUEUE_FAIL))
}
