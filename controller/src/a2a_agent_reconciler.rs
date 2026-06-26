// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `A2AAgent` reconciler — Phase 2 §8 entry 4 (S3).
//!
//! Watches `A2AAgent` CRs and, for each:
//!
//! 1. Ensures finalizer (`kars.azure.com/a2aagent-cleanup`) so the
//!    published AgentCard ConfigMap is cleaned up synchronously on
//!    delete.
//! 2. Runs the pure compile step
//!    [`crate::a2a_agent_compile::compile_agent_card`] to produce the
//!    A2A 1.2 AgentCard JSON the router will serve from
//!    `/.well-known/agent.json` once S7 wires the mount.
//! 3. Persists the card as a `ConfigMap` (`a2aagent-{name}-card`, key
//!    `agent.json`), labelled for router-pod selection, annotated with
//!    the version hash for change detection.
//! 4. Sets `status.observedGeneration`, `status.phase`,
//!    `status.conditions[]`, `status.agentCardConfigMapRef`,
//!    `status.versionHash`, `status.lastCompiledAt`.
//!
//! ## Reuse map (no-duplication rule, §0.2/§0.3)
//!
//! - **Conditions vocabulary + transition-time helpers**:
//!   [`crate::status::conditions`].
//! - **Reconciler shape** (Controller::new + non-fatal CRD-missing
//!   exit): mirrors [`crate::tool_policy_reconciler`] (S2) and
//!   [`crate::mcp_server_reconciler`] (S1).
//! - **`LocalObjectRef`**: re-used from [`crate::mcp_server`] via the
//!   already-public type — single struct, three semantic clients
//!   (S1 `signingKeyRef` / `jwksConfigMapRef`, S2 `profileConfigMapRef`,
//!   S3 `agentCardConfigMapRef`).
//! - **Compile**: single-purpose [`crate::a2a_agent_compile`]
//!   module — the reconciler does no JSON shaping itself.
//! - **Wire-format card**: matches
//!   [`inference-router::a2a::agent_projection::A2aAgentSpec`] verbatim
//!   on the `signingKeys` shape, so the trust-store rebuild
//!   orchestrator (S7) ingests the same bytes the router serves.

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

use crate::a2a_agent::{A2AAgent, A2AAgentStatus};
use crate::a2a_agent_compile::{compile_agent_card, version_hash};
use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason, status as cond_status};
use crate::status::phase::{PHASE_DEGRADED, PHASE_READY};

/// Field manager for SSA patches emitted by this reconciler. Distinct
/// from S1 `…/mcp` and S2 `…/toolpolicy` per §10.4 #1 — surfaces
/// out-of-band tampering.
const FIELD_MANAGER: &str = crate::field_managers::A2A_AGENT;

/// Finalizer name (DNS subdomain).
const FINALIZER: &str = "kars.azure.com/a2aagent-cleanup";

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

