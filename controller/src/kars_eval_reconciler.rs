// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsEval` reconciler — slice 6.3.
//!
//! Watches `KarsEval` CRs and, for each:
//!
//! 1. Ensures the cleanup finalizer.
//! 2. Resolves the corpus referenced by `spec.corpus` (builtin via the
//!    embedded `kars-eval-corpus` library, or a signed OCI bundle
//!    via `policy_fetcher::fetch_and_verify_generic::<EvalCorpusKind>`).
//! 3. Persists the resolved corpus into a `ConfigMap`
//!    (`karseval-<name>-corpus`, key `corpus.json`).
//! 4. If `spec.schedule` is set, ensures a controller-owned `CronJob`
//!    (`karseval-<name>`) that spawns runner pods on schedule.
//! 5. If the `kars.azure.com/run-now=true` annotation is present,
//!    spawns a one-shot `Job` and strips the annotation.
//! 6. Observes completed `Job`s, reads each runner pod's log, parses
//!    the `RunReport`, and stamps `status.last_result` + appends to
//!    bounded `status.history`.
//! 7. If `spec.fail_sandbox_on_drift` is true and the latest run
//!    reports any failed case, patches the target `KarsSandbox`'s
//!    `Degraded` condition with a UID/resourceVersion-fenced update,
//!    preserving unrelated sandbox conditions.
//!
//! Webhook delivery + the `kars eval run` CLI surface ship in
//! slice 6.4.

mod corpus;
mod evidence;
mod observation;
mod report;
#[cfg(test)]
mod result_tests;
mod runner;
mod status;
#[cfg(test)]
mod tests;
mod workloads;

use anyhow::Result;
use corpus::resolve_corpus;
use futures::StreamExt;
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams},
    runtime::controller::{Action, Controller},
};
use runner::runner_pod_spec_json;
use serde_json::json;
use status::{build_conditions, patch_sandbox_drift, write_degraded};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::crd::KarsSandbox;
use crate::kars_eval::{
    ANNOTATION_RUN_NOW, CorpusSource, EvalResult, KarsEval, KarsEvalStatus, TYPE_CONFORMANCE_DRIFT,
    reason,
};
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason as cond_reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_PENDING, PHASE_READY};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_EVAL;
const FINALIZER: &str = "kars.azure.com/karseval-cleanup";

const REQUEUE_OK: Duration = Duration::from_secs(300);
const REQUEUE_FAIL: Duration = Duration::from_secs(60);
const REQUEUE_AWAITING_RUN: Duration = Duration::from_secs(30);

const RUNNER_IMAGE_ENV: &str = "KARS_CONFORMANCE_RUNNER_IMAGE";
const DEFAULT_RUNNER_IMAGE: &str = "ghcr.io/azure/kars/conformance-runner:latest";

