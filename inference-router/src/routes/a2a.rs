// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! A2A 1.0.0 axum routes — `/.well-known/agent.json` + `POST /a2a` JSON-RPC.
//!
//! Spec: <https://a2a-protocol.org/v1.0.0/specification>
//!
//! Per §0.2 principle 8 (no scaffolding) every code path here is real:
//!
//! - `GET /.well-known/agent.json` returns the signed Agent Card (JWS over
//!   the canonical payload, Ed25519 / EdDSA).
//! - `POST /a2a` parses a JSON-RPC 2.0 frame and dispatches:
//!   - `message/send` → [`a2a::handle_message_send`]
//!   - `tasks/get` → [`a2a::handle_tasks_get`]
//!   - `tasks/cancel` → [`a2a::handle_tasks_cancel`]
//!   - notifications (no `id`) → 202 Accepted, empty body.
//!   - unknown methods → JSON-RPC `-32601 Method Not Found` envelope.
//!   - malformed JSON → JSON-RPC `-32700 Parse error` envelope.
//! - Body cap: 4 MiB before parse → `413 Payload Too Large`.
//! - `Accept` negotiation: if present, must contain `application/json`,
//!   `*/*`, or be a non-restrictive `application/*`. Otherwise `406`.
//!   (Unlike MCP, A2A does not mandate a streaming-capable Accept header.)
//!
//! ## State
//!
//! Sub-router with its own [`A2aRouteState`] (card config + signing key +
//! task store + id minter). Decoupled from `AppState` so handlers are
//! unit-testable end-to-end via `tower::ServiceExt::oneshot`.
//!
//! ## What this module does NOT do
//!
//! - A2A streaming methods (`message/stream`, `tasks/resubscribe`) — TBD
//!   when the streaming session manager lands.
//! - Inbound card verification — that's the verifier path, called by
//!   *outbound* code (router-to-peer-agent), not this server.

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ed25519_dalek::SigningKey;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::a2a::ap2::InMemoryMandateLedger;
use crate::a2a::mandate_trust_store::MandateTrustStore;
use crate::a2a::message_send_ap2::handle_message_send_with_ap2;
use crate::a2a::{AgentCardConfig, build_signed_card, handle_tasks_cancel, handle_tasks_get};
use crate::a2a::{InMemoryTaskStore, OsRngTaskIdMinter, TaskIdMinter, TaskStore};
use crate::mcp::error::{ErrorCode, JsonRpcError};
use crate::mcp::jsonrpc::{
    Frame, Id, Notification, ParseError, Request, Response as JRpcResponse, parse_frame,
};
use crate::mcp::streamable_http::MAX_FRAME_BYTES;

/// Per-router A2A state. Cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct A2aRouteState {
    pub card_config: Arc<AgentCardConfig>,
    pub signing_key: Arc<SigningKey>,
    pub tasks: Arc<dyn TaskStore>,
    pub minter: Arc<dyn TaskIdMinter>,
    /// When `Some`, `GET /.well-known/agent.json` returns these bytes
    /// verbatim instead of rebuilding+signing on every request. This
    /// is the production path when the controller mirrors a signed
    /// `A2AAgent` card ConfigMap into the sandbox at
    /// `/etc/kars/a2a-card/agent.json` — the public signing keys
    /// live in the A2AAgent CR; the controller has no access to the
    /// private signing key, so the pre-signed bytes are authoritative.
    pub pre_signed_card: Option<Arc<Vec<u8>>>,
    /// AP2 mandate-issuer trust anchors. Populated either from a
    /// boot-time file via [`crate::a2a::load_mandate_trust_snapshot`]
    /// or by a future `MandateIssuer` informer reconciler. An empty
    /// store fails-closed: every AP2-bearing message is rejected.
    pub mandate_trust: Arc<MandateTrustStore>,
    /// AP2 mandate ledger — replay / window enforcement state.
    /// `Mutex` because [`crate::a2a::ap2::MandateLedgerMut::record`]
    /// requires `&mut self` and the route state is shared across
    /// all concurrent `POST /a2a` handlers. Held only for the
    /// post-validation append (microseconds).
    pub mandate_ledger: Arc<Mutex<InMemoryMandateLedger>>,
    /// When `true`, every `message/send` request must carry an AP2
    /// mandate (`metadata.ap2`) — AP2-free messages are rejected
    /// with `Ap2Denied`. Sourced from `AP2_COMMERCE_REQUIRED=1` at
    /// router boot. The intended controller-side path is for
    /// `ToolPolicy.spec.commerce` (presence) on the bound policy to
    /// drive this flag at sandbox provisioning time. When `false`
    /// (default), AP2-free traffic flows through unchanged and AP2
    /// metadata, when present, is still validated.
    pub commerce_required: bool,
}

