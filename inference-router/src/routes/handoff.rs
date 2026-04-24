//! handoff route handlers and router builders.
//!
//! Extracted from `routes/mod.rs` as part of the Q1 split.
//! Function bodies are byte-identical to the originals (verified by
//! `item-manifest` drift-check).

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};

use super::AppState;
use super::mesh::lookup_parent_amid;
use crate::config::RegistryMode;
use crate::errors;
use crate::handoff;
use crate::spawn;

pub fn spawn_routes() -> Router<AppState> {
    Router::new()
        .route("/sandbox/spawn", post(sandbox_spawn))
        .route("/sandbox/list", get(sandbox_list))
        .route("/sandbox/{name}/status", get(sandbox_status))
        .route("/sandbox/{name}", axum::routing::delete(sandbox_delete))
}

/// POST /sandbox/spawn — create a new sub-agent sandbox.
async fn sandbox_spawn(
    State(state): State<AppState>,
    Json(req): Json<spawn::SpawnRequest>,
) -> impl IntoResponse {
    let parent_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());
    if let Err(resp) =
        crate::routes::spawn_policy::check_sandbox_spawn(&state, &parent_name, &req.agent_id).await
    {
        return resp;
    }
    match spawn::create_sandbox(&parent_name, &req).await {
        Ok(resp) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(resp).unwrap()),
        )
            .into_response(),
        Err(msg) => errors::flat(StatusCode::BAD_REQUEST, msg).into_response(),
    }
}

/// GET /sandbox/list — list sub-agents spawned by this sandbox.
async fn sandbox_list(State(_state): State<AppState>) -> impl IntoResponse {
    let parent_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());
    let is_dev = std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true";

    let result = if is_dev {
        spawn::list_sandboxes_docker(&parent_name).await
    } else {
        spawn::list_sandboxes(&parent_name).await
    };

    match result {
        Ok(entries) => Json(serde_json::json!({
            "parent": parent_name,
            "count": entries.len(),
            "sandboxes": entries,
        })),
        Err(msg) => Json(serde_json::json!({
            "parent": parent_name,
            "error": msg,
            "count": 0,
            "sandboxes": [],
        })),
    }
}

/// GET /sandbox/{name}/status — get status of a specific sub-agent.
async fn sandbox_status(Path(name): Path<String>) -> impl IntoResponse {
    match spawn::get_sandbox_status(&name).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(msg) => errors::flat(StatusCode::NOT_FOUND, msg).into_response(),
    }
}

/// DELETE /sandbox/{name} — tear down a sub-agent sandbox.
async fn sandbox_delete(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let parent_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());
    let is_dev = std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true";

    let result = if is_dev {
        spawn::delete_sandbox_docker(&parent_name, &name).await
    } else {
        spawn::delete_sandbox(&parent_name, &name).await
    };

    match result {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response(),
        Err(msg) => errors::flat(StatusCode::FORBIDDEN, msg).into_response(),
    }
}

// ==========================================================================
// Handoff routes — agent live migration (local ↔ cloud)
// ==========================================================================

/// Handoff init route — admin token only (no handoff token yet).
pub fn handoff_init_routes() -> Router<AppState> {
    Router::new().route("/agt/handoff/init", post(handoff_init_handler))
}

/// Handoff mutation routes — require BOTH admin token AND handoff token.
/// NO localhost bypass (critical for prompt injection protection).
/// Body limit raised to 50 MB to accommodate encrypted state snapshots.
pub fn handoff_protected_routes() -> Router<AppState> {
    Router::new()
        .route("/agt/handoff/snapshot", post(handoff_snapshot))
        .route("/agt/handoff/restore", post(handoff_restore))
        .route("/agt/handoff/verify", post(handoff_verify))
        .route("/agt/handoff/drain", post(handoff_drain))
        .route("/agt/handoff/decommission", post(handoff_decommission))
        .route("/agt/handoff/abort", post(handoff_abort))
        .route("/agt/handoff/succession", post(handoff_succession))
        .layer(DefaultBodyLimit::max(handoff::MAX_BLOB_SIZE_BYTES))
}

