// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ToolPolicy reconciler — Phase 2 §8 entry 4 (S2).
//!
//! Watches `ToolPolicy` CRs and, for each:
//!
//! 1. Ensures a finalizer (`kars.azure.com/toolpolicy-cleanup`) so
//!    the compiled-profile ConfigMap is cleaned up synchronously when
//!    the CR is removed.
//! 2. Runs the pure compile step
//!    [`crate::tool_policy_compile::compile_to_profile`] to produce the
//!    AGT-profile JSON the router will consume via
//!    `inference-router::policy_envelope::PolicyEntry::payload`.
//! 3. Persists the profile as a `ConfigMap` (one per CR), labelled for
//!    router-pod mount selection.
//! 4. Sets `status.observedGeneration`, `status.phase`,
//!    `status.conditions[]`, `status.profileConfigMapRef`,
//!    `status.versionHash`, `status.lastCompiledAt`.
//!
//! ## Reuse map
//!
//! Per the no-duplication rule (§0.2/§0.3): condition vocabulary +
//! transition-time helpers come from [`crate::status::conditions`].
//! Reconciler shape (Controller::new + non-fatal CRD missing) mirrors
//! [`crate::pairing_reconciler`] and [`crate::mcp_server_reconciler`].
//! `LocalObjectRef` is reused from [`crate::mcp_server`] — the same
//! struct is also (re-)exported here via a `pub use` for the
//! `ToolPolicyStatus`-side clients. The compiler is the
//! single-purpose [`crate::tool_policy_compile`] module — the
//! reconciler does not re-implement parsing.

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, ObjectMeta, Patch, PatchParams},
    runtime::controller::{Action, Controller},
    runtime::reflector::ObjectRef,
    runtime::watcher,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_COMPILED, PHASE_DEGRADED, PHASE_READY, PhaseEventReporter};
use crate::status::router_confirmation::{self, RouterEnforcementState, decide_enforcement_state};
use crate::status::router_confirmation_io::{list_sandboxes_matching, poll_referencing_sandboxes};
use crate::tool_policy::{ToolPolicy, ToolPolicyStatus};
use crate::tool_policy_compile::{
    AGT_PROFILE_FILENAME, agt_profile_digest, compile_to_profile, version_hash,
};

/// Field manager for SSA patches emitted by this reconciler. A unique
/// suffix per reconciler is the §10.4 #1 craftsmanship requirement —
/// detects out-of-band tampering.
const FIELD_MANAGER: &str = crate::field_managers::TOOL_POLICY;

/// Finalizer name (DNS subdomain).
const FINALIZER: &str = "kars.azure.com/toolpolicy-cleanup";

/// Requeue cadence on success.
const REQUEUE_OK: Duration = Duration::from_secs(300);

