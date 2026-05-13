// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Controller-side offload orchestration over the AgentMesh peer
//! channel.
//!
//! Extracted from `mesh_peer/mod.rs` per plan §4.2 hotspot
//! decomposition. Pure refactor — function bodies are byte-identical
//! to the originals; visibility on a small set of helpers in
//! `mesh_peer/mod.rs` (`hex_sha256`, `pair_error`, `send_to_peer`,
//! `enqueue_outbound`) was promoted to `pub(super)` so the
//! offload handlers can call them.

use anyhow::{Context as _, Result};
use chrono::Utc;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    ResourceExt,
    api::{Api, ListParams, Patch, PatchParams, PostParams},
};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::{
    FederationMessage, FileContent, IDENTITY_NAMESPACE, MeshPeerState, OffloadPreferences,
    enqueue_outbound, send_to_peer,
};

/// Offload-tier AGT policy profile, embedded at compile time from the CLI
/// asset so there is exactly one source of truth shared by the CLI
/// (which reads it from disk for normal sandboxes) and the controller
/// (which inlines it into the ToolPolicy CRs it mints when honouring an
/// offload request from a paired peer).
///
/// Post-Slice-1e the sandbox image no longer carries bundled profile
/// fallbacks, so every ToolPolicy CR — including the ones synthesized by
/// this module — must populate `spec.agtProfile.inline` or the
/// reconciler will fail closed with `Degraded / SpecInvalid`.
const OFFLOAD_AGT_PROFILE: &str = include_str!("../../../cli/profiles/agt/azureclaw-offload.yaml");
use crate::pairing::{ClawPairing, phase};

// ---------------------------------------------------------------------------
// Offload orchestration
// ---------------------------------------------------------------------------

