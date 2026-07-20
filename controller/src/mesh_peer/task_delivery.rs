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
    DEFAULT_REGISTRY_URL, FederationMessage, MeshPeerState, ReceivedArtifact, RunTelemetry,
    TaskReply, enqueue_outbound, is_lease_holder,
};
use anyhow::{Context, Result};
use chrono::Utc;
use kube::api::{Api, DynamicObject, ListParams, Patch, PatchParams};
use serde_json::json;
use sha2::Digest;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::time::Duration;

const RUN_REQUESTED_ANNOTATION: &str = "kars.azure.com/run-requested";
const RUN_COMPLETED_ANNOTATION: &str = "kars.azure.com/run-completed";
/// Stamped once this controller (the mesh-peer lease holder) has DISPATCHED the
/// objective to the agent — i.e. it acknowledged and is actively delivering.
/// Lets a client (the Bridge BFF) distinguish "the mesh peer picked this up and
/// is working" from "nobody is processing this" (no lease holder / relay down),
/// so it never silently falls back to a single model turn while the real agent
/// loop is running (which would double-write the deliverable).
const RUN_ACK_ANNOTATION: &str = "kars.azure.com/run-ack";
/// Tracks transient delivery attempts (timeout/unreachable) per run-request, so
/// a run whose agent wasn't ready yet is retried a bounded number of times
/// rather than recorded as a permanent timeout on the first miss.
const RUN_ATTEMPTS_ANNOTATION: &str = "kars.azure.com/run-attempts";
/// A fresh AKS sandbox can take several minutes to pull images and join the mesh.
/// Keep the local/kind default robust while allowing operators to tune the
/// bounded warm-up budget.
fn max_delivery_attempts() -> u32 {
    std::env::var("KARS_MESH_DELIVERY_MAX_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.clamp(1, 360))
        .unwrap_or(72)
}
/// Idle timeout: how long the controller waits with NO signal from the agent
/// (neither a `task_progress` heartbeat nor the terminal `task_response`)
/// before recording a delivery as dead. The native agent loop emits a
/// `task_progress` tick ~every 20s while it is actively working, so any
/// genuinely-progressing run resets this clock long before it elapses; only an
/// agent that has truly gone silent trips it. Terminal-timeout runs are retired
/// (not counted as active), so a slow run never permanently freezes the team's
/// ticks.
fn assignment_lease_ttl_secs() -> i64 {
    std::env::var("KARS_ASSIGNMENT_LEASE_TTL_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .map(|value| value.clamp(30, 300))
        .unwrap_or(90)
}
const POLL_INTERVAL_SECS: u64 = 5;

/// Process-local set of KarsTasks currently being delivered, so the 5s poll
/// loop never double-dispatches a task whose delivery is still in flight.
fn inflight() -> &'static StdMutex<HashSet<String>> {
    static INFLIGHT: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();
    INFLIGHT.get_or_init(|| StdMutex::new(HashSet::new()))
}

/// Outcome of awaiting a single mesh task delivery.
enum DeliveryOutcome {
    /// The agent returned its terminal `task_response`.
    Reply(TaskReply),
    /// The oneshot channel closed before any reply (waiter dropped).
    ChannelClosed,
    /// No `task_progress`/`task_response` before the progress lease expired.
    IdleTimeout,
}

#[derive(Clone)]
pub(super) struct PendingAssignmentProgress {
    pub clock: Arc<AtomicI64>,
    pub namespace: String,
    pub task_name: String,
    pub task_id: String,
}

#[derive(Default)]
pub(super) struct ProgressUpdate {
    pub stage: Option<String>,
    pub child_task_id: Option<String>,
    pub child_role: Option<String>,
    pub message: Option<String>,
}

/// Bump the last-activity clock for the in-flight delivery to `agent_did`,
/// called from the inbound `task_progress` handler. Returns true when a
/// delivery to that DID is currently tracked (the heartbeat was meaningful);
/// false when none is in flight (a late or duplicate tick).
pub(super) async fn touch_progress(
    state: &Arc<MeshPeerState>,
    agent_did: &str,
    update: ProgressUpdate,
) -> bool {
    let pending = state.pending_progress.lock().await.get(agent_did).cloned();
    let Some(pending) = pending else {
        return false;
    };
    pending
        .clock
        .store(Utc::now().timestamp_millis(), Ordering::Release);
    if let Err(error) = persist_assignment_progress(state, &pending, agent_did, update).await {
        tracing::warn!(
            task = %pending.task_name,
            task_id = %pending.task_id,
            error = %format!("{error:#}"),
            "failed to persist assignment progress"
        );
    }
    true
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
            // tick doesn't re-dispatch it. Recover from a poisoned mutex instead
            // of panicking — a panic here aborts the watch task permanently
            // (it is never re-spawned), silently stopping ALL mesh delivery.
            {
                let mut guard = match inflight().lock() {
                    Ok(g) => g,
                    Err(poisoned) => {
                        tracing::error!("inflight mutex poisoned — recovering");
                        poisoned.into_inner()
                    }
                };
                if !guard.insert(name.clone()) {
                    continue;
                }
            }
            let state = state.clone();
            tokio::spawn(async move {
                let result = deliver_for_task(&state, &task, &requested).await;
                match inflight().lock() {
                    Ok(mut g) => {
                        g.remove(&name);
                    }
                    Err(poisoned) => {
                        poisoned.into_inner().remove(&name);
                    }
                }
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

    // The model the run actually used, recorded on the deliverable so the
    // scorecard + efficiency frontier attribute the run's real token cost to a
    // real route. Team taskforce runs inherit the model (blueprint.model empty),
    // so fall back to the controller's effective default — the model the
    // sandbox's inference policy actually resolves to. Never left blank
    // (blank => a useless "unknown" route in the frontier).
    let model = task
        .data
        .get("spec")
        .and_then(|s| s.get("blueprint"))
        .and_then(|b| b.get("model"))
        .and_then(|m| m.get("deployment"))
        .and_then(|d| d.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("KARS_TASK_DEFAULT_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("AZURE_OPENAI_DEPLOYMENT")
                .ok()
                .filter(|s| !s.is_empty())
        })
        // Match the full materialization resolver (kars_task_execution::
        // default_model) so the recorded route is exactly what actually ran.
        .or_else(|| {
            std::env::var("DEFAULT_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| Some("gpt-4o-mini".to_string()));

    // The harness (agent runtime) the run used — mirror the materialization
    // resolver: blueprint.runtime → execution.runtime → OpenClaw. Empty/inherited
    // resolves the same way the sandbox was actually built.
    let harness = task
        .data
        .get("spec")
        .and_then(|s| s.get("blueprint"))
        .and_then(|b| b.get("runtime"))
        .and_then(|r| r.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            task.data
                .get("spec")
                .and_then(|s| s.get("execution"))
                .and_then(|e| e.get("runtime"))
                .and_then(|r| r.as_str())
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or("OpenClaw")
        .to_string();
    let owning_team = task
        .metadata
        .labels
        .as_ref()
        .and_then(|l| l.get("kars.azure.com/team"))
        .or_else(|| {
            task.metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("kars.azure.com/team"))
        })
        .cloned();
    let owner_sub = task
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kars.azure.com/owner-sub"))
        .cloned();
    let owner_name = task
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get("kars.azure.com/owner-name"))
        .cloned();

    let sandbox = task
        .data
        .get("status")
        .and_then(|s| s.get("sandboxRef"))
        .and_then(|r| r.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
        .context("KarsTask has no status.sandboxRef.name — not launched yet")?;

    tracing::info!(task = %name, sandbox = %sandbox, "task-delivery: dispatching objective over mesh");

    // How many transient (not-ready) attempts this run-request has already made.
    let attempts = task
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(RUN_ATTEMPTS_ANNOTATION))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    // Discover the running agent's mesh DID from the registry. The runtime
    // adapter registers under the sandbox name as a capability — harness
    // neutral, same discovery the Bridge BFF uses. A freshly-launched sandbox
    // may not be on the mesh yet; treat that as a transient miss and retry on
    // the next poll until the warm-up budget is exhausted (then record it).
    let agent_did = match discover_agent_did(&sandbox).await {
        Some(did) => did,
        None => {
            return handle_transient_miss(
                state,
                &namespace,
                &name,
                &objective,
                nonce,
                attempts,
                model.as_deref(),
                &harness,
                "agent not yet discoverable on the mesh registry (sandbox still warming up)",
            )
            .await;
        }
    };

    // Register a waiter keyed by the agent DID *before* sending, so a fast
    // reply can't race ahead of the registration.
    let (tx, rx) = tokio::sync::oneshot::channel::<TaskReply>();
    state
        .pending_tasks
        .lock()
        .await
        .insert(agent_did.clone(), tx);
    // Clear any stale artifact buffer for this DID from a prior run.
    state.pending_artifacts.lock().await.remove(&agent_did);
    let last_activity = Arc::new(AtomicI64::new(Utc::now().timestamp_millis()));
    let pending_progress = PendingAssignmentProgress {
        clock: last_activity.clone(),
        namespace: namespace.clone(),
        task_name: name.clone(),
        task_id: nonce.to_string(),
    };
    state
        .pending_progress
        .lock()
        .await
        .insert(agent_did.clone(), pending_progress.clone());
    persist_assignment_transition(
        state,
        &pending_progress,
        &agent_did,
        "assigned",
        "Assigned",
        None,
        None,
    )
    .await?;

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
        state.pending_progress.lock().await.remove(&agent_did);
        return Err(e).context("failed to enqueue task_request");
    }

    // ACK: the objective is dispatched to the agent over the mesh. Stamp it so a
    // client can tell "actively delivering" apart from "never processed" and not
    // race the deliverable with a single-turn fallback. Best-effort — a failed
    // ack annotation must not abort a delivery that already left.
    if let Err(e) = mark_ack(state, &namespace, &name, nonce).await {
        tracing::warn!(task = %name, err = %format!("{e:#}"), "failed to stamp run-ack (delivery continues)");
    }
    persist_assignment_transition(
        state,
        &pending_progress,
        &agent_did,
        "acknowledged",
        "Running",
        Some("assigned"),
        None,
    )
    .await?;

    // Await the agent's task_response, using an IDLE timeout that resets on
    // every `task_progress` heartbeat. The agent ticks ~every 20s while it
    // works, so a long-but-progressing run stays alive (up to the absolute
    // ceiling); only an agent that goes silent for `IDLE_TIMEOUT_SECS` — or one
    // that exceeds `ABS_MAX_SECS` overall — is reaped. Register the activity
    // clock before sending so a fast first heartbeat can't race it.
    let lease_ttl_secs = assignment_lease_ttl_secs();
    let mut rx = rx;
    let outcome = loop {
        match tokio::time::timeout(Duration::from_secs(POLL_INTERVAL_SECS), &mut rx).await {
            Ok(Ok(reply)) => break DeliveryOutcome::Reply(reply),
            Ok(Err(_)) => break DeliveryOutcome::ChannelClosed,
            Err(_) => {
                let idle_ms = Utc::now().timestamp_millis() - last_activity.load(Ordering::Acquire);
                if idle_ms >= lease_ttl_secs * 1000 {
                    break DeliveryOutcome::IdleTimeout;
                }
            }
        }
    };
    // Stop tracking liveness for this delivery regardless of outcome.
    state.pending_progress.lock().await.remove(&agent_did);

    let (content, artifact_count, trace, telemetry, ok) = match outcome {
        DeliveryOutcome::Reply(reply) => (
            reply.content,
            reply.artifact_count,
            reply.trace,
            reply.telemetry,
            reply.ok,
        ),
        DeliveryOutcome::ChannelClosed => (
            "mesh task delivery channel closed before a reply arrived".to_string(),
            0,
            Vec::new(),
            None,
            false,
        ),
        DeliveryOutcome::IdleTimeout => {
            // Drop the stale waiter so a late reply isn't misattributed.
            state.pending_tasks.lock().await.remove(&agent_did);
            (
                format!(
                    "assignment progress lease expired after {lease_ttl_secs}s without renewal"
                ),
                0,
                Vec::new(),
                None,
                false,
            )
        }
    };

    // The artifact `file_transfer` frames are independent relay messages; a few
    // may still be in flight when the task_response lands. Wait briefly for the
    // buffered set to reach the manifest count before flushing.
    let artifacts = drain_artifacts(state, &agent_did, artifact_count).await;
    let deliverable_ok = ok && is_substantive_deliverable(&content);
    persist_assignment_transition(
        state,
        &pending_progress,
        &agent_did,
        if deliverable_ok { "handback" } else { "failed" },
        if deliverable_ok {
            "Completed"
        } else {
            "Failed"
        },
        Some(if deliverable_ok {
            "completed"
        } else {
            "failed"
        }),
        (!deliverable_ok).then_some(content.as_str()),
    )
    .await?;

    let persisted_artifacts = if artifacts.is_empty() {
        std::collections::BTreeSet::new()
    } else {
        match write_mission_artifacts(state, &name, &artifacts).await {
            Ok(names) => names,
            Err(e) => {
                tracing::warn!(task = %name, err = %format!("{e:#}"), "failed to persist mission artifacts");
                std::collections::BTreeSet::new()
            }
        }
    };

    write_mission_output(
        state,
        &name,
        &objective,
        &content,
        deliverable_ok,
        &artifacts,
        &persisted_artifacts,
        artifact_count,
        owning_team.as_deref(),
        owner_sub.as_deref(),
        owner_name.as_deref(),
        telemetry.as_ref(),
        model.as_deref(),
        &harness,
    )
    .await?;
    if !trace.is_empty() {
        // The execution trace is the clean per-tool audit record. Persist it
        // verbatim so it's independently inspectable (kubectl get configmap).
        if let Err(e) = write_mission_trace(state, &name, &trace).await {
            tracing::warn!(task = %name, err = %format!("{e:#}"), "failed to persist execution trace");
        }
    }
    mark_completed(state, &namespace, &name, nonce).await?;

    tracing::info!(
        task = %name,
        ok,
        len = content.len(),
        artifacts = artifacts.len(),
        trace = trace.len(),
        tokens = telemetry.as_ref().map(|t| t.total_tokens).unwrap_or(0),
        "task-delivery: persisted mesh run result"
    );
    Ok(())
}

fn is_substantive_deliverable(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if ["aborted", "cancelled", "canceled", "stopped", "terminated"]
        .iter()
        .any(|status| {
            lower == *status
                || lower.strip_prefix(status).is_some_and(|rest| {
                    rest.starts_with([':', '-', '—'])
                        || rest.chars().next().is_some_and(char::is_whitespace)
                })
        })
    {
        return false;
    }
    let first_meaningful = trimmed
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_start_matches(|c: char| matches!(c, '#' | '*' | '_' | '`' | '-' | ' ' | '\t'));
    ![
        "[[NEEDS_CLARIFICATION]]",
        "[[NEEDS_EGRESS]]",
        "[[NEEDS_TIER]]",
    ]
    .iter()
    .any(|sentinel| {
        first_meaningful.starts_with(sentinel)
            || sentinel.strip_suffix("]]").is_some_and(|open| {
                first_meaningful
                    .strip_prefix(open)
                    .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
            })
    })
}

/// Wait up to a short window for the agent's `file_transfer` frames to land,
/// then take whatever artifacts were buffered for this agent DID. `expected` is
/// the manifest count from the `task_response`; we stop early once it's reached.
async fn drain_artifacts(
    state: &Arc<MeshPeerState>,
    agent_did: &str,
    expected: usize,
) -> Vec<ReceivedArtifact> {
    if expected > 0 {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let have = state
                .pending_artifacts
                .lock()
                .await
                .get(agent_did)
                .map(|v| v.len())
                .unwrap_or(0);
            if have >= expected || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    state
        .pending_artifacts
        .lock()
        .await
        .remove(agent_did)
        .unwrap_or_default()
}

/// Buffer an artifact file received over `file_transfer` under its sender DID.
pub(super) async fn buffer_artifact(
    state: &Arc<MeshPeerState>,
    from_amid: &str,
    name: String,
    source_agent: Option<String>,
    source_path: Option<String>,
    bytes: Vec<u8>,
) {
    state
        .pending_artifacts
        .lock()
        .await
        .entry(from_amid.to_string())
        .or_default()
        .push(ReceivedArtifact {
            name,
            source_agent,
            source_path,
            bytes,
        });
}

/// Resolve an in-flight delivery when the matching `task_response` arrives.
/// Correlation is by the responding agent's DID (`from_amid`): in this flow an
/// agent runs one delivered task at a time, so the first reply from that DID
/// belongs to the outstanding request. `artifact_count` is the manifest length
/// so the waiter knows how many `file_transfer` frames to expect.
pub(super) async fn resolve_pending(
    state: &Arc<MeshPeerState>,
    from_amid: &str,
    task_id: Option<String>,
    content: String,
    artifact_count: usize,
    trace: Vec<serde_json::Value>,
    telemetry: Option<RunTelemetry>,
    ok: bool,
) {
    let waiter = state.pending_tasks.lock().await.remove(from_amid);
    match waiter {
        Some(tx) => {
            if tx
                .send(TaskReply {
                    content,
                    artifact_count,
                    trace,
                    telemetry,
                    ok,
                })
                .is_err()
            {
                tracing::debug!(from = %from_amid, "task_response arrived after the waiter was dropped");
            }
        }
        None => {
            if let Some(task_id) = task_id {
                match find_task_by_nonce(state, &task_id).await {
                    Ok(Some((namespace, task_name))) => {
                        let pending = PendingAssignmentProgress {
                            clock: Arc::new(AtomicI64::new(Utc::now().timestamp_millis())),
                            namespace,
                            task_name: task_name.clone(),
                            task_id: task_id.clone(),
                        };
                        if let Err(error) = persist_assignment_transition(
                            state,
                            &pending,
                            from_amid,
                            if ok { "late_handback" } else { "late_failure" },
                            if ok { "Completed" } else { "Failed" },
                            Some("late_handback"),
                            (!ok).then_some(content.as_str()),
                        )
                        .await
                        {
                            tracing::warn!(
                                task = %task_name,
                                task_id = %task_id,
                                error = %format!("{error:#}"),
                                "late task_response could not update assignment ledger"
                            );
                        } else {
                            tracing::info!(
                                task = %task_name,
                                task_id = %task_id,
                                from = %from_amid,
                                "late task_response reconciled into durable assignment ledger"
                            );
                        }
                    }
                    Ok(None) => {
                        tracing::warn!(
                            task_id = %task_id,
                            from = %from_amid,
                            "late task_response has no matching KarsTask"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            task_id = %task_id,
                            from = %from_amid,
                            error = %format!("{error:#}"),
                            "failed to resolve late task_response"
                        );
                    }
                }
            } else {
                tracing::warn!(
                    from = %from_amid,
                    "task_response with no pending delivery or task correlation"
                );
            }
        }
    }
}

async fn find_task_by_nonce(
    state: &Arc<MeshPeerState>,
    task_id: &str,
) -> Result<Option<(String, String)>> {
    let tasks = karstask_api(state)
        .list(&ListParams::default())
        .await
        .context("list KarsTasks for late handback")?;
    Ok(tasks.into_iter().find_map(|task| {
        let matches = task
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(RUN_REQUESTED_ANNOTATION))
            .is_some_and(|requested| requested == task_id);
        if !matches {
            return None;
        }
        Some((
            task.metadata
                .namespace
                .unwrap_or_else(|| "kars-system".into()),
            task.metadata.name.unwrap_or_default(),
        ))
    }))
}

/// Query the AGT registry for the agent registered under `sandbox` and return
/// its mesh DID (most-recently-seen wins). In-cluster, so the registry service
/// URL is reached directly (no Kubernetes proxy hop).
async fn discover_agent_did(sandbox: &str) -> Option<String> {
    let base =
        std::env::var("MESH_REGISTRY_URL").unwrap_or_else(|_| DEFAULT_REGISTRY_URL.to_string());
    let base = base.trim_end_matches('/');
    // A stable mission/team name accumulates historical DIDs as pods are
    // recycled. The registry returns insertion order, so a small first page can
    // contain only stale identities and exclude the current pod entirely.
    let url = format!("{base}/v1/discover?capability={sandbox}&limit=100");

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
    select_newest_agent_did(body.get("results")?.as_array()?)
}

fn select_newest_agent_did(results: &[serde_json::Value]) -> Option<String> {
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
/// mission deliverable. Server-side apply, idempotent per task. Records the
/// artifact manifest (names + sizes) so the deliverable advertises the full
/// set even when individual files live in the companion artifacts ConfigMap.
#[allow(clippy::too_many_arguments)]
/// ConfigMap `data` values must be valid UTF-8 free of control characters the
/// API server's YAML decoder rejects: C0 (< 0x20, except tab/newline/CR), DEL
/// (0x7F), and C1 (0x80–0x9F). Replace any disallowed control char with a space
/// so the text stays readable.
fn cm_safe(s: &str) -> String {
    s.chars()
        .map(|c| if is_disallowed_control(c) { ' ' } else { c })
        .collect()
}

/// True when `s` carries a control character disallowed in ConfigMap `data`.
fn cm_has_disallowed_control(s: &str) -> bool {
    s.chars().any(is_disallowed_control)
}

/// A control char the K8s ConfigMap `data` YAML decoder rejects: C0 (< 0x20)
/// except tab/newline/CR, DEL (0x7F), and the C1 block (0x80–0x9F).
fn is_disallowed_control(c: char) -> bool {
    if matches!(c, '\t' | '\n' | '\r') {
        return false;
    }
    let u = c as u32;
    u < 0x20 || u == 0x7f || (0x80..=0x9f).contains(&u)
}

async fn write_mission_output(
    state: &Arc<MeshPeerState>,
    task: &str,
    objective: &str,
    output: &str,
    ok: bool,
    artifacts: &[ReceivedArtifact],
    persisted_artifacts: &std::collections::BTreeSet<String>,
    declared_artifact_count: usize,
    owning_team: Option<&str>,
    owner_sub: Option<&str>,
    owner_name: Option<&str>,
    telemetry: Option<&RunTelemetry>,
    model: Option<&str>,
    harness: &str,
) -> Result<()> {
    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let cms: Api<k8s_openapi::api::core::v1::ConfigMap> =
        Api::namespaced(state.client.clone(), &namespace);
    let name = format!("kars-mission-output-{task}");

    let mut data: BTreeMap<String, String> = BTreeMap::new();
    // Free-text fields can carry C0 control characters (the native agent's
    // reply is not control-char-stripped); the K8s API server rejects those in
    // ConfigMap `data`, so make them ConfigMap-safe.
    data.insert("output".into(), cm_safe(output));
    data.insert("objective".into(), cm_safe(objective));
    data.insert("finishedAt".into(), Utc::now().to_rfc3339());
    if let Some(m) = model {
        data.insert("model".into(), m.to_string());
    }
    // The harness (agent runtime) dimension — pairs with `model` so the
    // efficiency frontier is a real (harness × model) route, not model-only.
    if !harness.is_empty() {
        data.insert("harness".into(), harness.to_string());
    }
    // Distinguishes the agent-loop deliverable (tools + delegation over the
    // mesh) from the single-turn router path, and records success/failure.
    data.insert("source".into(), "mesh-task".into());
    data.insert(
        "status".into(),
        if ok { "ok".into() } else { "error".into() },
    );
    if let Some(team) = owning_team {
        data.insert("team".into(), team.to_string());
    }
    if let Some(owner_sub) = owner_sub {
        data.insert("ownerSub".into(), owner_sub.to_string());
    }
    if let Some(owner_name) = owner_name {
        data.insert("ownerName".into(), owner_name.to_string());
    }
    if declared_artifact_count > 0 || !artifacts.is_empty() {
        let mut seen = std::collections::BTreeSet::new();
        let manifest: Vec<serde_json::Value> = artifacts
            .iter()
            .filter(|a| persisted_artifacts.contains(&a.name) && seen.insert(a.name.clone()))
            .map(|a| {
                let digest = format!("sha256:{:x}", sha2::Sha256::digest(&a.bytes));
                json!({
                    "name": a.name,
                    "size_bytes": a.bytes.len(),
                    "source_agent": a.source_agent,
                    "source_path": a.source_path,
                    "digest": digest,
                })
            })
            .collect();
        data.insert(
            "artifacts".into(),
            serde_json::to_string(&manifest).unwrap_or_else(|_| "[]".into()),
        );
        data.insert("artifactCount".into(), manifest.len().to_string());
        data.insert(
            "declaredArtifactCount".into(),
            declared_artifact_count.to_string(),
        );
        data.insert(
            "artifactPersistence".into(),
            if manifest.len() == declared_artifact_count {
                "complete".into()
            } else {
                "partial".into()
            },
        );
    }
    // Real token telemetry — same key names the single-turn run path uses, so
    // the Bridge scorecard reads them uniformly regardless of run path.
    if let Some(t) = telemetry {
        if t.total_tokens > 0 {
            data.insert("totalTokens".into(), t.total_tokens.to_string());
        }
        if t.prompt_tokens > 0 {
            data.insert("promptTokens".into(), t.prompt_tokens.to_string());
        }
        if t.completion_tokens > 0 {
            data.insert("completionTokens".into(), t.completion_tokens.to_string());
        }
        data.insert("rounds".into(), t.rounds.to_string());
        data.insert("toolCalls".into(), t.tool_calls.to_string());
    }

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

/// Persist the full artifact set to `kars-mission-artifacts-<task>`. Text
/// artifacts go in `data` (directly readable); binary artifacts go in
/// `binaryData` (base64). A ConfigMap caps at ~1 MiB total — artifacts are
/// added until the budget is reached, largest-last, so the set is never
/// silently corrupted. This is the minimal §16 artifact record: a durable,
/// cluster-native object holding the complete deliverable set, readable on a
/// plain kars cluster with `kubectl get configmap` — no Bridge required.
async fn write_mission_artifacts(
    state: &Arc<MeshPeerState>,
    task: &str,
    artifacts: &[ReceivedArtifact],
) -> Result<std::collections::BTreeSet<String>> {
    use k8s_openapi::ByteString;
    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let cms: Api<k8s_openapi::api::core::v1::ConfigMap> =
        Api::namespaced(state.client.clone(), &namespace);
    let name = format!("kars-mission-artifacts-{task}");

    // ConfigMap hard limit is ~1 MiB; keep a margin for metadata.
    const BUDGET: usize = 900 * 1024;
    let mut used = 0usize;
    let mut text: BTreeMap<String, String> = BTreeMap::new();
    let mut binary: BTreeMap<String, ByteString> = BTreeMap::new();
    let mut persisted = std::collections::BTreeSet::new();

    for a in artifacts {
        // Sanitize to a valid ConfigMap key (alnum, '-', '_', '.').
        let key: String = a
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let key = if key.is_empty() {
            "artifact".into()
        } else {
            key
        };
        if used + a.bytes.len() > BUDGET {
            tracing::warn!(task = %task, file = %a.name, "artifact set exceeds ConfigMap budget — truncating set");
            break;
        }
        used += a.bytes.len();
        match String::from_utf8(a.bytes.clone()) {
            // Valid UTF-8 *and* free of disallowed control characters → store as
            // readable text. Otherwise (binary, or text with embedded C0 control
            // chars the API server rejects in `data`) preserve the exact bytes
            // in `binaryData`.
            Ok(s) if !cm_has_disallowed_control(&s) => {
                text.insert(key, s);
            }
            _ => {
                binary.insert(key, ByteString(a.bytes.clone()));
            }
        }
        persisted.insert(a.name.clone());
    }

    let patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": name, "labels": { "kars.azure.com/mission-artifacts": task } },
        "data": text,
        "binaryData": binary,
    });
    cms.patch(
        &name,
        &PatchParams::apply(crate::field_managers::MESH_PEER).force(),
        &Patch::Apply(patch),
    )
    .await
    .context("write mission-artifacts ConfigMap")?;
    Ok(persisted)
}

/// Persist the agent's execution trace to `kars-mission-trace-<task>` — the
/// clean per-tool audit record. The trace is a JSON array of `round`/`tool`
/// events emitted live by the agent loop (real token usage, tool names,
/// sanitized arg/result previews, durations). Stored verbatim under a single
/// `trace.json` key so it is independently inspectable on a plain kars cluster
/// (`kubectl get configmap kars-mission-trace-<task> -o jsonpath='{.data.trace\.json}'`),
/// and consumed by the Bridge to render the live activity timeline. Capped at
/// the ConfigMap budget; on overflow the oldest events are dropped so the most
/// recent activity is always retained.
async fn write_mission_trace(
    state: &Arc<MeshPeerState>,
    task: &str,
    trace: &[serde_json::Value],
) -> Result<()> {
    let namespace = std::env::var("KARS_NAMESPACE").unwrap_or_else(|_| "kars-system".into());
    let cms: Api<k8s_openapi::api::core::v1::ConfigMap> =
        Api::namespaced(state.client.clone(), &namespace);
    let name = format!("kars-mission-trace-{task}");

    // Keep within the ConfigMap ~1 MiB budget; drop oldest events if needed.
    const BUDGET: usize = 900 * 1024;
    let mut events = trace.to_vec();
    let mut serialized = serde_json::to_string(&events).unwrap_or_else(|_| "[]".into());
    while serialized.len() > BUDGET && events.len() > 1 {
        events.remove(0);
        serialized = serde_json::to_string(&events).unwrap_or_else(|_| "[]".into());
    }

    let mut data: BTreeMap<String, String> = BTreeMap::new();
    data.insert("trace.json".into(), serialized);
    data.insert("eventCount".into(), trace.len().to_string());
    data.insert("capturedAt".into(), Utc::now().to_rfc3339());

    let patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": name, "labels": { "kars.azure.com/mission-trace": task } },
        "data": data,
    });
    cms.patch(
        &name,
        &PatchParams::apply(crate::field_managers::MESH_PEER).force(),
        &Patch::Apply(patch),
    )
    .await
    .context("write mission-trace ConfigMap")?;
    Ok(())
}

fn assignment_api(state: &MeshPeerState, namespace: &str) -> Api<DynamicObject> {
    let api_resource = kube::api::ApiResource {
        group: "kars.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "kars.azure.com/v1alpha1".into(),
        kind: "KarsTask".into(),
        plural: "karstasks".into(),
    };
    Api::namespaced_with(state.client.clone(), namespace, &api_resource)
}

async fn persist_assignment_transition(
    state: &Arc<MeshPeerState>,
    pending: &PendingAssignmentProgress,
    worker_did: &str,
    event_type: &str,
    assignment_state: &str,
    stage: Option<&str>,
    message: Option<&str>,
) -> Result<()> {
    let api = assignment_api(state, &pending.namespace);
    let current = api
        .get(&pending.task_name)
        .await
        .context("read KarsTask assignment ledger")?;
    let status = current
        .data
        .get("status")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut events = status
        .get("assignmentEvents")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sequence = status
        .get("assignmentSequence")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default()
        + 1;
    let now = Utc::now().to_rfc3339();
    let child_task_id = None::<String>;
    let child_role = None::<String>;
    events.push(json!({
        "sequence": sequence,
        "eventId": format!("{}:{sequence}", pending.task_id),
        "taskId": pending.task_id,
        "eventType": event_type,
        "state": assignment_state,
        "at": now,
        "workerDid": worker_did,
        "stage": stage,
        "childTaskId": child_task_id,
        "childRole": child_role,
        "outcome": if assignment_state == "Completed" { Some("success") } else { None::<&str> },
        "message": message,
    }));
    if events.len() > 200 {
        events.drain(..events.len() - 200);
    }
    let completed_at =
        matches!(assignment_state, "Completed" | "Failed" | "Cancelled").then_some(now.clone());
    let patch = json!({
        "status": {
            "assignment": {
                "taskId": pending.task_id,
                "state": assignment_state,
                "workerDid": worker_did,
                "stage": stage,
                "lastProgressAt": now,
                "completedAt": completed_at,
                "error": if assignment_state == "Failed" { message } else { None::<&str> },
            },
            "assignmentEvents": events,
            "assignmentSequence": sequence,
        }
    });
    api.patch_status(
        &pending.task_name,
        &PatchParams::default(),
        &Patch::Merge(patch),
    )
    .await
    .context("patch KarsTask assignment transition")?;
    Ok(())
}

async fn persist_assignment_progress(
    state: &Arc<MeshPeerState>,
    pending: &PendingAssignmentProgress,
    worker_did: &str,
    update: ProgressUpdate,
) -> Result<()> {
    let api = assignment_api(state, &pending.namespace);
    let now = Utc::now().to_rfc3339();
    let patch = json!({
        "status": {
            "assignment": {
                "taskId": pending.task_id,
                "state": "Running",
                "workerDid": worker_did,
                "stage": update.stage,
                "childTaskId": update.child_task_id,
                "childRole": update.child_role,
                "lastProgressAt": now,
                "error": update.message,
            }
        }
    });
    api.patch_status(
        &pending.task_name,
        &PatchParams::default(),
        &Patch::Merge(patch),
    )
    .await
    .context("patch KarsTask assignment progress")?;
    Ok(())
}

/// Stamp `kars.azure.com/run-ack: <nonce>` once the objective has been
/// dispatched to the agent — the "actively delivering" signal.
async fn mark_ack(
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
            "annotations": { RUN_ACK_ANNOTATION: nonce }
        }
    });
    api.patch(
        task,
        &PatchParams::apply(crate::field_managers::MESH_PEER),
        &Patch::Merge(patch),
    )
    .await
    .context("annotate run-ack")?;
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