/// Requeue cadence on transient failure (ConfigMap write, etc).
const REQUEUE_FAIL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl ReconcileError {
    /// Closed-set error class string for safe logging (no operator-supplied
    /// strings — log-injection prevention per §15.3 of the plan).
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

async fn reconcile(tp: Arc<ToolPolicy>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = tp.name_any();
    let ns = tp.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(toolpolicy = %name, ns = %ns, "Reconciling ToolPolicy");

    let api: Api<ToolPolicy> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);

    // Deletion path — finalizer-cascading cleanup.
    if tp.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &configmaps, &tp, &name).await;
    }

    // Add finalizer if missing.
    if !tp
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"ToolPolicy","metadata":{"finalizers":[FINALIZER]}});
        api.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_conditions = tp
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let observed_generation = tp.metadata.generation;

    // 1. Compile spec → profile JSON.
    let profile = compile_to_profile(&tp.spec);
    let v_hash = version_hash(&profile);

    // 1b. Resolve the customer AGT profile from `spec.agtProfile`.
    //
    // Two sources, mutually exclusive:
    // - `inline`: raw YAML carried in the spec (Slice 1b path)
    // - `bundleRef`: signed OCI artifact (Slice 1c.2 path) — pulled
    //   and cosign-verified via the kind-generic policy_fetcher
    //   pipeline with `ToolsKind` as the discriminator.
    //
    // Both paths converge into `agt_profile_bytes: Option<String>`,
    // which is then written verbatim into the compiled-profile
    // ConfigMap under `agt-profile.yaml`. The router echoes the
    // length-prefixed digest computed over those exact bytes — same
    // wire contract for both sources, so the existing confirmation
    // poller works unchanged.
    //
    // `bundle_digest_for_status` carries the OCI manifest digest
    // forward to status.agtProfileBundleDigest when the profile came
    // from bundleRef (supply-chain attestation, distinct from the
    // length-prefixed wire-contract digest).
    let (agt_profile_bytes, bundle_digest_for_status, agt_source_degraded): (
        Option<String>,
        Option<String>,
        Option<(&'static str, String)>,
    ) = resolve_agt_profile_source(&tp).await;

    let agt_profile_inline: Option<&str> = agt_profile_bytes.as_deref();
    let agt_digest: Option<String> = agt_profile_inline.map(agt_profile_digest);

    // 2. Persist as ConfigMap.
    let cm_name = format!("toolpolicy-{name}-profile");
    let mut degraded: Option<(&'static str, String)> = agt_source_degraded;

    match ensure_profile_configmap(
        &configmaps,
        &cm_name,
        &name,
        &profile,
        &v_hash,
        agt_profile_inline,
        agt_digest.as_deref(),
    )
    .await
    {
        Ok(()) => {
            tracing::info!(
                toolpolicy = %name,
                ns = %ns,
                version_hash = %v_hash,
                generation = observed_generation.unwrap_or(0),
                has_commerce = tp.spec.commerce.is_some(),
                has_rate_limit = tp.spec.rate_limit.is_some(),
                has_approval = tp.spec.approval.is_some(),
                has_agt_profile = agt_profile_inline.is_some(),
                "ToolPolicyCompiled"
            );
        }
        Err(e) => {
            tracing::warn!(
                toolpolicy = %name,
                error_class = e.class(),
                "ToolPolicyProfileWriteFailed"
            );
            degraded = Some(("ProfileWriteFailed", e.to_string()));
        }
    }

    // 3. Build & write status.
    //
    // Slice 1c: when `agt_profile_inline` is set, poll every
    // referencing sandbox's inference-router for digest confirmation
    // and let the result drive both `phase` and the Ready condition.
    // When `agt_profile_inline` is None, the back-compat path
    // (NotApplicable) stamps `Ready` directly.
    let enforcement_state = if degraded.is_some() {
        // On compile failure, the state machine doesn't apply —
        // `degraded` short-circuits below. Use NotApplicable as a
        // sentinel value; `build_conditions` ignores it under degraded.
        RouterEnforcementState::NotApplicable
    } else {
        match (agt_profile_inline, agt_digest.as_deref()) {
            (Some(_), Some(expected)) => {
                let referrers = match list_sandboxes_matching(&ctx.client, &ns, |cs| {
                    cs.spec
                        .governance
                        .as_ref()
                        .is_some_and(|g| g.tool_policy_ref.name == name)
                })
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            toolpolicy = %name,
                            ns = %ns,
                            error = %e,
                            "KarsSandbox list failed; treating as no referrers"
                        );
                        Vec::new()
                    }
                };
                let results = poll_referencing_sandboxes(&ctx.client, &ctx.http, &referrers).await;
                decide_enforcement_state(expected, "AgtProfile", &results)
            }
            _ => RouterEnforcementState::NotApplicable,
        }
    };

    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        degraded
            .as_ref()
            .map(|(reason, msg)| (*reason, msg.as_str())),
        &enforcement_state,
    );
    let phase = if degraded.is_some() {
        PHASE_DEGRADED
    } else {
        match enforcement_state {
            RouterEnforcementState::Confirmed { .. } | RouterEnforcementState::NotApplicable => {
                PHASE_READY
            }
            RouterEnforcementState::NoSandboxesReferencing
            | RouterEnforcementState::Awaiting { .. } => PHASE_COMPILED,
        }
    };

    // Only emit a Warning event while we are *actually* awaiting
    // router-side confirmation (or starved of consumers). Once the
    // router echoes the digest, the Compiled→Ready transition fires
    // and the loop has been honestly closed — no need to keep
    // shouting.
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
                tp.as_ref(),
                "AwaitingRouterConfirmation",
                "ToolPolicy.spec.agtProfile.inline parsed and published, but the \
                 inference-router has not yet echoed the matching digest on \
                 /internal/policy-status. Ready=False until confirmation.",
            )
            .await
    {
        tracing::warn!(
            toolpolicy = %name,
            error = %e,
            "ToolPolicyEventPublishFailed"
        );
    }

    // SSA requires apiVersion + kind in the patch body — without
    // them, the API server returns "invalid object type: /, Kind=".
    let status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "ToolPolicy",
        "status": ToolPolicyStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            last_compiled_at: Some(rfc3339_now()),
            agt_profile_digest: agt_digest,
            agt_profile_bundle_digest: bundle_digest_for_status,
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
        match enforcement_state {
            RouterEnforcementState::Confirmed { .. } | RouterEnforcementState::NotApplicable => {
                Ok(Action::requeue(REQUEUE_OK))
            }
            // Short cadence while awaiting confirmation so the
            // Compiled→Ready transition fires quickly once the
            // router catches up.
            RouterEnforcementState::Awaiting { .. }
            | RouterEnforcementState::NoSandboxesReferencing => {
                Ok(Action::requeue(Duration::from_secs(15)))
            }
        }
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Resolve the customer AGT profile from `spec.agtProfile`. Returns:
///
/// - `(None, None, None)` — no `agtProfile` in the spec; back-compat
///   path (controller stamps `Ready=True / reason=NotApplicable`).
/// - `(Some(bytes), None, None)` — `inline` source; bytes are the spec
///   string verbatim.
/// - `(Some(bytes), Some(oci_digest), None)` — `bundleRef` source;
///   bytes are the verified OCI artifact payload; `oci_digest` is the
///   manifest digest matched against `bundleRef.digest`.
/// - `(None, None, Some((reason, msg)))` — fetch/validation failed or
///   the source is malformed. The reconcile loop short-circuits to
///   `phase=Degraded / Ready=False / reason=<reason>` using the
///   existing degraded-handling path.
///
/// Mutual exclusion: when both `inline` and `bundleRef` are set, this
/// is a spec error — admission CEL also rejects it, but we re-check at
/// runtime as defense in depth (older clusters may run without the
/// latest CRD).
async fn resolve_agt_profile_source(
    tp: &ToolPolicy,
) -> (
    Option<String>,
    Option<String>,
    Option<(&'static str, String)>,
) {
    let Some(src) = tp.spec.agt_profile.as_ref() else {
        return (None, None, None);
    };

    let inline_set = src
        .inline
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let bundle_set = src.bundle_ref.is_some();

    match (inline_set, bundle_set) {
        (false, false) => (None, None, None),
        (true, true) => (
            None,
            None,
            Some((
                "InvalidSpec",
                "spec.agtProfile.inline and spec.agtProfile.bundleRef are mutually exclusive"
                    .into(),
            )),
        ),
        (true, false) => {
            // Safe to unwrap: inline_set proves the inner string is Some + non-empty.
            let bytes = src.inline.clone().expect("inline_set implies Some");
            (Some(bytes), None, None)
        }
        (false, true) => {
            let bundle_ref = src
                .bundle_ref
                .as_ref()
                .expect("bundle_set implies Some")
                .clone();
            let signer_policy_handle = crate::signer_policy::global();
            let verify_result = match signer_policy_handle.snapshot() {
                crate::signer_policy::SignerPolicyState::FromConfigMap(p) => {
                    let cfg: crate::policy_fetcher::SignerPolicyConfig = p.into();
                    crate::policy_fetcher::fetch_and_verify_generic::<
                        crate::policy_canonical::tools::ToolsKind,
                    >(&bundle_ref, &cfg)
                    .await
                }
                crate::signer_policy::SignerPolicyState::Malformed(msg) => Err(
                    crate::policy_fetcher::FetchError::SignerPolicyMalformed(msg),
                ),
                crate::signer_policy::SignerPolicyState::Absent => {
                    let cfg = crate::policy_fetcher::SignerPolicyConfig::from_env();
                    crate::policy_fetcher::fetch_and_verify_generic::<
                        crate::policy_canonical::tools::ToolsKind,
                    >(&bundle_ref, &cfg)
                    .await
                }
            };
            match verify_result {
                Ok(verified) => {
                    // bytes are valid UTF-8 because canonical-form parse
                    // (`policy_canonical::tools::parse`) re-validates UTF-8;
                    // a malformed payload would have produced an error
                    // before reaching this point.
                    let bytes = String::from_utf8(verified.bytes)
                        .expect("ToolsKind::parse re-validates utf-8 before finalize");
                    (Some(bytes), Some(verified.digest), None)
                }
                Err(e) => {
                    let (reason, msg) = fetch_error_to_degraded(&e);
                    tracing::warn!(
                        toolpolicy = %tp.name_any(),
                        registry = %bundle_ref.registry,
                        repository = %bundle_ref.repository,
                        digest = %bundle_ref.digest,
                        reason,
                        "ToolPolicy bundleRef fetch/verify failed: {msg}"
                    );
                    (None, None, Some((reason, msg)))
                }
            }
        }
    }
}

/// Map [`crate::policy_fetcher::FetchError`] variants to the
/// `(reason, message)` pair the controller status machinery
/// consumes. Delegates to
/// [`crate::policy_fetcher::reason_for_error`] so the one shared
/// vocabulary stays in lockstep with the egress reconciler (one
/// vocabulary across all signed-policy kinds keeps operator-facing
/// docs minimal and the controller's class table closed — §15.3
/// log-injection prevention). [`crate::policy_fetcher::FetchError::Transient`]
/// is mapped to `"Transient"` here rather than surfaced as a `None`
/// reason: the ToolPolicy state machine does not have a
/// "preserve last-known-good" branch (unlike the egress sandbox
/// reconciler), so a transient fetch failure surfaces honestly as
/// Degraded until the next requeue succeeds.
fn fetch_error_to_degraded(e: &crate::policy_fetcher::FetchError) -> (&'static str, String) {
    let reason = crate::policy_fetcher::reason_for_error(e).unwrap_or("Transient");
    (reason, e.to_string())
}

/// Build the Conditions vector preserving prior `lastTransitionTime`
/// where status hasn't flipped. Always emits `Ready`, `Progressing`,
/// `Degraded`.
///
/// `awaiting_router` is `true` when the spec has an `agtProfile`
/// section — the controller cannot yet observe data-plane enforcement
/// of that artifact (Slice 1c closes that loop), so per principles.md
/// §3 we honestly report `Ready=False /
/// reason=AwaitingRouterEnforcement` rather than the optimistic
/// `Ready=True` we still stamp for the back-compat (no-agtProfile)
/// path.
/// Slice 1c expands the previous `awaiting_router: bool` to a richer
/// [`RouterEnforcementState`] so the Ready condition can carry the
/// specific reason — `NoSandboxesReferencing` /
/// `AwaitingRouterEnforcement` / `RouterEnforcing` — that operators
/// see in `kubectl describe toolpolicy …`.
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
                "compile failed",
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
                out.push(conditions::preserve_transition_time(
                    prior_ready,
                    conditions::TYPE_READY,
                    cond_status::TRUE,
                    reason::RECONCILED,
                    "ToolPolicy compiled and published",
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
                        "all {total} referencing sandbox router(s) confirmed agt-profile digest"
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
                    "no KarsSandbox references this ToolPolicy; nothing to enforce",
                    observed_generation,
                ));
                out.push(conditions::preserve_transition_time(
                    prior_progressing,
                    conditions::TYPE_PROGRESSING,
                    cond_status::TRUE,
                    reason::NO_SANDBOXES_REFERENCING,
                    "waiting for a KarsSandbox to reference this policy",
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

async fn ensure_profile_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &str,
    profile: &serde_json::Value,
    v_hash: &str,
    agt_profile_inline: Option<&str>,
    agt_profile_digest: Option<&str>,
) -> Result<(), ReconcileError> {
    let json_str = serde_json::to_string_pretty(profile)?;
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("profile.json".into(), json_str);
    // Slice 1b: when the customer supplies an AGT profile, publish
    // the raw bytes under the wire-contract filename. The sandbox
    // mirror picks the whole ConfigMap into the sandbox namespace,
    // and the inference-router's existing `Governance` loader
    // (filters `*.yaml`/`*.yml`) auto-discovers the new key —
    // no router-side change required for the Slice 1b producer
    // path. Slice 1c adds the controller-side confirmation poller.
    if let Some(inline) = agt_profile_inline {
        data.insert(AGT_PROFILE_FILENAME.into(), inline.into());
    }
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "kars.azure.com/toolpolicy-version-hash".into(),
        v_hash.into(),
    );
    if let Some(d) = agt_profile_digest {
        annotations.insert("kars.azure.com/agt-profile-digest".into(), d.into());
    }
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.into()),
            annotations: Some(annotations),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                ("kars.azure.com/toolpolicy".into(), owner.into()),
                ("kars.azure.com/artifact".into(), "compiled-profile".into()),
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
    api: &Api<ToolPolicy>,
    configmaps: &Api<ConfigMap>,
    tp: &ToolPolicy,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("toolpolicy-{name}-profile");
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

    let finalizers: Vec<String> = tp
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"ToolPolicy","metadata":{"finalizers": finalizers}});
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(toolpolicy = %name, "ToolPolicyDeleted");
    Ok(Action::await_change())
}