/// Handle an offload_request from a paired external agent.
/// Validates the pairing, checks budget/slots, creates file ConfigMap if needed,
/// creates a ClawSandbox CRD, watches pod completion, and relays results back.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_offload_request(
    state: &Arc<MeshPeerState>,
    out_tx: &tokio::sync::mpsc::UnboundedSender<WsMessage>,
    from_amid: &str,
    request_id: &str,
    task: &str,
    _files: &[String],
    _total_bytes: u64,
    _file_contents: &[FileContent],
    preferences: Option<&OffloadPreferences>,
    _timestamp: &str,
) -> Result<()> {
    // Phase 1: Validate pairing
    send_to_peer(
        state,
        out_tx,
        from_amid,
        &FederationMessage::OffloadStatus {
            request_id: request_id.into(),
            phase: "validating".into(),
            message: "Validating pairing and budget".into(),
            sandbox_name: None,
        },
    )
    .await?;

    let pairing = match validate_pairing_for_offload(state, from_amid).await {
        Ok(p) => p,
        Err(e) => {
            send_to_peer(
                state,
                out_tx,
                from_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: e.clone(),
                    phase: "validating".into(),
                },
            )
            .await?;
            return Ok(());
        }
    };

    // Phase 2: Create offload sandbox
    send_to_peer(
        state,
        out_tx,
        from_amid,
        &FederationMessage::OffloadStatus {
            request_id: request_id.into(),
            phase: "spawning".into(),
            message: "Creating offload sandbox".into(),
            sandbox_name: None,
        },
    )
    .await?;

    let sandbox_name = format!("offload-{}", &request_id[..8]);
    let model = preferences
        .and_then(|p| p.model.as_deref())
        .unwrap_or("gpt-5.4");
    let timeout_minutes = preferences.and_then(|p| p.timeout_minutes).unwrap_or(30);
    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());

    // Post-S10/S13 ClawSandbox shape:
    //   - `runtime` (required) — multi-runtime selector
    //   - `inferenceRef` (required) — by-name ref to an InferencePolicy CR
    //   - `governance.toolPolicyRef` (required) — by-name ref to a ToolPolicy CR
    //
    // The sandbox CRD no longer carries inline `inference` / `openclaw`
    // /  `governance.toolPolicy` blocks — those moved to dedicated CRs in
    // S10.A1 / S13. We mint the matching InferencePolicy + ToolPolicy
    // alongside the sandbox below so the controller can reconcile it.
    let inference_ref_name = format!("{sandbox_name}-inference");
    let tool_policy_ref_name = format!("{sandbox_name}-toolpolicy");

    let crd = build_offload_sandbox_crd(
        &sandbox_name,
        &namespace,
        from_amid,
        request_id,
        task,
        model,
        timeout_minutes,
        &inference_ref_name,
        &tool_policy_ref_name,
    );

    // No OFFLOAD_MODE — sandbox starts as a full AzureClaw agent.
    // The external agent talks to it directly via existing mesh protocol
    // (mesh_send, mesh_transfer_file). AGT_TRUSTED_PEERS locks the sandbox
    // so only the paired external agent can communicate with it.
    let extra_env = json!({
        "OFFLOAD_REQUEST_ID": request_id,
        "OFFLOAD_PARENT_AMID": from_amid,
        "OFFLOAD_TIMEOUT_MINUTES": timeout_minutes.to_string(),
        "OFFLOAD_TASK": task,
    });

    // S10.A1 / S13: ClawSandbox now references InferencePolicy + ToolPolicy
    // CRs by name; we mint them alongside so the controller can reconcile
    // the sandbox immediately. (`additionalProperties: false` means we
    // can't fall back to inline policy blocks any more.)
    let inference_policy = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "InferencePolicy",
        "metadata": {
            "name": inference_ref_name,
            "namespace": namespace,
            "labels": {
                "azureclaw.azure.com/spawned-by": "offload",
                "azureclaw.azure.com/sandbox": sandbox_name.clone(),
            }
        },
        "spec": {
            "appliesTo": { "sandboxName": sandbox_name.clone() },
            "modelPreference": {
                "primary": { "provider": "azure-openai", "deployment": model },
                "fallback": []
            },
            "contentSafety": { "requirePromptShields": true },
            "tokenBudget": {
                "daily": pairing.spec.token_budget,
                "perRequest": 32000
            }
        }
    });
    let tool_policy = json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ToolPolicy",
        "metadata": {
            "name": tool_policy_ref_name,
            "namespace": namespace,
            "labels": {
                "azureclaw.azure.com/spawned-by": "offload",
                "azureclaw.azure.com/sandbox": sandbox_name.clone(),
            }
        },
        "spec": {
            "appliesTo": {
                "sandboxMatchLabels": {
                    "azureclaw.azure.com/sandbox": sandbox_name.clone()
                }
            },
            "agtProfile": {
                "inline": OFFLOAD_AGT_PROFILE,
            },
        }
    });

    let inference_policy_ar = kube::api::ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "InferencePolicy".into(),
        plural: "inferencepolicies".into(),
    };
    let tool_policy_ar = kube::api::ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "ToolPolicy".into(),
        plural: "toolpolicies".into(),
    };
    let inference_api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(state.client.clone(), &namespace, &inference_policy_ar);
    let toolpolicy_api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(state.client.clone(), &namespace, &tool_policy_ar);

    for (label, payload, api) in [
        ("InferencePolicy", inference_policy, &inference_api),
        ("ToolPolicy", tool_policy, &toolpolicy_api),
    ] {
        let obj: kube::api::DynamicObject = match serde_json::from_value(payload) {
            Ok(o) => o,
            Err(e) => {
                send_to_peer(
                    state,
                    out_tx,
                    from_amid,
                    &FederationMessage::OffloadError {
                        request_id: request_id.into(),
                        error: format!("Failed to build {label} CRD: {e}"),
                        phase: "spawning".into(),
                    },
                )
                .await?;
                return Ok(());
            }
        };
        if let Err(e) = api.create(&PostParams::default(), &obj).await {
            // AlreadyExists is fine — we may be retrying the same offload.
            let s = e.to_string();
            if !s.contains("already exists") && !s.contains("AlreadyExists") {
                tracing::error!(sandbox = %sandbox_name, "Failed to create {label}: {e}");
                send_to_peer(
                    state,
                    out_tx,
                    from_amid,
                    &FederationMessage::OffloadError {
                        request_id: request_id.into(),
                        error: format!("Failed to create {label}: {e}"),
                        phase: "spawning".into(),
                    },
                )
                .await?;
                return Ok(());
            }
        }
    }

    // Create the sandbox itself.
    let api_resource = kube::api::ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "ClawSandbox".into(),
        plural: "clawsandboxes".into(),
    };
    let api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(state.client.clone(), &namespace, &api_resource);

    // Merge extra env into the CRD spec (S10.A1: inside spec.runtime.openclaw, not spec.openclaw)
    let mut crd_value = crd;
    if let Some(openclaw) = crd_value["spec"]["runtime"]["openclaw"].as_object_mut() {
        openclaw.insert("extraEnv".to_string(), extra_env);
    }

    let obj: kube::api::DynamicObject = match serde_json::from_value(crd_value) {
        Ok(o) => o,
        Err(e) => {
            send_to_peer(
                state,
                out_tx,
                from_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: format!("Failed to build sandbox CRD: {e}"),
                    phase: "spawning".into(),
                },
            )
            .await?;
            return Ok(());
        }
    };

    match api.create(&PostParams::default(), &obj).await {
        Ok(_) => {
            tracing::info!(
                sandbox = %sandbox_name,
                requester = %from_amid,
                request_id = %request_id,
                "Offload sandbox created"
            );
        }
        Err(e) => {
            tracing::error!(
                sandbox = %sandbox_name,
                "Failed to create offload sandbox: {e}"
            );
            send_to_peer(
                state,
                out_tx,
                from_amid,
                &FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: format!("Failed to create sandbox: {e}"),
                    phase: "spawning".into(),
                },
            )
            .await?;
            return Ok(());
        }
    }

    // Update pairing usage
    let pairing_name = pairing.name_any();
    let pairings_api: Api<ClawPairing> = Api::namespaced(state.client.clone(), IDENTITY_NAMESPACE);
    let usage_patch = json!({
        "status": {
            "slotsUsed": pairing.status.as_ref().and_then(|s| s.slots_used).unwrap_or(0) + 1,
            "lastOffloadAt": chrono::Utc::now().to_rfc3339(),
        }
    });
    let _ = pairings_api
        .patch_status(
            &pairing_name,
            &PatchParams::apply(crate::field_managers::MESH_PEER),
            &Patch::Merge(usage_patch),
        )
        .await;

    // Phase 3: CRD created — notify requester sandbox is being scheduled
    send_to_peer(
        state,
        out_tx,
        from_amid,
        &FederationMessage::OffloadStatus {
            request_id: request_id.into(),
            phase: "scheduled".into(),
            message: format!("Sandbox '{sandbox_name}' created, waiting for it to start..."),
            sandbox_name: None,
        },
    )
    .await?;

    // Phase 4: Watch for sandbox to become Running, then send sandbox name
    // back to the requester so they can talk to it directly via mesh.
    let watcher_state = Arc::clone(state);
    let watcher_amid = from_amid.to_string();
    let watcher_request_id = request_id.to_string();
    let watcher_sandbox = sandbox_name.clone();
    let watcher_ns = namespace.clone();
    // Capture the leader epoch at spawn time. If this controller loses and
    // regains leadership, a newer epoch will invalidate any late enqueues
    // from this watcher at the drain site.
    let watcher_epoch = watcher_state.leader_epoch.load(Ordering::Acquire);

    tokio::spawn(async move {
        if let Err(e) = watch_sandbox_ready(
            &watcher_state,
            &watcher_amid,
            &watcher_request_id,
            &watcher_sandbox,
            &watcher_ns,
            watcher_epoch,
        )
        .await
        {
            tracing::error!(
                request_id = %watcher_request_id,
                "Sandbox ready watcher failed: {e:#}"
            );
        }
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Sandbox ready watcher
// ---------------------------------------------------------------------------

/// Watch for an offload sandbox pod to become Running, then send its name back
/// to the requester so they can talk to it directly via the mesh.
///
/// The controller's job ends here — the external agent and sandbox communicate
/// directly via the existing mesh protocol (mesh_send, mesh_transfer_file).
/// AGT_TRUSTED_PEERS on the sandbox ensures only the paired agent can talk to it.
///
/// NOTE: ClawSandbox reconciliation creates the pod in a dedicated namespace
/// named `azureclaw-<sandbox_name>`, and labels it with
/// `azureclaw.azure.com/sandbox=<sandbox_name>` (NOT request-id). The watcher
/// must use that namespace + label, otherwise it lists zero pods forever and
/// times out at 5 minutes.
/// Handle an offload_cleanup from the parent agent. Finds the ClawSandbox CRD
/// labeled with the given request_id (set only by the offload_request handler,
/// so non-offload sandboxes are never targeted) and deletes it. The reconciler
/// in reconciler.rs:141 picks up the deletion and tears down the per-sandbox
/// namespace, deployment, service, and NetworkPolicy.
///
/// Also verifies that the requester AMID matches the CRD's
/// `azureclaw.azure.com/offload-requester` annotation — only the original
/// requester can clean up their own offload.
pub(super) async fn handle_offload_cleanup(
    state: &Arc<MeshPeerState>,
    from_amid: &str,
    request_id: &str,
) -> Result<()> {
    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());

    let api_resource = kube::api::ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "ClawSandbox".into(),
        plural: "clawsandboxes".into(),
    };
    let api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(state.client.clone(), &namespace, &api_resource);

    let lp = kube::api::ListParams::default()
        .labels(&format!("azureclaw.azure.com/request-id={request_id}"));
    let list = match api.list(&lp).await {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(
                request_id = %request_id,
                err = %e,
                "offload_cleanup: list by label failed"
            );
            return Ok(());
        }
    };

    if list.items.is_empty() {
        tracing::info!(
            request_id = %request_id,
            "offload_cleanup: no matching ClawSandbox (already cleaned up?)"
        );
        return Ok(());
    }

    for item in list.items {
        let name = item.metadata.name.as_deref().unwrap_or("");
        if name.is_empty() {
            continue;
        }

        // Authorize: only the original requester AMID may tear down its own offload.
        let requester_ok = item
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("azureclaw.azure.com/offload-requester"))
            .map(|v| v == from_amid)
            .unwrap_or(false)
            || item
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get("azureclaw.azure.com/offload-parent-amid"))
                .map(|v| v == from_amid)
                .unwrap_or(false);

        if !requester_ok {
            tracing::warn!(
                from = %from_amid,
                sandbox = %name,
                request_id = %request_id,
                "offload_cleanup: requester AMID does not match the offload's parent — refusing to delete"
            );
            continue;
        }

        match api.delete(name, &kube::api::DeleteParams::default()).await {
            Ok(_) => {
                tracing::info!(
                    sandbox = %name,
                    request_id = %request_id,
                    "offload_cleanup: ClawSandbox deletion requested; reconciler will clean up namespace"
                );
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                tracing::info!(
                    sandbox = %name,
                    request_id = %request_id,
                    "offload_cleanup: ClawSandbox already deleted"
                );
            }
            Err(e) => {
                tracing::warn!(
                    sandbox = %name,
                    request_id = %request_id,
                    err = %e,
                    "offload_cleanup: delete failed"
                );
            }
        }
    }

    Ok(())
}

