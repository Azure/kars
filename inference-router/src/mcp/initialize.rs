//! MCP `initialize` method handler — Streamable HTTP entry point.
//!
//! Spec: <https://modelcontextprotocol.io/specification/2025-03-26/basic/lifecycle>
//!
//! The first request a client makes on a fresh MCP connection is the
//! `initialize` JSON-RPC request. The server MUST respond with:
//!
//! - `protocolVersion` — the version it agrees to negotiate. If the
//!   server doesn't support the client's requested version, it MUST
//!   respond with one it does support (the client may then disconnect).
//! - `capabilities` — a bag of feature flags advertised by the server.
//! - `serverInfo` — `{ name, version }`.
//! - `instructions` — optional human-readable system prompt/notes.
//!
//! In addition, the **transport layer** SHOULD assign an
//! `Mcp-Session-Id` header (per Streamable HTTP §session management).
//! [`InitializeOutcome`] carries both the JSON-RPC `Response` and the
//! freshly minted `SessionId` so the route handler can write both into
//! the HTTP response in a single step.
//!
//! ## Validation
//!
//! Per the JSON-RPC 2.0 spec, malformed `params` produces an
//! `InvalidParams` (-32602) error response — never a panic. The
//! handler is **total**: every input either yields a valid Response
//! (success or error) or returns the error variant. No `unwrap`,
//! no `expect`.
//!
//! ## Session id minting
//!
//! Session IDs are 32 bytes of cryptographic randomness from `OsRng`,
//! base16-encoded, then wrapped in a [`SessionId`] (which enforces
//! visible-ASCII per spec). The minter is decoupled from the handler
//! via the [`SessionMinter`] trait so tests can supply deterministic
//! session ids.

use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::error::{ErrorCode, JsonRpcError};
use super::jsonrpc::{Id, Request, Response};
use super::streamable_http::{MCP_PROTOCOL_VERSION, SessionId};

/// Outcome of handling an `initialize` request: the JSON-RPC response
/// to send to the client *and* the session id the server is assigning.
/// On error, the session id is `None` because the server never began
/// a session.
#[derive(Debug, Clone)]
pub struct InitializeOutcome {
    pub response: Response,
    pub session_id: Option<SessionId>,
}

/// Static server identity advertised in the `initialize` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// MCP server capabilities advertised in the `initialize` response.
///
/// Each top-level key is `Some(...)` only when the server actually
/// supports that capability. The exact object shape is intentionally
/// `Value` so we can incrementally fill in capabilities without
/// rewriting this struct.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
}

/// Server-side configuration the `initialize` handler needs.
#[derive(Debug, Clone)]
pub struct InitializeConfig {
    pub server_info: ServerInfo,
    pub capabilities: ServerCapabilities,
    /// Optional human-readable instructions block surfaced to the
    /// client. Spec calls this "instructions" (system-prompt-like).
    pub instructions: Option<String>,
    /// List of protocol versions the server supports, newest first.
    /// The first entry MUST equal [`MCP_PROTOCOL_VERSION`].
    pub supported_protocol_versions: Vec<String>,
}

impl Default for InitializeConfig {
    fn default() -> Self {
        Self {
            server_info: ServerInfo {
                name: "azureclaw-inference-router".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: ServerCapabilities::default(),
            instructions: None,
            supported_protocol_versions: vec![MCP_PROTOCOL_VERSION.to_string()],
        }
    }
}

/// Parameters of a well-formed `initialize` JSON-RPC request.
///
/// Other fields the spec defines (e.g. `clientInfo`) are accepted but
/// not parsed — we only commit to fields the server actually consumes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    protocol_version: String,
    /// Client capabilities — accepted opaque, not validated here.
    #[serde(default)]
    #[allow(dead_code)] // surfaced to capability negotiation in a future PR
    capabilities: Value,
    /// Client info — `{name, version}`. Accepted but not consumed.
    #[serde(default)]
    #[allow(dead_code)]
    client_info: Value,
}

/// Source of fresh session ids. Indirected through a trait so unit
/// tests can supply deterministic ids; production wires
/// [`OsRngSessionMinter`].
pub trait SessionMinter {
    fn mint(&self) -> SessionId;
}

/// Production session minter: 32 bytes from `OsRng`, base16-encoded.
///
/// 32 bytes is the standard output of a 256-bit CSPRNG; well above
/// the spec's "globally unique and cryptographically secure"
/// requirement. Base16 (lowercase hex) keeps the wire bytes inside
/// the visible-ASCII range required by [`SessionId`].
pub struct OsRngSessionMinter;

impl SessionMinter for OsRngSessionMinter {
    fn mint(&self) -> SessionId {
        let mut buf = [0u8; 32];
        rand::rng().fill_bytes(&mut buf);
        let s = encode_hex_lower(&buf);
        // Construction cannot fail: hex is visible ASCII and non-empty.
        SessionId::try_new(s).expect("hex output is always visible ASCII")
    }
}

