//! Handoff payload handlers — snapshot, restore, verify.
//!
//! Extracted from `routes/handoff/mod.rs` per plan §4.2 hotspot
//! decomposition. These three handlers carry the bulk of the
//! handoff payload-processing logic (encrypt/decrypt the
//! state blob, build/restore the snapshot, compute the
//! verification hash). Pure refactor: bodies are byte-identical
//! to the originals; only visibility (`async fn` →
//! `pub(super) async fn`) changes.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::errors;
use crate::handoff;
use crate::routes::AppState;
use crate::routes::audit_events::handoff_event;
use crate::routes::mesh::lookup_parent_amid;
use crate::spawn;

/// POST /agt/handoff/snapshot — serialize and encrypt agent state.
///
/// Returns an encrypted blob that can be transferred to the target agent.
pub(super) async fn handoff_snapshot(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = state
        .handoff_session
        .try_transition(handoff::HandoffPhase::Snapshotting)
        .await
    {
        return errors::flat(StatusCode::CONFLICT, e).into_response();
    }

    let predecessor_amid = body
        .get("predecessor_amid")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let successor_amid = body
        .get("successor_amid")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let shared_secret = body
        .get("shared_secret")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if shared_secret.is_empty() {
        state
            .handoff_session
            .fail("Missing shared_secret".into())
            .await;
        return errors::flat(StatusCode::BAD_REQUEST, "shared_secret is required").into_response();
    }

    let direction = state
        .handoff_session
        .status()
        .await
        .direction
        .unwrap_or(handoff::HandoffDirection::LocalToAks);

    // Build snapshot from current state
    let mut snapshot =
        match handoff::build_snapshot(&state, direction, predecessor_amid, successor_amid).await {
            Ok(s) => s,
            Err(e) => {
                state.handoff_session.fail(e.clone()).await;
                return errors::flat(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
            }
        };

    // Inject workspace/chat if provided in request body
    if let Some(workspace) = body.get("workspace_tar").and_then(|v| v.as_str()) {
        if let Ok(bytes) =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, workspace)
        {
            snapshot.workspace_tar = bytes;
        }
    }
    if let Some(chat) = body.get("chat_snapshot").and_then(|v| v.as_str()) {
        if let Ok(bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, chat)
        {
            snapshot.chat_snapshot = Some(bytes);
        }
    }

    // Inject sub-agent snapshots if provided by the plugin (workspace data, AMIDs, etc.)
    if let Some(subs) = body.get("sub_agent_snapshots") {
        match serde_json::from_value::<Vec<handoff::SubAgentSnapshot>>(subs.clone()) {
            Ok(sub_snaps) => {
                let ws_count = sub_snaps
                    .iter()
                    .filter(|s| !s.workspace_tar.is_empty())
                    .count();
                tracing::info!(
                    count = sub_snaps.len(),
                    with_workspace = ws_count,
                    "Injected sub-agent snapshots into handoff state"
                );
                snapshot.sub_agent_snapshots = sub_snaps;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    json_preview = %subs.to_string().chars().take(500).collect::<String>(),
                    "Failed to deserialize sub_agent_snapshots — sub-agent workspaces will be lost"
                );
            }
        }
    }

    // Inject credential refs if provided
    if let Some(creds) = body.get("credentials") {
        if let Ok(cred_refs) = serde_json::from_value::<Vec<handoff::CredentialRef>>(creds.clone())
        {
            snapshot.credentials = cred_refs;
        }
    }

    // Serialize and compress
    let compressed = match handoff::serialize_state(&snapshot) {
        Ok(c) => c,
        Err(e) => {
            state.handoff_session.fail(e.clone()).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };

    // §9.9.4: Enforce blob size limit
    if compressed.len() > handoff::MAX_BLOB_SIZE_BYTES {
        let msg = format!(
            "State blob too large: {}MB (max {}MB)",
            compressed.len() / (1024 * 1024),
            handoff::MAX_BLOB_SIZE_BYTES / (1024 * 1024)
        );
        state.handoff_session.fail(msg.clone()).await;
        return errors::flat(StatusCode::PAYLOAD_TOO_LARGE, msg).into_response();
    }

    // Compute verification hash BEFORE encryption
    let verification_hash = handoff::compute_verification_hash(&compressed);

    // Generate HKDF salt (ThreadRng is !Send — scope before await)
    let salt = {
        let mut s = [0u8; 32];
        rand::Rng::fill(&mut rand::rng(), &mut s);
        s
    };

    // Decrypt the shared secret from base64
    let secret_bytes =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, shared_secret) {
            Ok(b) => b,
            Err(e) => {
                state
                    .handoff_session
                    .fail(format!("Invalid shared_secret: {e}"))
                    .await;
                return errors::flat(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid shared_secret base64: {e}"),
                )
                .into_response();
            }
        };

    // Encrypt with AES-256-GCM
    let blob = match handoff::encrypt_state(&compressed, &secret_bytes, &salt) {
        Ok(b) => b,
        Err(e) => {
            state.handoff_session.fail(e.clone()).await;
            return errors::flat(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };

    // Record snapshot stats
    let items = handoff::SnapshotItemCounts {
        chat_messages: snapshot.chat_snapshot.as_ref().map(|_| 1).unwrap_or(0),
        trust_scores: snapshot
            .trust_scores
            .as_array()
            .map(|a| a.len() as u32)
            .unwrap_or(0),
        audit_entries: snapshot.audit_entries.len() as u32,
        sub_agents: snapshot.sub_agent_snapshots.len() as u32,
        workspace_files: if snapshot.workspace_tar.is_empty() {
            0
        } else {
            1
        },
        credentials: snapshot.credentials.len() as u32,
    };
    state
        .handoff_session
        .record_snapshot(compressed.len(), items.clone())
        .await;

    // Audit log
    handoff_event(
        &state,
        "handoff:snapshot",
        &format!(
            "size={}B hash={}",
            compressed.len(),
            &verification_hash[..16]
        ),
    )
    .await;

    tracing::info!(
        size_bytes = compressed.len(),
        verification_hash = &verification_hash[..16],
        "Handoff snapshot created"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "blob": blob,
            "verification_hash": verification_hash,
            "snapshot_size_bytes": compressed.len(),
            "size_bytes": compressed.len(),
            "phase": "snapshotting",
            "items": {
                "chat_messages": items.chat_messages,
                "trust_scores": items.trust_scores,
                "audit_entries": items.audit_entries,
                "sub_agents": items.sub_agents,
                "workspace_files": items.workspace_files,
                "credentials": items.credentials,
            },
        })),
    )
        .into_response()
}

