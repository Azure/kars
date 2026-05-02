// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! MCP request pipeline — the full body→response transform.
//!
//! Spec: <https://modelcontextprotocol.io/specification/2025-11-25/basic/transports>
//!
//! This module is the entire MCP Streamable HTTP server logic short
//! of axum binding. It takes raw HTTP body bytes + the `Accept` header
//! and returns either a fully-formed JSON-RPC response (single or
//! batch) or an HTTP-level rejection (413 / 406 / 400).
//!
//! The future POST `/mcp` route handler is a thin wrapper around
//! [`process_request`]:
//!
//! ```ignore
//! async fn post_mcp(headers: HeaderMap, body: Bytes) -> impl IntoResponse {
//!     let outcome = process_request(
//!         &body,
//!         headers.get("accept").and_then(|v| v.to_str().ok()),
//!         &APP_STATE.mcp_config,
//!         &APP_STATE.session_minter,
//!     );
//!     // map outcome → axum Response
//! }
//! ```
//!
//! ## What this layer enforces
//!
//! 1. **Body size cap** — [`MAX_FRAME_BYTES`] (4 MiB). Refused with
//!    `413 Payload Too Large` *before* any JSON parsing. Defence
//!    against memory-exhaustion attacks.
//! 2. **`Accept` header negotiation** — POST handlers MUST list both
//!    `application/json` and `text/event-stream`. Anything less is
//!    `406 Not Acceptable`.
//! 3. **JSON-RPC frame parsing** — malformed bytes → JSON-RPC parse
//!    error response (-32700) at HTTP 200, per JSON-RPC convention.
//! 4. **Method dispatch** — `initialize` and `ping` handled in-tree;
//!    unknown methods produce -32601 Method Not Found.
//! 5. **Batch handling** — JSON-RPC §6: each batch item processed
//!    independently; notifications produce no response; an all-
//!    notifications batch yields no HTTP body (the spec mandates
//!    HTTP 202 Accepted for that case). Empty batches → -32600.
//! 6. **No panics** — the entire pipeline is total. Any unexpected
//!    state collapses to InternalError (-32603).

use serde_json::Value;

use super::error::{ErrorCode, JsonRpcError};
use super::initialize::{InitializeConfig, SessionMinter, handle_initialize};
use super::jsonrpc::{Frame, Id, Notification, ParseError, Request, Response, parse_frame};
use super::streamable_http::{
    AcceptNegotiation, MAX_FRAME_BYTES, SessionId, validate_accept_header,
};
use super::tools::{
    AsyncToolDispatcher, ToolDispatcher, handle_tools_call, handle_tools_call_async,
    handle_tools_list, handle_tools_list_async,
};

/// Outcome of `process_request`. Maps directly to an HTTP response.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessOutcome {
    /// One or more JSON-RPC responses ready to ship as
    /// `application/json` body. The `session_id`, when `Some`, MUST
    /// be written into the `Mcp-Session-Id` HTTP header.
    JsonRpcResponse {
        body: Vec<u8>,
        session_id: Option<SessionId>,
    },
    /// All-notifications batch (or single notification): no JSON-RPC
    /// body, return HTTP 202 Accepted with empty body.
    Accepted,
    /// HTTP 413 Payload Too Large — body exceeded `MAX_FRAME_BYTES`.
    PayloadTooLarge,
    /// HTTP 406 Not Acceptable — `Accept` header missing or
    /// incompatible. Carries a short diagnostic.
    NotAcceptable(&'static str),
}