/// Handoff status route — admin token required, localhost allowed (read-only).
pub fn handoff_status_routes() -> Router<AppState> {
    Router::new()
        .route("/agt/handoff/status", get(handoff_status))
        .route("/agt/handoff/sub-agents", get(handoff_sub_agents))
        // Two-stage confirmation gate (§9.9.9) — localhost allowed (agent tool calls these)
        .route("/agt/handoff/pending", post(handoff_pending))
        .route("/agt/handoff/confirm", post(handoff_confirm))
        .route("/agt/handoff/resume", post(handoff_resume))
}

/// POST /agt/handoff/init — create a one-time handoff token.
///
/// Only the CLI calls this. The token is stored in memory (never persisted).
/// Must be called before any other handoff endpoint (except status).
async fn handoff_init_handler(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    // ── Registry mode guard ──────────────────────────────────────────────────
    // Handoff requires a global registry — both agents must be in the same
    // registry for identity succession to work.
    if state.config.registry_mode == RegistryMode::Local {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Handoff requires a global registry. Start with --global-registry <url> to enable handoff.",
                "registry_mode": "local",
                "hint": "Run `azureclaw dev --global-registry <url>` or `azureclaw up --global-registry <url>`"
            })),
        )
            .into_response();
    }

    // Check if a handoff can be started
    if !state.handoff_session.can_start().await {
        let current = state.handoff_session.status().await;
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": format!("Handoff already in progress (phase: {})", current.phase),
                "phase": current.phase,
            })),
        )
            .into_response();
    }

    let ttl_secs = body
        .get("ttl_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(handoff::DEFAULT_TOKEN_TTL_SECS);

    let direction = match body.get("direction").and_then(|v| v.as_str()) {
        Some("aks_to_local") => handoff::HandoffDirection::AksToLocal,
        _ => handoff::HandoffDirection::LocalToAks,
    };

    // Validate direction vs environment (warn-only, don't block)
    let is_dev = std::env::var("AZURECLAW_DEV_MODE").unwrap_or_default() == "true";
    let expected = if is_dev {
        handoff::HandoffDirection::AksToLocal
    } else {
        handoff::HandoffDirection::LocalToAks
    };
    if direction != expected {
        tracing::warn!(
            direction = %direction,
            expected = %expected,
            is_dev,
            "Handoff direction does not match environment — proceeding with caution"
        );
    }

    let predecessor_amid = body
        .get("predecessor_amid")
        .and_then(|v| v.as_str())
        .map(String::from);

    let (token, token_hash) = state.handoff_tokens.create_token(ttl_secs).await;

    state
        .handoff_session
        .initialize(direction, predecessor_amid)
        .await;

    state.governance.audit.log(
        &state.sandbox_name,
        "handoff:init",
        &format!("token_hash={}", &token_hash[..16]),
    );

    tracing::info!(
        token_hash = &token_hash[..16],
        ttl_secs,
        direction = %direction,
        "Handoff initialized — token created"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "handoff_token": token,
            "token_hash": token_hash,
            "ttl_seconds": ttl_secs,
            "direction": direction.to_string(),
            "phase": "initialized",
        })),
    )
        .into_response()
}

