//! POST /mcp axum route — thin wrapper around [`crate::mcp::pipeline::process_request`].
//!
//! Spec: <https://modelcontextprotocol.io/specification/2025-03-26/basic/transports>
//!
//! Per §0.2 principle 8 (no scaffolding) every code path here is real:
//! body-size 413, Accept-header 406, JSON-RPC parse error, batch
//! handling, all-notifications 202, success 200 + `Mcp-Session-Id`.
//!
//! ## State
//!
//! This module is wired with its own [`McpRouteState`] (config +
//! session minter + tool dispatcher) rather than the global
//! [`AppState`]. Rationale: MCP doesn't read anything from the
//! ambient router state — sub-router with its own state keeps the
//! coupling explicit and the tests `oneshot`-able without
//! constructing an `AppState` (which does network I/O at build).
//!
//! In `main.rs`:
//!
//! ```ignore
//! let mcp_state = routes::McpRouteState::standard();
//! let app = Router::new()
//!     .merge(routes::mcp_route().with_state(mcp_state))
//!     .merge(other_routes.with_state(app_state));
//! ```
//!
//! ## What this module does NOT do
//!
//! - OAuth 2.1 verification — `mcp::oauth` is the pure verifier;
//!   binding it as a tower layer is the next PR.
//! - SSE streaming responses — pipeline returns single JSON-RPC
//!   bodies today. Accept negotiation already requires
//!   `text/event-stream` in the client list per spec.

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use std::sync::Arc;

use crate::mcp::initialize::{InitializeConfig, OsRngSessionMinter, SessionMinter};
use crate::mcp::pipeline::{ProcessOutcome, process_request};
use crate::mcp::tools::{EchoDispatcher, ToolDispatcher};

/// HTTP header name carrying the MCP session id on a successful
/// `initialize` response and on subsequent client requests.
pub const MCP_SESSION_HEADER: &str = "Mcp-Session-Id";

/// Per-router MCP state. Cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct McpRouteState {
    pub config: Arc<InitializeConfig>,
    pub minter: Arc<dyn SessionMinter + Send + Sync>,
    pub tools: Arc<dyn ToolDispatcher>,
}

impl McpRouteState {
    /// Default production state: stock `InitializeConfig`, `OsRng`
    /// session ids, in-tree `EchoDispatcher` (real ping/echo tool).
    /// Real upstream tools land via a future `RouterToolDispatcher`
    /// implementation that proxies into `McpServer` CRs.
    pub fn standard() -> Self {
        Self {
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(OsRngSessionMinter),
            tools: Arc::new(EchoDispatcher::standard()),
        }
    }
}

impl std::fmt::Debug for McpRouteState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRouteState")
            .field("config", &self.config)
            .field("minter", &"<dyn SessionMinter>")
            .field("tools", &"<dyn ToolDispatcher>")
            .finish()
    }
}

/// Axum router exposing `POST /mcp` (and `GET /mcp` → 405 + `Allow: POST`).
pub fn mcp_route() -> Router<McpRouteState> {
    Router::new().route("/mcp", post(post_mcp).get(method_not_allowed))
}

async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "GET /mcp is reserved for future SSE streaming",
    )
}

async fn post_mcp(
    State(state): State<McpRouteState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok());

    let outcome = process_request(
        &body,
        accept,
        &state.config,
        state.minter.as_ref(),
        Some(state.tools.as_ref()),
    );

    outcome_to_response(outcome)
}

