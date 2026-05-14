// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `ClawEval` reconciler — slice 6.3.
//!
//! Watches `ClawEval` CRs and, for each:
//!
//! 1. Ensures the cleanup finalizer.
//! 2. Resolves the corpus referenced by `spec.corpus` (builtin via the
//!    embedded `azureclaw-eval-corpus` library, or a signed OCI bundle
//!    via `policy_fetcher::fetch_and_verify_generic::<EvalCorpusKind>`).
//! 3. Persists the resolved corpus into a `ConfigMap`
//!    (`claweval-<name>-corpus`, key `corpus.json`).
//! 4. If `spec.schedule` is set, ensures a controller-owned `CronJob`
//!    (`claweval-<name>`) that spawns runner pods on schedule.
//! 5. If the `azureclaw.azure.com/run-now=true` annotation is present,
//!    spawns a one-shot `Job` and strips the annotation.
//! 6. Observes completed `Job`s, reads each runner pod's log, parses
//!    the `RunReport`, and stamps `status.last_result` + appends to
//!    bounded `status.history`.
//! 7. If `spec.fail_sandbox_on_drift` is true and the latest run
//!    reports any failed case, patches the target `ClawSandbox`'s
//!    `Degraded` condition via a distinct field manager
//!    (`azureclaw-controller/claweval-drift`) so the sandbox reconciler
//!    surfaces the regression to operators.
//!
//! Webhook delivery + the `azureclaw eval run` CLI surface ship in
//! slice 6.4.

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::claw_eval::{
    ANNOTATION_RUN_NOW, ClawEval, ClawEvalStatus, CorpusSource, EvalResult, EvalResultSummary,
    TYPE_CONFORMANCE_DRIFT, push_history_bounded, reason,
};
use crate::crd::ClawSandbox;
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason as cond_reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_PENDING, PHASE_READY};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_EVAL;
const DRIFT_FIELD_MANAGER: &str = "azureclaw-controller/claweval-drift";
const FINALIZER: &str = "azureclaw.azure.com/claweval-cleanup";

const REQUEUE_OK: Duration = Duration::from_secs(300);
const REQUEUE_FAIL: Duration = Duration::from_secs(60);
const REQUEUE_AWAITING_RUN: Duration = Duration::from_secs(30);

const RUNNER_IMAGE_ENV: &str = "AZURECLAW_CONFORMANCE_RUNNER_IMAGE";
const DEFAULT_RUNNER_IMAGE: &str = "ghcr.io/azure/azureclaw/conformance-runner:latest";

const CORPUS_LABEL_BUILTIN_PREFIX: &str = "builtin:";
const LABEL_KEY_CLAW_EVAL: &str = "azureclaw.azure.com/claweval";

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

/// Outcome of resolving `spec.corpus`. Either we have bytes + digest +
/// label ready to mount, or we have a hard error to surface as
/// `Degraded`.
#[derive(Debug)]
struct ResolvedCorpus {
    bytes: Vec<u8>,
    digest: String,
    /// Human-friendly identifier the runner echoes in its `RunReport`:
    /// either `builtin:<name>` or `<registry>/<repository>@<digest>`.
    label: String,
}

struct Ctx {
    client: Client,
}