/// POST /agt/handoff/snapshot — serialize and encrypt agent state.
///
/// Returns an encrypted blob that can be transferred to the target agent.
async fn handoff_snapshot(
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
    state.governance.audit.log(
        &state.sandbox_name,
        "handoff:snapshot",
        &format!(
            "size={}B hash={}",
            compressed.len(),
            &verification_hash[..16]
        ),
    );

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
async fn handoff_restore(
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
            state.governance.audit.log(
                &state.sandbox_name,
                "handoff:restore:failed",
                &format!("decryption_error={e}"),
            );

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
        state.governance.audit.log(
            &state.sandbox_name,
            "handoff:restore:rejected",
            &format!("blob_too_large size={}B", compressed.len()),
        );
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
            state.governance.audit.log(
                &state.sandbox_name,
                "handoff:restore:sanitized",
                &format!(
                    "chat_sanitized original={}B sanitized={}B",
                    original_len, sanitized_len
                ),
            );
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
                    state.governance.audit.log(
                        &state.sandbox_name,
                        "handoff:restore:sub-agent",
                        &format!(
                            "respawned={} original_amid={}",
                            sub_snap.agent_id, sub_snap.original_amid
                        ),
                    );
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
    state.governance.audit.log(
        &state.sandbox_name,
        "handoff:restore",
        &format!(
            "from={} size={}B",
            restored_state.predecessor_amid,
            compressed.len()
        ),
    );

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
async fn handoff_verify(
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

    state.governance.audit.log(
        &state.sandbox_name,
        "handoff:verify",
        &format!(
            "hash={} match={}",
            &verification_hash[..16],
            matches.map(|m| m.to_string()).unwrap_or("n/a".into())
        ),
    );

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

/// POST /agt/handoff/drain — enter drain mode (stop new work, complete in-flight).
async fn handoff_drain(State(state): State<AppState>) -> impl IntoResponse {
    if state.drain_state.is_draining().await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Already in drain mode",
                "drain_duration_secs": state.drain_state.drain_duration().await
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            })),
        )
            .into_response();
    }

    state.drain_state.start_drain().await;
    if let Err(e) = state
        .handoff_session
        .try_transition(handoff::HandoffPhase::Draining)
        .await
    {
        state.drain_state.stop_drain().await;
        return errors::flat(StatusCode::CONFLICT, e).into_response();
    }

    state
        .governance
        .audit
        .log(&state.sandbox_name, "handoff:drain", "drain_started");

    tracing::info!("Handoff: entering drain mode");

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "draining": true,
            "phase": "draining",
        })),
    )
        .into_response()
}

/// POST /agt/handoff/decommission — deregister from relay, enter dormant state.
async fn handoff_decommission(State(state): State<AppState>) -> impl IntoResponse {
    if let Err(e) = state
        .handoff_session
        .try_transition(handoff::HandoffPhase::Decommissioning)
        .await
    {
        return errors::flat(StatusCode::CONFLICT, e).into_response();
    }

    // Stop drain if still active
    state.drain_state.stop_drain().await;

    // Revoke the handoff token
    state.handoff_tokens.revoke().await;

    state
        .governance
        .audit
        .log(&state.sandbox_name, "handoff:decommission", "agent_dormant");

    // Mark session complete
    state.handoff_session.complete().await;

    tracing::info!("Handoff: agent decommissioned (dormant)");

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "decommissioned": true,
            "phase": "complete",
            "agent_name": state.sandbox_name.as_ref(),
        })),
    )
        .into_response()
}

/// POST /agt/handoff/abort — cancel an in-progress handoff.
async fn handoff_abort(State(state): State<AppState>) -> impl IntoResponse {
    let current_phase = state.handoff_session.phase().await;

    if matches!(
        current_phase,
        handoff::HandoffPhase::Idle
            | handoff::HandoffPhase::Complete
            | handoff::HandoffPhase::Aborted
    ) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "No handoff in progress to abort",
                "phase": current_phase.to_string(),
            })),
        )
            .into_response();
    }

    // Stop drain if active
    state.drain_state.stop_drain().await;

    // Revoke handoff token
    state.handoff_tokens.revoke().await;

    // Mark aborted
    state.handoff_session.abort().await;

    state.governance.audit.log(
        &state.sandbox_name,
        "handoff:abort",
        &format!("aborted_from_phase={current_phase}"),
    );

    tracing::info!(
        from_phase = %current_phase,
        "Handoff aborted"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "aborted": true,
            "previous_phase": current_phase.to_string(),
            "phase": "aborted",
        })),
    )
        .into_response()
}