fn error_policy(tp: Arc<ToolPolicy>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("ToolPolicy", error.class());
    tracing::warn!(
        toolpolicy = %tp.name_any(),
        error_class = error.class(),
        error = %error,
        "ToolPolicy reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

/// Start the controller loop. Non-fatal CRD-missing exit mirrors
/// `pairing_reconciler::run` and `mcp_server_reconciler::run`.
pub async fn run(client: Client) -> Result<()> {
    let tps: Api<ToolPolicy> = Api::all(client.clone());
    match tps.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("ToolPolicy CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("ToolPolicy CRD not installed — reconciler disabled: {e}");
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
    let http = reqwest::Client::builder()
        .timeout(router_confirmation::DEFAULT_TIMEOUT)
        .build()
        .expect("default reqwest client builds with infallible config");
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        http,
        phase_reporter: PhaseEventReporter::new(client.clone(), "ToolPolicy"),
    });
    // Watch KarsSandbox so when a sandbox is created/updated that points
    // at this ToolPolicy via spec.governance.toolPolicyRef.name (OR uses
    // the empty-toolPolicyRef → kars-default fallback), the ToolPolicy
    // reconciler re-runs immediately instead of waiting up to 5 min for
    // the next periodic resweep. Also handles the system-default
    // 'kars-default' ToolPolicy: every sandbox in the chart's release
    // namespace that doesn't set toolPolicyRef explicitly implicitly
    // references it.
    let sandboxes: Api<crate::crd::KarsSandbox> = Api::all(client);
    Controller::new(tps, watcher::Config::default())
        .watches(
            sandboxes,
            watcher::Config::default(),
            |sb: crate::crd::KarsSandbox| {
                let ns = sb.namespace().unwrap_or_default();
                if ns.is_empty() { return Vec::new().into_iter(); }
                let mut refs: Vec<ObjectRef<crate::tool_policy::ToolPolicy>> = Vec::new();
                if let Some(gov) = sb.spec.governance.as_ref() {
                    if !gov.enabled { return refs.into_iter(); }
                    let name = if gov.tool_policy_ref.name.is_empty() {
                        "kars-default".to_string()
                    } else {
                        gov.tool_policy_ref.name.clone()
                    };
                    refs.push(ObjectRef::<crate::tool_policy::ToolPolicy>::new(&name).within(&ns));
                }
                refs.into_iter()
            },
        )
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("ToolPolicy", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("ToolPolicy reconciled {:?}", o),
                Err(e) => tracing::warn!("ToolPolicy reconcile failed: {e:?}"),
            }
        })
        .await;
    Ok(())
}