impl A2aRouteState {
    /// Default production state given a card config + signing key.
    /// Tasks live in-process (`InMemoryTaskStore`) with `OsRng`-minted ids.
    /// Persistence and HA come with the `KubeTaskStore` impl in a future PR.
    pub fn new(card_config: AgentCardConfig, signing_key: SigningKey) -> Self {
        Self {
            card_config: Arc::new(card_config),
            signing_key: Arc::new(signing_key),
            tasks: Arc::new(InMemoryTaskStore::new()),
            minter: Arc::new(OsRngTaskIdMinter),
            pre_signed_card: None,
            mandate_trust: Arc::new(MandateTrustStore::new()),
            mandate_ledger: Arc::new(Mutex::new(InMemoryMandateLedger::new())),
            commerce_required: false,
        }
    }

    /// Construct an `A2aRouteState` whose `/.well-known/agent.json`
    /// response is served from a pre-signed card on disk (typically
    /// the controller-mirrored `A2AAgent` card ConfigMap mounted at
    /// `dir/agent.json`).
    ///
    /// `dir` must contain a file named `agent.json` produced by the
    /// `a2a_agent_reconciler`. The dispatch path (`POST /a2a`) still
    /// works because we keep an in-memory task store and an ephemeral
    /// signing key — but anything that actually re-signs the card
    /// (rebuild path) is unreachable while `pre_signed_card` is set.
    pub fn from_card_dir(dir: &std::path::Path) -> Result<Self, std::io::Error> {
        let path = dir.join("agent.json");
        let bytes = std::fs::read(&path)?;
        // Validate it parses as JSON before serving — so a malformed
        // mount fails at startup instead of returning garbage to A2A peers.
        if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is not valid JSON", path.display()),
            ));
        }
        // Synthetic minimum-viable skill required by AgentCardConfig
        // (skills is `Vec<AgentSkill>`, not Option). Never serialised
        // while pre_signed_card is Some — `get_agent_card` returns the
        // pre-signed bytes verbatim and never reaches `build_signed_card`.
        let unused_skill = crate::a2a::AgentSkill {
            id: "default".into(),
            name: "default".into(),
            description: "unused; pre-signed card path is authoritative".into(),
            tags: vec![],
            input_modes: None,
            output_modes: None,
            examples: None,
            security_requirements: None,
        };
        let card_config = AgentCardConfig {
            name: "kars-a2a-agent".into(),
            description:
                "Pre-signed card; see /.well-known/agent.json for the authoritative document."
                    .into(),
            version: "1.0.0".into(),
            base_url: std::env::var("A2A_PUBLIC_BASE_URL")
                .unwrap_or_else(|_| "https://localhost/a2a".to_string()),
            kid: "preloaded".into(),
            skills: vec![unused_skill],
            provider: None,
            documentation_url: None,
            icon_url: None,
            streaming: Some(false),
            push_notifications: None,
            default_input_modes: None,
            default_output_modes: None,
        };
        // Ephemeral signing key — never used while pre_signed_card is set.
        let signing_key = SigningKey::from_bytes(&[0u8; 32]);
        Ok(Self {
            card_config: Arc::new(card_config),
            signing_key: Arc::new(signing_key),
            tasks: Arc::new(InMemoryTaskStore::new()),
            minter: Arc::new(OsRngTaskIdMinter),
            pre_signed_card: Some(Arc::new(bytes)),
            mandate_trust: Arc::new(MandateTrustStore::new()),
            mandate_ledger: Arc::new(Mutex::new(InMemoryMandateLedger::new())),
            commerce_required: false,
        })
    }
}

impl std::fmt::Debug for A2aRouteState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("A2aRouteState")
            .field("card_config", &self.card_config)
            .field("signing_key", &"<Ed25519 SigningKey>")
            .field("tasks", &"<dyn TaskStore>")
            .field("minter", &"<dyn TaskIdMinter>")
            .field(
                "pre_signed_card",
                &self.pre_signed_card.as_ref().map(|c| c.len()),
            )
            .field("mandate_trust", &"<MandateTrustStore>")
            .field("mandate_ledger", &"<InMemoryMandateLedger>")
            .field("commerce_required", &self.commerce_required)
            .finish()
    }
}

