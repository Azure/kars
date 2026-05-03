// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `TrustGraph` reconciler — Phase F1.
//!
//! Mirrors [`crate::a2a_agent_reconciler`] in shape:
//!
//! 1. Adds the `azureclaw.azure.com/trustgraph-cleanup` finalizer.
//! 2. Calls the pure compile step
//!    [`crate::trust_graph_compile::compile_trust_graph`] to verify
//!    every vertex public key and every edge signature.
//! 3. Persists the verified subset as a `ConfigMap`
//!    `trustgraph-{name}-projection` in the
//!    [`PROJECTION_NAMESPACE`] (`azureclaw-system`). Cluster-scoped
//!    CR → namespaced projection, so ownerRef cascade is **not**
//!    available — the finalizer is the only correct cleanup
//!    mechanism.
//! 4. Patches `status.conditions[Ready,Progressing,Degraded]`,
//!    `status.{validVertices,validEdges,invalidEdges}`,
//!    `status.projectionConfigMapRef`, `status.observedGeneration`,
//!    `status.lastReconciledAt`.
//!
//! ## Reuse map (no-duplication rule)
//!
//! - Conditions vocabulary + transition-time helpers:
//!   [`crate::status::conditions`].
//! - Reconciler shape: [`crate::a2a_agent_reconciler`] (S3) and
//!   [`crate::tool_policy_reconciler`] (S2).
//! - `LocalObjectRef`: re-used from [`crate::mcp_server`].

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

use crate::mcp_server::LocalObjectRef;
use crate::status::conditions::{self, reason, status as cond_status};
use crate::trust_graph::{TrustGraph, TrustGraphStatus};
use crate::trust_graph_compile::{CompileResult, compile_trust_graph};

const FIELD_MANAGER: &str = crate::field_managers::TRUST_GRAPH;
const FINALIZER: &str = "azureclaw.azure.com/trustgraph-cleanup";
/// Where the verified-graph ConfigMap lives. `azureclaw-system` is
/// the cluster-control-plane namespace already used by other
/// controller-owned artefacts (see Helm chart). Pinned because the
/// router (Phase F2) mounts the same path.
pub const PROJECTION_NAMESPACE: &str = "azureclaw-system";

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