// `LocalObjectRef` is intentionally re-exported from this module so
// downstream slices (S3 A2AAgent etc.) that need the same status-side
// "namespaced object reference" type don't introduce yet another copy.
#[allow(unused_imports)]
pub use crate::mcp_server::LocalObjectRef as ProfileConfigMapRef;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::router_confirmation::ConfirmError;

    #[test]
    fn rfc3339_now_is_utc_z_suffixed() {
        let s = rfc3339_now();
        assert!(s.ends_with('Z'), "got {s}");
        assert_eq!(s.len(), 20, "RFC3339 with seconds + Z is 20 chars: {s}");
    }

    #[test]
    fn error_class_is_closed_set() {
        // Serde error path is the cheapest to exercise — kube::Error::Api
        // construction shape varies across kube-rs versions, but the match
        // arm is trivially correct.
        let serde_err: serde_json::Error =
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let e = ReconcileError::SerdeJson(serde_err);
        assert_eq!(e.class(), "serde");

        // Compile-time exhaustiveness: every variant must produce one of
        // the closed-set strings. This match is a tripwire — adding a
        // new variant forces an update here.
        fn assert_closed(class: &str) {
            assert!(matches!(class, "kube_api" | "serde"));
        }
        assert_closed(e.class());
    }

    #[test]
    fn build_conditions_emits_all_three_types_on_success() {
        let conds = build_conditions(&[], Some(1), None, &RouterEnforcementState::NotApplicable);
        assert_eq!(conds.len(), 3);
        let types: Vec<&str> = conds.iter().map(|c| c.type_.as_str()).collect();
        assert!(types.contains(&conditions::TYPE_READY));
        assert!(types.contains(&conditions::TYPE_PROGRESSING));
        assert!(types.contains(&conditions::TYPE_DEGRADED));
        // Ready=True on success path.
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::TRUE);
        // Degraded=False on success path.
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::FALSE);
    }

    #[test]
    fn build_conditions_emits_all_three_types_on_failure() {
        let conds = build_conditions(
            &[],
            Some(1),
            Some(("ProfileWriteFailed", "boom")),
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
        assert_eq!(degraded.reason, "ProfileWriteFailed");
    }

    #[test]
    fn build_conditions_preserves_transition_time_when_status_unchanged() {
        let first = build_conditions(&[], Some(1), None, &RouterEnforcementState::NotApplicable);
        let second = build_conditions(
            &first,
            Some(2),
            None,
            &RouterEnforcementState::NotApplicable,
        );
        let r1 = first
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        let r2 = second
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(
            r1.last_transition_time, r2.last_transition_time,
            "transition time must not move when status stays True"
        );
    }

    #[test]
    fn build_conditions_awaiting_branch_is_ready_false_with_reason() {
        let conds = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::Awaiting {
                total: 2,
                matched: 1,
                message: "1/2 sandbox routers confirmed digest".into(),
            },
        );
        assert_eq!(conds.len(), 3);
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::FALSE);
        assert_eq!(ready.reason, reason::AWAITING_ROUTER_ENFORCEMENT);
        assert!(
            ready.message.contains("1/2"),
            "message should surface match counts"
        );
        let progressing = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_PROGRESSING)
            .unwrap();
        assert_eq!(progressing.status, cond_status::TRUE);
        assert_eq!(progressing.reason, reason::AWAITING_ROUTER_ENFORCEMENT);
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::FALSE);
    }

    #[test]
    fn build_conditions_no_sandboxes_referencing_is_ready_false() {
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
        let progressing = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_PROGRESSING)
            .unwrap();
        assert_eq!(progressing.status, cond_status::TRUE);
    }

    #[test]
    fn build_conditions_confirmed_is_ready_true_with_router_enforcing() {
        let conds = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::Confirmed { total: 3 },
        );
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::TRUE);
        assert_eq!(ready.reason, reason::ROUTER_ENFORCING);
        assert!(
            ready.message.contains("3"),
            "should mention how many routers confirmed"
        );
    }

    #[test]
    fn build_conditions_degraded_overrides_awaiting_router() {
        let conds = build_conditions(
            &[],
            Some(1),
            Some(("ProfileWriteFailed", "boom")),
            &RouterEnforcementState::Awaiting {
                total: 1,
                matched: 0,
                message: "0/1".into(),
            },
        );
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::FALSE);
        assert_eq!(ready.reason, "ProfileWriteFailed");
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::TRUE);
    }

    // ── decide_enforcement_state ─────────────────────────────────────

    fn mk_response(
        digest: Option<&str>,
        last_error: Option<&str>,
    ) -> router_confirmation::PolicyStatusResponse {
        router_confirmation::PolicyStatusResponse {
            schema_version: 1,
            count: if digest.is_some() || last_error.is_some() {
                1
            } else {
                0
            },
            entries: if digest.is_some() || last_error.is_some() {
                vec![router_confirmation::PolicyStatusEntry {
                    kind: "AgtProfile".into(),
                    digest: digest.map(String::from),
                    source_path: "/etc/agt/policies".into(),
                    loaded_at: "2026-05-13T09:00:00.000Z".into(),
                    last_error: last_error.map(String::from),
                }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn decide_no_referrers_yields_no_sandboxes_referencing() {
        let s = decide_enforcement_state("sha256:abc", "AgtProfile", &[]);
        assert_eq!(s, RouterEnforcementState::NoSandboxesReferencing);
    }

    #[test]
    fn decide_all_match_yields_confirmed() {
        let results = vec![
            ("a".into(), Ok(mk_response(Some("sha256:abc"), None))),
            ("b".into(), Ok(mk_response(Some("sha256:abc"), None))),
        ];
        let s = decide_enforcement_state("sha256:abc", "AgtProfile", &results);
        assert_eq!(s, RouterEnforcementState::Confirmed { total: 2 });
    }

    #[test]
    fn decide_partial_match_yields_awaiting_with_count() {
        let results = vec![
            ("a".into(), Ok(mk_response(Some("sha256:abc"), None))),
            ("b".into(), Ok(mk_response(Some("sha256:DIFFERENT"), None))),
        ];
        let s = decide_enforcement_state("sha256:abc", "AgtProfile", &results);
        match s {
            RouterEnforcementState::Awaiting {
                total,
                matched,
                message,
            } => {
                assert_eq!(total, 2);
                assert_eq!(matched, 1);
                assert!(message.contains("1/2"));
                assert!(message.contains("mismatch"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn decide_unreachable_yields_awaiting_with_zero_matched() {
        let results = vec![(
            "a".into(),
            Err::<router_confirmation::PolicyStatusResponse, _>(ConfirmError::HttpStatus(0)),
        )];
        let s = decide_enforcement_state("sha256:abc", "AgtProfile", &results);
        match s {
            RouterEnforcementState::Awaiting {
                total,
                matched,
                message,
            } => {
                assert_eq!(total, 1);
                assert_eq!(matched, 0);
                assert!(
                    message.contains("unreachable"),
                    "expected 'unreachable' in {message:?}"
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn decide_router_returned_null_digest_surfaces_last_error() {
        let results = vec![("a".into(), Ok(mk_response(None, Some("bad yaml line 4"))))];
        let s = decide_enforcement_state("sha256:abc", "AgtProfile", &results);
        match s {
            RouterEnforcementState::Awaiting { message, .. } => {
                assert!(
                    message.contains("not yet loaded") && message.contains("bad yaml"),
                    "expected last_error in message: {message:?}"
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn decide_caps_detail_messages_at_three() {
        let results: Vec<_> = (0..10)
            .map(|i| {
                (
                    format!("sandbox-{i}"),
                    Ok(mk_response(Some("sha256:DIFFERENT"), None)),
                )
            })
            .collect();
        let s = decide_enforcement_state("sha256:abc", "AgtProfile", &results);
        match s {
            RouterEnforcementState::Awaiting { message, .. } => {
                assert!(message.contains("0/10"));
                // Three details + " (+7 more)" suffix.
                assert!(message.contains("(+7 more)"), "got: {message}");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn finalizer_constant_is_dns_subdomain() {
        assert!(FINALIZER.contains('/'));
        let (domain, key) = FINALIZER.split_once('/').unwrap();
        assert_eq!(domain, "kars.azure.com");
        assert!(!key.is_empty());
    }

    #[test]
    fn field_manager_is_per_reconciler() {
        // Distinct from the McpServer manager — required by §10.4 #1.
        assert_eq!(FIELD_MANAGER, "kars-controller/toolpolicy");
        assert_ne!(FIELD_MANAGER, "kars-controller/mcp");
    }

    /// `resolve_agt_profile_source` for the four spec-shape cases that
    /// do NOT touch network / global SignerPolicy state:
    ///   * `agtProfile` absent — back-compat path.
    ///   * `agtProfile` set but both children empty.
    ///   * `inline` only — happy path.
    ///   * `inline` + `bundleRef` both set — runtime mutual-exclusion
    ///     guard (CEL enforces this at admission, but older clusters
    ///     without the latest CRD must still surface a clear
    ///     `Degraded=True / reason=InvalidSpec` here).
    ///
    /// The pure `bundleRef`-only path is exercised by `policy_canonical::tools`
    /// unit tests + e2e (`tests/e2e/...`) rather than here, because it
    /// reads the global `SignerPolicy` handle and performs an HTTPS
    /// pull which is integration territory.
    use crate::tool_policy::{AgtProfileSource, AppliesToSelector, ToolPolicy, ToolPolicySpec};

    fn fixture_spec(agt: Option<AgtProfileSource>) -> ToolPolicySpec {
        ToolPolicySpec {
            applies_to: AppliesToSelector::default(),
            commerce: None,
            rate_limit: None,
            approval: None,
            display_name: None,
            agt_profile: agt,
        }
    }

    #[tokio::test]
    async fn resolve_agt_profile_absent_yields_all_none() {
        let tp = ToolPolicy::new("tp-fixture", fixture_spec(None));
        let (bytes, digest, degraded) = resolve_agt_profile_source(&tp).await;
        assert!(bytes.is_none());
        assert!(digest.is_none());
        assert!(degraded.is_none());
    }

    #[tokio::test]
    async fn resolve_agt_profile_both_children_empty_yields_all_none() {
        let tp = ToolPolicy::new(
            "tp-fixture",
            fixture_spec(Some(AgtProfileSource {
                inline: None,
                bundle_ref: None,
            })),
        );
        let (bytes, digest, degraded) = resolve_agt_profile_source(&tp).await;
        assert!(bytes.is_none());
        assert!(digest.is_none());
        assert!(degraded.is_none());
    }

    #[tokio::test]
    async fn resolve_agt_profile_inline_only_returns_bytes_no_digest() {
        let body = "version: 1\nagent: foo\npolicies: []\n";
        let tp = ToolPolicy::new(
            "tp-fixture",
            fixture_spec(Some(AgtProfileSource {
                inline: Some(body.to_string()),
                bundle_ref: None,
            })),
        );
        let (bytes, digest, degraded) = resolve_agt_profile_source(&tp).await;
        assert_eq!(bytes.as_deref(), Some(body));
        assert!(
            digest.is_none(),
            "inline path must NOT stamp the OCI manifest digest"
        );
        assert!(degraded.is_none());
    }

    #[tokio::test]
    async fn resolve_agt_profile_both_set_yields_invalid_spec_degraded() {
        let tp = ToolPolicy::new(
            "tp-fixture",
            fixture_spec(Some(AgtProfileSource {
                inline: Some("version: 1\nagent: a\npolicies: []\n".to_string()),
                bundle_ref: Some(crate::crd::OciArtifactRef {
                    registry: "example.azurecr.io".into(),
                    repository: "kars/tools".into(),
                    digest:
                        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                            .into(),
                    artifact_type: "application/vnd.kars.agt-profile.v1+yaml".into(),
                }),
            })),
        );
        let (bytes, digest, degraded) = resolve_agt_profile_source(&tp).await;
        assert!(bytes.is_none());
        assert!(digest.is_none());
        let (reason, msg) = degraded.expect("both-set must produce degraded tuple");
        assert_eq!(reason, "InvalidSpec");
        assert!(msg.contains("mutually exclusive"), "got: {msg}");
    }

    /// `fetch_error_to_degraded` must delegate to the shared
    /// `reason_for_error` taxonomy and only synthesize `"Transient"`
    /// for the Transient variant. This is a tripwire — if egress ever
    /// renames a reason, this test fails alongside.
    #[test]
    fn fetch_error_to_degraded_delegates_to_shared_taxonomy() {
        use crate::policy_fetcher::FetchError;
        let cases: &[(FetchError, &str)] = &[
            (FetchError::SignerPolicyMissing, "SignerPolicyMissing"),
            (
                FetchError::SignerPolicyMalformed("oops".into()),
                "SignerPolicyMalformed",
            ),
            (FetchError::InvalidRef("bad".into()), "InvalidRef"),
            (FetchError::Unauthorized("401".into()), "Unauthorized"),
            (
                FetchError::NotFound("ghcr.io/x@sha256:0".into()),
                "NotFound",
            ),
            (
                FetchError::SignatureVerifyFailed("bad sig".into()),
                "SignatureVerifyFailed",
            ),
            (
                FetchError::CanonicalFormViolation("non-empty".into()),
                "CanonicalFormViolation",
            ),
            (
                FetchError::IdentityMismatch("not pinned".into()),
                "IdentityMismatch",
            ),
            (FetchError::Transient("network".into()), "Transient"),
        ];
        for (err, expected) in cases {
            let (reason, msg) = fetch_error_to_degraded(err);
            assert_eq!(
                reason, *expected,
                "FetchError {err:?} mapped to {reason}, expected {expected}"
            );
            assert!(!msg.is_empty());
        }
    }
}
