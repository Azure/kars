// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `ClawEval` reconciler — Phase 2 §8 entry 6 (S6).
//!
//! Watches `ClawEval` CRs and, for each:
//!
//! 1. Ensures finalizer (`azureclaw.azure.com/claweval-cleanup`).
//! 2. Compiles the binding via
//!    [`crate::claw_eval_compile::compile_to_binding`].
//! 3. Persists as `ConfigMap` (`claweval-{name}-binding`,
//!    key `binding.json`).
//! 4. Sets controller-owned status (phase, observedGeneration,
//!    conditions, bindingConfigMapRef, versionHash, lastReconciledAt).
//!    The runtime (S7) writes `lastRunAt`, `lastScore`, `lastPass`
//!    using a distinct field manager.
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! Same shape as S2 (ToolPolicy), S3 (A2AAgent), S4 (InferencePolicy),
//! S5 (ClawMemory).
//! - Conditions vocabulary + transition-time helpers from
//!   [`crate::status::conditions`].
//! - Compile module is single-purpose and reconciler does no JSON
//!   shaping itself.
//! - **No Foundry calls** — Foundry interaction stays in the runtime
//!   path (`cli/src/commands/eval.ts` + the router's `/openai/evals`
//!   and `/evaluators` proxies).

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, ObjectMeta, Patch, PatchParams},
    runtime::controller::{Action, Controller},
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::claw_eval::{ClawEval, ClawEvalStatus};
use crate::claw_eval_compile::{compile_to_binding, version_hash};
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason, status as cond_status};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_EVAL;
const FINALIZER: &str = "azureclaw.azure.com/claweval-cleanup";

const REQUEUE_OK: Duration = Duration::from_secs(300);
const REQUEUE_FAIL: Duration = Duration::from_secs(60);

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