/// POST /agt/handoff/succession — sign and submit identity succession to registry.
///
/// The predecessor router signs the canonical succession message using its private
/// Ed25519 key and forwards the complete request to the registry. This avoids
/// exposing the private key to the CLI.
///
/// Request body: `{ "successor_amid": "...", "reason": "handoff" }`
/// The router resolves its own AMID + signing key, plus the successor's key,
/// by querying the registry.
async fn handoff_succession(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let successor_amid = match body.get("successor_amid").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => {
            return errors::flat(
                StatusCode::BAD_REQUEST,
                "Missing required field: successor_amid",
            )
            .into_response();
        }
    };

    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("handoff")
        .to_string();

    let registry_url = match std::env::var("AGT_REGISTRY_URL") {
        Ok(url) => url,
        Err(_) => {
            return errors::flat(
                StatusCode::SERVICE_UNAVAILABLE,
                "AGT_REGISTRY_URL not configured — cannot perform succession",
            )
            .into_response();
        }
    };

    let base = registry_url.trim_end_matches('/');

    // Look up our own AMID from the registry
    let predecessor_amid =
        match lookup_parent_amid(&state.client, &registry_url, &state.sandbox_name).await {
            Some(amid) => amid,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "Could not find predecessor (self) in registry",
                        "sandbox_name": *state.sandbox_name,
                    })),
                )
                    .into_response();
            }
        };

    // Get our signing public key in registry format: "ed25519:<base64>"
    let predecessor_signing_key = format!(
        "ed25519:{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            state.governance.identity.public_key.as_bytes(),
        )
    );

    // Look up successor's signing key from registry
    let successor_signing_key = match state
        .client
        .get(&format!("{}/v1/registry/lookup/{}", base, successor_amid))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => match v.get("signing_public_key").and_then(|k| k.as_str()) {
                Some(key) => key.to_string(),
                None => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({
                            "error": "Successor agent has no signing_public_key in registry",
                            "successor_amid": successor_amid,
                        })),
                    )
                        .into_response();
                }
            },
            Err(e) => {
                return errors::flat(
                    StatusCode::BAD_GATEWAY,
                    format!("Failed to parse registry lookup response: {}", e),
                )
                .into_response();
            }
        },
        Ok(resp) => {
            return errors::flat(
                StatusCode::NOT_FOUND,
                format!(
                    "Successor {} not found in registry (status {})",
                    successor_amid,
                    resp.status()
                ),
            )
            .into_response();
        }
        Err(e) => {
            return errors::flat(
                StatusCode::BAD_GATEWAY,
                format!("Failed to reach registry for successor lookup: {}", e),
            )
            .into_response();
        }
    };

    // Build canonical message and sign
    let timestamp = {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = d.as_secs();
        let millis = d.subsec_millis();
        // Manual RFC 3339 formatting (no chrono dependency)
        let days = secs / 86400;
        let rem = secs % 86400;
        let hours = rem / 3600;
        let minutes = (rem % 3600) / 60;
        let seconds = rem % 60;
        // Days since epoch to Y-M-D (algorithm from Howard Hinnant)
        let z = days as i64 + 719468;
        let era = z / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d_val = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y_val = if m <= 2 { y + 1 } else { y };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            y_val, m, d_val, hours, minutes, seconds, millis
        )
    };
    let canonical_message = format!(
        "succession:{}:{}:{}",
        predecessor_amid, successor_amid, timestamp
    );
    let signature_bytes = state.governance.identity.sign(canonical_message.as_bytes());
    let signature_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &signature_bytes);

    // Submit to registry
    let succession_request = serde_json::json!({
        "predecessor_amid": predecessor_amid,
        "predecessor_signing_key": predecessor_signing_key,
        "successor_amid": successor_amid,
        "successor_signing_key": successor_signing_key,
        "reason": reason,
        "timestamp": timestamp,
        "signature": signature_b64,
    });

    tracing::info!(
        predecessor = %predecessor_amid,
        successor = %successor_amid,
        reason = %reason,
        "Submitting signed succession to registry"
    );

    match state
        .client
        .post(&format!("{}/v1/registry/succession", base))
        .json(&succession_request)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.json::<serde_json::Value>().await.unwrap_or_else(
                |_| serde_json::json!({"error": "Failed to parse registry response"}),
            );

            state.governance.audit.log(
                &state.sandbox_name,
                "handoff:succession",
                &format!(
                    "predecessor={} successor={} registry_status={}",
                    predecessor_amid, successor_amid, status
                ),
            );

            // Forward the registry's status code
            let axum_status =
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

            (axum_status, Json(body)).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to submit succession to registry: {}", e);
            errors::flat(
                StatusCode::BAD_GATEWAY,
                format!("Failed to reach registry: {}", e),
            )
            .into_response()
        }
    }
}