/// POST /agt/handoff/restore — accept encrypted state blob and restore.
pub(super) async fn handoff_restore(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = state
        .handoff_session
        .try_transition(handoff::HandoffPhase::Restoring)
        .await
    {
        return errors::flat(StatusCode::CONFLICT, e).into_response();
    }

    let shared_secret = body
        .get("shared_secret")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if shared_secret.is_empty() {
        state
            .handoff_session
            .fail("Missing shared_secret".into())
            .await;
        return errors::flat(StatusCode::BAD_REQUEST, "shared_secret is required").into_response();
    }

    // Parse the encrypted blob
    let blob: handoff::EncryptedHandoffBlob = match body.get("blob") {
        Some(b) => match serde_json::from_value(b.clone()) {
            Ok(blob) => blob,
            Err(e) => {
                state
                    .handoff_session
                    .fail(format!("Invalid blob: {e}"))
                    .await;
                return errors::flat(StatusCode::BAD_REQUEST, format!("Invalid blob format: {e}"))
                    .into_response();
            }
        },
        None => {
            state.handoff_session.fail("Missing blob".into()).await;
            return errors::flat(StatusCode::BAD_REQUEST, "blob is required").into_response();
        }
    };

    let secret_bytes =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, shared_secret) {
            Ok(b) => b,
            Err(e) => {
                state
                    .handoff_session
                    .fail(format!("Invalid shared_secret: {e}"))
                    .await;
                return errors::flat(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid shared_secret: {e}"),
                )
                .into_response();
            }
        };

    // Decrypt and verify
    let compressed = match handoff::decrypt_state(&blob, &secret_bytes) {
        Ok(p) => p,
        Err(e) => {
            state.handoff_session.fail(e.clone()).await;

            // Audit: failed restore (potential tampering or wrong key)
            handoff_event(
                &state,
                "handoff:restore:failed",
                &format!("decryption_error={e}"),
            )
            .await;

            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "State decryption/verification failed",
                    "detail": e,
                })),
            )
                .into_response();
        }
    };

    // Compute verification hash of restored compressed bytes (matches source's hash)
    let restored_hash = handoff::compute_verification_hash(&compressed);
    state
        .handoff_session
        .set_restored_verification_hash(restored_hash)
        .await;

    // Deserialize
    let mut restored_state = match handoff::deserialize_state(&compressed) {
        Ok(s) => s,
        Err(e) => {
            state.handoff_session.fail(e.clone()).await;
            return errors::flat(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("State deserialization failed: {e}"),
            )
            .into_response();
        }
    };

    // Version check
    if restored_state.version > handoff::HANDOFF_STATE_VERSION {
        let msg = format!(
            "State version {} is newer than supported ({})",
            restored_state.version,
            handoff::HANDOFF_STATE_VERSION
        );
        state.handoff_session.fail(msg.clone()).await;
        return errors::flat(StatusCode::UNPROCESSABLE_ENTITY, msg).into_response();
    }

    // ── §9.9.4: State blob size/DoS limits ──────────────────────────────────

    if compressed.len() > handoff::MAX_BLOB_SIZE_BYTES {
        let msg = format!(
            "State blob too large: {}MB (max {}MB)",
            compressed.len() / (1024 * 1024),
            handoff::MAX_BLOB_SIZE_BYTES / (1024 * 1024)
        );
        state.handoff_session.fail(msg.clone()).await;
        handoff_event(
            &state,
            "handoff:restore:rejected",
            &format!("blob_too_large size={}B", compressed.len()),
        )
        .await;
        return errors::flat(StatusCode::PAYLOAD_TOO_LARGE, msg).into_response();
    }

    // Workspace tar size check
    if restored_state.workspace_tar.len() > handoff::MAX_BLOB_SIZE_BYTES {
        let msg = "Workspace tar exceeds size limit";
        state.handoff_session.fail(msg.into()).await;
        return errors::flat(StatusCode::PAYLOAD_TOO_LARGE, msg).into_response();
    }

    // ── §9.9.1: State blob prompt injection protections ─────────────────────

    // 1. Sanitize chat history — strip messages that look like system prompt injections
    if let Some(ref chat_bytes) = restored_state.chat_snapshot {
        let sanitized = handoff::sanitize_chat_snapshot(chat_bytes);
        let original_len = chat_bytes.len();
        let sanitized_len = sanitized.len();
        if original_len != sanitized_len {
            tracing::warn!(
                original_bytes = original_len,
                sanitized_bytes = sanitized_len,
                "Chat snapshot sanitized — removed suspicious system-prompt patterns"
            );
            handoff_event(
                &state,
                "handoff:restore:sanitized",
                &format!("chat_sanitized original={original_len}B sanitized={sanitized_len}B"),
            )
            .await;
        }
        restored_state.chat_snapshot = Some(sanitized);
    }

    // 2. Restore trust scores — tagged as "transferred" (§9.9.10)
    let mut trust_count = 0u32;
    if let Some(scores) = restored_state.trust_scores.as_array() {
        for score_entry in scores {
            if let (Some(agent_id), Some(score)) = (
                score_entry.get("agent_id").and_then(|v| v.as_str()),
                score_entry.get("score").and_then(|v| v.as_u64()),
            ) {
                // Set transferred trust — capped at 750 (cannot import max trust)
                let capped_score = (score as u32).min(750);
                state.governance.trust.set_trust(agent_id, capped_score);
                trust_count += 1;
            }
        }
        if trust_count > 0 {
            tracing::info!(
                count = trust_count,
                "Restored trust scores from handoff (capped at 750, tagged as transferred)"
            );
        }
    }

    // Restore token budget usage
    state
        .budget
        .record_usage(
            &state.sandbox_name,
            restored_state.token_budget_used.total_tokens,
        )
        .await;

    // ── Re-spawn sub-agents from snapshot ──────────────────────────────────
    let mut sub_agent_results: Vec<serde_json::Value> = Vec::new();
    if !restored_state.sub_agent_snapshots.is_empty() {
        tracing::info!(
            count = restored_state.sub_agent_snapshots.len(),
            "Re-spawning sub-agents from handoff snapshot"
        );

        // Look up the new parent's AMID for trusted_peers remapping.
        // Sub-agents had the old parent AMID in their trusted_peers; we need
        // to replace it with the new parent's AMID so they trust us.
        let new_parent_amid = if let Ok(reg_url) = std::env::var("AGT_REGISTRY_URL") {
            lookup_parent_amid(&state.client, &reg_url, &state.sandbox_name).await
        } else {
            None
        };
        let old_parent_amid = &restored_state.predecessor_amid;

        for sub_snap in &restored_state.sub_agent_snapshots {
            let mut spawn_req = sub_snap.spawn_config.clone();
            // Clear handoff meta — this is a fresh spawn, not a handoff target
            spawn_req.handoff = None;

            // Remap trusted_peers: ensure new parent is trusted.
            // Docker snapshots have trusted_peers=None, so we MUST set it —
            // otherwise sub-agents reject workspace_inject/resume messages.
            if let Some(new_amid) = &new_parent_amid {
                let parent_name = &state.sandbox_name;
                let new_entry = format!("{parent_name}:{new_amid}");

                match &spawn_req.trusted_peers {
                    Some(peers) if !peers.is_empty() => {
                        // Remap old parent AMID → new parent AMID
                        if !old_parent_amid.is_empty() && peers.contains(old_parent_amid) {
                            spawn_req.trusted_peers =
                                Some(peers.replace(old_parent_amid, new_amid));
                            tracing::info!(
                                sub_agent = %sub_snap.agent_id,
                                old = %old_parent_amid,
                                new = %new_amid,
                                "Remapped parent AMID in sub-agent trusted_peers"
                            );
                        } else {
                            // Old parent not found — append new parent
                            spawn_req.trusted_peers = Some(format!("{peers},{new_entry}"));
                        }
                    }
                    _ => {
                        // No trusted_peers at all — set new parent as trusted
                        spawn_req.trusted_peers = Some(new_entry.clone());
                        tracing::info!(
                            sub_agent = %sub_snap.agent_id,
                            parent_amid = %new_amid,
                            "Set trusted_peers for sub-agent (was empty)"
                        );
                    }
                }
            }

            match spawn::create_sandbox(&state.sandbox_name, &spawn_req).await {
                Ok(resp) => {
                    tracing::info!(
                        sub_agent = %sub_snap.agent_id,
                        namespace = ?resp.namespace,
                        "Re-spawned sub-agent from handoff snapshot"
                    );
                    handoff_event(
                        &state,
                        "handoff:restore:sub-agent",
                        &format!(
                            "respawned={} original_amid={}",
                            sub_snap.agent_id, sub_snap.original_amid
                        ),
                    )
                    .await;
                    sub_agent_results.push(serde_json::json!({
                        "agent_id": sub_snap.agent_id,
                        "original_amid": sub_snap.original_amid,
                        "status": "spawned",
                        "namespace": resp.namespace,
                    }));
                }
                Err(e) => {
                    tracing::warn!(
                        sub_agent = %sub_snap.agent_id,
                        error = %e,
                        "Failed to re-spawn sub-agent — may already exist or quota exceeded"
                    );
                    sub_agent_results.push(serde_json::json!({
                        "agent_id": sub_snap.agent_id,
                        "original_amid": sub_snap.original_amid,
                        "status": "failed",
                        "error": e,
                    }));
                }
            }
        }
    }

    // ── Return restored data to the plugin for hydration ─────────────────
    // In AKS, the router and openclaw are separate containers — they don't
    // share a filesystem. Return workspace tar and chat snapshot in the
    // response body so the plugin (in the openclaw container) can write them.

    let workspace_tar_b64 = if !restored_state.workspace_tar.is_empty() {
        Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &restored_state.workspace_tar,
        ))
    } else {
        None
    };

    let chat_snapshot = restored_state
        .chat_snapshot
        .as_ref()
        .and_then(|bytes| String::from_utf8(bytes.clone()).ok());

    let restored_at = {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        format!("{}Z", d.as_secs())
    };

    // Audit log the restore
    handoff_event(
        &state,
        "handoff:restore",
        &format!(
            "from={} size={}B",
            restored_state.predecessor_amid,
            compressed.len()
        ),
    )
    .await;

    tracing::info!(
        from = %restored_state.predecessor_amid,
        to = %restored_state.successor_amid,
        direction = %restored_state.metadata.direction,
        "Handoff state restored successfully"
    );

    // Build sub-agent workspace payloads for plugin-side injection.
    // The plugin will wait for each sub-agent to come online and push
    // its workspace + task context via E2E mesh.
    tracing::info!(
        snapshot_count = restored_state.sub_agent_snapshots.len(),
        names = %restored_state.sub_agent_snapshots.iter()
            .map(|s| format!("{}(ws={}B,ctx={})", s.agent_id, s.workspace_tar.len(), s.task_context.len()))
            .collect::<Vec<_>>().join(", "),
        "Building sub_agent_workspaces from restored snapshots"
    );
    let sub_agent_workspaces: Vec<serde_json::Value> = restored_state
        .sub_agent_snapshots
        .iter()
        .filter(|s| !s.workspace_tar.is_empty() || !s.task_context.is_empty())
        .map(|s| {
            serde_json::json!({
                "agent_id": s.agent_id,
                "original_amid": s.original_amid,
                "workspace_tar": if s.workspace_tar.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &s.workspace_tar,
                    ))
                },
                "task_context": s.task_context,
                "status": s.status,
                "checkpoint": s.checkpoint,
            })
        })
        .collect();
    tracing::info!(
        workspace_count = sub_agent_workspaces.len(),
        "Sub-agent workspaces built for plugin response"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "restored": true,
            "agent_name": restored_state.agent_name,
            "predecessor_amid": restored_state.predecessor_amid,
            "successor_amid": restored_state.successor_amid,
            "direction": restored_state.metadata.direction.to_string(),
            "initiated_at": restored_state.metadata.initiated_at,
            "restored_at": restored_at,
            "trust_scores_count": trust_count,
            "trust_scores_capped_at": 750,
            "audit_entries_count": restored_state.audit_entries.len(),
            "sub_agent_snapshots": restored_state.sub_agent_snapshots.len(),
            "sub_agent_results": sub_agent_results,
            "sub_agent_workspaces": sub_agent_workspaces,
            "credentials": restored_state.credentials.len(),
            "phase": "restoring",
            // Payload for plugin-side hydration (workspace + chat)
            "workspace_tar": workspace_tar_b64,
            "chat_snapshot": chat_snapshot,
        })),
    )
        .into_response()
}

