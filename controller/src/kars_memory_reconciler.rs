// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `KarsMemory` reconciler — Phase 2 §8 entry 5 (S5), upgraded in
//! Slice 3a of `crd-well-oiled-machine` to close the principles.md §3
//! "Ready ⇔ router echo" loop.
//!
//! Watches `KarsMemory` CRs and, for each:
//!
//! 1. Ensures finalizer (`kars.azure.com/karsmemory-cleanup`).
//! 2. Compiles the binding via
//!    [`crate::kars_memory_compile::compile_to_binding`].
//! 3. Persists as `ConfigMap` (`karsmemory-{name}-binding`,
//!    key `binding.json`), with the canonical compile digest stamped
//!    in an annotation for diff debugging.
//! 4. Polls every referencing `KarsSandbox`'s inference router via
//!    `GET /internal/policy-status` for `PolicyKind::Memory` and only
//!    promotes `phase=Compiled → Ready` when every router echoes the
//!    same digest.
//! 5. Sets full status (phase, observedGeneration, conditions,
//!    bindingConfigMapRef, versionHash, lastReconciledAt,
//!    compiledDigest, loadedDigest).
//!
//! ## Honesty rule (Slice 3a)
//!
//! Up to Slice 0 the reconciler stamped `Compiled` unconditionally
//! and emitted a `PolicyNotEnforced` Warning Event because the
//! data-plane consumer did not exist yet. Slice 3a wires the
//! router-side `memory_binding_loader` consumer; the controller
//! therefore now distinguishes three success states:
//!
//! * `Compiled` + `Ready=False / AwaitingRouterEnforcement` — binding
//!   published, no router has echoed the matching digest yet
//!   (also the `NoSandboxesReferencing` branch).
//! * `Compiled` + `Ready=False / NoSandboxesReferencing` — binding
//!   published but no `KarsSandbox` currently references this
//!   `KarsMemory` via `spec.memoryRef`.
//! * `Ready` + `Ready=True / RouterEnforcing` — every referencing
//!   sandbox's router has confirmed the digest.
//!
//! When binding write fails, `phase=Failed`, `Ready=False`,
//! `Degraded=True`.
//!
//! ## What landed in Slice 3b / 3c (no longer deferred)
//!
//! - **Slice 3b.4** — `AuthMisconfigured` Degraded reason: the router echoes
//!   403 on the memory tool call back through `status.observedRouters[]`,
//!   the reconciler elevates phase to `Degraded` with reason
//!   `AuthMisconfigured` and an actionable message pointing at the project
//!   MI / RBAC fix.
//! - **Slice 3b.5** — `MemoryStoreMissing` Degraded reason: same echo path,
//!   distinct reason so operators don't confuse "wrong RBAC" with "store
//!   never created".
//! - **Slice 3c.1** — router-side auto-provisioning. On first 404 from
//!   `/memory_stores/{id}`, the inference router POSTs to `/memory_stores`
//!   to create the store, then retries. See
//!   [`inference-router/src/mcp/platform.rs`] `ensure_memory_store`. The
//!   *controller* does NOT provision the upstream Foundry resource — that
//!   remains an operator/`azure-prepare` job — but the store *inside* a
//!   provisioned AI Services account is now self-healing.
//!
//! ## What is *still* operator-driven
//!
//! - Upstream Foundry **AI Services account** + **Project** lifecycle
//!   (not in scope for the operator; `azure-prepare` handles it).
//! - RBAC pre-grants: project MI needs `Azure AI User` on the resource
//!   group for the internal model calls that Memory Store makes — see
//!   `docs/operations/secret-rotation.md`.
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! Mirror of the Slice 2a (`InferencePolicy`) reconciler shape:
//! - Conditions vocabulary + transition-time helpers from
//!   [`crate::status::conditions`].
//! - `RouterEnforcementState` + `decide_enforcement_state` from
//!   [`crate::status::router_confirmation`].
//! - `list_sandboxes_matching` + `poll_referencing_sandboxes` from
//!   [`crate::status::router_confirmation_io`].
//! - Compile module is single-purpose and reconciler does no JSON
//!   shaping itself.

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

use crate::kars_memory::{KarsMemory, KarsMemoryStatus};
use crate::kars_memory_compile::{
    MEMORY_BINDING_FILENAME, compile_to_binding, kars_memory_digest, version_hash,
};
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_COMPILED, PHASE_FAILED, PHASE_READY, PhaseEventReporter};
use crate::status::router_confirmation::{self, RouterEnforcementState, decide_enforcement_state};
use crate::status::router_confirmation_io::{list_sandboxes_matching, poll_referencing_sandboxes};