/// POST /agt/handoff/resume — resume from a drained/aborted state.
async fn handoff_resume(State(state): State<AppState>) -> impl IntoResponse {
    match state.handoff_session.resume().await {
        Ok(()) => {
            state
                .governance
                .audit
                .log(&state.sandbox_name, "handoff:resume", "resumed_to_idle");

            tracing::info!("Handoff resumed to idle");

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "resumed",
                    "phase": "idle",
                })),
            )
                .into_response()
        }
        Err(msg) => errors::flat(StatusCode::CONFLICT, msg).into_response(),
    }
}

/// GET /agt/handoff/status — read-only handoff status.
/// POST /agt/handoff/pending — create a pending handoff request (§9.9.9 Stage 1).
///
/// Called by the agent tool (`azureclaw_handoff_request`). Generates a confirmation
/// token that the user must echo back to confirm.
///
/// Rate limited: max 1 request per 5 minutes.
async fn handoff_pending(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    // Registry mode guard
    if state.config.registry_mode == RegistryMode::Local {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Handoff requires global registry mode",
                "registry_mode": "local",
                "hint": "Start with --global-registry <url>"
            })),
        )
            .into_response();
    }

    // Prevent creating a pending request while a handoff is already in progress
    if !state.handoff_session.can_start().await {
        let current = state.handoff_session.status().await;
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": format!("Handoff already in progress (phase: {})", current.phase),
                "phase": current.phase,
            })),
        )
            .into_response();
    }

    let direction = match body.get("direction").and_then(|v| v.as_str()) {
        Some("local") | Some("aks_to_local") => handoff::HandoffDirection::AksToLocal,
        _ => handoff::HandoffDirection::LocalToAks,
    };

    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("user_requested")
        .to_string();

    match state
        .pending_handoff
        .create_pending(direction, reason.clone())
        .await
    {
        Ok(token) => {
            state.governance.audit.log(
                &state.sandbox_name,
                "handoff:pending",
                &format!("direction={direction} reason={reason}"),
            );

            tracing::info!(
                direction = %direction,
                reason = %reason,
                "Handoff pending — confirmation token generated"
            );

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "pending_confirmation",
                    "confirmation_token": token,
                    "direction": direction.to_string(),
                    "reason": reason,
                    "expires_in_secs": handoff::PENDING_HANDOFF_TTL_SECS,
                    "min_confirm_delay_secs": handoff::CONFIRMATION_MIN_DELAY_SECS,
                    "instruction": format!(
                        "Ask the user to confirm handoff by replying with code: {token}"
                    ),
                })),
            )
                .into_response()
        }
        Err(e) => {
            let status = match &e {
                handoff::PendingHandoffError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::BAD_REQUEST,
            };

            state.governance.audit.log(
                &state.sandbox_name,
                "handoff:pending:rejected",
                &format!("{e}"),
            );

            errors::flat(status, e.to_string()).into_response()
        }
    }
}