const CORPUS_LABEL_BUILTIN_PREFIX: &str = "builtin:";
const LABEL_KEY_CLAW_EVAL: &str = "kars.azure.com/karseval";

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Evaluation Kubernetes API request failed")]
    Kube(#[from] kube::Error),
    #[error("Evaluation JSON contract failed")]
    SerdeJson(#[from] serde_json::Error),
    #[error("Evaluation evidence unavailable")]
    Evidence(#[from] anyhow::Error),
}

impl ReconcileError {
    fn class(&self) -> &'static str {
        match self {
            ReconcileError::Kube(_) => "kube_api",
            ReconcileError::SerdeJson(_) => "serde",
            ReconcileError::Evidence(_) => "evidence",
        }
    }
}

/// Outcome of resolving `spec.corpus`. Either we have bytes + digest +
/// label ready to mount, or we have a hard error to surface as
/// `Degraded`.
#[derive(Debug)]
struct ResolvedCorpus {
    bytes: Vec<u8>,
    digest: String,
    /// Operator-facing source identifier retained in corpus annotations and
    /// evaluation status, separate from the runner's corpus name.
    label: String,
}

struct Ctx {
    client: Client,
}

async fn reconcile(eval: Arc<KarsEval>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let outcome = reconcile_inner(eval.clone(), ctx.clone()).await;
    if outcome.is_err() && eval.metadata.deletion_timestamp.is_none() {
        let api = Api::<KarsEval>::namespaced(ctx.client.clone(), workloads::namespace(&eval)?);
        let current = api.get(&eval.name_any()).await?;
        if current.metadata.uid == eval.metadata.uid
            && current.metadata.generation == eval.metadata.generation
            && serde_json::to_value(&current.spec)? == serde_json::to_value(&eval.spec)?
        {
            let prior = current.status.clone().unwrap_or_default();
            write_degraded(&api, &current, prior.conditions.as_deref().unwrap_or_default(), &prior,
                "EvidenceUnavailable", "Evaluation work or evidence persistence failed; no current successful result is asserted").await?;
        }
    }
    outcome
}

async fn reconcile_inner(eval: Arc<KarsEval>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let mut eval = (*eval).clone();
    let name = eval.name_any();
    let ns = eval.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(karseval = %name, ns = %ns, "Reconciling KarsEval");

    let evals_api: Api<KarsEval> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), &ns);
    let cronjobs: Api<CronJob> = Api::namespaced(ctx.client.clone(), &ns);

    if eval.metadata.deletion_timestamp.is_some() {
        return finalize(&evals_api, &configmaps, &jobs, &cronjobs, &eval, &name).await;
    }

    if !eval
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsEval",
            "metadata": {"finalizers": [FINALIZER]}
        });
        evals_api
            .patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(patch),
            )
            .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let observed_generation = eval.metadata.generation;
    let prior_status = eval.status.clone().unwrap_or_default();
    let prior_conditions = prior_status.conditions.clone().unwrap_or_default();

    // -------- 1. Resolve corpus -----------------------------------
    let resolved = match resolve_corpus(&eval.spec.corpus).await {
        Ok(r) => r,
        Err((why_reason, why_msg)) => {
            return write_degraded(
                &evals_api,
                &eval,
                &prior_conditions,
                &prior_status,
                why_reason,
                &why_msg,
            )
            .await;
        }
    };

    if workloads::claim_trigger(&evals_api, &eval).await? {
        return Ok(Action::requeue(Duration::from_secs(1)));
    }
    let runner_image = eval
        .spec
        .runner_image
        .clone()
        .unwrap_or_else(default_runner_image);
    let target_url = sandbox_router_url(&eval.spec.target_sandbox_ref.name);
    let mut intent = workloads::Intent::new(
        &ctx.client,
        &eval,
        &resolved.digest,
        &runner_image,
        &target_url,
    )
    .await?;

    // -------- 2. Ensure corpus ConfigMap --------------------------
    let cm_name = format!("karseval-{name}-corpus");
    if let Err(e) = ensure_corpus_configmap(&configmaps, &cm_name, &eval, &resolved).await {
        tracing::warn!(karseval = %name, error_class = e.class(), "KarsEvalCorpusWriteFailed");
        return write_degraded(
            &evals_api,
            &eval,
            &prior_conditions,
            &prior_status,
            reason::CORPUS_FETCH_FAILED,
            "owned corpus ConfigMap write failed",
        )
        .await;
    }

    // -------- 3. Handle run-now annotation ------------------------
    let mut spawned_run_now: Option<String> = None;
    if eval
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANNOTATION_RUN_NOW))
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        let job_name = run_now_job_name(
            &name,
            eval.annotations()
                .get(workloads::RUN_TOKEN)
                .map(String::as_str),
        );
        intent.revalidate(&ctx.client, &eval).await?;
        ensure_run_now_job(
            &jobs,
            &job_name,
            &eval,
            &intent,
            &cm_name,
            &runner_image,
            &target_url,
            &resolved.label,
        )
        .await?;
        spawned_run_now = Some(job_name.clone());
        eval = workloads::acknowledge_trigger(&evals_api, &eval, &job_name).await?;
        intent = workloads::Intent::new(
            &ctx.client,
            &eval,
            &resolved.digest,
            &runner_image,
            &target_url,
        )
        .await?;
    }

    // -------- 4. Ensure CronJob or delete -------------------------
    let cron_job_name = if let Some(schedule) = eval.spec.schedule.as_deref() {
        let cj_name = cron_job_name(&name);
        intent.revalidate(&ctx.client, &eval).await?;
        ensure_cronjob(
            &cronjobs,
            &cj_name,
            &eval,
            &intent,
            schedule,
            &cm_name,
            &runner_image,
            &target_url,
            &resolved.label,
        )
        .await?;
        Some(cj_name)
    } else {
        let cj_name = cron_job_name(&name);
        if let Some(existing) = cronjobs.get_opt(&cj_name).await? {
            workloads::require_owner(&existing.metadata, &eval)?;
            let (uid, rv) = workloads::identity(&existing.metadata)?;
            cronjobs
                .delete(
                    &cj_name,
                    &DeleteParams {
                        preconditions: Some(kube::api::Preconditions {
                            uid: Some(uid.into()),
                            resource_version: Some(rv.into()),
                        }),
                        ..Default::default()
                    },
                )
                .await?;
        }
        None
    };

    // -------- 5. Observe completed Jobs ---------------------------
    let observed = match observation::observe(&ctx.client, &eval, &intent, &resolved).await {
        Ok(observed) => observed,
        Err(_) => {
            return write_degraded(
                &evals_api,
                &eval,
                &prior_conditions,
                &prior_status,
                "EvidenceUnavailable",
                "Evaluation evidence could not be validated or durably persisted",
            )
            .await;
        }
    };
    let history = observed.history;
    let last_result = observed.result;
    let last_run_at = observed.last_run_at;
    let drift_detected = observed.drift;

    // -------- 6. Optionally patch sandbox Degraded ----------------
    if eval.spec.fail_sandbox_on_drift.unwrap_or(false) && drift_detected {
        let sandboxes: Api<KarsSandbox> = Api::namespaced(ctx.client.clone(), &ns);
        let fail_msg = last_result
            .as_ref()
            .map(|r| {
                format!(
                    "KarsEval/{} reported {} failed / {} total against corpus {}",
                    name, r.failed, r.total, r.corpus_label
                )
            })
            .unwrap_or_else(|| format!("KarsEval/{name} reported drift"));
        if let Err(e) = patch_sandbox_drift(
            &sandboxes,
            &eval.spec.target_sandbox_ref.name,
            intent.target_uid.as_deref(),
            &fail_msg,
        )
        .await
        {
            tracing::warn!(
                karseval = %name,
                target = %eval.spec.target_sandbox_ref.name,
                "KarsEvalSandboxDriftPatchFailed: {e}"
            );
        }
    }

    // -------- 7. Patch KarsEval status ----------------------------
    let phase = match observed.state {
        "AllPassed" => PHASE_READY,
        PHASE_PENDING => PHASE_PENDING,
        _ => PHASE_DEGRADED,
    };

    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        &resolved,
        spawned_run_now.as_deref(),
        cron_job_name.as_deref(),
        last_result.as_ref(),
        (observed.state, drift_detected),
    );

    let new_status = KarsEvalStatus {
        phase: Some(phase.into()),
        observed_generation,
        conditions: Some(new_conditions),
        last_run_at,
        last_result: last_result.clone(),
        history,
        corpus_config_map_ref: Some(LocalObjectRef {
            name: cm_name.clone(),
        }),
        corpus_digest: Some(resolved.digest.clone()),
        cron_job_name,
        report_config_map_ref: observed.has_report.then(|| LocalObjectRef {
            name: evidence::name(&eval),
        }),
        report_config_map_uid: observed.receipt.as_ref().map(|receipt| receipt.uid.clone()),
        report_evidence_digest: observed
            .receipt
            .as_ref()
            .map(|receipt| receipt.digest.clone()),
    };
    let current = intent.revalidate(&ctx.client, &eval).await?;
    let mut status_value = serde_json::to_value(&new_status)?;
    status_value["lastRunAt"] = json!(new_status.last_run_at);
    status_value["lastResult"] = json!(new_status.last_result);
    if let Some(result) = &new_status.last_result {
        status_value["lastResult"]["firstFailingCases"] = json!(result.first_failing_cases);
    }
    status_value["history"] = json!(new_status.history);
    status_value["cronJobName"] = json!(new_status.cron_job_name);
    status_value["reportConfigMapRef"] = json!(new_status.report_config_map_ref);
    status_value["reportConfigMapUid"] = json!(new_status.report_config_map_uid);
    status_value["reportEvidenceDigest"] = json!(new_status.report_evidence_digest);
    let status_patch = json!({
        "metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version},
        "status": status_value,
    });
    evals_api
        .patch_status(&name, &PatchParams::default(), &Patch::Merge(status_patch))
        .await?;

    if observed.state == PHASE_PENDING {
        Ok(Action::requeue(REQUEUE_AWAITING_RUN))
    } else if observed.state != "AllPassed" {
        Ok(Action::requeue(REQUEUE_FAIL))
    } else {
        Ok(Action::requeue(REQUEUE_OK))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

// ─────────────────────────────────────────────────────────────────────
// ConfigMap, Job, CronJob ensure-helpers
// ─────────────────────────────────────────────────────────────────────

async fn ensure_corpus_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    eval: &KarsEval,
    resolved: &ResolvedCorpus,
) -> Result<(), ReconcileError> {
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert(
        "corpus.json".into(),
        String::from_utf8(resolved.bytes.clone()).map_err(|e| {
            ReconcileError::SerdeJson(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )))
        })?,
    );
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "kars.azure.com/karseval-corpus-digest".into(),
        resolved.digest.clone(),
    );
    annotations.insert(
        "kars.azure.com/karseval-corpus-label".into(),
        resolved.label.clone(),
    );
    let owner = eval.name_any();
    let owner_refs = serde_json::from_value(workloads::owner(eval)?)?;
    let mut cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.into()),
            annotations: Some(annotations),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                (LABEL_KEY_CLAW_EVAL.into(), owner),
                ("kars.azure.com/artifact".into(), "claw-eval-corpus".into()),
            ])),
            owner_references: Some(owner_refs),
            namespace: eval.namespace(),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };
    if let Some(existing) = api.get_opt(cm_name).await? {
        workloads::require_owner(&existing.metadata, eval)?;
        workloads::identity(&existing.metadata)?;
        if existing.data == cm.data
            && workloads::contains(
                &json!(existing.metadata.annotations),
                &json!(cm.metadata.annotations),
            )
        {
            return Ok(());
        }
        cm.metadata.uid = existing.metadata.uid;
        cm.metadata.resource_version = existing.metadata.resource_version;
        api.replace(cm_name, &PostParams::default(), &cm).await?;
    } else {
        api.create(&PostParams::default(), &cm).await?;
    }
    Ok(())
}

