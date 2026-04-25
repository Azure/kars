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
use super::audit_events::{handoff_event, handoff_init as audit_handoff_init};
use super::mesh::lookup_parent_amid;
use crate::config::RegistryMode;
use crate::errors;
use crate::handoff;
use crate::spawn;

mod payload;

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
        .route("/agt/handoff/snapshot", post(payload::handoff_snapshot))
        .route("/agt/handoff/restore", post(payload::handoff_restore))
        .route("/agt/handoff/verify", post(payload::handoff_verify))
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

    audit_handoff_init(&state, &state.sandbox_name, &token_hash).await;

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

    handoff_event(
        &state,
        "handoff:abort",
        &format!("aborted_from_phase={current_phase}"),
    )
    .await;

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
/// Router signs the canonical succession message with its Ed25519 key (via the
/// `SigningProvider` seam) and forwards the full request to the registry so the
/// private key never leaves the sandbox. Request body: `{ "successor_amid":
/// "...", "reason": "handoff" }`.
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
    let signature_b64 = match crate::routes::signing_ops::sign_default_b64_or_response(
        &state,
        canonical_message.as_bytes(),
    )
    .await
    {
        Ok(s) => s,
        Err(r) => return r,
    };

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

            handoff_event(
                &state,
                "handoff:succession",
                &format!(
                    "predecessor={predecessor_amid} successor={successor_amid} registry_status={status}"
                ),
            )
            .await;

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
/// Called by `azureclaw_handoff_request`; mints a confirmation token the user
/// must echo back. Rate limited: max 1 request / 5 min.
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
            handoff_event(
                &state,
                "handoff:pending",
                &format!("direction={direction} reason={reason}"),
            )
            .await;

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

            handoff_event(&state, "handoff:pending:rejected", &format!("{e}")).await;

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

            handoff_event(
                &state,
                "handoff:confirmed",
                &format!(
                    "direction={direction} reason={reason} token_hash={}",
                    &token_hash[..16]
                ),
            )
            .await;

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

            handoff_event(&state, "handoff:confirm:rejected", &format!("{e}")).await;

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