fn outcome_to_response(outcome: ProcessOutcome) -> Response {
    match outcome {
        ProcessOutcome::JsonRpcResponse { body, session_id } => {
            let mut resp = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response();
            if let Some(sid) = session_id {
                if let Ok(value) = HeaderValue::from_str(sid.as_str()) {
                    resp.headers_mut().insert(MCP_SESSION_HEADER, value);
                }
            }
            resp
        }
        ProcessOutcome::Accepted => (StatusCode::ACCEPTED, "").into_response(),
        ProcessOutcome::PayloadTooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds MAX_FRAME_BYTES",
        )
            .into_response(),
        ProcessOutcome::NotAcceptable(reason) => {
            (StatusCode::NOT_ACCEPTABLE, reason).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    //! End-to-end axum tests via `tower::ServiceExt::oneshot`.

    use super::*;
    use crate::mcp::pipeline::ProcessOutcome;
    use crate::mcp::streamable_http::{MAX_FRAME_BYTES, SessionId};
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Deterministic minter for tests so we can assert on
    /// `Mcp-Session-Id`.
    struct FixedMinter(&'static str);
    impl SessionMinter for FixedMinter {
        fn mint(&self) -> SessionId {
            SessionId::try_new(self.0).expect("valid id literal")
        }
    }

    fn test_state() -> McpRouteState {
        McpRouteState {
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(FixedMinter("test-session-001")),
            tools: Arc::new(EchoDispatcher::standard()),
        }
    }

    fn app() -> Router {
        mcp_route().with_state(test_state())
    }

    fn post_body(body: &[u8], accept: Option<&str>) -> Request<Body> {
        let mut req = Request::builder().method("POST").uri("/mcp");
        if let Some(a) = accept {
            req = req.header("accept", a);
        }
        req.body(Body::from(body.to_vec())).unwrap()
    }

    async fn body_text(resp: Response) -> (StatusCode, HeaderMap, String) {
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
        (status, headers, String::from_utf8_lossy(&bytes).to_string())
    }

    #[tokio::test]
    async fn post_mcp_initialize_returns_session_header_and_result() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "t", "version": "0.0.0"}
            }
        });
        let req = post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let resp = app().oneshot(req).await.unwrap();
        let (status, headers, text) = body_text(resp).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(MCP_SESSION_HEADER).unwrap(),
            "test-session-001"
        );
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert!(v["result"]["protocolVersion"].is_string());
    }

    #[tokio::test]
    async fn post_mcp_oversized_returns_413() {
        let big = vec![b'x'; MAX_FRAME_BYTES + 1];
        let req = post_body(&big, Some("application/json, text/event-stream"));
        let (status, _, _) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn post_mcp_missing_accept_returns_406() {
        let req = post_body(b"{}", None);
        let (status, _, body) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
        assert!(body.contains("application/json"));
    }

    #[tokio::test]
    async fn post_mcp_only_json_accept_returns_406() {
        let req = post_body(b"{}", Some("application/json"));
        let (status, _, _) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn post_mcp_malformed_json_returns_200_with_parse_error() {
        let req = post_body(
            b"{not json",
            Some("application/json, text/event-stream"),
        );
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK); // JSON-RPC convention
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn post_mcp_unknown_method_returns_method_not_found() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": "x",
            "method": "does/not/exist",
            "params": {}
        });
        let req = post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn post_mcp_notification_only_returns_202() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let req = post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let (status, _, body) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn post_mcp_tools_list_returns_catalog() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {}
        });
        let req = post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        let tools = v["result"]["tools"].as_array().unwrap();
        assert!(!tools.is_empty(), "EchoDispatcher exposes >=1 tool");
    }

    #[tokio::test]
    async fn get_mcp_returns_405_with_allow_header() {
        let req = Request::builder()
            .method("GET")
            .uri("/mcp")
            .body(Body::empty())
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(resp.headers().get("allow").unwrap(), "POST");
    }

    #[tokio::test]
    async fn outcome_payload_too_large_maps_to_413() {
        let resp = outcome_to_response(ProcessOutcome::PayloadTooLarge);
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn outcome_not_acceptable_maps_to_406_with_reason() {
        let resp = outcome_to_response(ProcessOutcome::NotAcceptable("nope"));
        let (status, _, body) = body_text(resp).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
        assert_eq!(body, "nope");
    }

    #[tokio::test]
    async fn outcome_accepted_maps_to_202_with_empty_body() {
        let resp = outcome_to_response(ProcessOutcome::Accepted);
        let (status, _, body) = body_text(resp).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn standard_state_builds_without_panic() {
        let s = McpRouteState::standard();
        assert!(!s.config.supported_protocol_versions.is_empty());
    }
}