/// Default runner image — env-overridable for helm / dev workflows.
fn default_runner_image() -> String {
    std::env::var(RUNNER_IMAGE_ENV).unwrap_or_else(|_| DEFAULT_RUNNER_IMAGE.to_string())
}

fn sandbox_router_url(sandbox_name: &str) -> String {
    // Per-sandbox Service is named after the sandbox in namespace
    // `kars-<name>`, listening on 8443. Cross-namespace FQDN
    // keeps the runner Job free to live in the KarsEval CR's namespace.
    format!("http://{sandbox_name}.kars-{sandbox_name}.svc.cluster.local:8443")
}

/// Owner-reference fragment so spawned Jobs / CronJobs are garbage-
/// collected by the apiserver when the parent KarsEval is deleted.
/// Returns `null` if no UID is known yet (first reconcile before the
/// apiserver populated `metadata.uid` — should be rare; reconciler
/// re-runs after the next watch event will pick it up).
fn karseval_owner_refs(eval_name: &str, uid: &str) -> serde_json::Value {
    json!([{
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsEval",
        "name": eval_name,
        "uid": uid,
        "controller": true,
        "blockOwnerDeletion": true,
    }])
}

fn cron_job_name(eval_name: &str) -> String {
    format!("karseval-{eval_name}")
}