async fn reconcile(eval: Arc<ClawEval>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = eval.name_any();
    let ns = eval.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(claweval = %name, ns = %ns, "Reconciling ClawEval");

    let evals_api: Api<ClawEval> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), &ns);
    let cronjobs: Api<CronJob> = Api::namespaced(ctx.client.clone(), &ns);
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), &ns);

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
            "apiVersion": "azureclaw.azure.com/v1alpha1",
            "kind": "ClawEval",
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
                &name,
                observed_generation,
                &prior_conditions,
                &prior_status,
                why_reason,
                &why_msg,
            )
            .await;
        }
    };

    // -------- 2. Ensure corpus ConfigMap --------------------------
    let cm_name = format!("claweval-{name}-corpus");
    if let Err(e) = ensure_corpus_configmap(&configmaps, &cm_name, &name, &resolved).await {
        tracing::warn!(claweval = %name, error_class = e.class(), "ClawEvalCorpusWriteFailed");
        return write_degraded(
            &evals_api,
            &name,
            observed_generation,
            &prior_conditions,
            &prior_status,
            reason::CORPUS_FETCH_FAILED,
            &format!("corpus ConfigMap write failed: {e}"),
        )
        .await;
    }

    let runner_image = eval
        .spec
        .runner_image
        .clone()
        .unwrap_or_else(default_runner_image);
    let target_url = sandbox_router_url(&eval.spec.target_sandbox_ref.name);

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
        let job_name = run_now_job_name(&name, eval.metadata.resource_version.as_deref());
        match ensure_run_now_job(
            &jobs,
            &job_name,
            &name,
            eval.metadata.uid.as_deref(),
            &cm_name,
            &runner_image,
            &target_url,
            &resolved.label,
        )
        .await
        {
            Ok(()) => {
                spawned_run_now = Some(job_name.clone());
                if let Err(e) = clear_run_now_annotation(&evals_api, &name).await {
                    tracing::warn!(claweval = %name, "failed to clear run-now annotation: {e}");
                }
            }
            Err(e) => {
                tracing::warn!(claweval = %name, "ClawEvalRunNowJobCreateFailed: {e}");
            }
        }
    }

    // -------- 4. Ensure CronJob or delete -------------------------
    let cron_job_name = if let Some(schedule) = eval.spec.schedule.as_deref() {
        let cj_name = cron_job_name(&name);
        match ensure_cronjob(
            &cronjobs,
            &cj_name,
            &name,
            eval.metadata.uid.as_deref(),
            schedule,
            &cm_name,
            &runner_image,
            &target_url,
            &resolved.label,
        )
        .await
        {
            Ok(()) => Some(cj_name),
            Err(e) => {
                tracing::warn!(claweval = %name, "ClawEvalCronJobApplyFailed: {e}");
                None
            }
        }
    } else {
        let cj_name = cron_job_name(&name);
        if let Err(e) = delete_if_exists(&cronjobs, &cj_name).await {
            tracing::warn!(claweval = %name, "stale cronjob delete failed: {e}");
        }
        None
    };

    // -------- 5. Observe completed Jobs ---------------------------
    let prior_history = prior_status.history.clone();
    let (history, last_result, last_run_at) = observe_completed_jobs(
        &jobs,
        &pods,
        &name,
        &resolved.digest,
        &resolved.label,
        &prior_history,
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(claweval = %name, "observe_completed_jobs failed: {e}");
        (
            prior_history.clone(),
            prior_status.last_result.clone(),
            prior_status.last_run_at.clone(),
        )
    });

    let drift_detected = last_result.as_ref().map(|r| r.failed > 0).unwrap_or(false);

    // -------- 6. Optionally patch sandbox Degraded ----------------
    if eval.spec.fail_sandbox_on_drift.unwrap_or(false) && drift_detected {
        let sandboxes: Api<ClawSandbox> = Api::namespaced(ctx.client.clone(), &ns);
        let fail_msg = last_result
            .as_ref()
            .map(|r| {
                format!(
                    "ClawEval/{} reported {} failed / {} total against corpus {}",
                    name, r.failed, r.total, r.corpus_label
                )
            })
            .unwrap_or_else(|| format!("ClawEval/{name} reported drift"));
        if let Err(e) =
            patch_sandbox_drift(&sandboxes, &eval.spec.target_sandbox_ref.name, &fail_msg).await
        {
            tracing::warn!(
                claweval = %name,
                target = %eval.spec.target_sandbox_ref.name,
                "ClawEvalSandboxDriftPatchFailed: {e}"
            );
        }
    }

    // -------- 7. Patch ClawEval status ----------------------------
    let phase = match (&last_result, drift_detected) {
        (Some(_), false) => PHASE_READY,
        (Some(_), true) => PHASE_DEGRADED,
        (None, _) => PHASE_PENDING,
    };

    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        &resolved,
        spawned_run_now.as_deref(),
        cron_job_name.as_deref(),
        last_result.as_ref(),
        drift_detected,
    );

    let new_status = ClawEvalStatus {
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
    };
    let status_patch = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawEval",
        "status": new_status,
    });
    evals_api
        .patch_status(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(status_patch),
        )
        .await?;

    if last_result.is_none() {
        Ok(Action::requeue(REQUEUE_AWAITING_RUN))
    } else if drift_detected {
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
            let bytes = azureclaw_eval_corpus::builtin_bytes(name).ok_or_else(|| {
                (
                    reason::CORPUS_BUILTIN_MISSING,
                    format!("builtin corpus {name:?} not found"),
                )
            })?;
            // Validate parseability so we never mount a corpus the
            // runner cannot read.
            azureclaw_eval_corpus::parse(bytes).map_err(|e| {
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
    owner: &str,
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
        "azureclaw.azure.com/claweval-corpus-digest".into(),
        resolved.digest.clone(),
    );
    annotations.insert(
        "azureclaw.azure.com/claweval-corpus-label".into(),
        resolved.label.clone(),
    );
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.into()),
            annotations: Some(annotations),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "azureclaw-controller".into(),
                ),
                (LABEL_KEY_CLAW_EVAL.into(), owner.into()),
                (
                    "azureclaw.azure.com/artifact".into(),
                    "claw-eval-corpus".into(),
                ),
            ])),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };
    api.patch(
        cm_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&cm),
    )
    .await?;
    Ok(())
}