pub(super) async fn watch_sandbox_ready(
    state: &Arc<MeshPeerState>,
    requester_amid: &str,
    request_id: &str,
    sandbox_name: &str,
    _namespace: &str,
    epoch: u64,
) -> Result<()> {
    let sandbox_ns = format!("azureclaw-{sandbox_name}");
    let pods: Api<Pod> = Api::namespaced(state.client.clone(), &sandbox_ns);
    let label_selector = format!("azureclaw.azure.com/sandbox={sandbox_name}");
    // 5 minutes should be enough for pod scheduling + container pull + startup
    let timeout = Duration::from_secs(300);

    tracing::info!(
        request_id = %request_id,
        sandbox = %sandbox_name,
        namespace = %sandbox_ns,
        label_selector = %label_selector,
        "Watching for offload sandbox to become ready"
    );

    let ready_pod = tokio::time::timeout(timeout, async {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let list = pods
                .list(&ListParams::default().labels(&label_selector))
                .await;
            let pod_list = match list {
                Ok(l) => l,
                Err(e) => {
                    tracing::debug!(request_id = %request_id, "Pod list error: {e}");
                    continue;
                }
            };

            for pod in &pod_list {
                if let Some(status) = &pod.status {
                    let phase = status.phase.as_deref().unwrap_or("");
                    match phase {
                        "Running" => {
                            // Check that ALL containers are ready (not just pod phase)
                            // — the inference-router needs to pass its /healthz probe
                            let all_ready = status
                                .container_statuses
                                .as_ref()
                                .map(|cs| !cs.is_empty() && cs.iter().all(|c| c.ready))
                                .unwrap_or(false);
                            if all_ready {
                                return Ok::<Pod, anyhow::Error>(pod.clone());
                            }
                        }
                        "Failed" => {
                            anyhow::bail!("Sandbox pod failed before becoming ready");
                        }
                        _ => {}
                    }
                }
            }
        }
    })
    .await;

    match ready_pod {
        Ok(Ok(_pod)) => {
            tracing::info!(
                request_id = %request_id,
                sandbox = %sandbox_name,
                "Offload sandbox is running — sending name to requester"
            );

            // Send sandbox name so the external agent can discover it on the mesh
            // and talk to it directly via mesh_send / mesh_transfer_file.
            // Enqueued through the persistent outbox — drained by the active
            // relay connection. Stale epochs dropped at drain time.
            enqueue_outbound(
                state,
                epoch,
                requester_amid,
                FederationMessage::OffloadStatus {
                    request_id: request_id.into(),
                    phase: "ready".into(),
                    message: format!(
                        "Sandbox '{sandbox_name}' is running — send files and task directly via mesh"
                    ),
                    sandbox_name: Some(sandbox_name.into()),
                },
            )?;

            // Idempotency marker — annotate the ClawSandbox CRD so we don't
            // re-send `ready` after a controller restart-and-rewatch.
            let _ = annotate_ready_sent(state, sandbox_name).await;
        }
        Ok(Err(e)) => {
            tracing::error!(request_id = %request_id, "Sandbox failed: {e}");
            enqueue_outbound(
                state,
                epoch,
                requester_amid,
                FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: format!("Sandbox failed to start: {e}"),
                    phase: "spawning".into(),
                },
            )?;
        }
        Err(_) => {
            tracing::warn!(request_id = %request_id, "Sandbox did not become ready within 5 minutes");
            enqueue_outbound(
                state,
                epoch,
                requester_amid,
                FederationMessage::OffloadError {
                    request_id: request_id.into(),
                    error: "Sandbox timed out waiting to become ready".into(),
                    phase: "spawning".into(),
                },
            )?;
        }
    }

    Ok(())
}