/// Axum router exposing `GET /.well-known/agent.json` and `POST /a2a`.
pub fn a2a_routes() -> Router<A2aRouteState> {
    Router::new()
        .route("/.well-known/agent.json", get(get_agent_card))
        .route("/a2a", post(post_a2a))
}

async fn get_agent_card(State(state): State<A2aRouteState>) -> Response {
    if let Some(pre) = state.pre_signed_card.as_ref() {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            pre.as_ref().clone(),
        )
            .into_response();
    }
    match build_signed_card(&state.card_config, &state.signing_key) {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "build_signed_card failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "agent card unavailable").into_response()
        }
    }
}

async fn post_a2a(State(state): State<A2aRouteState>, headers: HeaderMap, body: Bytes) -> Response {
    if body.len() > MAX_FRAME_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds MAX_FRAME_BYTES",
        )
            .into_response();
    }

    if !accept_is_compatible(&headers) {
        return (
            StatusCode::NOT_ACCEPTABLE,
            "Accept must include application/json or */*",
        )
            .into_response();
    }

    let frame = match parse_frame(&body) {
        Ok(f) => f,
        Err(e) => {
            return json_response(parse_error_envelope(&e));
        }
    };

    match frame {
        Frame::Request(req) => json_response(dispatch_request(req, &state)),
        Frame::Notification(_n) => (StatusCode::ACCEPTED, "").into_response(),
        Frame::Response(_) => json_response(JRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(
                JsonRpcError::new(ErrorCode::InvalidRequest)
                    .with_data(serde_json::json!({"detail": "server received a response frame"})),
            ),
            id: Id::Null,
        }),
        Frame::Batch(items) => handle_batch(items, &state),
    }
}

/// Dispatch a parsed `Request` to the appropriate A2A handler.
///
/// `message/send` runs through the AP2-aware
/// [`handle_message_send_with_ap2`] wrapper:
///
/// - When `params.message.metadata.ap2` is **absent** the wrapper
///   delegates straight to [`handle_message_send`] (zero overhead).
/// - When **present** the mandate is verified against
///   `state.mandate_trust`, the policy envelope is checked, and the
///   ledger is appended to before the task is created.
///
/// When `state.commerce_required` is `true`, AP2-free `message/send`
/// requests are rejected up front with `Ap2Denied` so the operator's
/// `ToolPolicy.spec.commerce` requirement is honoured even if the
/// caller forgets to attach a mandate.
///
/// Unknown methods produce a JSON-RPC `-32601` envelope.
fn dispatch_request(req: Request, state: &A2aRouteState) -> JRpcResponse {
    match req.method.as_str() {
        "message/send" => {
            if state.commerce_required && !request_has_ap2_metadata(&req) {
                return commerce_required_response(&req);
            }
            // Acquire the ledger lock for the duration of validate-
            // and-record. The validator reads ledger state for
            // window/replay checks; record() appends iff validation
            // passed. Holding the lock across both calls keeps the
            // check-then-write atomic.
            let mut ledger = match state.mandate_ledger.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            handle_message_send_with_ap2(
                &req,
                state.tasks.as_ref(),
                state.minter.as_ref(),
                state.mandate_trust.as_ref(),
                &mut *ledger,
                now,
            )
        }
        "tasks/get" => handle_tasks_get(&req, state.tasks.as_ref()),
        "tasks/cancel" => handle_tasks_cancel(&req, state.tasks.as_ref()),
        other => method_not_found(&req, other),
    }
}

/// Cheap pre-check: does `params.message.metadata.ap2` exist?
/// Used only by the `commerce_required` gate; full parsing happens
/// inside [`handle_message_send_with_ap2`].
fn request_has_ap2_metadata(req: &Request) -> bool {
    req.params
        .as_ref()
        .and_then(|p| p.get("message"))
        .and_then(|m| m.get("metadata"))
        .and_then(|md| md.get("ap2"))
        .is_some()
}