/// Default runner image — env-overridable for helm / dev workflows.
fn default_runner_image() -> String {
    std::env::var(RUNNER_IMAGE_ENV).unwrap_or_else(|_| DEFAULT_RUNNER_IMAGE.to_string())
}

fn sandbox_router_url(sandbox_name: &str) -> String {
    // Per-sandbox Service is named after the sandbox in namespace
    // `azureclaw-<name>`, listening on 8443. Cross-namespace FQDN
    // keeps the runner Job free to live in the ClawEval CR's namespace.
    format!("http://{sandbox_name}.azureclaw-{sandbox_name}.svc.cluster.local:8443")
}

/// Owner-reference fragment so spawned Jobs / CronJobs are garbage-
/// collected by the apiserver when the parent ClawEval is deleted.
/// Returns `null` if no UID is known yet (first reconcile before the
/// apiserver populated `metadata.uid` — should be rare; reconciler
/// re-runs after the next watch event will pick it up).
fn claweval_owner_refs(eval_name: &str, uid: &str) -> serde_json::Value {
    json!([{
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawEval",
        "name": eval_name,
        "uid": uid,
        "controller": true,
        "blockOwnerDeletion": true,
    }])
}

fn cron_job_name(eval_name: &str) -> String {
    format!("claweval-{eval_name}")
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
    format!("claweval-{eval_name}-runnow-{suffix}")
}

fn short_hash(s: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    // 10 hex chars is plenty for K8s name uniqueness within one CR.
    digest.iter().take(5).map(|b| format!("{b:02x}")).collect()
}

fn runner_pod_spec_json(
    eval_name: &str,
    cm_name: &str,
    runner_image: &str,
    target_url: &str,
    corpus_label: &str,
) -> serde_json::Value {
    json!({
        "restartPolicy": "Never",
        "containers": [{
            "name": "runner",
            "image": runner_image,
            "imagePullPolicy": "IfNotPresent",
            "args": [
                "--corpus", "/etc/azureclaw/eval-corpus/corpus.json",
                "--corpus-label", corpus_label,
                "--router-base", target_url,
                "--output", "/dev/stdout",
            ],
            "env": [
                {"name": "RUST_LOG", "value": "info"},
                {"name": "AZURECLAW_EVAL_NAME", "value": eval_name},
            ],
            "volumeMounts": [{
                "name": "corpus",
                "mountPath": "/etc/azureclaw/eval-corpus",
                "readOnly": true,
            }],
            "resources": {
                "requests": {"cpu": "50m", "memory": "64Mi"},
                "limits": {"cpu": "500m", "memory": "256Mi"},
            },
        }],
        "volumes": [{
            "name": "corpus",
            "configMap": {"name": cm_name},
        }],
    })
}