/// POST /agt/handoff/verify — return verification digest of current state.
pub(super) async fn handoff_verify(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = state
        .handoff_session
        .try_transition(handoff::HandoffPhase::Verifying)
        .await
    {
        return errors::flat(StatusCode::CONFLICT, e).into_response();
    }

    // Use the hash computed during restore (same compressed bytes as the source)
    // instead of building a new snapshot (which would have different timestamps,
    // nonces, hostnames, etc. and never match).
    let verification_hash = match state.handoff_session.restored_verification_hash().await {
        Some(h) => h,
        None => {
            // Fallback for source-side verify (no restore happened here)
            let direction = state
                .handoff_session
                .status()
                .await
                .direction
                .unwrap_or(handoff::HandoffDirection::LocalToAks);
            let predecessor = body
                .get("predecessor_amid")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let successor = body
                .get("successor_amid")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match handoff::build_snapshot(&state, direction, predecessor, successor).await {
                Ok(s) => match handoff::serialize_state(&s) {
                    Ok(c) => handoff::compute_verification_hash(&c),
                    Err(e) => {
                        return errors::flat(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
                    }
                },
                Err(e) => {
                    return errors::flat(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
                }
            }
        }
    };

    let expected = body.get("expected_hash").and_then(|v| v.as_str());
    let matches = expected.map(|e| e == verification_hash);

    let session_status = state.handoff_session.status().await;

    handoff_event(
        &state,
        "handoff:verify",
        &format!(
            "hash={} match={}",
            &verification_hash[..16],
            matches.map(|m| m.to_string()).unwrap_or("n/a".into())
        ),
    )
    .await;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "verification_hash": verification_hash,
            "matches": matches,
            "trust_scores_count": session_status.snapshot_items.as_ref().map(|i| i.trust_scores).unwrap_or(0),
            "audit_entries_count": session_status.snapshot_items.as_ref().map(|i| i.audit_entries).unwrap_or(0),
            "phase": "verifying",
        })),
    )
        .into_response()
}