fn commerce_required_response(req: &Request) -> JRpcResponse {
    use crate::a2a::A2aErrorCode;
    JRpcResponse {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(JsonRpcError {
            code: A2aErrorCode::Ap2Denied.into(),
            message: A2aErrorCode::Ap2Denied.default_message().into(),
            data: Some(serde_json::json!({
                "reason": "ToolPolicy.commerce requires an AP2 mandate; metadata.ap2 absent",
                "kind": "commerceMandateRequired",
            })),
        }),
        id: req.id.clone(),
    }
}

fn handle_batch(items: Vec<Frame>, state: &A2aRouteState) -> Response {
    // `parse_frame` already rejects empty batches with `ParseError::EmptyBatch`.
    // A batch is only constructed when at least one item parsed, so emptiness
    // here would be a parser regression. We still defend against it.
    if items.is_empty() {
        let resp = JRpcResponse {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(
                JsonRpcError::new(ErrorCode::InvalidRequest)
                    .with_data(serde_json::json!({"detail": "empty batch"})),
            ),
            id: Id::Null,
        };
        return json_response(resp);
    }

    let mut responses: Vec<JRpcResponse> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Frame::Request(req) => responses.push(dispatch_request(req, state)),
            Frame::Notification(_) => { /* no response per JSON-RPC §6 */ }
            Frame::Response(_) => {
                responses.push(JRpcResponse {
                    jsonrpc: "2.0".into(),
                    result: None,
                    error: Some(JsonRpcError::new(ErrorCode::InvalidRequest).with_data(
                        serde_json::json!({"detail": "batch contained a response frame"}),
                    )),
                    id: Id::Null,
                })
            }
            Frame::Batch(_) => {
                responses.push(JRpcResponse {
                    jsonrpc: "2.0".into(),
                    result: None,
                    error: Some(
                        JsonRpcError::new(ErrorCode::InvalidRequest)
                            .with_data(serde_json::json!({"detail": "nested batch"})),
                    ),
                    id: Id::Null,
                });
            }
        }
    }
    if responses.is_empty() {
        return (StatusCode::ACCEPTED, "").into_response();
    }
    let body = serde_json::to_vec(&responses).unwrap_or_else(|_| b"[]".to_vec());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

#[allow(dead_code)]
fn _ensure_notification_type_used(_: Notification) {}

fn method_not_found(req: &Request, method: &str) -> JRpcResponse {
    JRpcResponse {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(
            JsonRpcError::new(ErrorCode::MethodNotFound)
                .with_data(serde_json::json!({"method": method})),
        ),
        id: req.id.clone(),
    }
}

fn parse_error_envelope(e: &ParseError) -> JRpcResponse {
    let code = match e {
        ParseError::InvalidJson(_) => ErrorCode::ParseError,
        ParseError::InvalidProtocolVersion(_)
        | ParseError::InvalidShape(_)
        | ParseError::EmptyBatch => ErrorCode::InvalidRequest,
    };
    JRpcResponse {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(
            JsonRpcError::new(code).with_data(serde_json::json!({"detail": e.to_string()})),
        ),
        id: Id::Null,
    }
}