/// Mark an offload ClawSandbox CRD as having had its `ready` event emitted,
/// so a controller restart doesn't re-send the event (and we can use the
/// annotation to decide whether to resume-watch after a leader handover).
pub(super) async fn annotate_ready_sent(state: &MeshPeerState, sandbox_name: &str) -> Result<()> {
    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api_resource = kube::api::ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "ClawSandbox".into(),
        plural: "clawsandboxes".into(),
    };
    let api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(state.client.clone(), &namespace, &api_resource);
    let patch = json!({
        "metadata": {
            "annotations": {
                "azureclaw.azure.com/offload-ready-sent": Utc::now().to_rfc3339(),
            }
        }
    });
    api.patch(
        sandbox_name,
        &PatchParams::apply(crate::field_managers::MESH_PEER),
        &Patch::Merge(patch),
    )
    .await
    .context("annotate offload-ready-sent")?;
    Ok(())
}

/// On controller startup (after leader election), resume watching any in-flight
/// offload sandboxes whose ready event has not yet been emitted. This closes
/// the gap where a controller restart would otherwise strand the requester
/// in the `scheduled` phase forever (watcher task was lost with the old
/// process).
pub(super) async fn resume_pending_offload_watchers(state: &Arc<MeshPeerState>) -> Result<()> {
    let namespace =
        std::env::var("AZURECLAW_NAMESPACE").unwrap_or_else(|_| "azureclaw-system".into());
    let api_resource = kube::api::ApiResource {
        group: "azureclaw.azure.com".into(),
        version: "v1alpha1".into(),
        api_version: "azureclaw.azure.com/v1alpha1".into(),
        kind: "ClawSandbox".into(),
        plural: "clawsandboxes".into(),
    };
    let api: Api<kube::api::DynamicObject> =
        Api::namespaced_with(state.client.clone(), &namespace, &api_resource);

    let list = api
        .list(&ListParams::default().labels("azureclaw.azure.com/spawned-by=offload"))
        .await
        .context("list offload sandboxes")?;

    let mut resumed = 0u32;
    for sandbox in list.items {
        let sandbox_name = sandbox.name_any();
        let annotations = sandbox.metadata.annotations.clone().unwrap_or_default();
        if annotations.contains_key("azureclaw.azure.com/offload-ready-sent") {
            continue;
        }
        let parent = match annotations.get("azureclaw.azure.com/offload-parent-amid") {
            Some(v) => v.clone(),
            None => continue,
        };
        let labels = sandbox.metadata.labels.clone().unwrap_or_default();
        let request_id = match labels.get("azureclaw.azure.com/request-id") {
            Some(v) => v.clone(),
            None => continue,
        };

        tracing::info!(
            sandbox = %sandbox_name,
            request_id = %request_id,
            parent = %parent,
            "Resuming offload ready-watcher after controller startup"
        );

        let state_cloned = Arc::clone(state);
        let ns_cloned = namespace.clone();
        let sandbox_name_cloned = sandbox_name.clone();
        // Resumed watchers adopt the current leader epoch — they're being
        // started fresh under this leader tenure.
        let resume_epoch = state.leader_epoch.load(Ordering::Acquire);
        tokio::spawn(async move {
            if let Err(e) = watch_sandbox_ready(
                &state_cloned,
                &parent,
                &request_id,
                &sandbox_name_cloned,
                &ns_cloned,
                resume_epoch,
            )
            .await
            {
                tracing::error!(
                    sandbox = %sandbox_name_cloned,
                    "Resumed ready-watcher failed: {e:#}"
                );
            }
        });
        resumed += 1;
    }

    if resumed > 0 {
        tracing::info!(resumed, "Resumed in-flight offload watchers");
    }
    Ok(())
}