/// Record a transient (not-yet-ready) delivery attempt on the run-request, so
/// the next poll retries instead of giving up.
async fn bump_attempts(
    state: &Arc<MeshPeerState>,
    namespace: &str,
    task: &str,
    attempts: u32,
) -> Result<()> {
    bump_attempt_annotation(state, namespace, task, RUN_ATTEMPTS_ANNOTATION, attempts).await
}

async fn bump_attempt_annotation(
    state: &Arc<MeshPeerState>,
    namespace: &str,
    task: &str,
    annotation: &str,
    attempts: u32,
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
            "annotations": { annotation: attempts.to_string() }
        }
    });
    api.patch(
        task,
        &PatchParams::apply(crate::field_managers::MESH_PEER),
        &Patch::Merge(patch),
    )
    .await
    .context("annotate run-attempts")?;
    Ok(())
}

/// Handle a transient delivery miss (agent not yet on the mesh). Retries on the
/// next poll until the warm-up budget is spent; only then records a terminal
/// "agent never came online" deliverable so the run doesn't hang forever.
#[allow(clippy::too_many_arguments)]
async fn handle_transient_miss(
    state: &Arc<MeshPeerState>,
    namespace: &str,
    task: &str,
    objective: &str,
    nonce: &str,
    attempts: u32,
    model: Option<&str>,
    harness: &str,
    reason: &str,
) -> Result<()> {
    let max_attempts = max_delivery_attempts();
    if attempts + 1 < max_attempts {
        bump_attempts(state, namespace, task, attempts + 1).await?;
        tracing::info!(
            task = %task, attempt = attempts + 1, max = max_attempts, reason,
            "task-delivery: agent not ready — will retry on next poll"
        );
        return Ok(());
    }
    tracing::warn!(
        task = %task, attempts = attempts + 1, reason,
        "task-delivery: warm-up budget exhausted — recording terminal miss"
    );
    write_mission_output(
        state,
        task,
        objective,
        &format!("agent did not come online after {max_attempts} attempts: {reason}"),
        false,
        &[],
        &std::collections::BTreeSet::new(),
        0,
        None,
        None,
        None,
        None,
        model,
        harness,
    )
    .await?;
    mark_completed(state, namespace, task, nonce).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_substantive_deliverable, select_newest_agent_did};
    use serde_json::json;

    #[test]
    fn aborted_and_human_blocked_outputs_are_not_successes() {
        assert!(!is_substantive_deliverable("aborted"));
        assert!(!is_substantive_deliverable("Aborted: operator cancelled"));
        assert!(!is_substantive_deliverable(
            "Stopped before completing the task"
        ));
        assert!(!is_substantive_deliverable(
            "[[NEEDS_CLARIFICATION]] Which environment?"
        ));
        assert!(!is_substantive_deliverable(
            "[[NEEDS_EGRESS example.com:443 - fetch evidence]]"
        ));
        assert!(is_substantive_deliverable(
            "Partial work\n[[NEEDS_EGRESS]] example.com:443 - fetch evidence"
        ));
        assert!(is_substantive_deliverable(
            "# NORTHSTAR_TEAM_INCOMPLETE\nBackend reported `[[NEEDS_CLARIFICATION]] repo access` as evidence."
        ));
        assert!(is_substantive_deliverable(
            "> [[NEEDS_CLARIFICATION]] quoted child question"
        ));
        assert!(!is_substantive_deliverable(
            "- [[NEEDS_CLARIFICATION]] Which environment?"
        ));
        assert!(is_substantive_deliverable(
            "Completed the review with evidence and a ship recommendation."
        ));
    }

    #[test]
    fn newest_mesh_identity_is_selected_beyond_the_first_ten() {
        let mut results = (0..12)
            .map(|i| {
                json!({
                    "did": format!("did:mesh:{i}"),
                    "last_seen": format!("2026-07-16T10:{i:02}:00Z")
                })
            })
            .collect::<Vec<_>>();
        results.push(json!({
            "did": "did:mesh:current",
            "last_seen": "2026-07-17T08:00:00Z"
        }));
        assert_eq!(
            select_newest_agent_did(&results).as_deref(),
            Some("did:mesh:current")
        );
    }
}