fn encode_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Handle a JSON-RPC `initialize` request.
///
/// Returns an [`InitializeOutcome`] in every case — never panics. If
/// the request shape is invalid, the response carries a structured
/// JSON-RPC error and `session_id` is `None`.
///
/// This function is **pure** and synchronous. It is the entire
/// business logic of the MCP `initialize` method; the future POST
/// `/mcp` axum handler will call this and write the result into the
/// HTTP response (status 200 + `Mcp-Session-Id` header on success).
pub fn handle_initialize(
    request: &Request,
    config: &InitializeConfig,
    minter: &dyn SessionMinter,
) -> InitializeOutcome {
    // 1. Method gate — refuse non-initialize requests routed here.
    if request.method != "initialize" {
        return error_outcome(
            &request.id,
            ErrorCode::MethodNotFound,
            Some(json!({"expected": "initialize", "got": request.method})),
        );
    }

    // 2. Protocol version sanity — handler config must list at least
    //    one supported version (defensive check; default config
    //    satisfies this).
    if config.supported_protocol_versions.is_empty() {
        return error_outcome(
            &request.id,
            ErrorCode::InternalError,
            Some(json!({"reason": "server has no supported protocol versions"})),
        );
    }

    // 3. Parse params.
    let params_value = match &request.params {
        Some(v) => v.clone(),
        None => {
            return error_outcome(
                &request.id,
                ErrorCode::InvalidParams,
                Some(json!({"reason": "params required for initialize"})),
            );
        }
    };
    let params: InitializeParams = match serde_json::from_value(params_value) {
        Ok(p) => p,
        Err(e) => {
            return error_outcome(
                &request.id,
                ErrorCode::InvalidParams,
                Some(json!({"reason": e.to_string()})),
            );
        }
    };
    if params.protocol_version.is_empty() {
        return error_outcome(
            &request.id,
            ErrorCode::InvalidParams,
            Some(json!({"reason": "protocolVersion must not be empty"})),
        );
    }

    // 4. Negotiate version. If the client's exact version is supported,
    //    echo it back; otherwise, return our newest. Either case is
    //    valid per spec — the client decides whether to proceed.
    let negotiated = if config
        .supported_protocol_versions
        .contains(&params.protocol_version)
    {
        params.protocol_version.clone()
    } else {
        config.supported_protocol_versions[0].clone()
    };

    // 5. Build the result body. Order in the JSON object is governed by
    //    the field order in the `json!` macro; we follow the spec's
    //    example ordering for ergonomics.
    let mut result = json!({
        "protocolVersion": negotiated,
        "capabilities": config.capabilities,
        "serverInfo": config.server_info,
    });
    if let Some(instr) = &config.instructions {
        result
            .as_object_mut()
            .expect("just constructed as object")
            .insert("instructions".into(), Value::String(instr.clone()));
    }

    // 6. Mint the session id and return.
    let session_id = minter.mint();
    InitializeOutcome {
        response: Response {
            jsonrpc: "2.0".into(),
            result: Some(result),
            error: None,
            id: request.id.clone(),
        },
        session_id: Some(session_id),
    }
}

