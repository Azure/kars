//! `InferencePolicy` reconciler — Phase 2 §8 entry 4 (S4).
//!
//! Watches `InferencePolicy` CRs and, for each:
//!
//! 1. Ensures finalizer (`azureclaw.azure.com/inferencepolicy-cleanup`)
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
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::inference_policy::{InferencePolicy, InferencePolicyStatus};
use crate::inference_policy_compile::{compile_to_profile, version_hash};
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason, status as cond_status};

/// Field manager for SSA patches emitted by this reconciler. Distinct
/// from S1 `…/mcp`, S2 `…/toolpolicy`, S3 `…/a2aagent` per §10.4 #1
/// — surfaces out-of-band tampering.
const FIELD_MANAGER: &str = crate::field_managers::INFERENCE_POLICY;

/// Finalizer name (DNS subdomain).
const FINALIZER: &str = "azureclaw.azure.com/inferencepolicy-cleanup";

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
        let patch = json!({"metadata":{"finalizers":[FINALIZER]}});
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

    let profile = compile_to_profile(&policy.spec);
    let v_hash = version_hash(&profile);

    let cm_name = format!("inferencepolicy-{name}-profile");
    let mut degraded: Option<(&'static str, String)> = None;

    match ensure_profile_configmap(&configmaps, &cm_name, &name, &profile, &v_hash).await {
        Ok(()) => {
            tracing::info!(
                inferencepolicy = %name,
                ns = %ns,
                version_hash = %v_hash,
                generation = observed_generation.unwrap_or(0),
                has_token_budget = policy.spec.token_budget.is_some(),
                has_content_safety = policy.spec.content_safety.is_some(),
                has_model_preference = policy.spec.model_preference.is_some(),
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
        "status": InferencePolicyStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            profile_config_map_ref: Some(LocalObjectRef { name: cm_name.clone() }),
            version_hash: Some(v_hash),
            last_compiled_at: Some(rfc3339_now()),
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

/// Build the Conditions vector preserving prior `lastTransitionTime`
/// where status hasn't flipped. Always emits `Ready`, `Progressing`,
/// `Degraded`. Same shape as S1 / S2 / S3.
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
        None => {
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
    }
    out
}

async fn ensure_profile_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &str,
    profile: &serde_json::Value,
    v_hash: &str,
) -> Result<(), ReconcileError> {
    let json_str = serde_json::to_string_pretty(profile)?;
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("profile.json".into(), json_str);
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "azureclaw.azure.com/inferencepolicy-version-hash".into(),
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
                ("azureclaw.azure.com/inferencepolicy".into(), owner.into()),
                (
                    "azureclaw.azure.com/artifact".into(),
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
    let patch = json!({"metadata":{"finalizers": finalizers}});
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
    tracing::warn!(
        inferencepolicy = %policy.name_any(),
        error_class = error.class(),
        error = %error,
        "InferencePolicy reconcile error — requeuing in 30s"
    );
    Action::requeue(Duration::from_secs(30))
}

/// Start the controller loop. Non-fatal CRD-missing exit mirrors
/// `pairing_reconciler::run` and the rest of Phase 2's reconcilers.
pub async fn run(client: Client) -> Result<()> {
    let policies: Api<InferencePolicy> = Api::all(client.clone());
    match policies.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("InferencePolicy CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("InferencePolicy CRD not installed — reconciler disabled: {e}");
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(policies, kube::runtime::watcher::Config::default())
        .run(reconcile, error_policy, ctx)
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
    fn build_conditions_emits_all_three_types_on_success() {
        let conds = build_conditions(&[], Some(1), None);
        assert_eq!(conds.len(), 3);
        let types: Vec<&str> = conds.iter().map(|c| c.type_.as_str()).collect();
        assert!(types.contains(&conditions::TYPE_READY));
        assert!(types.contains(&conditions::TYPE_PROGRESSING));
        assert!(types.contains(&conditions::TYPE_DEGRADED));
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
        let conds = build_conditions(&[], Some(1), Some(("ProfileWriteFailed", "boom")));
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
        assert_eq!(
            r1.last_transition_time, r2.last_transition_time,
            "transition time must not move when status stays True"
        );
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
        // Distinct from S1 / S2 / S3 — required by §10.4 #1.
        assert_eq!(FIELD_MANAGER, "azureclaw-controller/inferencepolicy");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/mcp");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/toolpolicy");
        assert_ne!(FIELD_MANAGER, "azureclaw-controller/a2aagent");
    }
}