/// POST /agt/handoff/confirm — confirm a pending handoff request (§9.9.9 Stage 2).
///
/// Validates the confirmation token, enforces minimum delay (3s), and on success
/// generates the real handoff token (Layer 1) and initializes the handoff session.
///
/// This is the bridge: user-confirmed intent → actual handoff execution.
async fn handoff_confirm(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    // Registry mode guard (defense in depth — /pending is also gated)
    if state.config.registry_mode == RegistryMode::Local {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Handoff requires global registry mode",
                "registry_mode": "local",
            })),
        )
            .into_response();
    }

    let token = match body.get("confirmation_token").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            return errors::flat(StatusCode::BAD_REQUEST, "confirmation_token is required")
                .into_response();
        }
    };

    match state.pending_handoff.confirm(token).await {
        Ok((direction, reason)) => {
            // Confirmation successful — now create the real handoff token
            let ttl_secs = body
                .get("ttl_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(handoff::DEFAULT_TOKEN_TTL_SECS);

            let (handoff_token, token_hash) = state.handoff_tokens.create_token(ttl_secs).await;

            let predecessor_amid = body
                .get("predecessor_amid")
                .and_then(|v| v.as_str())
                .map(String::from);

            state
                .handoff_session
                .initialize(direction, predecessor_amid)
                .await;

            state.governance.audit.log(
                &state.sandbox_name,
                "handoff:confirmed",
                &format!(
                    "direction={direction} reason={reason} token_hash={}",
                    &token_hash[..16]
                ),
            );

            tracing::info!(
                direction = %direction,
                token_hash = &token_hash[..16],
                "Handoff confirmed by user — token created"
            );

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "confirmed",
                    "handoff_token": handoff_token,
                    "token_hash": token_hash,
                    "ttl_seconds": ttl_secs,
                    "direction": direction.to_string(),
                    "reason": reason,
                    "phase": "initialized",
                })),
            )
                .into_response()
        }
        Err(e) => {
            let status = match &e {
                handoff::PendingHandoffError::TooFast { .. } => StatusCode::TOO_MANY_REQUESTS,
                handoff::PendingHandoffError::InvalidToken => StatusCode::UNAUTHORIZED,
                handoff::PendingHandoffError::Expired => StatusCode::GONE,
                handoff::PendingHandoffError::NoPending => StatusCode::NOT_FOUND,
                _ => StatusCode::BAD_REQUEST,
            };

            let is_too_fast = matches!(e, handoff::PendingHandoffError::TooFast { .. });

            state.governance.audit.log(
                &state.sandbox_name,
                "handoff:confirm:rejected",
                &format!("{e}"),
            );

            if is_too_fast {
                tracing::warn!(
                    error = %e,
                    "Handoff confirm rejected — possible LLM self-confirm attempt"
                );
            }

            errors::flat(status, e.to_string()).into_response()
        }
    }
}

/// GET /agt/handoff/sub-agents — collect sub-agent snapshots for handoff.
///
/// Lists active sub-agents spawned by this agent and reconstructs their
/// SpawnRequest config from the CRD spec. Used by the CLI to include
/// sub-agent state in the handoff snapshot.
async fn handoff_sub_agents(State(state): State<AppState>) -> impl IntoResponse {
    match spawn::collect_sub_agent_snapshots(&state.sandbox_name).await {
        Ok(snapshots) => {
            tracing::info!(
                count = snapshots.len(),
                "Collected sub-agent snapshots for handoff"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "sub_agent_snapshots": snapshots,
                    "count": snapshots.len(),
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!("Failed to collect sub-agent snapshots: {}", e);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "sub_agent_snapshots": [],
                    "count": 0,
                    "warning": format!("Could not collect sub-agents: {}", e),
                })),
            )
                .into_response()
        }
    }
}

async fn handoff_status(State(state): State<AppState>) -> impl IntoResponse {
    let session = state.handoff_session.status().await;
    let token_active = state.handoff_tokens.is_active().await;
    let draining = state.drain_state.is_draining().await;
    let drain_duration = state
        .drain_state
        .drain_duration()
        .await
        .map(|d| d.as_secs());

    let pending = state.pending_handoff.status().await;

    Json(serde_json::json!({
        "phase": session.phase,
        "direction": session.direction,
        "started_at": session.started_at,
        "predecessor_amid": session.predecessor_amid,
        "successor_amid": session.successor_amid,
        "snapshot_size_bytes": session.snapshot_size_bytes,
        "snapshot_items": session.snapshot_items,
        "error": session.error,
        "handoff_token_active": token_active,
        "draining": draining,
        "drain_duration_secs": drain_duration,
        "registry_mode": state.config.registry_mode.to_string(),
        "handoff_available": state.config.registry_mode == RegistryMode::Global,
        "pending_confirmation": pending,
    }))
}