#[allow(clippy::too_many_arguments)]
async fn ensure_run_now_job(
    api: &Api<Job>,
    job_name: &str,
    eval_name: &str,
    eval_uid: Option<&str>,
    cm_name: &str,
    runner_image: &str,
    target_url: &str,
    corpus_label: &str,
) -> Result<(), ReconcileError> {
    let pod_spec = runner_pod_spec_json(eval_name, cm_name, runner_image, target_url, corpus_label);
    let mut metadata = json!({
        "name": job_name,
        "labels": {
            "app.kubernetes.io/managed-by": "azureclaw-controller",
            LABEL_KEY_CLAW_EVAL: eval_name,
            "azureclaw.azure.com/claweval-trigger": "run-now",
        },
        "annotations": {
            "azureclaw.azure.com/claweval-name": eval_name,
        },
    });
    if let Some(uid) = eval_uid {
        metadata["ownerReferences"] = claweval_owner_refs(eval_name, uid);
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
                    "labels": {
                        "app.kubernetes.io/managed-by": "azureclaw-controller",
                        LABEL_KEY_CLAW_EVAL: eval_name,
                    },
                },
                "spec": pod_spec,
            },
        },
    });
    api.patch(
        job_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(body),
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn ensure_cronjob(
    api: &Api<CronJob>,
    cj_name: &str,
    eval_name: &str,
    eval_uid: Option<&str>,
    schedule: &str,
    cm_name: &str,
    runner_image: &str,
    target_url: &str,
    corpus_label: &str,
) -> Result<(), ReconcileError> {
    let pod_spec = runner_pod_spec_json(eval_name, cm_name, runner_image, target_url, corpus_label);
    let mut metadata = json!({
        "name": cj_name,
        "labels": {
            "app.kubernetes.io/managed-by": "azureclaw-controller",
            LABEL_KEY_CLAW_EVAL: eval_name,
        },
    });
    if let Some(uid) = eval_uid {
        metadata["ownerReferences"] = claweval_owner_refs(eval_name, uid);
    }
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
                        "app.kubernetes.io/managed-by": "azureclaw-controller",
                        LABEL_KEY_CLAW_EVAL: eval_name,
                        "azureclaw.azure.com/claweval-trigger": "schedule",
                    },
                },
                "spec": {
                    "backoffLimit": 0,
                    "ttlSecondsAfterFinished": 86400,
                    "template": {
                        "metadata": {
                            "labels": {
                                "app.kubernetes.io/managed-by": "azureclaw-controller",
                                LABEL_KEY_CLAW_EVAL: eval_name,
                            },
                        },
                        "spec": pod_spec,
                    },
                },
            },
        },
    });
    api.patch(
        cj_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(body),
    )
    .await?;
    Ok(())
}

async fn clear_run_now_annotation(api: &Api<ClawEval>, name: &str) -> Result<(), ReconcileError> {
    // JSON-merge `null` removes the key.
    let body = json!({
        "metadata": {
            "annotations": {ANNOTATION_RUN_NOW: null},
        },
    });
    api.patch(name, &PatchParams::default(), &Patch::Merge(body))
        .await?;
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
// Job observation + RunReport parsing
// ─────────────────────────────────────────────────────────────────────

async fn observe_completed_jobs(
    jobs: &Api<Job>,
    pods: &Api<Pod>,
    eval_name: &str,
    corpus_digest: &str,
    corpus_label: &str,
    prior_history: &[EvalResultSummary],
) -> Result<(Vec<EvalResultSummary>, Option<EvalResult>, Option<String>), ReconcileError> {
    let lp = ListParams::default().labels(&format!("{LABEL_KEY_CLAW_EVAL}={eval_name}"));
    let job_list = jobs.list(&lp).await?;

    // Collect (job, completion_time) pairs for jobs that have a
    // succeeded count > 0. Sort by completion_time ascending so we
    // ingest them in chronological order.
    let mut completed: Vec<(String, String)> = Vec::new();
    for j in job_list.items {
        let job_name = j.name_any();
        let succeeded = j.status.as_ref().and_then(|s| s.succeeded).unwrap_or(0);
        if succeeded == 0 {
            continue;
        }
        let completion_time = j
            .status
            .as_ref()
            .and_then(|s| s.completion_time.as_ref())
            .map(|t| timestamp_to_rfc3339(&t.0))
            .unwrap_or_else(rfc3339_now);
        completed.push((job_name, completion_time));
    }
    completed.sort_by(|a, b| a.1.cmp(&b.1));

    let already_seen: std::collections::HashSet<&str> =
        prior_history.iter().map(|h| h.job_name.as_str()).collect();

    let mut history = prior_history.to_vec();
    let mut last_result: Option<EvalResult> = None;
    let mut last_run_at: Option<String> = None;

    for (job_name, completion_time) in completed {
        if already_seen.contains(job_name.as_str()) {
            continue;
        }
        let report = match read_job_report(pods, &job_name).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::warn!(
                    claweval = %eval_name,
                    job = %job_name,
                    "no parseable RunReport on pod log"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    claweval = %eval_name,
                    job = %job_name,
                    "read_job_report failed: {e}"
                );
                continue;
            }
        };

        let first_failing_cases: Vec<String> = report
            .results
            .iter()
            .filter(|c| c.verdict_pass.is_none() || c.verdict_pass == Some(false))
            .take(5)
            .map(|c| c.case_id.clone())
            .collect();
        let failed_u32 = u32::try_from(report.failed).unwrap_or(u32::MAX);
        let passed_u32 = u32::try_from(report.passed).unwrap_or(u32::MAX);
        let total_u32 = u32::try_from(report.total).unwrap_or(u32::MAX);
        let errored_u32 = total_u32.saturating_sub(passed_u32.saturating_add(failed_u32));

        let result = EvalResult {
            schema_version: report.schema_version,
            corpus_digest: corpus_digest.to_string(),
            total: total_u32,
            passed: passed_u32,
            failed: failed_u32,
            errored: errored_u32,
            corpus_label: corpus_label.to_string(),
            job_name: job_name.clone(),
            first_failing_cases,
        };
        let summary = EvalResultSummary {
            at: completion_time.clone(),
            corpus_digest: corpus_digest.to_string(),
            total: total_u32,
            passed: passed_u32,
            failed: failed_u32,
            errored: errored_u32,
            job_name: job_name.clone(),
        };
        history = push_history_bounded(history, summary);
        last_result = Some(result);
        last_run_at = Some(completion_time);
    }

    if last_result.is_none() {
        // Preserve prior result if no new run completed this cycle.
        last_result = history.first().map(|h| EvalResult {
            schema_version: "v1".into(),
            corpus_digest: h.corpus_digest.clone(),
            total: h.total,
            passed: h.passed,
            failed: h.failed,
            errored: h.errored,
            corpus_label: corpus_label.to_string(),
            job_name: h.job_name.clone(),
            first_failing_cases: vec![],
        });
        if last_result.is_some() {
            last_run_at = history.first().map(|h| h.at.clone());
        }
    }

    Ok((history, last_result, last_run_at))
}

