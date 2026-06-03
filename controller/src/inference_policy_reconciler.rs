// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `InferencePolicy` reconciler — Phase 2 §8 entry 4 (S4).
//!
//! Watches `InferencePolicy` CRs and, for each:
//!
//! 1. Ensures finalizer (`kars.azure.com/inferencepolicy-cleanup`)
//!    so the published profile ConfigMap is cleaned up synchronously
//!    on delete.
//! 2. Runs the pure compile step
//!    [`crate::inference_policy_compile::compile_to_profile`] to
//!    produce the AGT-profile JSON the router will consume via
//!    `PolicyEntry.payload` once S7 wires the informer.
//! 3. Persists the profile as a `ConfigMap`
//!    (`inferencepolicy-{name}-profile`, key `profile.json`),
//!    labelled for router-pod selection, annotated with the version
//!    hash for change detection.
//! 4. Sets `status.observedGeneration`, `status.phase`,
//!    `status.conditions[]`, `status.profileConfigMapRef`,
//!    `status.versionHash`, `status.lastCompiledAt`.
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! - **Conditions vocabulary + transition-time helpers**:
//!   [`crate::status::conditions`].
//! - **Reconciler shape** (Controller::new + non-fatal CRD-missing
//!   exit): mirrors [`crate::a2a_agent_reconciler`] (S3),
//!   [`crate::tool_policy_reconciler`] (S2), and
//!   [`crate::mcp_server_reconciler`] (S1).
//! - **`LocalObjectRef`**: re-used from [`crate::mcp_server`] —
//!   single struct, four semantic clients now (S1
//!   `signingKeyRef`/`jwksConfigMapRef`, S2 `profileConfigMapRef`,
//!   S3 `agentCardConfigMapRef`, S4 `profileConfigMapRef`).
//! - **Compile**: single-purpose
//!   [`crate::inference_policy_compile`] module — the reconciler does
//!   no JSON shaping itself.
//! - **Router-side runtime gate**:
//!   [`inference-router::routes::inference_policy::check`] is the
//!   call-site gate; the compiled profile flows in via
//!   `PolicyEnvelope` (S7 wiring). **No router-side change in this
//!   slice.**

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

use crate::inference_policy::{InferencePolicy, InferencePolicyStatus};
use crate::inference_policy_compile::{
    INFERENCE_POLICY_FILENAME, compile_to_profile, inference_policy_digest, version_hash,
};
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_COMPILED, PHASE_DEGRADED, PHASE_READY, PhaseEventReporter};
use crate::status::router_confirmation::{self, RouterEnforcementState, decide_enforcement_state};
use crate::status::router_confirmation_io::{list_sandboxes_matching, poll_referencing_sandboxes};

/// Field manager for SSA patches emitted by this reconciler. Distinct
/// from S1 `…/mcp`, S2 `…/toolpolicy`, S3 `…/a2aagent` per §10.4 #1
/// — surfaces out-of-band tampering.
const FIELD_MANAGER: &str = crate::field_managers::INFERENCE_POLICY;

/// Finalizer name (DNS subdomain).
const FINALIZER: &str = "kars.azure.com/inferencepolicy-cleanup";

/// Requeue cadence on success.
const REQUEUE_OK: Duration = Duration::from_secs(300);

