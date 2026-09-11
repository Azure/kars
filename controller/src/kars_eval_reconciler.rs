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
//!    `Degraded` condition via a distinct field manager
//!    (`kars-controller/karseval-drift`) so the sandbox reconciler
//!    surfaces the regression to operators.
//!
//! Webhook delivery + the `kars eval run` CLI surface ship in
//! slice 6.4.

mod evidence;
mod observation;
mod report;
#[cfg(test)]
mod result_tests;
mod runner;
mod workloads;

use anyhow::Result;
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

// ─────────────────────────────────────────────────────────────────────
// Corpus resolution
// ─────────────────────────────────────────────────────────────────────

async fn resolve_corpus(src: &CorpusSource) -> Result<ResolvedCorpus, (&'static str, String)> {
    match (&src.builtin, &src.bundle_ref) {
        (Some(name), None) => {
            // Use the embedded bytes directly — the runner reads the
            // mounted file and validates with the same parser, so the
            // bytes the controller mounts must be the exact bytes the
            // library shipped, not a re-serialisation (which would
            // perturb whitespace and ordering, changing the digest).
            let bytes = kars_eval_corpus::builtin_bytes(name).ok_or_else(|| {
                (
                    reason::CORPUS_BUILTIN_MISSING,
                    format!("builtin corpus {name:?} not found"),
                )
            })?;
            // Validate parseability so we never mount a corpus the
            // runner cannot read.
            kars_eval_corpus::parse(bytes).map_err(|e| {
                (
                    reason::CORPUS_PARSE_FAILED,
                    format!("builtin corpus {name:?} failed to parse: {e}"),
                )
            })?;
            let digest = sha256_hex(bytes);
            Ok(ResolvedCorpus {
                bytes: bytes.to_vec(),
                digest,
                label: format!("{CORPUS_LABEL_BUILTIN_PREFIX}{name}"),
            })
        }
        (None, Some(bundle)) => {
            let signer_policy_handle = crate::signer_policy::global();
            let result = match signer_policy_handle.snapshot() {
                crate::signer_policy::SignerPolicyState::FromConfigMap(p) => {
                    let cfg: crate::policy_fetcher::SignerPolicyConfig = p.into();
                    crate::policy_fetcher::fetch_and_verify_generic::<
                        crate::policy_canonical::eval_corpus::EvalCorpusKind,
                    >(bundle, &cfg)
                    .await
                }
                crate::signer_policy::SignerPolicyState::Malformed(msg) => Err(
                    crate::policy_fetcher::FetchError::SignerPolicyMalformed(msg),
                ),
                crate::signer_policy::SignerPolicyState::Absent => {
                    let cfg = crate::policy_fetcher::SignerPolicyConfig::from_env();
                    crate::policy_fetcher::fetch_and_verify_generic::<
                        crate::policy_canonical::eval_corpus::EvalCorpusKind,
                    >(bundle, &cfg)
                    .await
                }
            };
            match result {
                Ok(v) => Ok(ResolvedCorpus {
                    bytes: v.bytes,
                    digest: v.digest,
                    label: format!(
                        "{}/{}@{}",
                        bundle.registry, bundle.repository, bundle.digest
                    ),
                }),
                Err(e) => Err((reason::CORPUS_FETCH_FAILED, e.to_string())),
            }
        }
        (Some(_), Some(_)) | (None, None) => {
            // Defence in depth — CEL already enforces XOR.
            Err((
                reason::SPEC_INVALID,
                "spec.corpus must set exactly one of builtin or bundleRef".into(),
            ))
        }
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
// Sandbox drift patch
// ─────────────────────────────────────────────────────────────────────

async fn patch_sandbox_drift(
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
    let condition = json!({
        "type": crate::status::conditions::TYPE_DEGRADED,
        "status": "True",
        "reason": "ConformanceDrift",
        "message": message,
        "lastTransitionTime": rfc3339_now(),
    });
    let body = json!({
        "metadata": {"uid":uid, "resourceVersion":rv},
        "status": {
            "conditions": [condition],
        },
    });
    sandboxes
        .patch_status(sandbox_name, &PatchParams::default(), &Patch::Merge(body))
        .await?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Condition builders
// ─────────────────────────────────────────────────────────────────────

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn build_conditions(
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
async fn write_degraded(
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

// ─────────────────────────────────────────────────────────────────────
// Unit tests — pure helpers only. K8s-API-touching paths are
// exercised by integration tests (slice 6.3 also adds end-to-end
// coverage via the in-cluster kind matrix in slice 6.5).
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn short_hash_is_deterministic_and_compact() {
        let a = short_hash("12345");
        let b = short_hash("12345");
        let c = short_hash("12346");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 10);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn run_now_job_name_differs_per_resource_version() {
        let n1 = run_now_job_name("e1", Some("100"));
        let n2 = run_now_job_name("e1", Some("101"));
        let n3 = run_now_job_name("e1", None);
        assert_ne!(n1, n2);
        assert!(n1.starts_with("karseval-e1-runnow-"));
        assert!(n3.ends_with("now"));
    }

    #[test]
    fn cron_job_name_stable() {
        assert_eq!(cron_job_name("my-eval"), "karseval-my-eval");
    }

    #[test]
    fn sandbox_router_url_format() {
        assert_eq!(
            sandbox_router_url("agent-001"),
            "http://agent-001.kars-agent-001.svc.cluster.local:8443"
        );
    }

    #[test]
    fn karseval_owner_refs_emits_controller_blocking_reference() {
        let refs =
            karseval_owner_refs("nightly-regression", "8f2a3b4c-1111-2222-3333-444455556666");
        let arr = refs.as_array().expect("owner refs must be a JSON array");
        assert_eq!(
            arr.len(),
            1,
            "exactly one owner reference per spawned child"
        );
        let r = &arr[0];
        assert_eq!(r["apiVersion"], "kars.azure.com/v1alpha1");
        assert_eq!(r["kind"], "KarsEval");
        assert_eq!(r["name"], "nightly-regression");
        assert_eq!(r["uid"], "8f2a3b4c-1111-2222-3333-444455556666");
        assert_eq!(
            r["controller"], true,
            "must mark controller=true so Jobs/CronJobs are GC'd with the parent",
        );
        assert_eq!(
            r["blockOwnerDeletion"], true,
            "finalizer correctness: parent delete waits for child cleanup",
        );
    }

    #[test]
    fn default_runner_image_env_override() {
        // Save / restore the env var around the call to avoid bleeding
        // into other tests (cargo runs tests in parallel; this is
        // tolerated only because the assertion runs strictly inside
        // the unsafe-set/unsafe-remove brackets).
        let prior = std::env::var(RUNNER_IMAGE_ENV).ok();
        // SAFETY: tests are single-threaded within this function and the
        // env-var roundtrip is restored before exit. Other parallel
        // tests do not touch `RUNNER_IMAGE_ENV`.
        unsafe {
            std::env::set_var(RUNNER_IMAGE_ENV, "myrepo/runner:1.2.3");
        }
        assert_eq!(default_runner_image(), "myrepo/runner:1.2.3");
        unsafe {
            std::env::remove_var(RUNNER_IMAGE_ENV);
        }
        assert_eq!(default_runner_image(), DEFAULT_RUNNER_IMAGE);
        if let Some(prior) = prior {
            unsafe {
                std::env::set_var(RUNNER_IMAGE_ENV, prior);
            }
        }
    }

    #[test]
    fn parse_report_happy_path() {
        let log = r#"
        2026-05-14T10:00:00Z INFO starting runner
        {"schemaVersion":"v1","corpusName":"builtin:jailbreak-baseline","corpusDigest":"sha256:abc","startedAt":"2026-05-14T10:00:00Z","completedAt":"2026-05-14T10:00:05Z","durationMs":5000,"routerBase":"http://x:8443","total":3,"passed":2,"failed":1,"results":[
          {"caseId":"c1","tags":[],"scenario":{"kind":"ChatCompletion","messageCount":1},"expected":{"decision":"Allowed"},"actual":{"decision":"Allowed"},"verdict":{"result":"Pass"},"durationMs":100},
          {"caseId":"c2","tags":[],"scenario":{"kind":"ChatCompletion","messageCount":1},"expected":{"decision":"Allowed"},"actual":{"decision":"Allowed"},"verdict":{"result":"Pass"},"durationMs":100},
          {"caseId":"c3","tags":[],"scenario":{"kind":"ChatCompletion","messageCount":1},"expected":{"decision":"Blocked"},"actual":{"decision":"Allowed"},"verdict":{"result":"Fail","reason":"DecisionMismatch","expected":"Blocked","actual":"Allowed"},"durationMs":100}
        ]}
        "#;
        let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
        let parsed =
            report::parse(log, &corpus, "unused", "unused").expect("parses readable legacy");
        assert_eq!(parsed.version, "v1");
        assert_eq!(parsed.total, 3);
        assert_eq!(parsed.passed, 2);
        assert_eq!(parsed.failed, 1);
        assert!(!parsed.qualified());
        assert_eq!(parsed.wire["results"][0]["verdict"]["result"], "Pass");
        assert_eq!(parsed.wire["results"][2]["verdict"]["result"], "Fail");
    }

    #[test]
    fn parse_report_ignores_non_json_lines() {
        let log = "INFO booting\nERROR oh no\nnot json at all\n";
        let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
        assert!(report::parse(log, &corpus, "unused", "unused").is_err());
    }

    #[test]
    fn malformed_count_only_reports_are_not_current_evidence() {
        let log = r#"{"schemaVersion":"v1","total":1,"passed":1,"failed":0,"results":[]}
{"schemaVersion":"v1","total":5,"passed":4,"failed":1,"results":[]}"#;
        let corpus = kars_eval_corpus::load_builtin("jailbreak-baseline").unwrap();
        assert!(report::parse(log, &corpus, "unused", "unused").is_err());
    }

    #[tokio::test]
    async fn resolve_corpus_builtin_roundtrip() {
        let src = CorpusSource {
            builtin: Some("jailbreak-baseline".into()),
            bundle_ref: None,
        };
        let resolved = resolve_corpus(&src).await.expect("builtin loads");
        assert!(resolved.label.starts_with("builtin:"));
        assert!(resolved.digest.starts_with("sha256:"));
        assert!(!resolved.bytes.is_empty());
        // Bytes must round-trip through the eval-corpus parser.
        let _ = kars_eval_corpus::parse(&resolved.bytes).expect("re-parses");
    }

    #[tokio::test]
    async fn resolve_corpus_unknown_builtin_errors() {
        let src = CorpusSource {
            builtin: Some("does-not-exist".into()),
            bundle_ref: None,
        };
        let (why, _msg) = resolve_corpus(&src).await.unwrap_err();
        assert_eq!(why, reason::CORPUS_BUILTIN_MISSING);
    }

    #[tokio::test]
    async fn resolve_corpus_neither_set_errors() {
        let src = CorpusSource {
            builtin: None,
            bundle_ref: None,
        };
        let (why, _msg) = resolve_corpus(&src).await.unwrap_err();
        assert_eq!(why, reason::SPEC_INVALID);
    }

    #[test]
    fn build_conditions_first_reconcile_has_no_result() {
        let resolved = ResolvedCorpus {
            bytes: vec![],
            digest: "sha256:deadbeef".into(),
            label: "builtin:jailbreak-baseline".into(),
        };
        let conds = build_conditions(
            &[],
            Some(1),
            &resolved,
            None,
            Some("karseval-x"),
            None,
            ("Pending", false),
        );
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "Scheduled");
        let drift = conds
            .iter()
            .find(|c| c.type_ == TYPE_CONFORMANCE_DRIFT)
            .unwrap();
        assert_eq!(drift.status, "False");
        assert_eq!(drift.reason, "NotEvaluated");
    }

    #[test]
    fn build_conditions_drift_branches_ready_false_and_drift_true() {
        let resolved = ResolvedCorpus {
            bytes: vec![],
            digest: "sha256:abc".into(),
            label: "builtin:jailbreak-baseline".into(),
        };
        let r = EvalResult {
            schema_version: "v2".into(),
            corpus_digest: "sha256:abc".into(),
            total: 10,
            passed: 7,
            failed: 3,
            errored: 0,
            corpus_label: "builtin:jailbreak-baseline".into(),
            job_name: "karseval-x-runnow-aa".into(),
            first_failing_cases: vec!["c-1".into()],
        };
        let conds = build_conditions(
            &[],
            Some(2),
            &resolved,
            Some("job-x"),
            None,
            Some(&r),
            ("DriftDetected", true),
        );
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        let degraded = conds.iter().find(|c| c.type_ == "Degraded").unwrap();
        let drift = conds
            .iter()
            .find(|c| c.type_ == TYPE_CONFORMANCE_DRIFT)
            .unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "DriftDetected");
        assert_eq!(degraded.status, "True");
        assert_eq!(drift.status, "True");
    }

    #[test]
    fn build_conditions_all_pass_branches_ready_true() {
        let resolved = ResolvedCorpus {
            bytes: vec![],
            digest: "sha256:abc".into(),
            label: "builtin:jailbreak-baseline".into(),
        };
        let r = EvalResult {
            schema_version: "v2".into(),
            corpus_digest: "sha256:abc".into(),
            total: 10,
            passed: 10,
            failed: 0,
            errored: 0,
            corpus_label: "builtin:jailbreak-baseline".into(),
            job_name: "karseval-x-runnow-bb".into(),
            first_failing_cases: vec![],
        };
        let conds = build_conditions(
            &[],
            Some(3),
            &resolved,
            None,
            Some("cj"),
            Some(&r),
            ("AllPassed", false),
        );
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "AllPassed");
    }

    #[test]
    fn pod_spec_renders_corpus_mount_and_args() {
        let spec = runner_pod_spec_json(
            "my-eval",
            "karseval-my-eval-corpus",
            "myrepo/runner:1",
            "http://agent-1.kars-agent-1.svc.cluster.local:8443",
            "builtin:jailbreak-baseline",
        );
        let args = spec["containers"][0]["args"].as_array().unwrap();
        let args: Vec<&str> = args.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(args.contains(&"--corpus"));
        assert!(args.contains(&"/etc/kars/eval-corpus/corpus.json"));
        assert!(args.contains(&"--router-base"));
        assert!(args.contains(&"http://agent-1.kars-agent-1.svc.cluster.local:8443"));
        assert!(!args.contains(&"--corpus-label"));
        assert!(!args.contains(&"builtin:jailbreak-baseline"));
        let vol = &spec["volumes"][0];
        assert_eq!(vol["name"], "corpus");
        assert_eq!(vol["configMap"]["name"], "karseval-my-eval-corpus");
    }
}
