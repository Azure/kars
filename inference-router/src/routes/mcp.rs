// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! POST /mcp axum route — thin wrapper around [`crate::mcp::pipeline::process_request`].
//!
//! Spec: <https://modelcontextprotocol.io/specification/2025-11-25/basic/transports>
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
//!     // unauthenticated dev/test surface:
//!     .merge(routes::mcp_route().with_state(mcp_state.clone()))
//!     // production surface, OAuth 2.1 gated:
//!     .merge(routes::protected_mcp_route(mcp_state, oauth_cfg))
//!     .merge(other_routes.with_state(app_state));
//! ```
//!
//! ## OAuth wiring
//!
//! [`protected_mcp_route`] applies [`crate::mcp::OAuthLayer`] in front
//! of [`mcp_route`]. Production deployments select the protected
//! variant; `kars dev` and the test suite use the bare variant.
//! Selection is a deployment-time decision: when an `McpServer` CR has
//! `spec.productionMode == true` the controller routes traffic through
//! the protected mount; otherwise through the bare mount.

use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, Extension, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use std::{collections::BTreeSet, net::SocketAddr, sync::Arc};

use crate::mcp::initialize::{InitializeConfig, OsRngSessionMinter, SessionMinter};
use crate::mcp::oauth::{OAuthVerifierConfig, VerifiedToken};
use crate::mcp::oauth_layer::OAuthLayer;
use crate::mcp::pipeline::{ProcessOutcome, process_request_async};
use crate::mcp::tools::{AsyncToolDispatcher, EchoDispatcher, SyncToAsync};

mod scoped;

/// HTTP header name carrying the MCP session id on a successful
/// `initialize` response and on subsequent client requests.
pub const MCP_SESSION_HEADER: &str = "Mcp-Session-Id";

/// Per-router MCP state. Cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct McpRouteState {
    pub task_telemetry: Option<Arc<crate::task_telemetry::TaskTelemetry>>,
    pub config: Arc<InitializeConfig>,
    pub minter: Arc<dyn SessionMinter + Send + Sync>,
    pub tools: Arc<dyn AsyncToolDispatcher>,
    pub caller_policy: Option<Arc<McpCallerPolicy>>,
    pub oauth_required: bool,
    pub managed_prefixes: BTreeSet<String>,
}

pub struct McpCallerPolicy {
    governance: Arc<crate::governance::Governance>,
    services: Arc<crate::governed_services::GovernedServices>,
    sandbox: String,
}

impl McpRouteState {
    /// Default production state: stock `InitializeConfig`, `OsRng`
    /// session ids, in-tree `EchoDispatcher` (real ping/echo tool).
    ///
    /// Slice 4d.4 replaces this dispatcher with [`crate::mcp::forwarder::RouterToolDispatcher`]
    /// at mount time when the registry advertises at least one usable
    /// `McpServer.spec.url`; see [`with_tools`].
    pub fn standard() -> Self {
        Self {
            task_telemetry: None,
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(OsRngSessionMinter),
            tools: Arc::new(SyncToAsync::new(EchoDispatcher::standard())),
            caller_policy: None,
            oauth_required: false,
            managed_prefixes: BTreeSet::new(),
        }
    }