/// Requeue cadence on transient failure.
const REQUEUE_FAIL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl ReconcileError {
    /// Closed-set error class for safe logging (no operator-supplied
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

async fn reconcile(policy: Arc<InferencePolicy>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = policy.name_any();
    let ns = policy.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(inferencepolicy = %name, ns = %ns, "Reconciling InferencePolicy");

    let api: Api<InferencePolicy> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);

    if policy.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &configmaps, &policy, &name).await;
    }

    if !policy
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy","metadata":{"finalizers":[FINALIZER]}});
        api.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_conditions = policy
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let observed_generation = policy.metadata.generation;

    let (effective_spec, bundle_ref_digest, source_degraded) =
        resolve_inference_source(policy.as_ref()).await;

    let profile = compile_to_profile(&effective_spec);
    let v_hash = version_hash(&profile);
    // Slice 2a — canonical bytes the router-side
    // `inference_policy_loader` will sha256 + echo via
    // `GET /internal/policy-status`. The bytes written into the
    // ConfigMap must be **exactly** what we hash here so the
    // controller-emitted digest matches the router-loaded digest;
    // any reformatting (pretty-print, trailing newline, key
    // reordering) silently breaks the §3 echo contract.
    let canonical_bytes = serde_json::to_vec(&profile)?;
    let canonical_str =
        String::from_utf8(canonical_bytes.clone()).expect("serde_json::to_vec emits valid UTF-8");
    let compiled_digest = inference_policy_digest(&canonical_bytes);

    let cm_name = format!("inferencepolicy-{name}-profile");
    let mut degraded: Option<(&'static str, String)> = source_degraded;

    if degraded.is_none() {
        match ensure_profile_configmap(
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
                    inferencepolicy = %name,
                    ns = %ns,
                    version_hash = %v_hash,
                    compiled_digest = %compiled_digest,
                    bundle_ref_digest = bundle_ref_digest.as_deref().unwrap_or("-"),
                    generation = observed_generation.unwrap_or(0),
                    has_token_budget = effective_spec.token_budget.is_some(),
                    has_content_safety = effective_spec.content_safety.is_some(),
                    has_model_preference = effective_spec.model_preference.is_some(),
                    "InferencePolicyCompiled"
                );
            }
            Err(e) => {
                tracing::warn!(
                    inferencepolicy = %name,
                    error_class = e.class(),
                    "InferencePolicyProfileWriteFailed"
                );
                degraded = Some(("ProfileWriteFailed", e.to_string()));
            }
        }
    } else {
        tracing::warn!(
            inferencepolicy = %name,
            ns = %ns,
            reason = degraded.as_ref().map(|(r, _)| *r).unwrap_or("-"),
            "InferencePolicySourceResolveFailed; skipping ConfigMap write"
        );
    }

    // Slice 2a — close the §3 "Ready ⇔ router echo" loop. List
    // KarsSandboxes that reference this InferencePolicy by
    // `spec.inferenceRef.name`, GET `/internal/policy-status` on
    // each router, and let the result drive both `phase` and the
    // Ready condition. The `compiled_digest` we just wrote is the
    // value every router must echo before we promote to Ready.
    let enforcement_state = if degraded.is_some() {
        RouterEnforcementState::NotApplicable
    } else {
        let referrers = match list_sandboxes_matching(&ctx.client, &ns, |cs| {
            cs.spec.inference_ref.name == name
        })
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    inferencepolicy = %name,
                    ns = %ns,
                    error = %e,
                    "KarsSandbox list failed; treating as no referrers"
                );
                Vec::new()
            }
        };
        let results = poll_referencing_sandboxes(&ctx.client, &ctx.http, &referrers).await;
        decide_enforcement_state(&compiled_digest, "InferencePolicy", &results)
    };

    // Latest digest echoed by any referring router — surfaced in
    // status for `kubectl describe` / `kars inspect`. When
    // every router reports the same value we promote to Ready; in
    // the Awaiting branch we surface what we actually saw so the
    // operator can diff against `compiledDigest`.
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

    // Only emit the Warning while truly awaiting router-side
    // confirmation. See
    // `crate::status::router_confirmation::should_publish_warning`
    // for the full rationale.
    let publish_warning = crate::status::router_confirmation::should_publish_warning(
        &enforcement_state,
        degraded.is_some(),
    );
    if publish_warning
        && let Err(e) = ctx
            .phase_reporter
            .warn_policy_not_enforced(
                policy.as_ref(),
                "AwaitingRouterConfirmation",
                "InferencePolicy compiled and published, but the inference-router has not \
                 yet echoed the matching digest on /internal/policy-status. \
                 tokenBudget.perRequestTokens is the only axis enforced today; \
                 contentSafety / modelPreference still land in later sub-slices.",
            )
            .await
    {
        tracing::warn!(
            inferencepolicy = %name,
            error = %e,
            "InferencePolicyEventPublishFailed",
        );
    }

    let status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "InferencePolicy",
        "status": InferencePolicyStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            profile_config_map_ref: Some(LocalObjectRef { name: cm_name.clone() }),
            version_hash: Some(v_hash),
            last_compiled_at: Some(rfc3339_now()),
            compiled_digest: Some(compiled_digest),
            loaded_digest,
            bundle_ref_digest: bundle_ref_digest.clone(),
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
        // Short requeue while awaiting echo — mirrors slice-1c.
        Ok(Action::requeue(Duration::from_secs(15)))
    } else {
        Ok(Action::requeue(REQUEUE_OK))
    }
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Resolve `InferencePolicy.spec` to the effective spec the reconciler
/// will compile + serialize. Slice 1c.3 (`crd-well-oiled-machine`).
///
/// Four spec shapes are accepted, matching the
/// [`PolicyKind`](crate::policy_canonical::PolicyKind) trait's
/// invariants:
///
/// - **Inline (back-compat, no signature)**: any of `tokenBudget` /
///   `contentSafety` / `modelPreference` / `displayName` set; no
///   `bundleRef`. Returns the spec verbatim. `bundle_ref_digest = None`.
/// - **Signed bundle**: `bundleRef` set, content fields all `None`.
///   Fetches + verifies the OCI artifact via
///   [`crate::policy_fetcher::fetch_and_verify_generic`]
///   parameterised by
///   [`crate::policy_canonical::inference::InferenceKind`]. The
///   bundle's content fields are merged onto the CR's `appliesTo`
///   selector. `bundle_ref_digest = Some(<manifest digest>)`.
/// - **Selector-only (no content)**: no `bundleRef` and no inline
///   content fields. Acceptable today: the compile path still emits
///   a valid `inference-policy.json` (all content fields null) so
///   the router echo loop still closes. `bundle_ref_digest = None`.
/// - **Both inline + bundleRef** *(rejected at runtime as
///   defense-in-depth — admission CEL already rejects)*: returns a
///   `(InvalidSpec, msg)` degraded reason without performing the
///   fetch. Older clusters running without the latest CRD CEL still
///   surface the error honestly.
///
/// Returns a triple:
/// - `effective_spec`: the spec the reconciler will compile (always
///   contains `appliesTo` from the CR; content fields come from
///   either the inline fields or the verified bundle).
/// - `bundle_ref_digest`: `Some(<sha256:...>)` when the bundleRef path
///   was taken (operator-visible proof the signature was checked);
///   `None` otherwise.
/// - `degraded`: `Some((reason, message))` on fetch/verify failure or
///   spec-shape error. The reconcile loop short-circuits to
///   `phase=Degraded / Ready=False / reason=<reason>` using the
///   existing degraded-handling path.
async fn resolve_inference_source(
    policy: &InferencePolicy,
) -> (
    crate::inference_policy::InferencePolicySpec,
    Option<String>,
    Option<(&'static str, String)>,
) {
    let spec = &policy.spec;
    let inline_any = spec.token_budget.is_some()
        || spec.content_safety.is_some()
        || spec.model_preference.is_some()
        || spec.display_name.is_some();
    let bundle_set = spec.bundle_ref.is_some();

    if inline_any && bundle_set {
        return (
            // selector-only synthesis; we won't compile this branch
            crate::inference_policy::InferencePolicySpec {
                applies_to: spec.applies_to.clone(),
                ..Default::default()
            },
            None,
            Some((
                "InvalidSpec",
                "spec.bundleRef is mutually exclusive with spec.tokenBudget, \
                 spec.contentSafety, spec.modelPreference, and spec.displayName"
                    .into(),
            )),
        );
    }

    if !bundle_set {
        // Inline-only or selector-only: pass spec verbatim.
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
                crate::policy_canonical::inference::InferenceKind,
            >(&bundle_ref, &cfg)
            .await
        }
        crate::signer_policy::SignerPolicyState::Malformed(msg) => Err(
            crate::policy_fetcher::FetchError::SignerPolicyMalformed(msg),
        ),
        crate::signer_policy::SignerPolicyState::Absent => {
            let cfg = crate::policy_fetcher::SignerPolicyConfig::from_env();
            crate::policy_fetcher::fetch_and_verify_generic::<
                crate::policy_canonical::inference::InferenceKind,
            >(&bundle_ref, &cfg)
            .await
        }
    };

    match verify_result {
        Ok(verified) => {
            let effective = merge_bundle_with_selector(&spec.applies_to, &verified);
            (effective, Some(verified.digest), None)
        }
        Err(e) => {
            let (reason, msg) = fetch_error_to_degraded(&e);
            tracing::warn!(
                inferencepolicy = %policy.name_any(),
                registry = %bundle_ref.registry,
                repository = %bundle_ref.repository,
                digest = %bundle_ref.digest,
                reason,
                "InferencePolicy bundleRef fetch/verify failed: {msg}"
            );
            (
                crate::inference_policy::InferencePolicySpec {
                    applies_to: spec.applies_to.clone(),
                    ..Default::default()
                },
                None,
                Some((reason, msg)),
            )
        }
    }
}

