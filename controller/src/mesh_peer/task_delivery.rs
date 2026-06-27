// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Harness-neutral mesh task delivery.
//!
//! The controller is already a first-class mesh peer (it pairs, offloads, and
//! exchanges federation messages). This module adds one neutral capability on
//! top of that substrate: deliver a governed *task* straight into a running
//! agent's native loop over the mesh, and capture the agent's reply.
//!
//! It is a CORE kars capability — useful on any plain cluster — triggered
//! purely declaratively so downstream products consume it without core ever
//! depending on them. The trigger is a KarsTask annotation:
//!
//! - `kars.azure.com/run-requested: <nonce>` — set by whoever wants the agent
//!   to execute its objective now (the Bridge BFF, a `kubectl annotate`, …).
//! - `kars.azure.com/run-completed: <nonce>` — written back by the controller
//!   once the agent has replied (or the delivery timed out).
//!
//! The run result is persisted to the `kars-mission-output-<task>` ConfigMap —
//! the same durable artifact record the rest of the system already reads — so
//! no new surface is required to observe the deliverable.
//!
//! Everything here is additive: with no run-request annotation present, the
//! watcher lists, finds nothing, and sleeps. Original controller behaviour is
//! untouched.

use super::{
    DEFAULT_REGISTRY_URL, FederationMessage, MeshPeerState, enqueue_outbound, is_lease_holder,
};
use anyhow::{Context, Result};
use chrono::Utc;
use kube::api::{Api, DynamicObject, ListParams, Patch, PatchParams};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::time::Duration;

const RUN_REQUESTED_ANNOTATION: &str = "kars.azure.com/run-requested";
const RUN_COMPLETED_ANNOTATION: &str = "kars.azure.com/run-completed";
/// How long to wait for the agent's `task_response` before recording a timeout.
/// The native agent loop (tools + delegation) can take a while; this matches
/// the order of magnitude of the offload watchers' patience.
const TASK_TIMEOUT_SECS: u64 = 180;
const POLL_INTERVAL_SECS: u64 = 5;

/// Process-local set of KarsTasks currently being delivered, so the 5s poll
/// loop never double-dispatches a task whose delivery is still in flight (a
/// delivery can take up to `TASK_TIMEOUT_SECS`). Single-leader, so a plain
/// in-memory guard is sufficient and avoids annotation churn.
fn inflight() -> &'static StdMutex<HashSet<String>> {
    static INFLIGHT: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();
    INFLIGHT.get_or_init(|| StdMutex::new(HashSet::new()))
}

fn karstask_api(state: &MeshPeerState) -> Api<DynamicObject> {
    let api_resource = kube::api::ApiResource {
        group: "kars.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsTask".into(),
        plural: "karstasks".into(),
    };
    Api::all_with(state.client.clone(), &api_resource)
}

/// Long-lived poll loop. Watches every KarsTask in the cluster for a pending
/// run-request and dispatches delivery. Gates on lease ownership so only the
/// mesh-peer leader drives delivery.
pub(super) async fn watch_run_requests(state: Arc<MeshPeerState>) {
    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    loop {
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
        if !is_lease_holder(&state.client, &namespace).await {
            continue;
        }
        let api = karstask_api(&state);
        let list = match api.list(&ListParams::default()).await {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(err = %e, "task-delivery: KarsTask list failed");
                continue;
            }
        };
        for task in list {
            let annotations = task.metadata.annotations.clone().unwrap_or_default();
            let requested = match annotations.get(RUN_REQUESTED_ANNOTATION) {
                Some(v) if !v.is_empty() => v.clone(),
                _ => continue,
            };
            let completed = annotations
                .get(RUN_COMPLETED_ANNOTATION)
                .cloned()
                .unwrap_or_default();
            if requested == completed {
                continue;
            }
            let name = task.metadata.name.clone().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            // Claim this task for the life of the delivery so the next poll
            // tick doesn't re-dispatch it.
            if !inflight()
                .lock()
                .expect("inflight poisoned")
                .insert(name.clone())
            {
                continue;
            }
            let state = state.clone();
            tokio::spawn(async move {
                let result = deliver_for_task(&state, &task, &requested).await;
                inflight().lock().expect("inflight poisoned").remove(&name);
                if let Err(e) = result {
                    tracing::warn!(task = %name, err = %format!("{e:#}"), "mesh task delivery failed");
                }
            });
        }
    }
}