fn error_outcome(id: &Id, code: ErrorCode, data: Option<Value>) -> InitializeOutcome {
    let mut err = JsonRpcError::new(code);
    if let Some(d) = data {
        err = err.with_data(d);
    }
    InitializeOutcome {
        response: Response {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(err),
            id: id.clone(),
        },
        session_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic test minter — always returns the same session id.
    struct FixedMinter(&'static str);
    impl SessionMinter for FixedMinter {
        fn mint(&self) -> SessionId {
            SessionId::try_new(self.0).unwrap()
        }
    }

    fn req(method: &str, params: Option<Value>) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id: Id::Number(1),
        }
    }

    fn ok_init_params() -> Value {
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "0.1.0"}
        })
    }

    #[test]
    fn happy_path_returns_response_and_session() {
        let r = req("initialize", Some(ok_init_params()));
        let out = handle_initialize(
            &r,
            &InitializeConfig::default(),
            &FixedMinter("test-session-001"),
        );
        let resp = out.response;
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Id::Number(1));
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], json!(MCP_PROTOCOL_VERSION));
        assert!(result["serverInfo"]["name"].is_string());
        assert!(result["capabilities"].is_object());
        assert_eq!(out.session_id.unwrap().as_str(), "test-session-001");
    }

    #[test]
    fn rejects_non_initialize_method() {
        let r = req("ping", Some(ok_init_params()));
        let out = handle_initialize(&r, &InitializeConfig::default(), &FixedMinter("ignored"));
        assert!(out.session_id.is_none());
        let err = out.response.error.unwrap();
        assert_eq!(err.code, ErrorCode::MethodNotFound.code());
        assert!(out.response.result.is_none());
    }

    #[test]
    fn rejects_missing_params() {
        let r = req("initialize", None);
        let out = handle_initialize(&r, &InitializeConfig::default(), &FixedMinter("ignored"));
        assert!(out.session_id.is_none());
        let err = out.response.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn rejects_garbage_params_shape() {
        let r = req("initialize", Some(json!("not-an-object")));
        let out = handle_initialize(&r, &InitializeConfig::default(), &FixedMinter("ignored"));
        assert!(out.session_id.is_none());
        let err = out.response.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn rejects_empty_protocol_version() {
        let mut p = ok_init_params();
        p["protocolVersion"] = json!("");
        let r = req("initialize", Some(p));
        let out = handle_initialize(&r, &InitializeConfig::default(), &FixedMinter("ignored"));
        assert!(out.session_id.is_none());
        let err = out.response.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn negotiates_to_newest_when_client_version_unknown() {
        let mut p = ok_init_params();
        p["protocolVersion"] = json!("9999-99-99");
        let r = req("initialize", Some(p));
        let out = handle_initialize(
            &r,
            &InitializeConfig::default(),
            &FixedMinter("test-session-002"),
        );
        let result = out.response.result.unwrap();
        // Server returned its newest version, NOT the client's unknown one.
        assert_eq!(result["protocolVersion"], json!(MCP_PROTOCOL_VERSION));
        // Session is still established — the client gets to decide
        // whether to disconnect.
        assert!(out.session_id.is_some());
    }

    #[test]
    fn echoes_client_version_when_supported() {
        let cfg = InitializeConfig {
            supported_protocol_versions: vec!["2025-03-26".into(), "2024-11-05".into()],
            ..Default::default()
        };
        let mut p = ok_init_params();
        p["protocolVersion"] = json!("2024-11-05");
        let r = req("initialize", Some(p));
        let out = handle_initialize(&r, &cfg, &FixedMinter("test-session-003"));
        let result = out.response.result.unwrap();
        assert_eq!(result["protocolVersion"], json!("2024-11-05"));
    }

    #[test]
    fn instructions_included_when_set() {
        let cfg = InitializeConfig {
            instructions: Some("hello world".into()),
            ..Default::default()
        };
        let r = req("initialize", Some(ok_init_params()));
        let out = handle_initialize(&r, &cfg, &FixedMinter("s"));
        let result = out.response.result.unwrap();
        assert_eq!(result["instructions"], json!("hello world"));
    }

    #[test]
    fn instructions_omitted_when_unset() {
        let r = req("initialize", Some(ok_init_params()));
        let out = handle_initialize(&r, &InitializeConfig::default(), &FixedMinter("s"));
        let result = out.response.result.unwrap();
        assert!(result.get("instructions").is_none());
    }

    #[test]
    fn capabilities_serialise_camel_case() {
        let cfg = InitializeConfig {
            capabilities: ServerCapabilities {
                tools: Some(json!({})),
                logging: Some(json!({})),
                ..Default::default()
            },
            ..Default::default()
        };
        let r = req("initialize", Some(ok_init_params()));
        let out = handle_initialize(&r, &cfg, &FixedMinter("s"));
        let body = serde_json::to_string(&out.response.result.unwrap()).unwrap();
        // Spec §lifecycle uses lowercase keys.
        assert!(body.contains("\"tools\""));
        assert!(body.contains("\"logging\""));
    }

    #[test]
    fn id_preserved_across_response() {
        let mut r = req("initialize", Some(ok_init_params()));
        r.id = Id::String("abc-123".into());
        let out = handle_initialize(&r, &InitializeConfig::default(), &FixedMinter("s"));
        assert_eq!(out.response.id, Id::String("abc-123".into()));
    }

    #[test]
    fn empty_supported_versions_returns_internal_error() {
        let cfg = InitializeConfig {
            supported_protocol_versions: vec![],
            ..Default::default()
        };
        let r = req("initialize", Some(ok_init_params()));
        let out = handle_initialize(&r, &cfg, &FixedMinter("s"));
        let err = out.response.error.unwrap();
        assert_eq!(err.code, ErrorCode::InternalError.code());
        assert!(out.session_id.is_none());
    }

    #[test]
    fn os_rng_minter_produces_64_char_hex() {
        let m = OsRngSessionMinter;
        let id = m.mint();
        assert_eq!(id.as_str().len(), 64);
        assert!(
            id.as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex, got: {}",
            id.as_str()
        );
    }

    #[test]
    fn os_rng_minter_produces_distinct_ids() {
        let m = OsRngSessionMinter;
        let a = m.mint();
        let b = m.mint();
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn unknown_extra_params_fields_are_tolerated() {
        // Forward compatibility: the spec allows future field
        // additions; our parser must ignore them rather than reject.
        let mut p = ok_init_params();
        p["futureField"] = json!({"some": "value"});
        let r = req("initialize", Some(p));
        let out = handle_initialize(&r, &InitializeConfig::default(), &FixedMinter("s"));
        assert!(out.response.error.is_none());
    }
}