/// Process a POST `/mcp` request body. Pure, synchronous, total.
///
/// `accept_header` is the raw value of the inbound `Accept` HTTP
/// header (or `None` if the client omitted it).
pub fn process_request(
    body: &[u8],
    accept_header: Option<&str>,
    config: &InitializeConfig,
    minter: &(dyn SessionMinter + Send + Sync),
    tools: Option<&dyn ToolDispatcher>,
) -> ProcessOutcome {
    // 1. Body size gate.
    if body.len() > MAX_FRAME_BYTES {
        return ProcessOutcome::PayloadTooLarge;
    }

    // 2. Accept header negotiation. POST bodies require both content
    //    types per spec.
    let neg = match accept_header {
        Some(h) => validate_accept_header(h),
        None => AcceptNegotiation::Neither,
    };
    match neg {
        AcceptNegotiation::Both => {} // ok
        AcceptNegotiation::OnlyJson => {
            return ProcessOutcome::NotAcceptable(
                "Accept must include both application/json and text/event-stream",
            );
        }
        AcceptNegotiation::OnlySse | AcceptNegotiation::Neither => {
            return ProcessOutcome::NotAcceptable(
                "Accept must include both application/json and text/event-stream",
            );
        }
    }

    // 3. JSON-RPC frame parse. A parse error is a JSON-RPC error
    //    response (HTTP 200 with -32700 body), not an HTTP-level
    //    error — that's the JSON-RPC convention.
    let frame = match parse_frame(body) {
        Ok(f) => f,
        Err(e) => {
            let resp = parse_error_response(&e);
            return json_rpc_response(vec![resp], None);
        }
    };

    // 4. Dispatch.
    dispatch(frame, config, minter, tools)
}

fn dispatch(
    frame: Frame,
    config: &InitializeConfig,
    minter: &(dyn SessionMinter + Send + Sync),
    tools: Option<&dyn ToolDispatcher>,
) -> ProcessOutcome {
    match frame {
        Frame::Request(req) => {
            let (resp, sid) = handle_request(&req, config, minter, tools);
            json_rpc_response(vec![resp], sid)
        }
        Frame::Notification(notif) => {
            handle_notification(&notif);
            ProcessOutcome::Accepted
        }
        Frame::Response(_) => {
            // The server should not receive Responses on the inbound
            // path. Reject with InvalidRequest.
            let resp = error_response(
                &Id::Null,
                ErrorCode::InvalidRequest,
                Some(serde_json::json!({"reason": "server received unsolicited Response frame"})),
            );
            json_rpc_response(vec![resp], None)
        }
        Frame::Batch(items) => {
            let mut responses = Vec::new();
            let mut session_id: Option<SessionId> = None;
            for item in items {
                match item {
                    Frame::Request(req) => {
                        let (resp, sid) = handle_request(&req, config, minter, tools);
                        // First session id wins — multiple `initialize`
                        // calls in one batch is malformed but we
                        // surface a deterministic answer.
                        if session_id.is_none() {
                            session_id = sid;
                        }
                        responses.push(resp);
                    }
                    Frame::Notification(notif) => {
                        handle_notification(&notif);
                    }
                    Frame::Response(_) => {
                        responses.push(error_response(
                            &Id::Null,
                            ErrorCode::InvalidRequest,
                            Some(serde_json::json!({
                                "reason": "server received unsolicited Response frame in batch"
                            })),
                        ));
                    }
                    Frame::Batch(_) => {
                        // parse_frame already rejects nested batches,
                        // so this branch is unreachable; collapse
                        // defensively to InternalError instead of
                        // panicking.
                        responses.push(error_response(
                            &Id::Null,
                            ErrorCode::InternalError,
                            Some(serde_json::json!({"reason": "nested batch reached dispatch"})),
                        ));
                    }
                }
            }
            if responses.is_empty() {
                // All-notifications batch — HTTP 202 Accepted.
                ProcessOutcome::Accepted
            } else {
                json_rpc_response(responses, session_id)
            }
        }
    }
}

/// Dispatch a single JSON-RPC request to the correct method handler.
/// Returns the response and (for `initialize`) the freshly minted
/// session id.
fn handle_request(
    req: &Request,
    config: &InitializeConfig,
    minter: &(dyn SessionMinter + Send + Sync),
    tools: Option<&dyn ToolDispatcher>,
) -> (Response, Option<SessionId>) {
    match req.method.as_str() {
        "initialize" => {
            let outcome = handle_initialize(req, config, minter);
            (outcome.response, outcome.session_id)
        }
        "ping" => (handle_ping(req), None),
        "tools/list" => match tools {
            Some(d) => (handle_tools_list(req, d), None),
            None => (
                error_response(
                    &req.id,
                    ErrorCode::MethodNotFound,
                    Some(serde_json::json!({
                        "method": req.method,
                        "reason": "tools dispatcher not configured on this server",
                    })),
                ),
                None,
            ),
        },
        "tools/call" => match tools {
            Some(d) => (handle_tools_call(req, d), None),
            None => (
                error_response(
                    &req.id,
                    ErrorCode::MethodNotFound,
                    Some(serde_json::json!({
                        "method": req.method,
                        "reason": "tools dispatcher not configured on this server",
                    })),
                ),
                None,
            ),
        },
        _ => (
            error_response(
                &req.id,
                ErrorCode::MethodNotFound,
                Some(serde_json::json!({"method": req.method})),
            ),
            None,
        ),
    }
}