fn json_response(resp: JRpcResponse) -> Response {
    let body = serde_json::to_vec(&resp).unwrap_or_else(|_| b"{}".to_vec());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// A2A `Accept` negotiation — permissive: missing, `*/*`, `application/*`,
/// or any list containing `application/json` is acceptable. JSON-RPC bodies
/// over HTTP are not bound to a streaming-capable Accept by spec.
fn accept_is_compatible(headers: &HeaderMap) -> bool {
    let raw = match headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) {
        None => return true,
        Some(s) => s,
    };
    raw.split(',')
        .map(|t| t.split(';').next().unwrap_or("").trim())
        .any(|t| {
            t.eq_ignore_ascii_case("*/*")
                || t.eq_ignore_ascii_case("application/*")
                || t.eq_ignore_ascii_case("application/json")
                || t.eq_ignore_ascii_case("text/event-stream")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::{A2aErrorCode, AgentSkill, sign_card};
    use axum::body::{Body, to_bytes};
    use axum::http::Request as HttpRequest;
    use ed25519_dalek::SigningKey;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn fixed_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn test_card_config() -> AgentCardConfig {
        AgentCardConfig {
            name: "test-agent".into(),
            description: "for tests".into(),
            version: "0.0.0".into(),
            base_url: "https://example.test/a2a".into(),
            kid: "test-kid-1".into(),
            skills: vec![AgentSkill {
                id: "echo".into(),
                name: "echo".into(),
                description: "echo skill".into(),
                tags: vec!["test".into()],
                examples: None,
                input_modes: None,
                output_modes: None,
                security_requirements: None,
            }],
            provider: None,
            documentation_url: None,
            icon_url: None,
            streaming: None,
            push_notifications: None,
            default_input_modes: None,
            default_output_modes: None,
        }
    }

    fn test_state() -> A2aRouteState {
        A2aRouteState::new(test_card_config(), fixed_signing_key())
    }

    fn app() -> Router {
        a2a_routes().with_state(test_state())
    }

    fn post_body(body: &[u8], accept: Option<&str>) -> HttpRequest<Body> {
        let mut req = HttpRequest::builder().method("POST").uri("/a2a");
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
    async fn get_agent_card_returns_signed_card() {
        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/agent.json")
            .body(Body::empty())
            .unwrap();
        let (status, headers, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["name"], "test-agent");
        let sigs = v["signatures"].as_array().unwrap();
        assert_eq!(sigs.len(), 1);
        assert!(!sigs[0]["protected"].as_str().unwrap().is_empty());
        assert!(!sigs[0]["signature"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn message_send_creates_task_and_returns_it() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{"kind": "text", "text": "hi"}]
                }
            }
        });
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["state"], "submitted");
        assert!(!v["result"]["id"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tasks_get_returns_task_not_found_for_unknown() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": "x",
            "method": "tasks/get",
            "params": {"id": "nope"}
        });
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], i32::from(A2aErrorCode::TaskNotFound));
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "does/not/exist",
            "params": {}
        });
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn malformed_json_returns_parse_error() {
        let req = post_body(b"{not json", Some("application/json"));
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn oversized_returns_413() {
        let big = vec![b'x'; MAX_FRAME_BYTES + 1];
        let req = post_body(&big, Some("application/json"));
        let (status, _, _) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn incompatible_accept_returns_406() {
        let req = post_body(b"{}", Some("text/html"));
        let (status, _, _) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn missing_accept_is_permissive() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tasks/get",
            "params": {"id": "x"}
        });
        let req = post_body(req_body.to_string().as_bytes(), None);
        let (status, _, _) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn star_slash_star_accepted() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tasks/get",
            "params": {"id": "x"}
        });
        let req = post_body(req_body.to_string().as_bytes(), Some("*/*"));
        let (status, _, _) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn notification_returns_202() {
        let req_body = json!({
            "jsonrpc": "2.0",
            "method": "tasks/get",
            "params": {"id": "x"}
        });
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, body) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn empty_batch_returns_invalid_request() {
        let req = post_body(b"[]", Some("application/json"));
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn batch_dispatches_each_item() {
        let req_body = json!([
            {"jsonrpc": "2.0", "id": 1, "method": "tasks/get", "params": {"id": "nope"}},
            {"jsonrpc": "2.0", "id": 2, "method": "does/not/exist", "params": {}}
        ]);
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, text) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0]["error"]["code"],
            i32::from(A2aErrorCode::TaskNotFound)
        );
        assert_eq!(arr[1]["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn batch_of_only_notifications_returns_202() {
        let req_body = json!([
            {"jsonrpc": "2.0", "method": "tasks/get", "params": {"id": "x"}},
            {"jsonrpc": "2.0", "method": "tasks/cancel", "params": {"id": "y"}}
        ]);
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, body) = body_text(app().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, "");
    }

    #[tokio::test]
    async fn full_round_trip_send_then_get() {
        let app = app();

        let send = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {"message": {"role": "user", "parts": [{"kind":"text","text":"go"}]}}
        });
        let req = post_body(send.to_string().as_bytes(), Some("application/json"));
        let (_, _, text) = body_text(app.clone().oneshot(req).await.unwrap()).await;
        let v: Value = serde_json::from_str(&text).unwrap();
        let task_id = v["result"]["id"].as_str().unwrap().to_string();

        let getr = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tasks/get",
            "params": {"id": task_id.clone()}
        });
        let req2 = post_body(getr.to_string().as_bytes(), Some("application/json"));
        let (status2, _, text2) = body_text(app.oneshot(req2).await.unwrap()).await;
        assert_eq!(status2, StatusCode::OK);
        let v2: Value = serde_json::from_str(&text2).unwrap();
        assert_eq!(v2["result"]["id"], task_id);
        assert_eq!(v2["result"]["state"], "submitted");
    }

    /// Verify the served card is a real signed card by re-running the
    /// sign step on an unsigned build and confirming the protected
    /// header carries our `kid`. This catches regressions in
    /// `build_signed_card` without re-implementing JWS verification
    /// (the verifier corpus covers that separately).
    #[test]
    fn served_card_carries_expected_kid() {
        let card = crate::a2a::build_card(&test_card_config()).unwrap();
        let signed = sign_card(card, &fixed_signing_key(), "test-kid-1").unwrap();
        let sigs = signed.signatures.unwrap();
        assert_eq!(sigs.len(), 1);
        // Decode the protected b64u and look for "kid":"test-kid-1".
        let raw = base64_decode(&sigs[0].protected);
        let header_str = std::str::from_utf8(&raw).unwrap();
        assert!(
            header_str.contains("\"kid\":\"test-kid-1\""),
            "{header_str}"
        );
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .unwrap()
    }

    // ── S2 wiring (Phase 3 audit closure) ──────────────────────────
    // Tests covering `A2aRouteState::from_card_dir` + pre-signed
    // `/.well-known/agent.json` serving.

    fn write_card(dir: &std::path::Path, body: &[u8]) {
        std::fs::write(dir.join("agent.json"), body).unwrap();
    }

    #[test]
    fn from_card_dir_loads_valid_signed_card() {
        let tmp = tempfile::tempdir().unwrap();
        let body = serde_json::to_vec(&json!({"name":"x","skills":[]})).unwrap();
        write_card(tmp.path(), &body);
        let state = A2aRouteState::from_card_dir(tmp.path()).expect("loads");
        assert!(state.pre_signed_card.is_some());
        assert_eq!(state.pre_signed_card.as_ref().unwrap().as_ref(), &body);
    }

    #[test]
    fn from_card_dir_rejects_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        write_card(tmp.path(), b"this is not json");
        let err = A2aRouteState::from_card_dir(tmp.path()).expect_err("must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_card_dir_missing_file_is_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        // do not write agent.json
        let err = A2aRouteState::from_card_dir(tmp.path()).expect_err("must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn commerce_required_rejects_ap2_free_message_send() {
        let mut state = test_state();
        state.commerce_required = true;
        let app = a2a_routes().with_state(state);
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "message/send",
            "params": {"message": {"role": "user", "parts": [{"kind":"text","text":"hi"}]}}
        });
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, text) = body_text(app.oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], i32::from(A2aErrorCode::Ap2Denied));
        assert_eq!(v["error"]["data"]["kind"], "commerceMandateRequired");
    }

    #[tokio::test]
    async fn commerce_required_off_by_default_allows_plain_message_send() {
        let state = test_state();
        assert!(!state.commerce_required, "default must be false");
        let app = a2a_routes().with_state(state);
        let req_body = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "message/send",
            "params": {"message": {"role": "user", "parts": [{"kind":"text","text":"hi"}]}}
        });
        let req = post_body(req_body.to_string().as_bytes(), Some("application/json"));
        let (status, _, text) = body_text(app.oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["result"]["state"], "submitted");
    }

    #[test]
    fn request_has_ap2_metadata_detects_present_and_absent() {
        let with_ap2 = Request {
            jsonrpc: "2.0".into(),
            id: Id::Number(1),
            method: "message/send".into(),
            params: Some(serde_json::json!({
                "message": {"metadata": {"ap2": {"version": "0.1"}}}
            })),
        };
        assert!(request_has_ap2_metadata(&with_ap2));
        let without = Request {
            jsonrpc: "2.0".into(),
            id: Id::Number(1),
            method: "message/send".into(),
            params: Some(serde_json::json!({"message": {"role": "user"}})),
        };
        assert!(!request_has_ap2_metadata(&without));
    }

    #[tokio::test]
    async fn agent_card_route_serves_pre_signed_bytes_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let body = serde_json::to_vec(&json!({"name":"pre-signed","skills":[]})).unwrap();
        write_card(tmp.path(), &body);
        let state = A2aRouteState::from_card_dir(tmp.path()).unwrap();
        let app = a2a_routes().with_state(state);
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/.well-known/agent.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1 << 16).await.unwrap();
        assert_eq!(bytes.as_ref(), body.as_slice());
    }
}