/// Build a deterministic Job name for a run-now spawn. The CR's
/// `resourceVersion` is included so two consecutive
/// `kubectl annotate ... run-now=true` updates resolve to two distinct
/// Job names, but a re-reconcile of the same CR generation does not
/// spam Jobs.
fn run_now_job_name(eval_name: &str, resource_version: Option<&str>) -> String {
    let suffix = resource_version
        .map(short_hash)
        .unwrap_or_else(|| "now".into());
    format!("karseval-{eval_name}-runnow-{suffix}")
}

fn short_hash(s: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    // 10 hex chars is plenty for K8s name uniqueness within one CR.
    digest.iter().take(5).map(|b| format!("{b:02x}")).collect()
}

#[allow(clippy::too_many_arguments)]
async fn ensure_run_now_job(
    api: &Api<Job>,
    job_name: &str,
    eval: &KarsEval,
    intent: &workloads::Intent,
    cm_name: &str,
    runner_image: &str,
    target_url: &str,
    corpus_label: &str,
) -> Result<(), ReconcileError> {
    let eval_name = eval.name_any();
    let pod_spec =
        runner_pod_spec_json(&eval_name, cm_name, runner_image, target_url, corpus_label);
    let mut metadata = json!({
        "name": job_name,
        "labels": {
            "app.kubernetes.io/managed-by": "kars-controller",
            LABEL_KEY_CLAW_EVAL: eval_name,
            "kars.azure.com/karseval-trigger": "run-now",
        },
        "annotations": {
            "kars.azure.com/karseval-name": eval_name,
        },
    });
    metadata["namespace"] = json!(eval.namespace());
    metadata["ownerReferences"] = workloads::owner(eval)?;
    if let Some(token) = eval.annotations().get(workloads::RUN_TOKEN) {
        metadata["annotations"][workloads::RUN_TOKEN] = json!(token);
    }
    let body = json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": metadata,
        "spec": {
            "backoffLimit": 0,
            "ttlSecondsAfterFinished": 3600,
            "template": {
                "metadata": {
                    "annotations": intent.annotations(),
                    "labels": {
                        "app.kubernetes.io/managed-by": "kars-controller",
                        LABEL_KEY_CLAW_EVAL: eval_name,
                    },
                },
                "spec": pod_spec,
            },
        },
    });
    if let Some(existing) = api.get_opt(job_name).await? {
        workloads::require_owner(&existing.metadata, eval)?;
        workloads::identity(&existing.metadata)?;
        if !workloads::contains(&serde_json::to_value(&existing.spec)?, &body["spec"])
            || existing.annotations().get(workloads::RUN_TOKEN)
                != eval.annotations().get(workloads::RUN_TOKEN)
        {
            return Err(
                anyhow::anyhow!("existing run slot differs from its produced intent").into(),
            );
        }
    } else {
        api.create(&PostParams::default(), &serde_json::from_value(body)?)
            .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn ensure_cronjob(
    api: &Api<CronJob>,
    cj_name: &str,
    eval: &KarsEval,
    intent: &workloads::Intent,
    schedule: &str,
    cm_name: &str,
    runner_image: &str,
    target_url: &str,
    corpus_label: &str,
) -> Result<(), ReconcileError> {
    let eval_name = eval.name_any();
    let pod_spec =
        runner_pod_spec_json(&eval_name, cm_name, runner_image, target_url, corpus_label);
    let mut metadata = json!({
        "name": cj_name,
        "labels": {
            "app.kubernetes.io/managed-by": "kars-controller",
            LABEL_KEY_CLAW_EVAL: eval_name,
        },
    });
    metadata["namespace"] = json!(eval.namespace());
    metadata["ownerReferences"] = workloads::owner(eval)?;
    let body = json!({
        "apiVersion": "batch/v1",
        "kind": "CronJob",
        "metadata": metadata,
        "spec": {
            "schedule": schedule,
            "concurrencyPolicy": "Forbid",
            "successfulJobsHistoryLimit": 3,
            "failedJobsHistoryLimit": 3,
            "jobTemplate": {
                "metadata": {
                    "annotations": intent.annotations(),
                    "labels": {
                        "app.kubernetes.io/managed-by": "kars-controller",
                        LABEL_KEY_CLAW_EVAL: eval_name,
                        "kars.azure.com/karseval-trigger": "schedule",
                    },
                },
                "spec": {
                    "backoffLimit": 0,
                    "ttlSecondsAfterFinished": 86400,
                    "template": {
                        "metadata": {
                            "annotations": intent.annotations(),
                            "labels": {
                                "app.kubernetes.io/managed-by": "kars-controller",
                                LABEL_KEY_CLAW_EVAL: eval_name,
                            },
                        },
                        "spec": pod_spec,
                    },
                },
            },
        },
    });
    let mut desired: CronJob = serde_json::from_value(body)?;
    if let Some(existing) = api.get_opt(cj_name).await? {
        workloads::require_owner(&existing.metadata, eval)?;
        workloads::identity(&existing.metadata)?;
        if workloads::contains(
            &serde_json::to_value(&existing.spec)?,
            &serde_json::to_value(&desired.spec)?,
        ) {
            return Ok(());
        }
        desired.metadata.uid = existing.metadata.uid;
        desired.metadata.resource_version = existing.metadata.resource_version;
        desired.status = existing.status;
        api.replace(cj_name, &PostParams::default(), &desired)
            .await?;
    } else {
        api.create(&PostParams::default(), &desired).await?;
    }
    Ok(())
}

async fn delete_if_exists<K>(api: &Api<K>, name: &str) -> Result<(), ReconcileError>
where
    K: kube::Resource + Clone + serde::de::DeserializeOwned + std::fmt::Debug,
    <K as kube::Resource>::DynamicType: Default,
{
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// ─────────────────────────────────────────────────────────────────────
// Finalizer
// ─────────────────────────────────────────────────────────────────────

async fn finalize(
    api: &Api<KarsEval>,
    configmaps: &Api<ConfigMap>,
    jobs: &Api<Job>,
    cronjobs: &Api<CronJob>,
    eval: &KarsEval,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("karseval-{name}-corpus");
    let _ = delete_if_exists(configmaps, &cm_name).await;
    let cj_name = cron_job_name(name);
    let _ = delete_if_exists(cronjobs, &cj_name).await;

    // Delete any Jobs the controller spawned. Per `Job` semantics, the
    // pods are garbage-collected by the JobController as a side effect.
    let job_lp = ListParams::default().labels(&format!("{LABEL_KEY_CLAW_EVAL}={name}"));
    if let Ok(job_list) = jobs.list(&job_lp).await {
        for j in job_list.items {
            let jn = j.name_any();
            let _ = jobs
                .delete(&jn, &DeleteParams::default().grace_period(0))
                .await;
        }
    }

    let finalizers: Vec<String> = eval
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsEval",
        "metadata": {"finalizers": finalizers},
    });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(karseval = %name, "KarsEvalDeleted");
    Ok(Action::await_change())
}

fn error_policy(eval: Arc<KarsEval>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsEval", error.class());
    tracing::warn!(
        karseval = %eval.name_any(),
        error_class = error.class(),
        error = %error,
        "KarsEval reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

pub async fn run(client: Client) -> Result<()> {
    let evals: Api<KarsEval> = Api::all(client.clone());
    match evals.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsEval CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("KarsEval CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(evals, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsEval", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("KarsEval reconciled {:?}", o),
                Err(e) => tracing::warn!("KarsEval reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}