/// Async counterpart to [`process_request`]. Identical pre-dispatch
/// gates (size, Accept negotiation, JSON-RPC parse), but the
/// `tools/call` and `tools/list` legs run against an
/// [`AsyncToolDispatcher`]. The streamable HTTP route uses this entry
/// point so dispatchers can make upstream HTTP calls without spinning a
/// runtime inside a sync trait method.
pub async fn process_request_async(
    body: &[u8],
    accept_header: Option<&str>,
    config: &InitializeConfig,
    minter: &(dyn SessionMinter + Send + Sync),
    tools: Option<&dyn AsyncToolDispatcher>,
) -> ProcessOutcome {
    if body.len() > MAX_FRAME_BYTES {
        return ProcessOutcome::PayloadTooLarge;
    }
    let neg = match accept_header {
        Some(h) => validate_accept_header(h),
        None => AcceptNegotiation::Neither,
    };
    if !matches!(neg, AcceptNegotiation::Both) {
        return ProcessOutcome::NotAcceptable(
            "Accept must include both application/json and text/event-stream",
        );
    }
    let frame = match parse_frame(body) {
        Ok(f) => f,
        Err(e) => {
            let resp = parse_error_response(&e);
            return json_rpc_response(vec![resp], None);
        }
    };
    dispatch_async(frame, config, minter, tools).await
}

async fn dispatch_async(
    frame: Frame,
    config: &InitializeConfig,
    minter: &(dyn SessionMinter + Send + Sync),
    tools: Option<&dyn AsyncToolDispatcher>,
) -> ProcessOutcome {
    match frame {
        Frame::Request(req) => {
            let (resp, sid) = handle_request_async(&req, config, minter, tools).await;
            json_rpc_response(vec![resp], sid)
        }
        Frame::Notification(notif) => {
            handle_notification(&notif);
            ProcessOutcome::Accepted
        }
        Frame::Response(_) => {
            let resp = error_response(
                &Id::Null,
                ErrorCode::InvalidRequest,
                Some(serde_json::json!({"reason": "server received unsolicited Response frame"})),
            );
            json_rpc_response(vec![resp], None)
        }
        Frame::Batch(items) => {
            let mut responses = Vec::new();
            let mut session_id: Option<SessionId> = None;
            for item in items {
                match item {
                    Frame::Request(req) => {
                        let (resp, sid) = handle_request_async(&req, config, minter, tools).await;
                        if session_id.is_none() {
                            session_id = sid;
                        }
                        responses.push(resp);
                    }
                    Frame::Notification(notif) => handle_notification(&notif),
                    Frame::Response(_) => {
                        responses.push(error_response(
                            &Id::Null,
                            ErrorCode::InvalidRequest,
                            Some(serde_json::json!({
                                "reason": "server received unsolicited Response frame in batch"
                            })),
                        ));
                    }
                    Frame::Batch(_) => {
                        responses.push(error_response(
                            &Id::Null,
                            ErrorCode::InternalError,
                            Some(serde_json::json!({"reason": "nested batch reached dispatch"})),
                        ));
                    }
                }
            }
            if responses.is_empty() {
                ProcessOutcome::Accepted
            } else {
                json_rpc_response(responses, session_id)
            }
        }
    }
}

