// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
//! variant; `azureclaw dev` and the test suite use the bare variant.
//! Selection is a deployment-time decision: when an `McpServer` CR has
//! `spec.productionMode == true` the controller routes traffic through
//! the protected mount; otherwise through the bare mount.

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
use crate::mcp::oauth::OAuthVerifierConfig;
use crate::mcp::oauth_layer::OAuthLayer;
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

    /// State for the **platform MCP server** mounted at `/platform/mcp`.
    ///
    /// Publishes the runtime-agnostic Foundry-shim catalog
    /// ([`crate::mcp::PlatformDispatcher`]) — the 9 Class-A tools from
    /// the OpenClaw plugin survey, lifted into the router so every
    /// runtime adapter (OpenClaw, OpenAI Agents Python, Microsoft
    /// Agent Framework, BYO) discovers them through one MCP endpoint.
    /// See `mcp/platform.rs` and `plan.md` S10.B.
    pub fn platform() -> Self {
        Self {
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(OsRngSessionMinter),
            tools: Arc::new(crate::mcp::PlatformDispatcher::standard()),
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
pub fn protected_mcp_route(state: McpRouteState, oauth: Arc<OAuthVerifierConfig>) -> Router {
    mcp_route().with_state(state).layer(OAuthLayer::new(oauth))
}

async fn method_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "GET /mcp is reserved for future SSE streaming",
    )
}

async fn post_mcp(State(state): State<McpRouteState>, headers: HeaderMap, body: Bytes) -> Response {
    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());

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
        McpRouteState {
            config: Arc::new(InitializeConfig::default()),
            minter: Arc::new(FixedMinter("platform-session-001")),
            tools: Arc::new(crate::mcp::PlatformDispatcher::standard()),
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
        req.body(Body::from(body.to_vec())).unwrap()
    }

    #[tokio::test]
    async fn platform_state_publishes_nine_foundry_tools() {
        let s = McpRouteState::platform();
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
    async fn platform_tools_call_returns_deferred_wiring_is_error() {
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
        // Per MCP spec, a tool that "ran but errored" returns a normal
        // JSON-RPC 200 result with isError:true on the content payload —
        // distinct from a JSON-RPC error envelope.
        assert!(v["error"].is_null(), "no JSON-RPC envelope error: {v}");
        assert_eq!(v["result"]["isError"], true);
        let content_text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            content_text.contains("S10.B"),
            "content text mentions slice id, got: {content_text}"
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
            allowed_algorithms: vec![Algorithm::EdDSA],
            leeway_seconds: 30,
            required_scopes: vec![],
        })
    }

    fn route_issue_token(sk: &SigningKey, kid: &str) -> String {
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
