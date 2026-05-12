// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `ClawMemory` reconciler — Phase 2 §8 entry 5 (S5).
//!
//! Watches `ClawMemory` CRs and, for each:
//!
//! 1. Ensures finalizer (`azureclaw.azure.com/clawmemory-cleanup`).
//! 2. Compiles the binding via
//!    [`crate::claw_memory_compile::compile_to_binding`].
//! 3. Persists as `ConfigMap` (`clawmemory-{name}-binding`,
//!    key `binding.json`).
//! 4. Sets full status (phase, observedGeneration, conditions,
//!    bindingConfigMapRef, versionHash, lastReconciledAt).
//!
//! ## Honesty rule (status reporting)
//!
//! The controller does **not** create the upstream Azure AI Foundry
//! Memory Store — that happens runtime-side via the CLI plugin and
//! the router's `/memory_stores/*` proxy on first use (Slice 3
//! wiring). Therefore, on a successful binding compile, the
//! controller reports:
//!
//! - `phase = "Compiled"` (not `"Ready"`)
//! - `Ready = False` / reason `AwaitingFoundryProvisioning`
//! - `Progressing = True` / reason `AwaitingFoundryProvisioning`
//! - A `Warning` Event with reason `PolicyNotEnforced` pointing at
//!   the Slice 3 tracking issue (see
//!   `crate::status::phase::PhaseEventReporter::warn_policy_not_enforced`).
//!
//! Reporting `Ready=True` after only writing the binding ConfigMap is
//! a lie: the router will still 404 on `/memory_stores/<name>` until
//! the runtime path provisions the store. `kubectl wait
//! --for=condition=Ready` must block here — flipping it `True`
//! prematurely was the bug this module guards against. The new
//! `Compiled` phase (introduced by Slice 0 of
//! `crd-well-oiled-machine`) is the canonical signal for "spec parsed
//! but data plane not yet enforcing" and replaces the previous
//! `Pending`-forever apology.
//!
//! When binding write itself fails, phase is `Failed` (not
//! `Degraded`), `Ready=False`, `Degraded=True`.
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! Same shape as S2 (ToolPolicy), S3 (A2AAgent), S4 (InferencePolicy).
//! - Conditions vocabulary + transition-time helpers from
//!   [`crate::status::conditions`].
//! - Compile module is single-purpose and reconciler does no JSON
//!   shaping itself.
//! - **No Foundry calls** — Foundry interaction stays in the runtime
//!   path (`cli/src/plugin.ts::ensureMemoryStore` + the router's
//!   `/memory_stores/*` proxy). The S7 informer wires the consumer.

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

use crate::claw_memory::{ClawMemory, ClawMemoryStatus};
use crate::claw_memory_compile::{compile_to_binding, version_hash};
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_COMPILED, PHASE_FAILED, PhaseEventReporter};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_MEMORY;
const FINALIZER: &str = "azureclaw.azure.com/clawmemory-cleanup";

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
    phase_reporter: PhaseEventReporter,
}