const FIELD_MANAGER: &str = crate::field_managers::CLAW_MEMORY;
const FINALIZER: &str = "kars.azure.com/karsmemory-cleanup";

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
    http: reqwest::Client,
    phase_reporter: PhaseEventReporter,
}

async fn reconcile(memory: Arc<KarsMemory>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = memory.name_any();
    let ns = memory.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(karsmemory = %name, ns = %ns, "Reconciling KarsMemory");

    let api: Api<KarsMemory> = Api::namespaced(ctx.client.clone(), &ns);
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
        let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsMemory","metadata":{"finalizers":[FINALIZER]}});
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

    // Slice 1c.4 — resolve bundleRef (if set) into an effective spec
    // before compile. The bundle carries content (storeName, scope,
    // retentionDays, deleteOnSandboxDelete, displayName); `sandboxRef`
    // always stays in the CR. When `bundleRef` is unset the path is
    // a no-op and returns the CR's spec verbatim.
    let (effective_spec, bundle_ref_digest, bundle_degraded) =
        resolve_memory_source(memory.as_ref()).await;
    let mut degraded: Option<(&'static str, String)> = bundle_degraded;

    let binding = compile_to_binding(&effective_spec);
    let v_hash = version_hash(&binding);
    // Slice 3a — canonical bytes the router-side `memory_binding_loader`
    // will sha256 + echo via `GET /internal/policy-status`. The bytes
    // written into the ConfigMap MUST equal what we hash here, byte
    // for byte; any reformatting (pretty-print, trailing newline, key
    // reordering) silently breaks the §3 echo contract.
    let canonical_bytes = serde_json::to_vec(&binding)?;
    let canonical_str =
        String::from_utf8(canonical_bytes.clone()).expect("serde_json::to_vec emits valid UTF-8");
    let compiled_digest = kars_memory_digest(&canonical_bytes);

    let cm_name = format!("karsmemory-{name}-binding");

    // If bundleRef resolution already marked us Degraded
    // (InvalidSpec mutex / fetch / verify failure), skip the
    // ConfigMap write — `effective_spec` is the selector-only
    // sentinel and compiling it would publish empty bytes that
    // every router would reject anyway. Mirrors the 1c.3
    // InferencePolicy path.
    if degraded.is_none() {
        match ensure_binding_configmap(
            &configmaps,
            &cm_name,
            &name,
            &canonical_str,
            &v_hash,
            &compiled_digest,
        )
        .await
        {
            Ok(()) => {
                tracing::info!(
                    karsmemory = %name,
                    ns = %ns,
                    version_hash = %v_hash,
                    compiled_digest = %compiled_digest,
                    generation = observed_generation.unwrap_or(0),
                    store_name = effective_spec.store_name.as_deref().unwrap_or(""),
                    scope = effective_spec.scope.as_deref().unwrap_or(""),
                    bundle_ref_digest = bundle_ref_digest.as_deref().unwrap_or(""),
                    "KarsMemoryCompiled"
                );
            }
            Err(e) => {
                tracing::warn!(
                    karsmemory = %name,
                    error_class = e.class(),
                    "KarsMemoryBindingWriteFailed"
                );
                degraded = Some(("BindingWriteFailed", e.to_string()));
            }
        }
    }

    // Slice 3a — close the §3 "Ready ⇔ router echo" loop. List
    // KarsSandboxes that reference this KarsMemory by
    // `spec.memoryRef.name`, GET `/internal/policy-status` on each
    // router, and let the result drive both `phase` and the `Ready`
    // condition. The `compiled_digest` we just wrote is the value
    // every router must echo before we promote to Ready.
    //
    // Slice 3b.4 — auth probe surfacing. The pre-aggregation
    // `results` carry per-sandbox `last_error` strings. We scan them
    // for the router-side `AuthMisconfigured:` prefix (wire contract
    // pinned in `crate::status::conditions::AUTH_MISCONFIGURED_PREFIX`)
    // *before* the aggregator collapses them into the Awaiting
    // message. When any sandbox surfaces an auth failure, we elevate
    // `degraded` directly so the resulting `Degraded=True` /
    // `reason=AuthMisconfigured` condition jumps to the top instead
    // of getting buried inside a generic Awaiting message — RBAC
    // misconfigs are not transient.
    let enforcement_state = if degraded.is_some() {
        RouterEnforcementState::NotApplicable
    } else {
        let referrers = match list_sandboxes_matching(&ctx.client, &ns, |cs| {
            cs.spec.memory_ref.as_ref().is_some_and(|r| r.name == name)
        })
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    karsmemory = %name,
                    ns = %ns,
                    error = %e,
                    "KarsSandbox list failed; treating as no referrers"
                );
                Vec::new()
            }
        };
        let results = poll_referencing_sandboxes(&ctx.client, &ctx.http, &referrers).await;
        if let Some(msg) = first_auth_misconfigured_message(&results) {
            degraded = Some((conditions::reason::AUTH_MISCONFIGURED, msg));
            RouterEnforcementState::NotApplicable
        } else if let Some(msg) = first_memory_store_missing_message(&results) {
            // Slice 3b.5 — `MemoryStoreMissing:` is the second-tier
            // Degraded signal. `AuthMisconfigured:` is checked first
            // (RBAC dominates: if we can't read the store, we can't
            // even tell if it's missing). A 404 means the bound
            // store does not exist on the upstream — operator-
            // visible until Slice 3c's router-side auto-provision
            // ships.
            degraded = Some((conditions::reason::MEMORY_STORE_MISSING, msg));
            RouterEnforcementState::NotApplicable
        } else {
            decide_enforcement_state(&compiled_digest, "Memory", &results)
        }
    };

    let loaded_digest: Option<String> = match &enforcement_state {
        RouterEnforcementState::Confirmed { .. } => Some(compiled_digest.clone()),
        _ => None,
    };

    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        degraded.as_ref().map(|(r, m)| (*r, m.as_str())),
        &enforcement_state,
    );

    let phase = if degraded.is_some() {
        PHASE_FAILED
    } else {
        match enforcement_state {
            RouterEnforcementState::Confirmed { .. } | RouterEnforcementState::NotApplicable => {
                PHASE_READY
            }
            RouterEnforcementState::NoSandboxesReferencing
            | RouterEnforcementState::Awaiting { .. } => PHASE_COMPILED,
        }
    };

    // Only emit the Warning while truly awaiting router-side
    // confirmation (mirrors slice-1c ToolPolicy + slice-2a
    // InferencePolicy). Once Confirmed fires we stop shouting — the
    // loop is honestly closed.
    let publish_warning = degraded.is_none()
        && matches!(
            enforcement_state,
            RouterEnforcementState::Awaiting { .. }
                | RouterEnforcementState::NoSandboxesReferencing
        );
    if publish_warning
        && let Err(e) = ctx
            .phase_reporter
            .warn_policy_not_enforced(
                memory.as_ref(),
                "AwaitingRouterConfirmation",
                "KarsMemory binding compiled and published, but the inference-router has \
                 not yet echoed the matching digest on /internal/policy-status. \
                 Memory MCP tools today still authenticate via the chart-fed \
                 FOUNDRY_MEMORY_STORE_ID env; Slice 3b will rewire them through the binding.",
            )
            .await
    {
        tracing::warn!(
            karsmemory = %name,
            error = %e,
            "KarsMemoryEventPublishFailed",
        );
    }

    // SSA requires apiVersion + kind in the patch body — without
    // them, the API server returns "invalid object type: /, Kind=".
    let status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsMemory",
        "status": KarsMemoryStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            binding_config_map_ref: Some(LocalObjectRef { name: cm_name.clone() }),
            version_hash: Some(v_hash),
            last_reconciled_at: Some(rfc3339_now()),
            compiled_digest: Some(compiled_digest),
            loaded_digest,
            bundle_ref_digest,
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
    } else if matches!(enforcement_state, RouterEnforcementState::Awaiting { .. }) {
        // Short requeue while awaiting echo — mirrors slice-1c / 2a.
        Ok(Action::requeue(Duration::from_secs(15)))
    } else {
        Ok(Action::requeue(REQUEUE_OK))
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Slice 1c.4 — resolve `spec.bundleRef` (if set) into an effective
/// [`crate::kars_memory::KarsMemorySpec`] for [`compile_to_binding`].
///
/// Returns a 3-tuple `(effective_spec, bundle_ref_digest, degraded)`:
/// - `effective_spec` is what compile_to_binding will see. When
///   `bundleRef` is unset, this is the CR's spec verbatim (the
///   inline path). When `bundleRef` is set and the fetch + cosign
///   verification succeed, the bundle's content fields are merged
///   onto the CR's `sandboxRef`. On any error this is a
///   selector-only sentinel that won't be compiled.
/// - `bundle_ref_digest` is the verified bundle's
///   `sha256:...` (stamped into `status.bundleRefDigest`) when the
///   bundle path succeeded; `None` otherwise.
/// - `degraded`, when `Some`, carries the `(reason, message)` for
///   the `Degraded=True` condition the reconciler will surface.
///
/// The bundle path goes through
/// [`crate::policy_fetcher::fetch_and_verify_generic`] with
/// [`crate::policy_canonical::memory::MemoryKind`], so this function
/// inherits the same cosign trust-root, ACR auth, and `FetchError`
/// taxonomy as the egress, tools, and inference paths.
async fn resolve_memory_source(
    memory: &KarsMemory,
) -> (
    crate::kars_memory::KarsMemorySpec,
    Option<String>,
    Option<(&'static str, String)>,
) {
    let spec = &memory.spec;
    let inline_any = spec.store_name.is_some()
        || spec.scope.is_some()
        || spec.retention_days.is_some()
        || spec.delete_on_sandbox_delete.is_some()
        || spec.display_name.is_some();
    let bundle_set = spec.bundle_ref.is_some();

    if inline_any && bundle_set {
        return (
            crate::kars_memory::KarsMemorySpec {
                sandbox_ref: spec.sandbox_ref.clone(),
                ..Default::default()
            },
            None,
            Some((
                "InvalidSpec",
                "spec.bundleRef is mutually exclusive with spec.storeName, \
                 spec.scope, spec.retentionDays, spec.deleteOnSandboxDelete, \
                 and spec.displayName"
                    .into(),
            )),
        );
    }

    if !bundle_set {
        return (spec.clone(), None, None);
    }

    let bundle_ref = spec
        .bundle_ref
        .as_ref()
        .expect("bundle_set implies Some")
        .clone();

    let signer_policy_handle = crate::signer_policy::global();
    let verify_result = match signer_policy_handle.snapshot() {
        crate::signer_policy::SignerPolicyState::FromConfigMap(p) => {
            let cfg: crate::policy_fetcher::SignerPolicyConfig = p.into();
            crate::policy_fetcher::fetch_and_verify_generic::<
                crate::policy_canonical::memory::MemoryKind,
            >(&bundle_ref, &cfg)
            .await
        }
        crate::signer_policy::SignerPolicyState::Malformed(msg) => Err(
            crate::policy_fetcher::FetchError::SignerPolicyMalformed(msg),
        ),
        crate::signer_policy::SignerPolicyState::Absent => {
            let cfg = crate::policy_fetcher::SignerPolicyConfig::from_env();
            crate::policy_fetcher::fetch_and_verify_generic::<
                crate::policy_canonical::memory::MemoryKind,
            >(&bundle_ref, &cfg)
            .await
        }
    };

    match verify_result {
        Ok(verified) => {
            let effective = merge_bundle_with_selector(&spec.sandbox_ref, &verified);
            (effective, Some(verified.digest), None)
        }
        Err(e) => {
            let (reason, msg) = fetch_error_to_degraded(&e);
            tracing::warn!(
                karsmemory = %memory.name_any(),
                registry = %bundle_ref.registry,
                repository = %bundle_ref.repository,
                digest = %bundle_ref.digest,
                reason,
                "KarsMemory bundleRef fetch/verify failed: {msg}"
            );
            (
                crate::kars_memory::KarsMemorySpec {
                    sandbox_ref: spec.sandbox_ref.clone(),
                    ..Default::default()
                },
                None,
                Some((reason, msg)),
            )
        }
    }
}

/// Merge the verified-bundle content fields onto the CR's
/// `sandboxRef` to produce the effective spec for
/// [`compile_to_binding`]. The bundle owns content; the CR owns the
/// sandbox the binding applies to.
fn merge_bundle_with_selector(
    sandbox_ref: &crate::kars_memory::SandboxRef,
    verified: &crate::policy_canonical::memory::VerifiedMemoryBinding,
) -> crate::kars_memory::KarsMemorySpec {
    crate::kars_memory::KarsMemorySpec {
        store_name: verified.store_name.clone(),
        sandbox_ref: sandbox_ref.clone(),
        scope: verified.scope.clone(),
        retention_days: verified.retention_days,
        delete_on_sandbox_delete: verified.delete_on_sandbox_delete,
        display_name: verified.display_name.clone(),
        bundle_ref: None,
    }
}

/// Map [`crate::policy_fetcher::FetchError`] to the `(reason,
/// message)` pair surfaced as a `Degraded=True` condition. Shares
/// the one vocabulary [`crate::policy_fetcher::reason_for_error`]
/// owns across all signed-policy kinds.
fn fetch_error_to_degraded(e: &crate::policy_fetcher::FetchError) -> (&'static str, String) {
    let reason = crate::policy_fetcher::reason_for_error(e).unwrap_or("Transient");
    (reason, e.to_string())
}

/// Slice 3b.4 — scan the per-sandbox poll outcomes for a router-side
/// `AuthMisconfigured:` `last_error` on the `Memory` policy kind. If
/// any referencing sandbox surfaces one, return a deterministic
/// message attributing the failure to that sandbox so the controller
/// can elevate it to a `Degraded=True / reason=AuthMisconfigured`
/// condition instead of folding it into a generic Awaiting message.
///
/// Returns the first match (sandbox iteration order is preserved by
/// the upstream pollers, so this is stable across reconciles).
/// Multiple-sandbox cases are flagged in the message tail.
fn first_auth_misconfigured_message(
    results: &[(
        String,
        Result<
            crate::status::router_confirmation::PolicyStatusResponse,
            crate::status::router_confirmation::ConfirmError,
        >,
    )],
) -> Option<String> {
    first_prefixed_memory_error(results, conditions::AUTH_MISCONFIGURED_PREFIX)
}

/// Slice 3b.5 — scan the per-sandbox poll outcomes for a router-side
/// `MemoryStoreMissing:` `last_error` on the `Memory` policy kind.
/// Mirrors [`first_auth_misconfigured_message`] but matches the
/// missing-store prefix.
///
/// Lower precedence than auth misconfigs: callers must check
/// AuthMisconfigured first. 404 only tells you the store does not
/// exist *given* the operator has access to ask; if RBAC is broken
/// (403) the controller cannot trust a 404 result at all.
fn first_memory_store_missing_message(
    results: &[(
        String,
        Result<
            crate::status::router_confirmation::PolicyStatusResponse,
            crate::status::router_confirmation::ConfirmError,
        >,
    )],
) -> Option<String> {
    first_prefixed_memory_error(results, conditions::MEMORY_STORE_MISSING_PREFIX)
}

/// Shared scan-for-prefixed-error helper used by Slice 3b.4
/// (`AuthMisconfigured:`) and Slice 3b.5 (`MemoryStoreMissing:`).
/// Walks per-sandbox `PolicyStatusResponse` results, looks for
/// `find_last_error("Memory")` starting with `prefix`, and returns a
/// deterministic `"<sandbox>: <error>"` message with a tail when
/// multiple sandboxes are affected.
fn first_prefixed_memory_error(
    results: &[(
        String,
        Result<
            crate::status::router_confirmation::PolicyStatusResponse,
            crate::status::router_confirmation::ConfirmError,
        >,
    )],
    prefix: &str,
) -> Option<String> {
    let mut hits: Vec<(String, String)> = Vec::new();
    for (sandbox, outcome) in results {
        if let Ok(resp) = outcome
            && let Some(err) = resp.find_last_error("Memory")
            && err.starts_with(prefix)
        {
            hits.push((sandbox.clone(), err.to_string()));
        }
    }
    if hits.is_empty() {
        return None;
    }
    let (first_sb, first_err) = &hits[0];
    let mut msg = format!("{first_sb}: {first_err}");
    if hits.len() > 1 {
        msg.push_str(&format!(" (+{} more sandbox(es) affected)", hits.len() - 1));
    }
    Some(msg)
}

fn build_conditions(
    prior: &[Condition],
    observed_generation: Option<i64>,
    degraded: Option<(&str, &str)>,
    enforcement: &RouterEnforcementState,
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
        None => match enforcement {
            RouterEnforcementState::NotApplicable => {
                // Not reached today (the success branch maps
                // degraded=None → one of the three states below).
                // Kept for build_conditions truth-table symmetry
                // with the other reconcilers.
                out.push(conditions::preserve_transition_time(
                    prior_ready,
                    conditions::TYPE_READY,
                    cond_status::TRUE,
                    reason::RECONCILED,
                    "KarsMemory binding compiled and published",
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
            RouterEnforcementState::Confirmed { total } => {
                out.push(conditions::preserve_transition_time(
                    prior_ready,
                    conditions::TYPE_READY,
                    cond_status::TRUE,
                    reason::ROUTER_ENFORCING,
                    &format!(
                        "all {total} referencing sandbox router(s) confirmed \
                         claw-memory binding digest"
                    ),
                    observed_generation,
                ));
                out.push(conditions::preserve_transition_time(
                    prior_progressing,
                    conditions::TYPE_PROGRESSING,
                    cond_status::FALSE,
                    reason::RECONCILED,
                    "router echo confirmed",
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
            RouterEnforcementState::NoSandboxesReferencing => {
                out.push(conditions::preserve_transition_time(
                    prior_ready,
                    conditions::TYPE_READY,
                    cond_status::FALSE,
                    reason::NO_SANDBOXES_REFERENCING,
                    "no KarsSandbox references this KarsMemory; nothing to enforce",
                    observed_generation,
                ));
                out.push(conditions::preserve_transition_time(
                    prior_progressing,
                    conditions::TYPE_PROGRESSING,
                    cond_status::TRUE,
                    reason::NO_SANDBOXES_REFERENCING,
                    "waiting for a KarsSandbox to reference this binding",
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
            RouterEnforcementState::Awaiting { message, .. } => {
                out.push(conditions::preserve_transition_time(
                    prior_ready,
                    conditions::TYPE_READY,
                    cond_status::FALSE,
                    reason::AWAITING_ROUTER_ENFORCEMENT,
                    message,
                    observed_generation,
                ));
                out.push(conditions::preserve_transition_time(
                    prior_progressing,
                    conditions::TYPE_PROGRESSING,
                    cond_status::TRUE,
                    reason::AWAITING_ROUTER_ENFORCEMENT,
                    "awaiting router-side enforcement",
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
        },
    }
    out
}

async fn ensure_binding_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &str,
    canonical_body: &str,
    v_hash: &str,
    compiled_digest: &str,
) -> Result<(), ReconcileError> {
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    // Canonical key: the router-side `memory_binding_loader` reads
    // this filename from the mount directory and sha256s the exact
    // bytes, so it MUST match
    // `kars_memory_compile::MEMORY_BINDING_FILENAME`.
    data.insert(MEMORY_BINDING_FILENAME.into(), canonical_body.into());
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "kars.azure.com/karsmemory-version-hash".into(),
        v_hash.into(),
    );
    annotations.insert(
        "kars.azure.com/claw-memory-digest".into(),
        compiled_digest.into(),
    );
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.into()),
            annotations: Some(annotations),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                ("kars.azure.com/karsmemory".into(), owner.into()),
                (
                    "kars.azure.com/artifact".into(),
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
    api: &Api<KarsMemory>,
    configmaps: &Api<ConfigMap>,
    memory: &KarsMemory,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("karsmemory-{name}-binding");
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
    let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsMemory","metadata":{"finalizers": finalizers}});
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(karsmemory = %name, "KarsMemoryDeleted");
    Ok(Action::await_change())
}

fn error_policy(memory: Arc<KarsMemory>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("KarsMemory", error.class());
    tracing::warn!(
        karsmemory = %memory.name_any(),
        error_class = error.class(),
        error = %error,
        "KarsMemory reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

pub async fn run(client: Client) -> Result<()> {
    let memories: Api<KarsMemory> = Api::all(client.clone());
    match memories.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("KarsMemory CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("KarsMemory CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let http = reqwest::Client::builder()
        .timeout(router_confirmation::DEFAULT_TIMEOUT)
        .build()
        .expect("default reqwest client builds with infallible config");
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        http,
        phase_reporter: PhaseEventReporter::new(client, "KarsMemory"),
    });
    Controller::new(memories, kube::runtime::watcher::Config::default())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("KarsMemory", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("KarsMemory reconciled {:?}", o),
                Err(e) => tracing::warn!("KarsMemory reconcile failed: {e:?}"),
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
    fn build_conditions_confirmed_promotes_ready_true() {
        // Slice 3a: every referencing router echoed the matching
        // digest → Ready=True / reason=RouterEnforcing. This is the
        // §3 honest-Ready transition that replaces the old
        // unconditional AwaitingFoundryProvisioning stamp.
        let conds = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::Confirmed { total: 2 },
        );
        assert_eq!(conds.len(), 3);
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::TRUE);
        assert_eq!(ready.reason, reason::ROUTER_ENFORCING);
        let progressing = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_PROGRESSING)
            .unwrap();
        assert_eq!(progressing.status, cond_status::FALSE);
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::FALSE);
    }

    #[test]
    fn build_conditions_awaiting_keeps_ready_false() {
        let conds = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::Awaiting {
                total: 1,
                matched: 0,
                message: "1/1 awaiting".into(),
            },
        );
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::FALSE);
        assert_eq!(ready.reason, reason::AWAITING_ROUTER_ENFORCEMENT);
    }

    #[test]
    fn build_conditions_no_sandboxes_referencing_keeps_ready_false() {
        let conds = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::NoSandboxesReferencing,
        );
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::FALSE);
        assert_eq!(ready.reason, reason::NO_SANDBOXES_REFERENCING);
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::FALSE);
    }

    #[test]
    fn build_conditions_failure_branch_sets_degraded_true() {
        let conds = build_conditions(
            &[],
            Some(1),
            Some(("BindingWriteFailed", "boom")),
            &RouterEnforcementState::NotApplicable,
        );
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
        let first = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::Confirmed { total: 1 },
        );
        let second = build_conditions(
            &first,
            Some(2),
            None,
            &RouterEnforcementState::Confirmed { total: 1 },
        );
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
        assert_eq!(domain, "kars.azure.com");
        assert!(!key.is_empty());
    }

    fn ok_resp(kind: &str, last_error: Option<&str>) -> router_confirmation::PolicyStatusResponse {
        router_confirmation::PolicyStatusResponse {
            schema_version: router_confirmation::SUPPORTED_SCHEMA_VERSION,
            count: 1,
            entries: vec![router_confirmation::PolicyStatusEntry {
                kind: kind.to_string(),
                digest: Some("sha256:dead".to_string()),
                source_path: "/etc/kars/memory/binding.json".to_string(),
                loaded_at: "1970-01-01T00:00:00Z".to_string(),
                last_error: last_error.map(str::to_string),
            }],
        }
    }

    #[test]
    fn first_auth_misconfigured_message_none_when_no_errors() {
        let results = vec![
            ("sb-a".to_string(), Ok(ok_resp("Memory", None))),
            ("sb-b".to_string(), Ok(ok_resp("Memory", None))),
        ];
        assert!(first_auth_misconfigured_message(&results).is_none());
    }

    #[test]
    fn first_auth_misconfigured_message_ignores_unrelated_errors() {
        // A non-prefixed `last_error` on Memory must not trip the
        // AuthMisconfigured detector — generic transient errors stay
        // in the Awaiting bucket.
        let results = vec![(
            "sb-a".to_string(),
            Ok(ok_resp("Memory", Some("upstream 500"))),
        )];
        assert!(first_auth_misconfigured_message(&results).is_none());
    }

    #[test]
    fn first_auth_misconfigured_message_ignores_other_kinds() {
        // Router echoed AuthMisconfigured on a different policy kind
        // — the KarsMemory reconciler must scope its scan to
        // `kind == "Memory"`.
        let results = vec![(
            "sb-a".to_string(),
            Ok(ok_resp(
                "InferencePolicy",
                Some("AuthMisconfigured: 403 from Foundry"),
            )),
        )];
        assert!(first_auth_misconfigured_message(&results).is_none());
    }

    #[test]
    fn first_auth_misconfigured_message_returns_first_hit() {
        let results = vec![
            (
                "sb-a".to_string(),
                Ok(ok_resp(
                    "Memory",
                    Some("AuthMisconfigured: 403 search_memories"),
                )),
            ),
            (
                "sb-b".to_string(),
                Ok(ok_resp(
                    "Memory",
                    Some("AuthMisconfigured: 403 update_memories"),
                )),
            ),
        ];
        let msg = first_auth_misconfigured_message(&results).unwrap();
        assert!(msg.starts_with("sb-a: AuthMisconfigured: 403 search_memories"));
        assert!(msg.contains("+1 more sandbox(es) affected"));
    }

    #[test]
    fn first_auth_misconfigured_message_skips_failed_polls() {
        // Poll failures (Err) don't carry a router-reported
        // last_error, so the detector must ignore them and not
        // misattribute the failure as AuthMisconfigured.
        let results = vec![(
            "sb-a".to_string(),
            Err(router_confirmation::ConfirmError::HttpStatus(503)),
        )];
        assert!(first_auth_misconfigured_message(&results).is_none());
    }

    #[test]
    fn build_conditions_auth_misconfigured_sets_degraded_true_with_reason() {
        // Slice 3b.4 — when the reconciler pre-scan elevates an
        // AuthMisconfigured router error to `degraded`, the resulting
        // conditions must surface that reason on Degraded (not a
        // generic Awaiting) so operators get a clear pointer at the
        // RBAC misconfig rather than a transient-looking digest gap.
        let conds = build_conditions(
            &[],
            Some(1),
            Some((
                reason::AUTH_MISCONFIGURED,
                "sb-a: AuthMisconfigured: 403 from Foundry",
            )),
            &RouterEnforcementState::NotApplicable,
        );
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
        assert_eq!(degraded.reason, reason::AUTH_MISCONFIGURED);
        assert!(degraded.message.contains("AuthMisconfigured: 403"));
    }

    // ----- Slice 3b.5: MemoryStoreMissing detector + condition -----

    #[test]
    fn first_memory_store_missing_message_none_when_no_errors() {
        let results = vec![
            ("sb-a".to_string(), Ok(ok_resp("Memory", None))),
            ("sb-b".to_string(), Ok(ok_resp("Memory", None))),
        ];
        assert!(first_memory_store_missing_message(&results).is_none());
    }

    #[test]
    fn first_memory_store_missing_message_ignores_unrelated_errors() {
        // A generic transient error and an AuthMisconfigured error
        // must not trip the MemoryStoreMissing detector — those
        // belong to other buckets (Awaiting / AuthMisconfigured).
        let results = vec![
            (
                "sb-a".to_string(),
                Ok(ok_resp("Memory", Some("upstream 500"))),
            ),
            (
                "sb-b".to_string(),
                Ok(ok_resp("Memory", Some("AuthMisconfigured: 403 search"))),
            ),
        ];
        assert!(first_memory_store_missing_message(&results).is_none());
    }

    #[test]
    fn first_memory_store_missing_message_ignores_other_kinds() {
        // Router echoed `MemoryStoreMissing:` against a different
        // policy kind. The KarsMemory reconciler scans only Memory.
        let results = vec![(
            "sb-a".to_string(),
            Ok(ok_resp(
                "InferencePolicy",
                Some("MemoryStoreMissing: 404 search_memories"),
            )),
        )];
        assert!(first_memory_store_missing_message(&results).is_none());
    }

    #[test]
    fn first_memory_store_missing_message_returns_first_hit() {
        let results = vec![
            (
                "sb-a".to_string(),
                Ok(ok_resp(
                    "Memory",
                    Some("MemoryStoreMissing: 404 search_memories"),
                )),
            ),
            (
                "sb-b".to_string(),
                Ok(ok_resp(
                    "Memory",
                    Some("MemoryStoreMissing: 404 update_memories"),
                )),
            ),
        ];
        let msg = first_memory_store_missing_message(&results).unwrap();
        assert!(msg.starts_with("sb-a: MemoryStoreMissing: 404 search_memories"));
        assert!(msg.contains("+1 more sandbox(es) affected"));
    }

    #[test]
    fn first_memory_store_missing_message_skips_failed_polls() {
        let results = vec![(
            "sb-a".to_string(),
            Err(router_confirmation::ConfirmError::HttpStatus(503)),
        )];
        assert!(first_memory_store_missing_message(&results).is_none());
    }

    #[test]
    fn build_conditions_memory_store_missing_sets_degraded_true_with_reason() {
        // Slice 3b.5 — when the reconciler pre-scan elevates a
        // MemoryStoreMissing router error to `degraded`, the
        // resulting conditions must surface that reason on Degraded
        // (not Awaiting, not AuthMisconfigured) so operators get a
        // clear pointer at the missing-store fact rather than a
        // transient-looking digest gap.
        let conds = build_conditions(
            &[],
            Some(1),
            Some((
                reason::MEMORY_STORE_MISSING,
                "sb-a: MemoryStoreMissing: 404 from Foundry",
            )),
            &RouterEnforcementState::NotApplicable,
        );
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
        assert_eq!(degraded.reason, reason::MEMORY_STORE_MISSING);
        assert!(degraded.message.contains("MemoryStoreMissing: 404"));
    }

    #[test]
    fn field_manager_is_per_reconciler() {
        assert_eq!(FIELD_MANAGER, "kars-controller/karsmemory");
        assert_ne!(FIELD_MANAGER, "kars-controller/mcp");
        assert_ne!(FIELD_MANAGER, "kars-controller/toolpolicy");
        assert_ne!(FIELD_MANAGER, "kars-controller/a2aagent");
        assert_ne!(FIELD_MANAGER, "kars-controller/inferencepolicy");
    }
}