async fn reconcile(tg: Arc<TrustGraph>, ctx: Arc<Ctx>) -> Result<Action, ReconcileError> {
    let name = tg.name_any();
    tracing::info!(trustgraph = %name, "Reconciling TrustGraph");

    let api: Api<TrustGraph> = Api::all(ctx.client.clone());
    let configmaps: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), PROJECTION_NAMESPACE);

    if tg.metadata.deletion_timestamp.is_some() {
        return finalize(&api, &configmaps, &tg, &name).await;
    }

    if !tg
        .metadata
        .finalizers
        .as_ref()
        .map(|f| f.iter().any(|s| s == FINALIZER))
        .unwrap_or(false)
    {
        let patch = json!({
            "apiVersion": "azureclaw.azure.com/v1alpha1",
            "kind": "TrustGraph",
            "metadata": { "finalizers": [FINALIZER] }
        });
        api.patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(patch),
        )
        .await?;
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    let prior_conditions = tg
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();
    let observed_generation = tg.metadata.generation;

    let CompileResult {
        projection,
        vertex_rejects,
        edge_rejects,
    } = compile_trust_graph(&tg.spec);

    let cm_name = format!("trustgraph-{name}-projection");
    let mut degraded: Option<(&'static str, String)> = None;

    if let Err(e) = ensure_projection_configmap(&configmaps, &cm_name, &name, &projection).await {
        tracing::warn!(
            trustgraph = %name,
            error_class = e.class(),
            "TrustGraphProjectionWriteFailed"
        );
        degraded = Some(("ProjectionWriteFailed", e.to_string()));
    } else {
        tracing::info!(
            trustgraph = %name,
            valid_vertices = projection.vertices.len(),
            valid_edges = projection.edges.len(),
            invalid_edges = edge_rejects.len(),
            invalid_vertices = vertex_rejects.len(),
            version_hash = %projection.version_hash,
            generation = observed_generation.unwrap_or(0),
            "TrustGraphCompiled"
        );
        for (i, why) in &edge_rejects {
            // index is operator-supplied position; reason is closed set.
            tracing::info!(
                trustgraph = %name,
                edge_index = i,
                reject_reason = why.as_str(),
                "TrustGraphEdgeRejected"
            );
        }
    }

    let invalid_total = edge_rejects.len();
    let degraded_for_cond = if let Some((r, m)) = degraded.as_ref() {
        Some((*r, m.as_str()))
    } else if invalid_total > 0 {
        // Operator-input invalid edges: surface as Ready=True +
        // Degraded=False with a non-empty status counter, matching the
        // convention that "Degraded" is reserved for controller-side
        // failure (e.g. apiserver write). Invalid edges are user data
        // problems, not controller failures.
        None
    } else {
        None
    };

    let new_conditions =
        build_conditions(&prior_conditions, observed_generation, degraded_for_cond);
    let phase = if degraded.is_some() {
        "Degraded"
    } else {
        "Ready"
    };

    let status_patch = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "TrustGraph",
        "status": TrustGraphStatus {
            phase: Some(phase.into()),
            observed_generation,
            conditions: Some(new_conditions),
            valid_vertices: Some(projection.vertices.len() as i64),
            valid_edges: Some(projection.edges.len() as i64),
            invalid_edges: Some(invalid_total as i64),
            projection_config_map_ref: Some(LocalObjectRef { name: cm_name.clone() }),
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
                "TrustGraph compiled and projection published",
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

async fn ensure_projection_configmap(
    api: &Api<ConfigMap>,
    cm_name: &str,
    owner: &str,
    projection: &crate::trust_graph_compile::ProjectedGraph,
) -> Result<(), ReconcileError> {
    let json_str = serde_json::to_string_pretty(projection)?;
    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("graph.json".into(), json_str);

    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(
        "azureclaw.azure.com/trustgraph-version-hash".into(),
        projection.version_hash.clone(),
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
                ("azureclaw.azure.com/trustgraph".into(), owner.into()),
                (
                    "azureclaw.azure.com/artifact".into(),
                    "trustgraph-projection".into(),
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
    api: &Api<TrustGraph>,
    configmaps: &Api<ConfigMap>,
    tg: &TrustGraph,
    name: &str,
) -> Result<Action, ReconcileError> {
    let cm_name = format!("trustgraph-{name}-projection");
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

    let finalizers: Vec<String> = tg
        .metadata
        .finalizers
        .as_ref()
        .map(|v| v.iter().filter(|f| *f != FINALIZER).cloned().collect())
        .unwrap_or_default();
    let patch = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "TrustGraph",
        "metadata": { "finalizers": finalizers }
    });
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(patch),
    )
    .await?;
    tracing::info!(trustgraph = %name, "TrustGraphDeleted");
    Ok(Action::await_change())
}

fn error_policy(tg: Arc<TrustGraph>, error: &ReconcileError, _ctx: Arc<Ctx>) -> Action {
    crate::metrics::record_reconcile_error("TrustGraph", error.class());
    tracing::warn!(
        trustgraph = %tg.name_any(),
        error_class = error.class(),
        error = %error,
        "TrustGraph reconcile error — requeuing in ~30s (±20% jitter)"
    );
    Action::requeue(crate::backoff::requeue_secs_with_jitter(30))
}

pub async fn run(client: Client) -> Result<()> {
    let graphs: Api<TrustGraph> = Api::all(client.clone());
    match graphs.list(&ListParams::default().limit(1)).await {
        Ok(_) => tracing::info!("TrustGraph CRD found — starting controller"),
        Err(e) => {
            tracing::warn!("TrustGraph CRD not installed — reconciler disabled: {e}");
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            return Ok(());
        }
    }
    let ctx = Arc::new(Ctx { client });
    Controller::new(graphs, kube::runtime::watcher::Config::default())
        .run(
            |x, ctx| async move {
                crate::metrics::observe_reconcile("TrustGraph", reconcile(x, ctx)).await
            },
            error_policy,
            ctx,
        )
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::debug!("TrustGraph reconciled {:?}", o),
                Err(e) => tracing::warn!("TrustGraph reconcile failed: {e:?}"),
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
    }

    #[test]
    fn build_conditions_ok_path_emits_three_conditions() {
        let out = build_conditions(&[], Some(1), None);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].type_, conditions::TYPE_READY);
        assert_eq!(out[0].status, "True");
        assert_eq!(out[1].type_, conditions::TYPE_PROGRESSING);
        assert_eq!(out[1].status, "False");
        assert_eq!(out[2].type_, conditions::TYPE_DEGRADED);
        assert_eq!(out[2].status, "False");
    }

    #[test]
    fn build_conditions_degraded_path_flips_ready() {
        let out = build_conditions(&[], Some(1), Some(("WriteFailed", "boom")));
        assert_eq!(out[0].status, "False");
        assert_eq!(out[0].reason, "WriteFailed");
        assert_eq!(out[2].status, "True");
    }

    #[test]
    fn projection_namespace_is_pinned() {
        // The router (Phase F2) mounts a ConfigMap from this namespace.
        // Changing it requires a coordinated rollout — guard with a test
        // so casual edits surface in review.
        assert_eq!(PROJECTION_NAMESPACE, "azureclaw-system");
    }

    #[test]
    fn finalizer_string_is_pinned() {
        // Finalizer string is part of the on-cluster contract — once
        // set on a CR, only a controller using the same string will
        // remove it. Guard against drift.
        assert_eq!(FINALIZER, "azureclaw.azure.com/trustgraph-cleanup");
    }
}