/// Merge the verified-bundle content fields onto the CR's selector to
/// produce the effective spec for [`compile_to_profile`]. The bundle
/// owns the content; the CR owns the selector.
fn merge_bundle_with_selector(
    applies_to: &crate::inference_policy::InferenceAppliesTo,
    verified: &crate::policy_canonical::inference::VerifiedInferencePolicy,
) -> crate::inference_policy::InferencePolicySpec {
    use crate::inference_policy::{
        ContentSafetyFloor, InferencePolicySpec, ModelPreference, ModelRef, TokenBudget,
    };
    use serde_json::Value;

    let token_budget = verified.token_budget.as_ref().map(|v| TokenBudget {
        per_request_tokens: v.get("perRequestTokens").and_then(Value::as_u64),
        daily_tokens: v.get("dailyTokens").and_then(Value::as_u64),
        monthly_tokens: v.get("monthlyTokens").and_then(Value::as_u64),
    });

    let content_safety = verified
        .content_safety
        .as_ref()
        .map(|v| ContentSafetyFloor {
            hate: v.get("hate").and_then(Value::as_str).map(str::to_owned),
            self_harm: v.get("selfHarm").and_then(Value::as_str).map(str::to_owned),
            sexual: v.get("sexual").and_then(Value::as_str).map(str::to_owned),
            violence: v.get("violence").and_then(Value::as_str).map(str::to_owned),
            require_prompt_shields: v.get("requirePromptShields").and_then(Value::as_bool),
        });

    let model_preference = verified.model_preference.as_ref().and_then(|v| {
        let primary_obj = v.get("primary")?.as_object()?;
        let primary = ModelRef {
            provider: primary_obj
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            deployment: primary_obj
                .get("deployment")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        };
        let fallback = v
            .get("fallback")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let o = e.as_object()?;
                        Some(ModelRef {
                            provider: o.get("provider").and_then(Value::as_str)?.to_owned(),
                            deployment: o.get("deployment").and_then(Value::as_str)?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(ModelPreference { primary, fallback })
    });

    InferencePolicySpec {
        applies_to: applies_to.clone(),
        token_budget,
        content_safety,
        model_preference,
        display_name: verified.display_name.clone(),
        bundle_ref: None,
    }
}

/// Map [`crate::policy_fetcher::FetchError`] variants to the
/// `(reason, message)` pair the controller status machinery consumes.
/// Delegates to [`crate::policy_fetcher::reason_for_error`] so the one
/// shared vocabulary stays in lockstep with the egress + tools
/// reconcilers (one vocabulary across all signed-policy kinds keeps
/// operator-facing docs minimal and the controller's class table
/// closed — §15.3 log-injection prevention).
/// [`crate::policy_fetcher::FetchError::Transient`] is mapped to
/// `"Transient"` here rather than surfaced as a `None` reason: the
/// InferencePolicy state machine does not have a "preserve last-known-
/// good" branch (unlike the egress sandbox reconciler), so a transient
/// fetch failure surfaces honestly as Degraded until the next requeue
/// succeeds.
fn fetch_error_to_degraded(e: &crate::policy_fetcher::FetchError) -> (&'static str, String) {
    let reason = crate::policy_fetcher::reason_for_error(e).unwrap_or("Transient");
    (reason, e.to_string())
}

/// Build the Conditions vector preserving prior `lastTransitionTime`
/// where status hasn't flipped. Always emits `Ready`, `Progressing`,
/// `Degraded`. Driven by `enforcement` once the compile step
/// succeeds — Slice 2a closes the §3 echo loop and replaces the
/// always-`AwaitingRouterEnforcement` branch.
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
                // Used when degraded — never reached here in the
                // success branch. Kept for completeness and
                // build_conditions truth-table symmetry with
                // tool_policy_reconciler.
                out.push(conditions::preserve_transition_time(
                    prior_ready,
                    conditions::TYPE_READY,
                    cond_status::TRUE,
                    reason::RECONCILED,
                    "InferencePolicy compiled and published",
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
                         inference-policy digest"
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
                    "no KarsSandbox references this InferencePolicy; nothing to enforce",
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
    canonical_body: &str,
    v_hash: &str,
    compiled_digest: &str,
) -> Result<(), ReconcileError> {
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    // Canonical key: the router-side `inference_policy_loader`
    // reads this filename from the mount directory and sha256s the
    // exact bytes, so it must match
    // `inference_policy_compile::INFERENCE_POLICY_FILENAME`.
    data.insert(INFERENCE_POLICY_FILENAME.into(), canonical_body.into());
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "kars.azure.com/inferencepolicy-version-hash".into(),
        v_hash.into(),
    );
    annotations.insert(
        "kars.azure.com/inference-policy-digest".into(),
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
                ("kars.azure.com/inferencepolicy".into(), owner.into()),
                (
                    "kars.azure.com/artifact".into(),
                    "inference-policy-profile".into(),
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
    api: &Api<InferencePolicy>,
    configmaps: &Api<ConfigMap>,
    policy: &InferencePolicy,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("inferencepolicy-{name}-profile");
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

    let finalizers: Vec<String> = policy
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy","metadata":{"finalizers": finalizers}});
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(inferencepolicy = %name, "InferencePolicyDeleted");
    Ok(Action::await_change())
}

fn error_policy(policy: Arc<InferencePolicy>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("InferencePolicy", error.class());
    tracing::warn!(
        inferencepolicy = %policy.name_any(),
        error_class = error.class(),
        error = %error,
        "InferencePolicy reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

/// Start the controller loop. Non-fatal CRD-missing exit mirrors
/// `pairing_reconciler::run` and the rest of Phase 2's reconcilers.
pub async fn run(client: Client) -> Result<()> {
    let policies: Api<InferencePolicy> = Api::all(client.clone());
    match policies.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("InferencePolicy CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("InferencePolicy CRD not installed — reconciler disabled: {e}");
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
        phase_reporter: PhaseEventReporter::new(client.clone(), "InferencePolicy"),
    });
    // Watch KarsSandbox so a sandbox create/update that references an
    // InferencePolicy triggers immediate reconcile of that policy —
    // without this we'd wait up to 5 min (REQUEUE_OK) for the next
    // periodic resweep to flip NoSandboxesReferencing → RouterEnforcing.
    // Only handles the explicit `spec.inferenceRef.name` direction;
    // pure label-selector-only policies (no sandboxName + no
    // inferenceRef from sandbox) still rely on the periodic resync.
    let sandboxes: Api<crate::crd::KarsSandbox> = Api::all(client);
    Controller::new(policies, watcher::Config::default())
        .watches(
            sandboxes,
            watcher::Config::default(),
            |sb: crate::crd::KarsSandbox| {
                let name = sb.spec.inference_ref.name.clone();
                let ns = sb.namespace().unwrap_or_default();
                if name.is_empty() || ns.is_empty() {
                    return Vec::new().into_iter();
                }
                vec![ObjectRef::<InferencePolicy>::new(&name).within(&ns)].into_iter()
            },
        )
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("InferencePolicy", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("InferencePolicy reconciled {:?}", o),
                Err(e) => tracing::warn!("InferencePolicy reconcile failed: {e:?}"),
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
        assert_eq!(s.len(), 20, "RFC3339 with seconds + Z is 20 chars: {s}");
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
    fn build_conditions_no_referrers_is_compiled_not_ready() {
        // Slice 2a: when no KarsSandbox references this
        // InferencePolicy there's nothing to enforce — Ready=False
        // with reason `NoSandboxesReferencing`, no Degraded.
        let conds = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::NoSandboxesReferencing,
        );
        assert_eq!(conds.len(), 3);
        let types: Vec<&str> = conds.iter().map(|c| c.type_.as_str()).collect();
        assert!(types.contains(&conditions::TYPE_READY));
        assert!(types.contains(&conditions::TYPE_PROGRESSING));
        assert!(types.contains(&conditions::TYPE_DEGRADED));
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
        let degraded = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_DEGRADED)
            .unwrap();
        assert_eq!(degraded.status, cond_status::FALSE);
    }

    #[test]
    fn build_conditions_awaiting_branch_is_ready_false_with_reason() {
        // Slice 2a: router has not yet echoed our digest — keep
        // Progressing=True and surface the awaiting message on
        // Ready.
        let conds = build_conditions(
            &[],
            Some(1),
            None,
            &RouterEnforcementState::Awaiting {
                total: 2,
                matched: 1,
                message: "1/2 routers confirmed".into(),
            },
        );
        let ready = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_READY)
            .unwrap();
        assert_eq!(ready.status, cond_status::FALSE);
        assert_eq!(ready.reason, reason::AWAITING_ROUTER_ENFORCEMENT);
        assert!(ready.message.contains("1/2"));
        let progressing = conds
            .iter()
            .find(|c| c.type_ == conditions::TYPE_PROGRESSING)
            .unwrap();
        assert_eq!(progressing.status, cond_status::TRUE);
    }

    #[test]
    fn build_conditions_confirmed_is_ready_true_router_enforcing() {
        // Slice 2a: every referring router echoes the digest — this
        // is the only path that promotes Ready=True. Reason must be
        // `RouterEnforcing`, never the legacy `Reconciled` (which
        // historically meant "controller stamped it" — Slice 2a
        // redirects that meaning to the router echo).
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
        assert!(ready.message.contains("3"));
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
        assert_eq!(
            r1.last_transition_time, r2.last_transition_time,
            "transition time must not move when status stays True"
        );
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
        // Distinct from S1 / S2 / S3 — required by §10.4 #1.
        assert_eq!(FIELD_MANAGER, "kars-controller/inferencepolicy");
        assert_ne!(FIELD_MANAGER, "kars-controller/mcp");
        assert_ne!(FIELD_MANAGER, "kars-controller/toolpolicy");
        assert_ne!(FIELD_MANAGER, "kars-controller/a2aagent");
    }
}