/// Compact projection of the runner's `RunReport`. We deliberately
/// re-declare the relevant fields here (instead of importing the
/// `RunReport` type from the runner crate) so the controller is not
/// coupled to the runner's full schema. Only the contract surface in
/// `REPORT_SCHEMA_VERSION = "v1"` is parsed.
#[derive(Debug, serde::Deserialize)]
struct ParsedReport {
    #[serde(rename = "schemaVersion")]
    schema_version: String,
    total: usize,
    passed: usize,
    failed: usize,
    #[serde(default)]
    results: Vec<ParsedCase>,
}

#[derive(Debug, serde::Deserialize)]
struct ParsedCase {
    #[serde(rename = "caseId")]
    case_id: String,
    /// Materialised from `verdict.result == "Pass"`. `None` for
    /// malformed entries.
    #[serde(skip_deserializing, default)]
    verdict_pass: Option<bool>,
    /// Raw verdict object — used to populate `verdict_pass`.
    #[serde(default)]
    verdict: serde_json::Value,
}

async fn read_job_report(
    pods: &Api<Pod>,
    job_name: &str,
) -> Result<Option<ParsedReport>, ReconcileError> {
    let lp = ListParams::default().labels(&format!("job-name={job_name}"));
    let pod_list = pods.list(&lp).await?;
    let mut pod_name: Option<String> = None;
    for p in pod_list.items {
        // Prefer pods that have already reached terminal state.
        let phase = p
            .status
            .as_ref()
            .and_then(|s| s.phase.clone())
            .unwrap_or_default();
        if phase == crate::status::phase::POD_PHASE_SUCCEEDED
            || phase == crate::status::phase::POD_PHASE_FAILED
        {
            pod_name = Some(p.name_any());
            break;
        }
        if pod_name.is_none() {
            pod_name = Some(p.name_any());
        }
    }
    let Some(pod) = pod_name else {
        return Ok(None);
    };
    let logs = match pods.logs(&pod, &kube::api::LogParams::default()).await {
        Ok(l) => l,
        Err(kube::Error::Api(ae)) if ae.code == 404 => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(parse_report_from_log(&logs))
}

/// Walk a pod log looking for the runner's JSON report. The runner
/// emits the report to stdout as a single JSON object — typically
/// compact on one line, but the parser tolerates a pretty-printed
/// (multi-line) report as well. Tracing goes to stderr (kept
/// separate by container's log mux). The latest successfully-parsed
/// report wins (a re-run within the same pod, unlikely but
/// supported, is honoured).
fn parse_report_from_log(log: &str) -> Option<ParsedReport> {
    let mut latest: Option<ParsedReport> = None;
    // Fast path: a compact single-line report.
    for line in log.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        if let Some(p) = try_parse_report(line) {
            latest = Some(p);
        }
    }
    if latest.is_some() {
        return latest;
    }
    // Slow path: pretty-printed JSON spanning multiple lines. Scan
    // for every `{` byte and let `serde_json::Deserializer::into_iter`
    // consume one JSON value starting at that offset; the deserializer
    // tolerates trailing input, so a multi-line object embedded in a
    // larger log is recoverable.
    let bytes = log.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'{' {
            continue;
        }
        let suffix = &log[i..];
        let mut de = serde_json::Deserializer::from_str(suffix).into_iter::<ParsedReport>();
        if let Some(Ok(mut parsed)) = de.next() {
            if parsed.schema_version.is_empty() {
                continue;
            }
            for case in parsed.results.iter_mut() {
                case.verdict_pass = case
                    .verdict
                    .get("result")
                    .and_then(|v| v.as_str())
                    .map(|s| s == "Pass");
            }
            latest = Some(parsed);
        }
    }
    latest
}

