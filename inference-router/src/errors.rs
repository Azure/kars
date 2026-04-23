//! Centralised error-response constructors for the inference router.
//!
//! The router exposes two distinct error shapes on the wire:
//!
//! 1. **OpenAI-compatible** (`{"error": {"message": "...", "type": "..."}}`) —
//!    used on endpoints that proxy to OpenAI-shaped inference APIs
//!    (`/v1/chat/completions`, `/v1/images/*`, model listing, token-budget
//!    denials on those paths). The shape is a hard wire contract: upstream
//!    clients (OpenClaw, the OpenAI SDK) pattern-match on it.
//!
//! 2. **Flat** (`{"error": "string"}`) — used on everything else: admin
//!    endpoints, trust registry, handoff, mesh federation, egress proxy,
//!    sandbox spawn/destroy. This shape predates any wire contract; it is
//!    consumed only by the plugin and the CLI.
//!
//! These helpers DO NOT change the wire format — they emit byte-identical
//! JSON to the prior hand-written `serde_json::json!` callsites. Their
//! purpose is to make the shape choice a conscious, searchable decision
//! instead of an ad-hoc repetition.
//!
//! Callsites that embed extra fields on top of either shape (e.g. `code`,
//! `domain`, `action`) must continue to hand-build the JSON — there are too
//! few of them to justify a builder, and making the helpers extensible
//! would defeat the "one obvious shape per call" goal.
//!
//! See `docs/threat-model.md` for the list of endpoints per shape.

use axum::{Json, http::StatusCode};
use serde_json::{Value, json};

// ---- OpenAI-compatible error `type` constants -----------------------------
//
// The set below reflects the `type` strings actually emitted on the wire
// today. Add a new constant only when you introduce a genuinely new error
// category — reusing an existing one is preferred so SDK consumers can
// pattern-match on a stable vocabulary.

/// Upstream (Azure AI Foundry / OpenAI endpoint) was unreachable or
/// returned a connection-level error.
pub const PROXY_ERROR: &str = "proxy_error";

/// Router could not obtain or refresh a token for the upstream call.
pub const AUTH_ERROR: &str = "auth_error";

/// Per-agent token budget exhausted before the request could be forwarded.
pub const TOKEN_BUDGET_EXCEEDED: &str = "token_budget_exceeded";

// ---- Flat shape -----------------------------------------------------------

/// Build a `(StatusCode, Json<Value>)` response with the flat shape:
/// `{"error": "<msg>"}`.
///
/// Use this for admin / trust / handoff / mesh / egress / spawn endpoints.
/// Axum's `IntoResponse` impl for tuples turns the return value directly
/// into an HTTP response, so callsites can `return flat(StatusCode::X, ...)`.
pub fn flat(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": msg.into() })))
}

// ---- OpenAI-compatible shape ---------------------------------------------

/// Build a `(StatusCode, Json<Value>)` response with the OpenAI shape:
/// `{"error": {"message": "<msg>", "type": "<type>"}}`.
///
/// Use this ONLY on endpoints that already speak the OpenAI error shape.
/// Switching any other endpoint to this shape is a wire-format change and
/// must be discussed separately.
pub fn openai(
    status: StatusCode,
    msg: impl Into<String>,
    type_: &str,
) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({
            "error": {
                "message": msg.into(),
                "type": type_,
            }
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Pin the exact flat shape. Any future edit that adds or removes a
    /// top-level field will trip this test — that is deliberate, because
    /// the plugin parses this body directly.
    #[test]
    fn flat_shape_is_stable() {
        let (status, Json(body)) = flat(StatusCode::BAD_REQUEST, "oops");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let obj = body.as_object().expect("object");
        assert_eq!(obj.len(), 1, "flat error body must have exactly one key");
        assert_eq!(obj.get("error").and_then(Value::as_str), Some("oops"));
    }

    /// Pin the OpenAI shape. The `message` and `type` keys are a hard
    /// contract with OpenClaw and the OpenAI SDK.
    #[test]
    fn openai_shape_is_stable() {
        let (status, Json(body)) =
            openai(StatusCode::BAD_GATEWAY, "upstream timed out", PROXY_ERROR);
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let err = body
            .get("error")
            .and_then(Value::as_object)
            .expect("error object");
        assert_eq!(err.len(), 2, "openai error must have exactly message+type");
        assert_eq!(
            err.get("message").and_then(Value::as_str),
            Some("upstream timed out")
        );
        assert_eq!(err.get("type").and_then(Value::as_str), Some("proxy_error"));
    }

    #[test]
    fn flat_accepts_owned_and_borrowed_strings() {
        let (_, Json(a)) = flat(StatusCode::BAD_REQUEST, "literal");
        let owned: String = "owned".to_string();
        let (_, Json(b)) = flat(StatusCode::BAD_REQUEST, owned);
        assert_eq!(a.get("error").and_then(Value::as_str), Some("literal"));
        assert_eq!(b.get("error").and_then(Value::as_str), Some("owned"));
    }

    /// Byte-exact pin so future serde/serde_json changes can't silently
    /// reorder fields on the wire.
    #[test]
    fn openai_is_byte_exact() {
        let (_, Json(body)) = openai(StatusCode::BAD_REQUEST, "x", PROXY_ERROR);
        let s = serde_json::to_string(&body).unwrap();
        assert_eq!(s, r#"{"error":{"message":"x","type":"proxy_error"}}"#);
    }

    #[test]
    fn flat_is_byte_exact() {
        let (_, Json(body)) = flat(StatusCode::BAD_REQUEST, "x");
        let s = serde_json::to_string(&body).unwrap();
        assert_eq!(s, r#"{"error":"x"}"#);
    }
}
