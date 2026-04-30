//! `POST /agt/handoff/succession` handler.
//!
//! Extracted from `handoff/mod.rs` in S15.h
//! (`phase2-hotspot-handoff-router`) so that mod.rs lands under the
//! §4.2 cap of 800 LOC. Body byte-identical to the previous inline
//! function; only `pub(super)` visibility added so the routes table
//! in `mod.rs` can still reference it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use super::AppState;
use crate::errors;
use crate::routes::audit_events::handoff_event;
use crate::routes::mesh::lookup_parent_amid;

/// POST /agt/handoff/succession — sign and submit identity succession to registry.
/// Router signs the canonical succession message with its Ed25519 key (via the
/// `SigningProvider` seam) and forwards the full request to the registry so the
/// private key never leaves the sandbox. Request body: `{ "successor_amid":
/// "...", "reason": "handoff" }`.
pub(super) async fn handoff_succession(
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