fn try_parse_report(s: &str) -> Option<ParsedReport> {
    let mut parsed: ParsedReport = serde_json::from_str(s).ok()?;
    if parsed.schema_version.is_empty() {
        return None;
    }
    for case in parsed.results.iter_mut() {
        case.verdict_pass = case
            .verdict
            .get("result")
            .and_then(|v| v.as_str())
            .map(|s| s == "Pass");
    }
    Some(parsed)
}

// ─────────────────────────────────────────────────────────────────────
// Sandbox drift patch
// ─────────────────────────────────────────────────────────────────────

async fn patch_sandbox_drift(
    sandboxes: &Api<ClawSandbox>,
    sandbox_name: &str,
    message: &str,
) -> Result<(), ReconcileError> {
    let condition = json!({
        "type": crate::status::conditions::TYPE_DEGRADED,
        "status": "True",
        "reason": "ConformanceDrift",
        "message": message,
        "lastTransitionTime": rfc3339_now(),
    });
    let body = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawSandbox",
        "status": {
            "conditions": [condition],
        },
    });
    sandboxes
        .patch_status(
            sandbox_name,
            &PatchParams::apply(DRIFT_FIELD_MANAGER).force(),
            &Patch::Apply(body),
        )
        .await?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Condition builders
// ─────────────────────────────────────────────────────────────────────

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Convert a `jiff::Timestamp` (k8s_openapi default time type) to a
/// seconds-precision RFC 3339 string. `jiff::Timestamp`'s `Display`
/// impl emits sub-second precision; we strip via chrono to keep status
/// strings stable across reconciles.
fn timestamp_to_rfc3339(ts: &k8s_openapi::jiff::Timestamp) -> String {
    let secs = ts.as_second();
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(rfc3339_now)
}