/// Validate that an AMID has an active pairing with available slots and budget.
pub(super) async fn validate_pairing_for_offload(
    state: &MeshPeerState,
    from_amid: &str,
) -> Result<ClawPairing, String> {
    let pairings: Api<ClawPairing> = Api::namespaced(state.client.clone(), IDENTITY_NAMESPACE);
    let pairing_list = pairings
        .list(&kube::api::ListParams::default())
        .await
        .map_err(|e| format!("Internal error — could not list pairings: {e}"))?;

    let matching = pairing_list
        .items
        .into_iter()
        .find(|p| p.status.as_ref().and_then(|s| s.bound_amid.as_deref()) == Some(from_amid));

    let pairing = matching.ok_or_else(|| {
        format!("No pairing found for AMID {from_amid}. Pair first with mesh_pair.")
    })?;

    let status = pairing.status.as_ref();
    let current_phase = status.and_then(|s| s.phase.as_deref()).unwrap_or("");

    if current_phase != phase::ACTIVE {
        return Err(format!("Pairing is '{current_phase}' — must be Active"));
    }

    // Check expiry
    if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(&pairing.spec.expires_at)
        && chrono::Utc::now() >= expiry.to_utc()
    {
        return Err("Pairing has expired".into());
    }

    // Check slots
    let slots_used = status.and_then(|s| s.slots_used).unwrap_or(0);
    if slots_used >= pairing.spec.slots_max {
        return Err(format!(
            "No available slots ({slots_used}/{} used)",
            pairing.spec.slots_max
        ));
    }

    // Check capability
    if !pairing.spec.capabilities.contains(&"offload".to_string()) {
        return Err("Pairing does not include 'offload' capability".into());
    }

    Ok(pairing)
}