async fn handle_request_async(
    req: &Request,
    config: &InitializeConfig,
    minter: &(dyn SessionMinter + Send + Sync),
    tools: Option<&dyn AsyncToolDispatcher>,
) -> (Response, Option<SessionId>) {
    match req.method.as_str() {
        "initialize" => {
            let outcome = handle_initialize(req, config, minter);
            (outcome.response, outcome.session_id)
        }
        "ping" => (handle_ping(req), None),
        "tools/list" => match tools {
            Some(d) => (handle_tools_list_async(req, d), None),
            None => (
                error_response(
                    &req.id,
                    ErrorCode::MethodNotFound,
                    Some(serde_json::json!({
                        "method": req.method,
                        "reason": "tools dispatcher not configured on this server",
                    })),
                ),
                None,
            ),
        },
        "tools/call" => match tools {
            Some(d) => (handle_tools_call_async(req, d).await, None),
            None => (
                error_response(
                    &req.id,
                    ErrorCode::MethodNotFound,
                    Some(serde_json::json!({
                        "method": req.method,
                        "reason": "tools dispatcher not configured on this server",
                    })),
                ),
                None,
            ),
        },
        _ => (
            error_response(
                &req.id,
                ErrorCode::MethodNotFound,
                Some(serde_json::json!({"method": req.method})),
            ),
            None,
        ),
    }
}

fn handle_ping(req: &Request) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: Some(serde_json::json!({})),
        error: None,
        id: req.id.clone(),
    }
}

/// Notifications are fire-and-forget. We log nothing here (the route
/// handler will emit the OTel span); future method-specific handlers
/// (`notifications/initialized`, `notifications/cancelled`, ...) plug
/// in by extending this match.
fn handle_notification(_notif: &Notification) {
    // Intentionally a no-op for now: we accept any notification and
    // produce no response, per JSON-RPC §4.1.
}

/// Build a JSON-RPC error response from a [`ParseError`].
fn parse_error_response(err: &ParseError) -> Response {
    let (code, data): (ErrorCode, Value) = match err {
        ParseError::InvalidJson(msg) => (ErrorCode::ParseError, serde_json::json!({"reason": msg})),
        ParseError::InvalidProtocolVersion(v) => (
            ErrorCode::InvalidRequest,
            serde_json::json!({"reason": "jsonrpc must be \"2.0\"", "got": v}),
        ),
        ParseError::InvalidShape(msg) => (
            ErrorCode::InvalidRequest,
            serde_json::json!({"reason": msg}),
        ),
        ParseError::EmptyBatch => (
            ErrorCode::InvalidRequest,
            serde_json::json!({"reason": "empty batch"}),
        ),
    };
    error_response(&Id::Null, code, Some(data))
}

fn error_response(id: &Id, code: ErrorCode, data: Option<Value>) -> Response {
    let mut err = JsonRpcError::new(code);
    if let Some(d) = data {
        err = err.with_data(d);
    }
    Response {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(err),
        id: id.clone(),
    }
}

/// Serialise a single Response or batch into the wire body.
///
/// Single-element vectors serialise as a single object; multi-element
/// vectors as a JSON array. Per JSON-RPC §6, the server MUST respond
/// with whichever shape the client sent, but our parser collapses the
/// distinction at the Frame level — we use vector length here.
fn json_rpc_response(responses: Vec<Response>, session_id: Option<SessionId>) -> ProcessOutcome {
    let body = if responses.len() == 1 {
        // Take ownership of the single response without cloning.
        let single = responses.into_iter().next().expect("len==1");
        match serde_json::to_vec(&single) {
            Ok(v) => v,
            Err(e) => {
                // Serialisation cannot realistically fail for our
                // frames, but if it does, fall back to a minimal
                // error envelope to keep the function total.
                fallback_internal_error(e.to_string())
            }
        }
    } else {
        match serde_json::to_vec(&responses) {
            Ok(v) => v,
            Err(e) => fallback_internal_error(e.to_string()),
        }
    };
    ProcessOutcome::JsonRpcResponse { body, session_id }
}