#[allow(clippy::too_many_arguments)]
fn build_conditions(
    prior: &[Condition],
    observed_generation: Option<i64>,
    resolved: &ResolvedCorpus,
    spawned_run_now: Option<&str>,
    cron_job_name: Option<&str>,
    last_result: Option<&EvalResult>,
    drift_detected: bool,
) -> Vec<Condition> {
    let mut out: Vec<Condition> = Vec::with_capacity(4);

    let prior_ready = conditions::find(prior, conditions::TYPE_READY);
    let prior_progressing = conditions::find(prior, conditions::TYPE_PROGRESSING);
    let prior_degraded = conditions::find(prior, conditions::TYPE_DEGRADED);
    let prior_drift = conditions::find(prior, TYPE_CONFORMANCE_DRIFT);

    let ready_status = if last_result.is_some() && !drift_detected {
        cond_status::TRUE
    } else {
        cond_status::FALSE
    };
    let ready_reason = if last_result.is_none() {
        if spawned_run_now.is_some() {
            reason::RUN_TRIGGERED
        } else if cron_job_name.is_some() {
            reason::SCHEDULED
        } else {
            cond_reason::RECONCILED
        }
    } else if drift_detected {
        reason::DRIFT_DETECTED
    } else {
        reason::ALL_PASSED
    };
    let ready_msg = match (last_result, drift_detected) {
        (None, _) => format!(
            "awaiting first run — corpus {} ({})",
            resolved.label, resolved.digest
        ),
        (Some(r), false) => format!(
            "all {} cases passed against corpus {}",
            r.total, r.corpus_label
        ),
        (Some(r), true) => format!(
            "{} of {} cases failed against corpus {}",
            r.failed, r.total, r.corpus_label
        ),
    };
    out.push(conditions::preserve_transition_time(
        prior_ready,
        conditions::TYPE_READY,
        ready_status,
        ready_reason,
        &ready_msg,
        observed_generation,
    ));

    let progressing_status = if last_result.is_none() {
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
        if drift_detected {
            cond_status::TRUE
        } else {
            cond_status::FALSE
        },
        if drift_detected {
            reason::DRIFT_DETECTED
        } else {
            cond_reason::RECONCILED
        },
        if drift_detected {
            "most recent run reported case failures"
        } else {
            "no errors"
        },
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
        } else {
            reason::ALL_PASSED
        },
        match last_result {
            Some(r) if drift_detected => format!(
                "{} failing cases out of {} (corpus {})",
                r.failed, r.total, r.corpus_label
            ),
            Some(r) => format!("all {} cases passed", r.total),
            None => "no runs observed yet".into(),
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
    api: &Api<ClawEval>,
    name: &str,
    observed_generation: Option<i64>,
    prior_conditions: &[Condition],
    prior_status: &ClawEvalStatus,
    why_reason: &str,
    why_msg: &str,
) -> Result<Action, ReconcileError> {
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
    let new_status = ClawEvalStatus {
        phase: Some(PHASE_DEGRADED.into()),
        observed_generation,
        conditions: Some(new_conditions),
        last_run_at: prior_status.last_run_at.clone(),
        last_result: prior_status.last_result.clone(),
        history: prior_status.history.clone(),
        corpus_config_map_ref: prior_status.corpus_config_map_ref.clone(),
        corpus_digest: prior_status.corpus_digest.clone(),
        cron_job_name: prior_status.cron_job_name.clone(),
    };
    let patch = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawEval",
        "status": new_status,
    });
    api.patch_status(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    Ok(Action::requeue(REQUEUE_FAIL))
}

// ─────────────────────────────────────────────────────────────────────
// Finalizer
// ─────────────────────────────────────────────────────────────────────

async fn finalize(
    api: &Api<ClawEval>,
    configmaps: &Api<ConfigMap>,
    jobs: &Api<Job>,
    cronjobs: &Api<CronJob>,
    eval: &ClawEval,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("claweval-{name}-corpus");
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
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawEval",
        "metadata": {"finalizers": finalizers},
    });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(claweval = %name, "ClawEvalDeleted");
    Ok(Action::await_change())
}