async fn reconcile(agent: Arc<A2AAgent>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = agent.name_any();
    let ns = agent.namespace().unwrap_or_else(|| "default".into());
    tracing::info!(a2aagent = %name, ns = %ns, "Reconciling A2AAgent");

    let api: Api<A2AAgent> = Api::namespaced(ctx.client.clone(), &ns);
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);

    // Deletion path — finalizer-cascading cleanup.
    if agent.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &configmaps, &agent, &name).await;
    }

    // Add finalizer if missing.
    if !agent
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"A2AAgent","metadata":{"finalizers":[FINALIZER]}});
        api.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_conditions = agent
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let observed_generation = agent.metadata.generation;

    // 1. Compile spec → AgentCard JSON.
    let card = compile_agent_card(&agent.spec, &ns, &name);
    let v_hash = version_hash(&card);

    // 2. Persist as ConfigMap.
    let cm_name = format!("a2aagent-{name}-card");
    let mut degraded: Option<(&'static str, String)> = None;

    match ensure_card_configmap(&configmaps, &cm_name, &name, &card, &v_hash).await {
        Ok(()) => {
            tracing::info!(
                a2aagent = %name,
                ns = %ns,
                version_hash = %v_hash,
                generation = observed_generation.unwrap_or(0),
                signing_key_count = agent.spec.signing_keys.len(),
                production_mode = agent.spec.production_mode,
                federation_peer_count = agent.spec.federation.len(),
                has_trust = agent.spec.trust.is_some(),
                has_policy_refs = agent.spec.policy_refs.is_some(),
                "A2AAgentCardCompiled"
            );
        }
        Err(e) => {
            tracing::warn!(
                a2aagent = %name,
                error_class = e.class(),
                "A2AAgentCardWriteFailed"
            );
            degraded = Some(("CardWriteFailed", e.to_string()));
        }
    }

    // 3. Build & write status.
    let new_conditions = build_conditions(
        &prior_conditions,
        observed_generation,
        degraded
            .as_ref()
            .map(|(reason, msg)| (*reason, msg.as_str())),
    );
    let phase = if degraded.is_some() {
        PHASE_DEGRADED
    } else {
        PHASE_READY
    };

    // SSA requires apiVersion + kind in the patch body — without
    // them, the API server returns "invalid object type: /, Kind=".
    let status_patch = json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "A2AAgent",
        "status": A2AAgentStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            agent_card_config_map_ref: Some(LocalObjectRef { name: cm_name.clone() }),
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
/// `Degraded`. Same shape as S1 / S2.
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
                "AgentCard compiled and published",
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

async fn ensure_card_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &str,
    card: &serde_json::Value,
    v_hash: &str,
) -> Result<(), ReconcileError> {
    let json_str = serde_json::to_string_pretty(card)?;
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("agent.json".into(), json_str);
    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert("kars.azure.com/a2aagent-version-hash".into(), v_hash.into());
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.into()),
            annotations: Some(annotations),
            labels: Some(BTreeMap::from([
                (
                    "app.kubernetes.io/managed-by".into(),
                    "kars-controller".into(),
                ),
                ("kars.azure.com/a2aagent".into(), owner.into()),
                ("kars.azure.com/artifact".into(), "agent-card".into()),
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
    api: &Api<A2AAgent>,
    configmaps: &Api<ConfigMap>,
    agent: &A2AAgent,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("a2aagent-{name}-card");
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

    let finalizers: Vec<String> = agent
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"A2AAgent","metadata":{"finalizers": finalizers}});
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(a2aagent = %name, "A2AAgentDeleted");
    Ok(Action::await_change())
}

fn error_policy(agent: Arc<A2AAgent>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("A2AAgent", error.class());
    tracing::warn!(
        a2aagent = %agent.name_any(),
        error_class = error.class(),
        error = %error,
        "A2AAgent reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

/// Start the controller loop. Non-fatal CRD-missing exit mirrors
/// `pairing_reconciler::run`, `mcp_server_reconciler::run`, and
/// `tool_policy_reconciler::run`.
pub async fn run(client: Client) -> Result<()> {
    let agents: Api<A2AAgent> = Api::all(client.clone());
    match agents.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("A2AAgent CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("A2AAgent CRD not installed — reconciler disabled: {e}");
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
    let ctx = Arc::new(Ctx { client });
    Controller::new(agents, crate::watch_config::bounded())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("A2AAgent", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("A2AAgent reconciled {:?}", o),
                Err(e) => tracing::warn!("A2AAgent reconcile failed: {e:?}"),
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

        // Compile-time exhaustiveness: every variant must produce one of
        // the closed-set strings.
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
        let conds = build_conditions(&[], Some(1), Some(("CardWriteFailed", "boom")));
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
        assert_eq!(degraded.reason, "CardWriteFailed");
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
        assert_eq!(domain, "kars.azure.com");
        assert!(!key.is_empty());
    }

    #[test]
    fn field_manager_is_per_reconciler() {
        // Distinct from S1 / S2 — required by §10.4 #1.
        assert_eq!(FIELD_MANAGER, "kars-controller/a2aagent");
        assert_ne!(FIELD_MANAGER, "kars-controller/mcp");
        assert_ne!(FIELD_MANAGER, "kars-controller/toolpolicy");
    }
}