async fn reconcile(eval: Arc<ClawEval>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = eval.name_any();
    let ns = eval.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(claweval = %name, ns = %ns, "Reconciling ClawEval");

    let api: Api<ClawEval> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);

    if eval.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &configmaps, &eval, &name).await;
    }

    if !eval
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({"metadata":{"finalizers":[FINALIZER]}});
        api.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_conditions = eval
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let observed_generation = eval.metadata.generation;

    let binding = compile_to_binding(&eval.spec);
    let v_hash = version_hash(&binding);

    let cm_name = format!("claweval-{name}-binding");
    let mut degraded: Option<(&'static str, String)> = None;

    match ensure_binding_configmap(&configmaps, &cm_name, &name, &binding, &v_hash).await {
        Ok(()) => {
            tracing::info!(
                claweval = %name,
                ns = %ns,
                version_hash = %v_hash,
                generation = observed_generation.unwrap_or(0),
                sandbox = %eval.spec.sandbox_ref.name,
                "ClawEvalReconciled"
            );
        }
        Err(e) => {
            tracing::warn!(
                claweval = %name,
                error_class = e.class(),
                "ClawEvalBindingWriteFailed"
            );
            degraded = Some(("BindingWriteFailed", e.to_string()));
        }
    }

    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        degraded.as_ref().map(|(r, m)| (*r, m.as_str())),
    );
    let phase = if degraded.is_some() {
        "Degraded"
    } else {
        "Ready"
    };

    let status_patch = json!({
        "status": ClawEvalStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            binding_config_map_ref: Some(LocalObjectRef { name: cm_name.clone() }),
            version_hash: Some(v_hash),
            last_reconciled_at: Some(rfc3339_now()),
            // Runtime-owned fields are intentionally None — the
            // controller's field manager does not own them and SSA
            // leaves them untouched.
            last_run_at: None,
            last_score: None,
            last_pass: None,
        }
    });
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(status_patch),
    )
    .await?;

    if degraded.is_some() {
        Ok(Action::requeue(REQUEUE_FAIL))
    } else {
        Ok(Action::requeue(REQUEUE_OK))
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn build_conditions(
    prior: &[Condition],
    observed_generation: Option<i64>,
    degraded: Option<(&str, &str)>,
) -> Vec<Condition> {
    let mut out: Vec<Condition> = Vec::with_capacity(3);
    let prior_ready = conditions::find(prior, conditions::TYPE_READY);
    let prior_progressing = conditions::find(prior, conditions::TYPE_PROGRESSING);
    let prior_degraded = conditions::find(prior, conditions::TYPE_DEGRADED);

    match degraded {
        Some((reason_value, message)) => {
            out.push(conditions::preserve_transition_time(
                prior_ready,
                conditions::TYPE_READY,
                cond_status::FALSE,
                reason_value,
                message,
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_progressing,
                conditions::TYPE_PROGRESSING,
                cond_status::FALSE,
                reason::FAILED,
                "binding write failed",
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_degraded,
                conditions::TYPE_DEGRADED,
                cond_status::TRUE,
                reason_value,
                message,
                observed_generation,
            ));
        }
        None => {
            out.push(conditions::preserve_transition_time(
                prior_ready,
                conditions::TYPE_READY,
                cond_status::TRUE,
                reason::RECONCILED,
                "ClawEval binding compiled and published",
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_progressing,
                conditions::TYPE_PROGRESSING,
                cond_status::FALSE,
                reason::RECONCILED,
                "compile complete",
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_degraded,
                conditions::TYPE_DEGRADED,
                cond_status::FALSE,
                reason::RECONCILED,
                "no errors",
                observed_generation,
            ));
        }
    }
    out
}

async fn ensure_binding_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &str,
    binding: &serde_json::Value,
    v_hash: &str,
) -> Result<(), ReconcileError> {
    let json_str = serde_json::to_string_pretty(binding)?;
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("binding.json".into(), json_str);
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "azureclaw.azure.com/claweval-version-hash".into(),
        v_hash.into(),
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
                ("azureclaw.azure.com/claweval".into(), owner.into()),
                (
                    "azureclaw.azure.com/artifact".into(),
                    "claw-eval-binding".into(),
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

async fn finalize(
    api: &Api<ClawEval>,
    configmaps: &Api<ConfigMap>,
    eval: &ClawEval,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("claweval-{name}-binding");
    let _ = configmaps
        .delete(&cm_name, &Default::default())
        .await
        .map(|_| ())
        .or_else(|e: kube::Error| -> Result<(), kube::Error> {
            if matches!(e, kube::Error::Api(ref ae) if ae.code == 404) {
                Ok(())
            } else {
                Err(e)
            }
        });

    let finalizers: Vec<String> = eval
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({"metadata":{"finalizers": finalizers}});
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_now_is_utc_z_suffixed() {
        let s = rfc3339_now();
        assert!(s.ends_with('Z'), "got {s}");
        assert_eq!(s.len(), 20);
    }

    #[test]
    fn error_class_is_closed_set() {
        let serde_err: serde_json::Error =
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let e = ReconcileError::SerdeJson(serde_err);
        assert_eq!(e.class(), "serde");

        fn assert_closed(class: &str) {
            assert!(matches!(class, "kube_api" | "serde"));
        }
        assert_closed(e.class());
    }

    #[test]
    fn build_conditions_emits_all_three_types_on_success() {
        let conds = build_conditions(&[], Some(1), None);
        assert_eq!(conds.len(), 3);
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::TRUE);
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::FALSE);
    }

    #[test]
    fn build_conditions_emits_all_three_types_on_failure() {
        let conds = build_conditions(&[], Some(1), Some(("BindingWriteFailed", "boom")));
        assert_eq!(conds.len(), 3);
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::FALSE);
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::TRUE);
        assert_eq!(degraded.reason, "BindingWriteFailed");
    }

    #[test]
    fn build_conditions_preserves_transition_time_when_status_unchanged() {
        let first = build_conditions(&[], Some(1), None);
        let second = build_conditions(&first, Some(2), None);
        let r1 = first
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        let r2 = second
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(r1.last_transition_time, r2.last_transition_time);
    }

    #[test]
    fn finalizer_constant_is_dns_subdomain() {
        assert!(FINALIZER.contains('/'));
        let (domain, key) = FINALIZER.split_once('/').unwrap();
        assert_eq!(domain, "azureclaw.azure.com");
        assert!(!key.is_empty());
    }

    #[test]
    fn field_manager_is_per_reconciler() {
        assert_eq!(FIELD_MANAGER, "azureclaw-controller/claweval");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/mcp");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/toolpolicy");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/a2aagent");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/inferencepolicy");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/clawmemory");
    }

    #[test]
    fn field_manager_distinct_from_runtime_writer() {
        // S7 will use this field manager for runtime-owned status
        // fields (lastRunAt, lastScore, lastPass, EvalsPassed
        // condition). Distinct from the controller's manager so SSA
        // partitions ownership cleanly.
        const RUNTIME_FIELD_MANAGER: &str = "azureclaw-router/claweval";
        assert_ne!(FIELD_MANAGER, RUNTIME_FIELD_MANAGER);
    }
}