fn error_policy(eval: Arc<ClawEval>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("ClawEval", error.class());
    tracing::warn!(
        claweval = %eval.name_any(),
        error_class = error.class(),
        error = %error,
        "ClawEval reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

pub async fn run(client: Client) -> Result<()> {
    let evals: Api<ClawEval> = Api::all(client.clone());
    match evals.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("ClawEval CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("ClawEval CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(evals, kube::runtime::watcher::Config::default())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("ClawEval", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("ClawEval reconciled {:?}", o),
                Err(e) => tracing::warn!("ClawEval reconcile failed: {e:?}"),
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
        assert!(n1.starts_with("claweval-e1-runnow-"));
        assert!(n3.ends_with("now"));
    }

    #[test]
    fn cron_job_name_stable() {
        assert_eq!(cron_job_name("my-eval"), "claweval-my-eval");
    }

    #[test]
    fn sandbox_router_url_format() {
        assert_eq!(
            sandbox_router_url("agent-001"),
            "http://agent-001.azureclaw-agent-001.svc.cluster.local:8443"
        );
    }

    #[test]
    fn claweval_owner_refs_emits_controller_blocking_reference() {
        let refs =
            claweval_owner_refs("nightly-regression", "8f2a3b4c-1111-2222-3333-444455556666");
        let arr = refs.as_array().expect("owner refs must be a JSON array");
        assert_eq!(
            arr.len(),
            1,
            "exactly one owner reference per spawned child"
        );
        let r = &arr[0];
        assert_eq!(r["apiVersion"], "azureclaw.azure.com/v1alpha1");
        assert_eq!(r["kind"], "ClawEval");
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
        let parsed = parse_report_from_log(log).expect("parses");
        assert_eq!(parsed.schema_version, "v1");
        assert_eq!(parsed.total, 3);
        assert_eq!(parsed.passed, 2);
        assert_eq!(parsed.failed, 1);
        assert_eq!(parsed.results.len(), 3);
        assert_eq!(parsed.results[0].verdict_pass, Some(true));
        assert_eq!(parsed.results[2].verdict_pass, Some(false));
    }

    #[test]
    fn parse_report_ignores_non_json_lines() {
        let log = "INFO booting\nERROR oh no\nnot json at all\n";
        assert!(parse_report_from_log(log).is_none());
    }

    #[test]
    fn parse_report_picks_latest_match() {
        let log = r#"{"schemaVersion":"v1","total":1,"passed":1,"failed":0,"results":[]}
{"schemaVersion":"v1","total":5,"passed":4,"failed":1,"results":[]}"#;
        let parsed = parse_report_from_log(log).expect("parses");
        assert_eq!(parsed.total, 5);
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
        let _ = azureclaw_eval_corpus::parse(&resolved.bytes).expect("re-parses");
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
            Some("claweval-x"),
            None,
            false,
        );
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason, "Scheduled");
        let drift = conds
            .iter()
            .find(|c| c.type_ == TYPE_CONFORMANCE_DRIFT)
            .unwrap();
        assert_eq!(drift.status, "False");
        assert_eq!(drift.reason, "AllPassed");
    }

    #[test]
    fn build_conditions_drift_branches_ready_false_and_drift_true() {
        let resolved = ResolvedCorpus {
            bytes: vec![],
            digest: "sha256:abc".into(),
            label: "builtin:jailbreak-baseline".into(),
        };
        let r = EvalResult {
            schema_version: "v1".into(),
            corpus_digest: "sha256:abc".into(),
            total: 10,
            passed: 7,
            failed: 3,
            errored: 0,
            corpus_label: "builtin:jailbreak-baseline".into(),
            job_name: "claweval-x-runnow-aa".into(),
            first_failing_cases: vec!["c-1".into()],
        };
        let conds = build_conditions(&[], Some(2), &resolved, Some("job-x"), None, Some(&r), true);
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
            schema_version: "v1".into(),
            corpus_digest: "sha256:abc".into(),
            total: 10,
            passed: 10,
            failed: 0,
            errored: 0,
            corpus_label: "builtin:jailbreak-baseline".into(),
            job_name: "claweval-x-runnow-bb".into(),
            first_failing_cases: vec![],
        };
        let conds = build_conditions(&[], Some(3), &resolved, None, Some("cj"), Some(&r), false);
        let ready = conds.iter().find(|c| c.type_ == "Ready").unwrap();
        assert_eq!(ready.status, "True");
        assert_eq!(ready.reason, "AllPassed");
    }

    #[test]
    fn pod_spec_renders_corpus_mount_and_args() {
        let spec = runner_pod_spec_json(
            "my-eval",
            "claweval-my-eval-corpus",
            "myrepo/runner:1",
            "http://agent-1.azureclaw-agent-1.svc.cluster.local:8443",
            "builtin:jailbreak-baseline",
        );
        let args = spec["containers"][0]["args"].as_array().unwrap();
        let args: Vec<&str> = args.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(args.contains(&"--corpus"));
        assert!(args.contains(&"/etc/azureclaw/eval-corpus/corpus.json"));
        assert!(args.contains(&"--router-base"));
        assert!(args.contains(&"http://agent-1.azureclaw-agent-1.svc.cluster.local:8443"));
        assert!(args.contains(&"--corpus-label"));
        assert!(args.contains(&"builtin:jailbreak-baseline"));
        let vol = &spec["volumes"][0];
        assert_eq!(vol["name"], "corpus");
        assert_eq!(vol["configMap"]["name"], "claweval-my-eval-corpus");
    }
}