fn fallback_internal_error(reason: String) -> Vec<u8> {
    let env = serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": ErrorCode::InternalError.code(),
            "message": ErrorCode::InternalError.message(),
            "data": {"reason": reason},
        },
        "id": Value::Null,
    });
    // serde_json::to_vec on a fully-owned Value should never fail;
    // if it does, return an empty body — the route handler will map
    // ProcessOutcome::JsonRpcResponse with empty body to a 500.
    serde_json::to_vec(&env).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::initialize::OsRngSessionMinter;
    use crate::mcp::streamable_http::{MAX_FRAME_BYTES, MCP_PROTOCOL_VERSION};
    use serde_json::json;

    struct FixedMinter(&'static str);
    impl SessionMinter for FixedMinter {
        fn mint(&self) -> SessionId {
            SessionId::try_new(self.0).unwrap()
        }
    }

    fn cfg() -> InitializeConfig {
        InitializeConfig::default()
    }

    fn ok_accept() -> Option<&'static str> {
        Some("application/json, text/event-stream")
    }

    fn init_body() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "x", "version": "0"}
            },
            "id": 1,
        }))
        .unwrap()
    }

    #[test]
    fn happy_path_single_initialize() {
        let out = process_request(
            &init_body(),
            ok_accept(),
            &cfg(),
            &FixedMinter("session-001"),
            None,
        );
        match out {
            ProcessOutcome::JsonRpcResponse { body, session_id } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(resp["jsonrpc"], json!("2.0"));
                assert!(resp["result"]["serverInfo"].is_object());
                assert_eq!(session_id.unwrap().as_str(), "session-001");
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn ping_round_trips() {
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "ping",
            "id": 42,
        }))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, session_id } => {
                assert!(session_id.is_none());
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(resp["id"], json!(42));
                assert_eq!(resp["result"], json!({}));
                assert!(resp.get("error").is_none() || resp["error"].is_null());
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn payload_too_large_short_circuits() {
        let body = vec![0u8; MAX_FRAME_BYTES + 1];
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        assert_eq!(out, ProcessOutcome::PayloadTooLarge);
    }

    #[test]
    fn missing_accept_header_is_406() {
        let out = process_request(&init_body(), None, &cfg(), &FixedMinter("ignored"), None);
        assert!(matches!(out, ProcessOutcome::NotAcceptable(_)));
    }

    #[test]
    fn accept_only_json_is_406() {
        let out = process_request(
            &init_body(),
            Some("application/json"),
            &cfg(),
            &FixedMinter("ignored"),
            None,
        );
        assert!(matches!(out, ProcessOutcome::NotAcceptable(_)));
    }

    #[test]
    fn accept_only_sse_is_406() {
        let out = process_request(
            &init_body(),
            Some("text/event-stream"),
            &cfg(),
            &FixedMinter("ignored"),
            None,
        );
        assert!(matches!(out, ProcessOutcome::NotAcceptable(_)));
    }

    #[test]
    fn malformed_json_returns_parse_error_at_http_200() {
        let out = process_request(
            b"not json",
            ok_accept(),
            &cfg(),
            &FixedMinter("ignored"),
            None,
        );
        match out {
            ProcessOutcome::JsonRpcResponse { body, session_id } => {
                assert!(session_id.is_none());
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(resp["error"]["code"], json!(ErrorCode::ParseError.code()));
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "id": 1,
        }))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    resp["error"]["code"],
                    json!(ErrorCode::MethodNotFound.code())
                );
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn notification_returns_accepted() {
        // No id → notification.
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        assert_eq!(out, ProcessOutcome::Accepted);
    }

    #[test]
    fn batch_of_only_notifications_returns_accepted() {
        let body = serde_json::to_vec(&json!([
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "method": "notifications/progress", "params": {}}
        ]))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        assert_eq!(out, ProcessOutcome::Accepted);
    }

    #[test]
    fn batch_with_mixed_requests_and_notifications_returns_only_request_responses() {
        let body = serde_json::to_vec(&json!([
            {"jsonrpc": "2.0", "method": "ping", "id": 1},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "method": "ping", "id": 2}
        ]))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let arr: Vec<Value> = serde_json::from_slice(&body).unwrap();
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0]["id"], json!(1));
                assert_eq!(arr[1]["id"], json!(2));
            }
            other => panic!("expected JsonRpcResponse with batch, got {other:?}"),
        }
    }

    #[test]
    fn empty_batch_returns_invalid_request() {
        let body = b"[]";
        let out = process_request(body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    resp["error"]["code"],
                    json!(ErrorCode::InvalidRequest.code())
                );
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn server_rejects_inbound_response_frame() {
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "result": {},
            "id": 1,
        }))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    resp["error"]["code"],
                    json!(ErrorCode::InvalidRequest.code())
                );
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn body_at_exactly_max_frame_bytes_is_accepted() {
        // Pad a valid initialize body with whitespace up to exactly
        // MAX_FRAME_BYTES; it should still parse and dispatch.
        let mut body = init_body();
        let pad = MAX_FRAME_BYTES.saturating_sub(body.len());
        // Append no-op JSON whitespace inside the object: replace the
        // last `}` with spaces+`}`. This keeps the JSON valid.
        if let Some(last) = body.pop() {
            assert_eq!(last, b'}');
            body.extend(std::iter::repeat_n(b' ', pad));
            body.push(b'}');
        }
        assert_eq!(body.len(), MAX_FRAME_BYTES);
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("s"), None);
        // At-cap is allowed; only > cap rejects.
        assert!(matches!(out, ProcessOutcome::JsonRpcResponse { .. }));
    }

    #[test]
    fn nested_batch_is_rejected_at_parse() {
        let body = b"[[{\"jsonrpc\":\"2.0\",\"method\":\"ping\",\"id\":1}]]";
        let out = process_request(body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    resp["error"]["code"],
                    json!(ErrorCode::InvalidRequest.code())
                );
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn id_preserved_for_unknown_method_error() {
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "does/not/exist",
            "id": "my-correlation-id",
        }))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(resp["id"], json!("my-correlation-id"));
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn os_rng_minter_works_at_pipeline_level() {
        let out = process_request(&init_body(), ok_accept(), &cfg(), &OsRngSessionMinter, None);
        match out {
            ProcessOutcome::JsonRpcResponse { session_id, .. } => {
                let id = session_id.unwrap();
                assert_eq!(id.as_str().len(), 64);
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn batch_with_initialize_surfaces_session_id() {
        let body = serde_json::to_vec(&json!([
            {"jsonrpc": "2.0", "method": "ping", "id": 1},
            {
                "jsonrpc": "2.0",
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "x", "version": "0"}
                },
                "id": 2,
            }
        ]))
        .unwrap();
        let out = process_request(
            &body,
            ok_accept(),
            &cfg(),
            &FixedMinter("batch-session"),
            None,
        );
        match out {
            ProcessOutcome::JsonRpcResponse { session_id, .. } => {
                assert_eq!(session_id.unwrap().as_str(), "batch-session");
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn tools_list_method_unsupported_when_no_dispatcher() {
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1,
        }))
        .unwrap();
        let out = process_request(&body, ok_accept(), &cfg(), &FixedMinter("ignored"), None);
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(resp["error"]["code"], json!(-32601));
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn tools_list_returns_catalog_when_dispatcher_provided() {
        use super::super::tools::EchoDispatcher;
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1,
        }))
        .unwrap();
        let dispatcher = EchoDispatcher::standard();
        let out = process_request(
            &body,
            ok_accept(),
            &cfg(),
            &FixedMinter("ignored"),
            Some(&dispatcher),
        );
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                let tools = &resp["result"]["tools"];
                assert!(tools.is_array());
                assert_eq!(tools[0]["name"], json!("echo"));
                // Verify camelCase wire format leak-test
                let raw = std::str::from_utf8(&body).unwrap();
                assert!(raw.contains("inputSchema"), "must use camelCase");
                assert!(!raw.contains("input_schema"), "must not leak snake_case");
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn tools_call_invokes_dispatcher_via_pipeline() {
        use super::super::tools::EchoDispatcher;
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": "echo", "arguments": { "text": "hi" } },
            "id": 7,
        }))
        .unwrap();
        let dispatcher = EchoDispatcher::standard();
        let out = process_request(
            &body,
            ok_accept(),
            &cfg(),
            &FixedMinter("ignored"),
            Some(&dispatcher),
        );
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(resp["id"], json!(7));
                assert_eq!(resp["result"]["content"][0]["text"], json!("hi"));
                assert_eq!(resp["result"]["isError"], json!(false));
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }

    #[test]
    fn tools_call_unknown_tool_returns_jsonrpc_error() {
        use super::super::tools::EchoDispatcher;
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": "nope", "arguments": {} },
            "id": 8,
        }))
        .unwrap();
        let dispatcher = EchoDispatcher::standard();
        let out = process_request(
            &body,
            ok_accept(),
            &cfg(),
            &FixedMinter("ignored"),
            Some(&dispatcher),
        );
        match out {
            ProcessOutcome::JsonRpcResponse { body, .. } => {
                let resp: Value = serde_json::from_slice(&body).unwrap();
                assert!(resp["error"].is_object());
            }
            other => panic!("expected JsonRpcResponse, got {other:?}"),
        }
    }
}