    /// Swap the dispatcher (used by Slice 4d.4 to mount the
    /// namespaced upstream forwarder when the registry is non-empty).
    pub fn with_tools(mut self, tools: Arc<dyn AsyncToolDispatcher>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_governance(
        mut self,
        governance: Arc<crate::governance::Governance>,
        services: Arc<crate::governed_services::GovernedServices>,
        sandbox: String,
    ) -> Self {
        self.caller_policy = Some(Arc::new(McpCallerPolicy {
            governance,
            services,
            sandbox,
        }));
        self
    }

    /// State for the **platform MCP server** mounted at `/platform/mcp`.
    ///
    /// Publishes the runtime-agnostic Foundry-shim catalog
    /// ([`crate::mcp::PlatformDispatcher`]) — the 9 Class-A tools from
    /// the OpenClaw plugin survey, lifted into the router so every
    /// runtime adapter (OpenClaw, OpenAI Agents Python, Microsoft
    /// Agent Framework, BYO) discovers them through one MCP endpoint.
    /// See `mcp/platform.rs` and `plan.md` S10.B.
    ///
    /// Slice 3b.3: takes an optional KarsMemory binding handle so the
    /// dispatcher's `foundry.memory` calls can prefer the CRD-driven
    /// `store_name` over the chart-fed env. `None` keeps the legacy
    /// env-only behaviour (for sandboxes without `spec.memoryRef`).
    ///
    /// Slice 3b.4: takes an optional `PolicyStatusRegistry` handle so
    /// the dispatcher can surface upstream Foundry Memory Store
    /// 401/403s as `AuthMisconfigured:` prefixed `last_error` entries
    /// on `PolicyKind::Memory`. Without this, 403s still propagate to
    /// the agent envelope but never reach the KarsMemory CRD status.
    pub fn platform(
        memory_binding: Option<crate::memory_binding_loader::LoadedMemoryBindingHandle>,
        policy_status: Option<Arc<crate::policy_status::PolicyStatusRegistry>>,
    ) -> Self {
        let mut dispatcher = crate::mcp::PlatformDispatcher::standard();
        if let Some(handle) = memory_binding {
            dispatcher = dispatcher.with_memory_binding(handle);
        }
        if let Some(registry) = policy_status {
            dispatcher = dispatcher.with_policy_status(registry);
        }
        Self {
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(OsRngSessionMinter),
            tools: Arc::new(dispatcher),
            task_telemetry: None,
            caller_policy: None,
            oauth_required: false,
            managed_prefixes: BTreeSet::new(),
        }
    }
}

impl std::fmt::Debug for McpRouteState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRouteState")
            .field("config", &self.config)
            .field("minter", &"<dyn SessionMinter>")
            .field("tools", &"<dyn AsyncToolDispatcher>")
            .finish()
    }
}

/// Axum router exposing `POST /mcp` (and `GET /mcp` → 405 + `Allow: POST`).
pub fn mcp_route() -> Router<McpRouteState> {
    Router::new().route("/mcp", post(post_mcp).get(method_not_allowed))
}

/// Axum router exposing the **platform MCP server** at `POST /platform/mcp`
/// (with `GET /platform/mcp` → 405 + `Allow: POST`, mirroring `/mcp`).
///
/// Reuses the same JSON-RPC pipeline as [`mcp_route`]; only the path
/// and the injected [`ToolDispatcher`] differ. Caller is expected to
/// bind state via [`McpRouteState::platform`].
///
/// # Security posture
///
/// Loopback-only (`127.0.0.1:8443`) by virtue of the router bind
/// address; the egress-guard init container keeps any other UID off
/// the loopback interface; the agent container (UID 1000) is the only
/// process that can reach this endpoint. Single-tenant by construction
/// — no OAuth gate is added because the platform MCP server has no
/// cross-tenant trust boundary inside the router process. Customer-
/// facing MCP servers (provisioned via the `McpServer` CRD) wear the
/// OAuth 2.1 layer through [`protected_mcp_route`] instead.
pub fn platform_mcp_route() -> Router<McpRouteState> {
    Router::new().route("/platform/mcp", post(post_mcp).get(method_not_allowed))
}