// ---------------------------------------------------------------------------
// Pure CRD builders (kept testable; called from `handle_offload_request`)
// ---------------------------------------------------------------------------

/// Build the ClawSandbox CRD payload for a cross-cluster offload sandbox.
///
/// Pure function — no I/O — so it round-trips through
/// `serde_json::from_value::<ClawSandboxSpec>` in unit tests, catching
/// schema regressions (legacy `spec.openclaw` / `spec.inference` /
/// `governance.toolPolicy: <string>` shapes are rejected by the
/// post-S10/S13 schema with `additionalProperties: false`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_offload_sandbox_crd(
    sandbox_name: &str,
    namespace: &str,
    from_amid: &str,
    request_id: &str,
    task: &str,
    model: &str,
    timeout_minutes: u32,
    inference_ref_name: &str,
    tool_policy_ref_name: &str,
) -> serde_json::Value {
    let spec = json!({
        "runtime": {
            "kind": "OpenClaw",
            "openclaw": {}
        },
        "inferenceRef": { "name": inference_ref_name },
        "sandbox": {
            "isolation": "confidential",
            "readOnlyRootFilesystem": true,
            "runAsNonRoot": true,
            "allowPrivilegeEscalation": false
        },
        "networkPolicy": {
            "defaultDeny": true,
            "approvalRequired": true,
            "egressMode": "Learn",
        },
        "governance": {
            "enabled": true,
            "toolPolicyRef": { "name": tool_policy_ref_name },
            "trustThreshold": 900,
            "trustedPeers": format!("offload-parent:{from_amid}"),
            "registryMode": "global"
        }
    });

    json!({
        "apiVersion": "azureclaw.azure.com/v1alpha1",
        "kind": "ClawSandbox",
        "metadata": {
            "name": sandbox_name,
            "namespace": namespace,
            "labels": {
                "azureclaw.azure.com/spawned-by": "offload",
                "azureclaw.azure.com/offload-requester": from_amid,
                "azureclaw.azure.com/request-id": request_id,
            },
            "annotations": {
                "azureclaw.azure.com/offload-task": &task[..task.len().min(256)],
                "azureclaw.azure.com/offload-timeout": format!("{timeout_minutes}m"),
                "azureclaw.azure.com/offload-parent-amid": from_amid,
                "azureclaw.azure.com/model": model,
            }
        },
        "spec": spec,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::ClawSandboxSpec;

    #[test]
    fn offload_payload_round_trips_through_clawsandboxspec() {
        // CI guard for the audit class of bugs: any json! shape we send
        // to the API server MUST parse cleanly through the canonical
        // `ClawSandboxSpec` (post-S10/S13). This catches reverts to the
        // legacy `spec.openclaw` / `spec.inference` / `governance.toolPolicy`
        // shapes — the API server rejects them at admission time
        // (`additionalProperties: false`), but admission is only reached
        // in the live cluster; this test fails at `cargo test` time.
        let crd = build_offload_sandbox_crd(
            "offload-abcdef12",
            "azureclaw-system",
            "did:agentmesh:peer.example",
            "abcdef1234567890",
            "summarize the attached PDF",
            "gpt-5.4",
            30,
            "offload-abcdef12-inference",
            "offload-abcdef12-toolpolicy",
        );

        // 1. Top-level CRD shape.
        assert_eq!(crd["apiVersion"], "azureclaw.azure.com/v1alpha1");
        assert_eq!(crd["kind"], "ClawSandbox");

        // 2. Spec must round-trip through ClawSandboxSpec.
        let spec_value = crd
            .get("spec")
            .cloned()
            .expect("offload payload missing `spec`");
        let spec: ClawSandboxSpec = serde_json::from_value(spec_value)
            .expect("offload payload must deserialize as ClawSandboxSpec");

        // 3. Required references are present.
        assert_eq!(spec.inference_ref.name, "offload-abcdef12-inference");
        let gov = spec.governance.as_ref().expect("governance required");
        assert_eq!(gov.tool_policy_ref.name, "offload-abcdef12-toolpolicy");

        // 4. Negative guard: legacy fields must not be present at all
        //    (the controller's `additionalProperties: false` would catch
        //    them in production, but we want the test to fail loudly
        //    at compile-equivalent speed if anyone re-adds them).
        let raw_spec = &crd["spec"];
        assert!(
            raw_spec.get("openclaw").is_none(),
            "legacy spec.openclaw must not be present"
        );
        assert!(
            raw_spec.get("inference").is_none(),
            "legacy spec.inference must not be present"
        );
        assert!(
            raw_spec.get("model").is_none(),
            "legacy spec.model must not be present"
        );
        assert!(
            raw_spec.get("handoff").is_none(),
            "legacy spec.handoff must not be present"
        );
        assert!(
            raw_spec["governance"].get("toolPolicy").is_none(),
            "legacy governance.toolPolicy (string) must not be present"
        );
    }

    #[test]
    fn offload_payload_uses_canonical_api_group() {
        // Pre-S10 prototype briefly used `azureclaw.io/v1alpha1`. The
        // canonical group is `azureclaw.azure.com`; using anything else
        // makes the API server return 404 for the resource path.
        let crd = build_offload_sandbox_crd(
            "x",
            "azureclaw-system",
            "p",
            "rid",
            "t",
            "m",
            30,
            "x-inference",
            "x-toolpolicy",
        );
        assert_eq!(crd["apiVersion"], "azureclaw.azure.com/v1alpha1");
    }
}
