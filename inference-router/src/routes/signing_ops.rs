//! Thin helpers routing signing call-sites through the `SigningProvider`
//! trait, so the legacy `state.governance.identity.sign(...)` path can be
//! migrated one site at a time without breaking unrelated code.
//!
//! Failure policy: signing is an in-process operation on a live key, so
//! unlike audit/policy it should not "fail soft". A signing error here
//! means the agent's own identity is broken, which is a genuine 5xx and
//! must bubble up to the caller. These helpers return the raw signature
//! bytes so each route can encode (base64/hex/multibase) as it sees fit.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;

use crate::providers::{KeyRef, SigningError};
use crate::routes::AppState;

/// Sign `payload` with the agent's default identity key. Returns the
/// 64-byte Ed25519 signature. Only returns `Err` if the signing backend
/// itself failed — a corruption-level condition.
pub(crate) async fn sign_default(
    state: &AppState,
    payload: &[u8],
) -> Result<Vec<u8>, SigningError> {
    let kref = KeyRef(crate::providers::DEFAULT_KEY_REF.to_string());
    let sig = state.signing_provider.sign(&kref, payload).await?;
    Ok(sig.0)
}

fn sign_err_response(e: SigningError) -> Response {
    tracing::error!(error = %e, "signing backend failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": "signing backend unavailable"})),
    )
        .into_response()
}

/// Signs `payload` and returns the base64-STANDARD-encoded signature in
/// one call. On signing failure, returns a ready-to-return 500 response so
/// handlers can `match { Ok(s) => s, Err(r) => return r }` in two lines.
pub(crate) async fn sign_default_b64_or_response(
    state: &AppState,
    payload: &[u8],
) -> Result<String, Response> {
    let bytes = sign_default(state, payload)
        .await
        .map_err(sign_err_response)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}