async fn reconcile(memory: Arc<ClawMemory>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = memory.name_any();
    let ns = memory.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(clawmemory = %name, ns = %ns, "Reconciling ClawMemory");

    let api: Api<ClawMemory> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);

    if memory.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &configmaps, &memory, &name).await;
    }

    if !memory
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({"apiVersion":"azureclaw.azure.com/v1alpha1","kind":"ClawMemory","metadata":{"finalizers":[FINALIZER]}});
        api.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_conditions = memory
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let observed_generation = memory.metadata.generation;

    let binding = compile_to_binding(&memory.spec);
    let v_hash = version_hash(&binding);

    let cm_name = format!("clawmemory-{name}-binding");
    let mut degraded: Option<(&'static str, String)> = None;

    match ensure_binding_configmap(&configmaps, &cm_name, &name, &binding, &v_hash).await {
        Ok(()) => {
            tracing::info!(
                clawmemory = %name,
                ns = %ns,
                version_hash = %v_hash,
                generation = observed_generation.unwrap_or(0),
                store_name = %memory.spec.store_name,
                scope = %memory.spec.scope,
                "ClawMemoryReconciled"
            );
        }
        Err(e) => {
            tracing::warn!(
                clawmemory = %name,
                error_class = e.class(),
                "ClawMemoryBindingWriteFailed"
            );
            degraded = Some(("BindingWriteFailed", e.to_string()));
        }
    }

    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        degraded.as_ref().map(|(r, m)| (*r, m.as_str())),
    );
    // Honesty rule (principles.md §3, slice-0-honesty-events):
    // The controller only compiles a binding ConfigMap. Creating the
    // upstream Azure AI Foundry Memory Store is the runtime path's job
    // (CLI plugin → router `/memory_stores/*` proxy), and is not wired
    // in this slice. Until Slice 3 lands a sandbox-side informer that
    // confirms the upstream store exists and stamps `foundryStoreId`,
    // we MUST NOT report `phase=Ready` / `Ready=True` — doing so makes
    // operators believe the store is provisioned when the router will
    // still 404 on `/memory_stores/<name>`. Report `Compiled` instead
    // and publish a `PolicyNotEnforced` Warning Event so the user sees
    // *why* in `kubectl describe`. `kubectl wait
    // --for=condition=Ready` continues to block.
    let phase = if degraded.is_some() {
        PHASE_FAILED
    } else {
        PHASE_COMPILED
    };

    if degraded.is_none() {
        // Slice 0: surface the gap loudly. Slice 3 will delete this
        // call site when ClawMemory becomes router-echoed.
        if let Err(e) = ctx
            .phase_reporter
            .warn_policy_not_enforced(
                memory.as_ref(),
                "CompileBinding",
                "ClawMemory binding ConfigMap is compiled but the upstream Azure AI Foundry \
                 Memory Store has not been provisioned yet. The router will 404 on \
                 /memory_stores/<name> until the runtime path (Slice 3) creates the store. \
                 Tracking: crd-well-oiled-machine slice-3-claw-memory.",
            )
            .await
        {
            tracing::warn!(
                clawmemory = %name,
                error = %e,
                "ClawMemoryEventPublishFailed",
            );
        }
    }

    // SSA requires apiVersion + kind in the patch body — without
    // them, the API server returns "invalid object type: /, Kind=".
    let status_patch = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawMemory",
        "status": ClawMemoryStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            binding_config_map_ref: Some(LocalObjectRef { name: cm_name.clone() }),
            version_hash: Some(v_hash),
            last_reconciled_at: Some(rfc3339_now()),
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
            // Binding compiled OK, but the upstream Foundry Memory
            // Store is provisioned by the runtime path (S7+), not by
            // the controller. Report this honestly: `Ready=False` with
            // `Progressing=True` until upstream confirmation lands.
            // Operators / `kubectl wait --for=condition=Ready` should
            // block here, not race ahead to talk to a store the router
            // will 404 on.
            const AWAITING_MSG: &str = "binding ConfigMap compiled; awaiting upstream Foundry memory store provisioning by runtime path (router proxy on first use)";
            out.push(conditions::preserve_transition_time(
                prior_ready,
                conditions::TYPE_READY,
                cond_status::FALSE,
                reason::AWAITING_FOUNDRY_PROVISIONING,
                AWAITING_MSG,
                observed_generation,
            ));
            out.push(conditions::preserve_transition_time(
                prior_progressing,
                conditions::TYPE_PROGRESSING,
                cond_status::TRUE,
                reason::AWAITING_FOUNDRY_PROVISIONING,
                AWAITING_MSG,
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
        "azureclaw.azure.com/clawmemory-version-hash".into(),
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
                ("azureclaw.azure.com/clawmemory".into(), owner.into()),
                (
                    "azureclaw.azure.com/artifact".into(),
                    "claw-memory-binding".into(),
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
    api: &Api<ClawMemory>,
    configmaps: &Api<ConfigMap>,
    memory: &ClawMemory,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("clawmemory-{name}-binding");
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

    let finalizers: Vec<String> = memory
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({"apiVersion":"azureclaw.azure.com/v1alpha1","kind":"ClawMemory","metadata":{"finalizers": finalizers}});
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(clawmemory = %name, "ClawMemoryDeleted");
    Ok(Action::await_change())
}

fn error_policy(memory: Arc<ClawMemory>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("ClawMemory", error.class());
    tracing::warn!(
        clawmemory = %memory.name_any(),
        error_class = error.class(),
        error = %error,
        "ClawMemory reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

pub async fn run(client: Client) -> Result<()> {
    let memories: Api<ClawMemory> = Api::all(client.clone());
    match memories.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("ClawMemory CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("ClawMemory CRD not installed — reconciler disabled: {e}");
            // Park forever so the tokio::select! in main() does not see
            // this reconciler exit cleanly and tear the whole controller
            // down. The CRD is only optional from the controller's
            // perspective; its absence is operator config, not a fatal
            // condition.
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        phase_reporter: PhaseEventReporter::new(client, "ClawMemory"),
    });
    Controller::new(memories, kube::runtime::watcher::Config::default())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("ClawMemory", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("ClawMemory reconciled {:?}", o),
                Err(e) => tracing::warn!("ClawMemory reconcile failed: {e:?}"),
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
        // "Success" here = binding ConfigMap published. Upstream Foundry
        // store provisioning is runtime-path; controller intentionally
        // reports Ready=False / Progressing=True until S7 confirmation.
        let conds = build_conditions(&[], Some(1), None);
        assert_eq!(conds.len(), 3);
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::FALSE);
        assert_eq!(ready.reason, reason::AWAITING_FOUNDRY_PROVISIONING);
        let progressing = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_PROGRESSING)
            .unwrap();
        assert_eq!(progressing.status, cond_status::TRUE);
        assert_eq!(progressing.reason, reason::AWAITING_FOUNDRY_PROVISIONING);
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
        assert_eq!(FIELD_MANAGER, "azureclaw-controller/clawmemory");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/mcp");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/toolpolicy");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/a2aagent");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/inferencepolicy");
    }
}