/// Production-mode router: same MCP surface as [`mcp_route`], but every
/// request is OAuth 2.1 verified by [`OAuthLayer`] *before* it reaches
/// the JSON-RPC pipeline.
///
/// On verification failure the layer short-circuits with `401
/// Unauthorized` and an RFC 6750 §3 `WWW-Authenticate: Bearer ...`
/// challenge; the inner MCP handler is never invoked.
///
/// On success a [`crate::mcp::oauth::VerifiedToken`] is attached to
/// `request.extensions_mut()`, available to downstream handlers via an
/// `axum::Extension<VerifiedToken>` extractor (consumed by the
/// upcoming per-tool scope check in `pipeline::process_request`).
pub fn protected_mcp_route(mut state: McpRouteState, oauth: Arc<OAuthVerifierConfig>) -> Router {
    state.oauth_required = true;
    mcp_route().with_state(state).layer(OAuthLayer::new(oauth))
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
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    verified: Option<Extension<VerifiedToken>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let local = peer
        .as_ref()
        .is_some_and(|peer| peer.0.0.ip().is_loopback());
    let principal = if state.oauth_required {
        let Some(Extension(token)) = verified else {
            return (StatusCode::UNAUTHORIZED, "Verified MCP caller required").into_response();
        };
        serde_json::json!(["oauth", token.issuer, token.subject]).to_string()
    } else {
        if !local {
            return (
                StatusCode::FORBIDDEN,
                "Same-router socket peer required for local MCP",
            )
                .into_response();
        }
        state
            .caller_policy
            .as_ref()
            .map(|policy| policy.sandbox.clone())
            .unwrap_or_default()
    };
    let base_tools: Arc<dyn AsyncToolDispatcher> = if state.oauth_required && !local {
        match scoped::ScopedDispatcher::exclude(state.tools.clone(), &state.managed_prefixes) {
            Ok(tools) => Arc::new(tools),
            Err(error) => return (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
        }
    } else {
        state.tools.clone()
    };
    let tools: Arc<dyn AsyncToolDispatcher> = if let Some(server) = headers.get("x-kars-mcp-server")
    {
        let Ok(server) = server.to_str() else {
            return (StatusCode::BAD_REQUEST, "Invalid MCP server scope").into_response();
        };
        match scoped::ScopedDispatcher::new(base_tools, server) {
            Ok(scoped) => Arc::new(scoped),
            Err(error) => return (StatusCode::NOT_FOUND, error).into_response(),
        }
    } else {
        base_tools
    };
    let tools = if let Some(policy) = state.caller_policy.as_ref() {
        Arc::new(crate::mcp::governed::GovernedDispatcher::new(
            tools,
            policy.governance.clone(),
            policy.services.clone(),
            principal,
        )) as Arc<dyn AsyncToolDispatcher>
    } else {
        tools
    };
    let telemetry_scope = state
        .task_telemetry
        .as_ref()
        .map(|telemetry| telemetry.cursor().0);
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let started = std::time::Instant::now();
    let (method, tool) = peek_request_method_and_tool(&body);

    let outcome = process_request_async(
        &body,
        accept.as_deref(),
        state.config.as_ref(),
        state.minter.as_ref(),
        Some(tools.as_ref()),
    )
    .await;
    if let (Some(telemetry), Some(scope)) = (&state.task_telemetry, telemetry_scope) {
        crate::task_telemetry::mcp::record(
            telemetry,
            &scope,
            &body,
            &outcome,
            started.elapsed().as_millis() as u64,
        );
    }

    let status_label: &str = match &outcome {
        ProcessOutcome::JsonRpcResponse { .. } => "200",
        ProcessOutcome::Accepted => "202",
        ProcessOutcome::PayloadTooLarge => "413",
        ProcessOutcome::NotAcceptable(_) => "406",
    };

    tracing::info!(
        method = method.as_deref().unwrap_or("(none)"),
        tool = tool.as_deref().unwrap_or(""),
        status = status_label,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "/mcp request"
    );

    outcome_to_response(outcome)
}

/// Best-effort extraction of `(method, tools/call.name)` from a JSON-RPC
/// request body for log emission. Returns `(None, None)` on parse
/// failure — logging must never fail the request.
fn peek_request_method_and_tool(body: &[u8]) -> (Option<String>, Option<String>) {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    // Batch: take the first request's method/tool.
    let first = if v.is_array() {
        v.as_array().and_then(|a| a.first()).cloned().unwrap_or(v)
    } else {
        v
    };
    let method = first
        .get("method")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());
    let tool = if method.as_deref() == Some("tools/call") {
        first
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };
    (method, tool)
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
            task_telemetry: None,
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(FixedMinter("test-session-001")),
            tools: Arc::new(SyncToAsync::new(EchoDispatcher::standard())),
            ..McpRouteState::standard()
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
        let mut request = req.body(Body::from(body.to_vec())).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
        request
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
        assert_eq!(headers.get(MCP_SESSION_HEADER).unwrap(), "test-session-001");
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
        let req = post_body(b"{not json", Some("application/json, text/event-stream"));
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

    // ----------------------------------------------------------------
    // platform_mcp_route — Foundry-shim discovery surface
    // ----------------------------------------------------------------

    fn platform_test_state() -> McpRouteState {
        // Point the dispatcher at an unreachable loopback port so the
        // route-level tests below exercise the dispatch seam without
        // needing a fake upstream. Per-tool wiring is covered by
        // `mcp::platform::tests` (wiremock) and the dedicated
        // `platform_mcp_dispatch` integration test.
        McpRouteState {
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(FixedMinter("platform-session-001")),
            task_telemetry: None,
            tools: Arc::new(crate::mcp::PlatformDispatcher::with_base_url(
                "http://127.0.0.1:1",
            )),
            ..McpRouteState::standard()
        }
    }

    fn platform_app() -> Router {
        platform_mcp_route().with_state(platform_test_state())
    }

    fn platform_post_body(body: &[u8], accept: Option<&str>) -> Request<Body> {
        let mut req = Request::builder().method("POST").uri("/platform/mcp");
        if let Some(a) = accept {
            req = req.header("accept", a);
        }
        let mut request = req.body(Body::from(body.to_vec())).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
        request
    }

    #[tokio::test]
    async fn platform_state_publishes_nine_foundry_tools() {
        let s = McpRouteState::platform(None, None);
        assert_eq!(
            s.tools.catalog().tools().len(),
            9,
            "platform state must publish exactly the 9 Foundry shims"
        );
    }

    #[tokio::test]
    async fn platform_post_initialize_returns_session_header() {
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
        let req = platform_post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let resp = platform_app().oneshot(req).await.unwrap();
        let (status, headers, text) = body_text(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(MCP_SESSION_HEADER).unwrap(),
            "platform-session-001"
        );
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
    }

    #[tokio::test]
    async fn platform_tools_list_returns_all_nine_foundry_shims() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {}
        });
        let req = platform_post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let resp = platform_app().oneshot(req).await.unwrap();
        let (status, _, text) = body_text(resp).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        let tools = v["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in [
            "foundry.web_search",
            "foundry.code_execute",
            "foundry.file_search",
            "foundry.memory",
            "foundry.image_generation",
            "foundry.conversations",
            "foundry.evaluations",
            "foundry.deployments",
            "foundry.agents",
        ] {
            assert!(
                names.contains(&expected),
                "expected {expected} in tools/list, got {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn platform_tools_call_real_dispatch_returns_is_error_on_unreachable_upstream() {
        // The platform dispatcher self-calls back into the router. In
        // this unit-test harness the dispatcher is pointed at an
        // unreachable loopback port (see `platform_test_state`), so
        // the upstream HTTP call collapses to a transport error,
        // which the dispatcher surfaces as a normal JSON-RPC 200
        // result with isError:true (the wire shape adapters validate
        // their MCP client against). End-to-end success is covered
        // by `tests/platform_mcp_dispatch.rs`.
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "foundry.web_search",
                "arguments": { "query": "anything" }
            }
        });
        let req = platform_post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let resp = platform_app().oneshot(req).await.unwrap();
        let (status, _, text) = body_text(resp).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["error"].is_null(), "no JSON-RPC envelope error: {v}");
        assert_eq!(v["result"]["isError"], true);
        let content_text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            content_text.contains("transport error") || content_text.contains("foundry.web_search"),
            "expected transport-layer error message, got: {content_text}"
        );
    }

    #[tokio::test]
    async fn platform_get_returns_405() {
        let req = Request::builder()
            .method("GET")
            .uri("/platform/mcp")
            .body(Body::empty())
            .unwrap();
        let (status, headers, _) = body_text(platform_app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(headers.get(header::ALLOW).unwrap(), "POST");
    }

    #[tokio::test]
    async fn platform_unknown_tool_returns_jsonrpc_error() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "foundry.does_not_exist",
                "arguments": {}
            }
        });
        let req = platform_post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let resp = platform_app().oneshot(req).await.unwrap();
        let (status, _, text) = body_text(resp).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(
            !v["error"].is_null(),
            "unknown tool surfaces a JSON-RPC error envelope, got: {v}"
        );
    }

    /// END-TO-END server proof for the Foundry memory path: a `tools/call`
    /// for `foundry.memory` sent to `/platform/mcp` WITH the required Accept
    /// header must traverse the pipeline, reach the real `PlatformDispatcher`,
    /// and hit the upstream Foundry `:update_memories` with the correct
    /// contract body. Pairs with the OpenClaw/Hermes thin-client tests (which
    /// prove the client sends that Accept header) to cover the full path the
    /// runtime memory bug lived in.
    #[tokio::test]
    async fn platform_foundry_memory_dispatches_to_upstream_with_accept() {
        use wiremock::matchers::{method as wm_method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let upstream = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(path_regex(r"^/memory_stores/.*:update_memories$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "queued"})))
            .mount(&upstream)
            .await;

        // Real platform dispatcher, pointed at the mock upstream.
        let state = McpRouteState {
            task_telemetry: None,
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(FixedMinter("platform-session-001")),
            tools: Arc::new(crate::mcp::PlatformDispatcher::with_base_url(
                upstream.uri(),
            )),
            ..McpRouteState::standard()
        };
        let app = platform_mcp_route().with_state(state);

        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "foundry.memory",
                "arguments": { "operation": "update", "text": "likes dark roast" }
            }
        });
        let req = platform_post_body(
            req_body.to_string().as_bytes(),
            Some("application/json, text/event-stream"),
        );
        let (status, _, text) = body_text(app.oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["error"].is_null(), "no JSON-RPC envelope error: {v}");
        assert_eq!(
            v["result"]["isError"], false,
            "memory update should succeed: {v}"
        );

        // The dispatcher actually called the upstream with the real contract.
        let reqs = upstream.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1, "exactly one upstream memory call");
        let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert!(
            body.get("messages").is_none(),
            "must use items[], not legacy messages: {body}"
        );
        assert_eq!(body["update_delay"], json!(0));
        assert_eq!(
            body["items"][0]["content"][0]["text"],
            json!("likes dark roast")
        );
        let scope = body["scope"].as_str().unwrap();
        assert!(
            !scope.is_empty() && !scope.contains(':'),
            "valid scope: {scope}"
        );
    }

    /// The same call WITHOUT the Accept header is rejected 406 — i.e. the
    /// exact failure the runtime hit ("Accept must include both
    /// application/json and text/event-stream"). Guards against a regression
    /// where the server stops enforcing (which would mask a broken client).
    #[tokio::test]
    async fn platform_foundry_memory_without_accept_is_406() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": { "name": "foundry.memory", "arguments": { "operation": "search" } }
        });
        let req = platform_post_body(req_body.to_string().as_bytes(), None);
        let (status, _, text) = body_text(platform_app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
        assert!(text.contains("Accept must include both"), "got: {text}");
    }

    // ----------------------------------------------------------------
    // protected_mcp_route — OAuth 2.1 wiring tests
    // ----------------------------------------------------------------

    use crate::mcp::oauth::OAuthVerifierConfig;
    use base64::Engine;
    use ed25519_dalek::SigningKey;
    use jsonwebtoken::jwk::{
        AlgorithmParameters as JwkAlg, CommonParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm,
        OctetKeyPairParameters, OctetKeyPairType, PublicKeyUse,
    };
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use std::collections::HashMap;

    const ROUTE_TEST_KID: &str = "route-kid-1";
    const ROUTE_TEST_ISS: &str = "https://route.example/iss";
    const ROUTE_TEST_AUD: &str = "https://route.example/aud";

    fn route_keypair_seeded(seed: u8) -> (SigningKey, ed25519_dalek::VerifyingKey) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn route_jwks_with(vk: &ed25519_dalek::VerifyingKey, kid: &str) -> JwkSet {
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vk.as_bytes());
        JwkSet {
            keys: vec![Jwk {
                common: CommonParameters {
                    public_key_use: Some(PublicKeyUse::Signature),
                    key_operations: None,
                    key_algorithm: Some(KeyAlgorithm::EdDSA),
                    key_id: Some(kid.into()),
                    x509_url: None,
                    x509_chain: None,
                    x509_sha1_fingerprint: None,
                    x509_sha256_fingerprint: None,
                },
                algorithm: JwkAlg::OctetKeyPair(OctetKeyPairParameters {
                    key_type: OctetKeyPairType::OctetKeyPair,
                    curve: EllipticCurve::Ed25519,
                    x,
                }),
            }],
        }
    }

    /// Build a PKCS#8 v1 PEM Ed25519 private key (RFC 8410 §7) without
    /// enabling the `pkcs8` feature on ed25519-dalek.
    fn route_signing_pem(sk: &SigningKey) -> EncodingKey {
        let prefix: [u8; 16] = [
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
            0x04, 0x20,
        ];
        let mut der = Vec::with_capacity(48);
        der.extend_from_slice(&prefix);
        der.extend_from_slice(&sk.to_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{b64}\n-----END PRIVATE KEY-----\n");
        EncodingKey::from_ed_pem(pem.as_bytes()).unwrap()
    }

    fn route_oauth_cfg(jwks: JwkSet) -> Arc<OAuthVerifierConfig> {
        let mut trusted = HashMap::new();
        trusted.insert(ROUTE_TEST_ISS.to_string(), jwks);
        Arc::new(OAuthVerifierConfig {
            trusted_issuers: trusted,
            expected_audience: ROUTE_TEST_AUD.into(),
            per_issuer_audience: HashMap::new(),
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            required_scopes: vec![],
            per_issuer_scopes: HashMap::new(),
        })
    }

    fn route_issue_token(sk: &SigningKey, kid: &str) -> String {
        crate::install_jsonwebtoken_crypto_provider();
        let now = jsonwebtoken::get_current_timestamp() as i64;
        let claims = json!({
            "iss": ROUTE_TEST_ISS,
            "sub": "route-sub",
            "aud": ROUTE_TEST_AUD,
            "iat": now - 1,
            "nbf": now - 1,
            "exp": now + 600,
            "scope": "mcp.read"
        });
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(kid.into());
        encode(&header, &claims, &route_signing_pem(sk)).unwrap()
    }

    fn protected_app() -> (Router, Arc<OAuthVerifierConfig>, SigningKey) {
        let (sk, vk) = route_keypair_seeded(11);
        let jwks = route_jwks_with(&vk, ROUTE_TEST_KID);
        let cfg = route_oauth_cfg(jwks);
        let app = protected_mcp_route(test_state(), Arc::clone(&cfg));
        (app, cfg, sk)
    }

    fn initialize_request_body() -> Vec<u8> {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "t", "version": "0.0.0"}
            }
        })
        .to_string()
        .into_bytes()
    }

    #[tokio::test]
    async fn protected_route_rejects_missing_bearer_with_401_and_challenge() {
        let (app, _cfg, _sk) = protected_app();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("accept", "application/json, text/event-stream")
            .body(Body::from(initialize_request_body()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let challenge = resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .expect("RFC 6750 §3 challenge required")
            .to_str()
            .unwrap();
        assert!(challenge.starts_with("Bearer error=\"invalid_token\""));
    }

    #[tokio::test]
    async fn protected_route_rejects_malformed_bearer_with_401() {
        let (app, _cfg, _sk) = protected_app();
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("accept", "application/json, text/event-stream")
            .header(header::AUTHORIZATION, "Bearer not-a-jwt")
            .body(Body::from(initialize_request_body()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(
            resp.headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("error=\"invalid_token\"")
        );
    }

    #[tokio::test]
    async fn protected_route_rejects_token_signed_by_untrusted_key_with_401() {
        // Trust kid `ROUTE_TEST_KID` bound to vk(seed=11); sign with seed=42.
        let (sk, _vk_unused) = route_keypair_seeded(42);
        let (_sk_trusted, vk_trusted) = route_keypair_seeded(11);
        let cfg = route_oauth_cfg(route_jwks_with(&vk_trusted, ROUTE_TEST_KID));
        let app = protected_mcp_route(test_state(), cfg);

        let token = route_issue_token(&sk, ROUTE_TEST_KID);
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("accept", "application/json, text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(initialize_request_body()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn protected_route_accepts_valid_bearer_and_returns_initialize_result() {
        let (app, _cfg, sk) = protected_app();
        let token = route_issue_token(&sk, ROUTE_TEST_KID);
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("accept", "application/json, text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(initialize_request_body()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let (status, headers, text) = body_text(resp).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(MCP_SESSION_HEADER).unwrap(),
            "test-session-001",
            "MCP session header survives the OAuth layer"
        );
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert!(v["result"]["protocolVersion"].is_string());
    }

    #[tokio::test]
    async fn remote_oauth_uses_verified_caller_and_cannot_borrow_managed_sessions() {
        use crate::mcp::tools::{
            DispatchError, ToolCallOutput, ToolCatalog, ToolContent, ToolDefinition,
        };
        struct Tools(ToolCatalog, Arc<std::sync::atomic::AtomicUsize>);
        #[async_trait::async_trait]
        impl AsyncToolDispatcher for Tools {
            fn catalog(&self) -> &ToolCatalog {
                &self.0
            }
            async fn invoke(&self, _: &str, _: &Value) -> Result<ToolCallOutput, DispatchError> {
                self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(ToolCallOutput {
                    content: vec![ToolContent::text("ok")],
                    is_error: false,
                    ..Default::default()
                })
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tools = ToolCatalog::new(
            ["legacy.echo", "managed.echo"]
                .into_iter()
                .map(|name| ToolDefinition {
                    name: name.into(),
                    description: "test".into(),
                    input_schema: json!({"type":"object"}),
                })
                .collect(),
        )
        .unwrap();
        let governance = Arc::new(crate::governance::Governance::new("victim"));
        let policy = tempfile::tempdir_in(".").unwrap();
        let path = policy.path().join("allow.yaml");
        std::fs::write(&path,"version: '1.0'\nagent: test\npolicies:\n  - name: allow\n    type: capability\n    allowed_actions: ['tool:*']\n    priority: 100\n").unwrap();
        governance
            .policy
            .load_from_file(path.to_str().unwrap())
            .unwrap();
        let services = Arc::new(crate::governed_services::GovernedServices::new(
            crate::access_request::Identity::standalone("victim"),
            None,
        ));
        let mut state = McpRouteState::standard()
            .with_tools(Arc::new(Tools(tools, calls.clone())))
            .with_governance(governance.clone(), services, "victim".into());
        state.managed_prefixes.insert("managed.".into());
        let (sk, vk) = route_keypair_seeded(15);
        let app = protected_mcp_route(state, route_oauth_cfg(route_jwks_with(&vk, ROUTE_TEST_KID)));
        let token = route_issue_token(&sk, ROUTE_TEST_KID);
        for (id, name) in [(1, "legacy.echo"), (2, "managed.echo")] {
            let mut request = Request::post("/mcp")
                .header("accept", "application/json, text/event-stream")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("x-forwarded-for", "127.0.0.1")
                .header("mcp-session-id", "victim-session")
                .body(Body::from(
                    json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                    "params":{"name":name,"arguments":{}}})
                    .to_string(),
                ))
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([10, 2, 3, 4], 12345))));
            let (_, _, body) = body_text(app.clone().oneshot(request).await.unwrap()).await;
            let body: Value = serde_json::from_str(&body).unwrap();
            if id == 1 {
                assert_eq!(body["result"]["content"][0]["text"], "ok");
            } else {
                assert!(body.get("error").is_some() || body["result"]["isError"] == true);
            }
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let entries = governance.audit_json()["entries"].clone();
        let actor = entries
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["action"] == "tool:legacy.echo")
            .unwrap();
        assert_eq!(
            actor["agent_id"],
            json!(["oauth", ROUTE_TEST_ISS, "route-sub"]).to_string()
        );
    }

    #[tokio::test]
    async fn protected_route_rejects_get_with_401_before_method_check() {
        // The OAuth layer runs before the per-route method matcher;
        // an unauthenticated GET must fail closed with 401, not leak
        // the 405 + Allow header that bare `mcp_route` would return.
        let (app, _cfg, _sk) = protected_app();
        let req = Request::builder()
            .method("GET")
            .uri("/mcp")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