/// Deliver one KarsTask's objective to its running agent over the mesh and
/// persist the reply.
async fn deliver_for_task(
    state: &Arc<MeshPeerState>,
    task: &DynamicObject,
    nonce: &str,
) -> Result<()> {
    let name = task.metadata.name.clone().unwrap_or_default();
    let namespace = task
        .metadata
        .namespace
        .clone()
        .unwrap_or_else(|| "kars-system".into());

    let objective = task
        .data
        .get("spec")
        .and_then(|s| s.get("objective"))
        .and_then(|o| o.as_str())
        .map(str::to_string)
        .context("KarsTask has no spec.objective")?;

    let sandbox = task
        .data
        .get("status")
        .and_then(|s| s.get("sandboxRef"))
        .and_then(|r| r.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
        .context("KarsTask has no status.sandboxRef.name — not launched yet")?;

    tracing::info!(task = %name, sandbox = %sandbox, "task-delivery: dispatching objective over mesh");

    // Discover the running agent's mesh DID from the registry. The runtime
    // adapter registers under the sandbox name as a capability — harness
    // neutral, same discovery the Bridge BFF uses.
    let agent_did = discover_agent_did(&sandbox)
        .await
        .context("agent not discoverable on the mesh registry (is the sandbox Ready?)")?;

    // Register a waiter keyed by the agent DID *before* sending, so a fast
    // reply can't race ahead of the registration.
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    state
        .pending_tasks
        .lock()
        .await
        .insert(agent_did.clone(), tx);

    let epoch = state.leader_epoch.load(Ordering::Acquire);
    let send_result = enqueue_outbound(
        state,
        epoch,
        &agent_did,
        FederationMessage::TaskRequest {
            content: objective.clone(),
            request_id: Some(nonce.to_string()),
            timestamp: Some(Utc::now().to_rfc3339()),
        },
    );
    if let Err(e) = send_result {
        state.pending_tasks.lock().await.remove(&agent_did);
        return Err(e).context("failed to enqueue task_request");
    }

    // Await the agent's task_response (or time out).
    let (content, ok) = match tokio::time::timeout(Duration::from_secs(TASK_TIMEOUT_SECS), rx).await
    {
        Ok(Ok(reply)) => (reply, true),
        Ok(Err(_)) => (
            "mesh task delivery channel closed before a reply arrived".to_string(),
            false,
        ),
        Err(_) => {
            // Drop the stale waiter so a late reply isn't misattributed.
            state.pending_tasks.lock().await.remove(&agent_did);
            (
                format!(
                    "timed out after {TASK_TIMEOUT_SECS}s waiting for the agent's task_response"
                ),
                false,
            )
        }
    };

    write_mission_output(state, &name, &objective, &content, ok).await?;
    mark_completed(state, &namespace, &name, nonce).await?;

    tracing::info!(
        task = %name,
        ok,
        len = content.len(),
        "task-delivery: persisted mesh run result"
    );
    Ok(())
}

/// Resolve an in-flight delivery when the matching `task_response` arrives.
/// Correlation is by the responding agent's DID (`from_amid`): in this flow an
/// agent runs one delivered task at a time, so the first reply from that DID
/// belongs to the outstanding request.
pub(super) async fn resolve_pending(state: &Arc<MeshPeerState>, from_amid: &str, content: String) {
    let waiter = state.pending_tasks.lock().await.remove(from_amid);
    match waiter {
        Some(tx) => {
            if tx.send(content).is_err() {
                tracing::debug!(from = %from_amid, "task_response arrived after the waiter was dropped");
            }
        }
        None => {
            tracing::debug!(from = %from_amid, "task_response with no pending delivery — ignoring");
        }
    }
}

/// Query the AGT registry for the agent registered under `sandbox` and return
/// its mesh DID (most-recently-seen wins). In-cluster, so the registry service
/// URL is reached directly (no Kubernetes proxy hop).
async fn discover_agent_did(sandbox: &str) -> Option<String> {
    let base =
        std::env::var("MESH_REGISTRY_URL").unwrap_or_else(|_| DEFAULT_REGISTRY_URL.to_string());
    let base = base.trim_end_matches('/');
    let url = format!("{base}/v1/discover?capability={sandbox}&limit=10");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        tracing::debug!(sandbox = %sandbox, status = %resp.status(), "registry discover non-200");
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let results = body.get("results")?.as_array()?;

    let mut best: Option<(String, String)> = None; // (did, last_seen)
    for r in results {
        let Some(did) = r.get("did").and_then(|v| v.as_str()) else {
            continue;
        };
        let last_seen = r
            .get("last_seen")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match &best {
            Some((_, best_seen)) if *best_seen >= last_seen => {}
            _ => best = Some((did.to_string(), last_seen)),
        }
    }
    best.map(|(did, _)| did)
}

/// Persist the run result to `kars-mission-output-<task>` in the controller's
/// namespace — the same durable ConfigMap the rest of the system reads as the
/// mission deliverable. Server-side apply, idempotent per task.
async fn write_mission_output(
    state: &Arc<MeshPeerState>,
    task: &str,
    objective: &str,
    output: &str,
    ok: bool,
) -> Result<()> {
    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let cms: Api<k8s_openapi::api::core::v1::ConfigMap> =
        Api::namespaced(state.client.clone(), &namespace);
    let name = format!("kars-mission-output-{task}");

    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("output".into(), output.to_string());
    data.insert("objective".into(), objective.to_string());
    data.insert("finishedAt".into(), Utc::now().to_rfc3339());
    // Distinguishes the agent-loop deliverable (tools + delegation over the
    // mesh) from the single-turn router path, and records success/failure.
    data.insert("source".into(), "mesh-task".into());
    data.insert(
        "status".into(),
        if ok { "ok".into() } else { "error".into() },
    );

    let patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": name, "labels": { "kars.azure.com/mission-output": task } },
        "data": data,
    });
    cms.patch(
        &name,
        &PatchParams::apply(crate::field_managers::MESH_PEER).force(),
        &Patch::Apply(patch),
    )
    .await
    .context("write mission-output ConfigMap")?;
    Ok(())
}

/// Stamp `kars.azure.com/run-completed: <nonce>` so the watcher treats this
/// run-request as satisfied and won't re-dispatch it.
async fn mark_completed(
    state: &Arc<MeshPeerState>,
    namespace: &str,
    task: &str,
    nonce: &str,
) -> Result<()> {
    let api_resource = kube::api::ApiResource {
        group: "kars.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsTask".into(),
        plural: "karstasks".into(),
    };
    let api: Api<DynamicObject> =
        Api::namespaced_with(state.client.clone(), namespace, &api_resource);
    let patch = json!({
        "metadata": {
            "annotations": { RUN_COMPLETED_ANNOTATION: nonce }
        }
    });
    api.patch(
        task,
        &PatchParams::apply(crate::field_managers::MESH_PEER),
        &Patch::Merge(patch),
    )
    .await
    .context("annotate run-completed")?;
    Ok(())
}
