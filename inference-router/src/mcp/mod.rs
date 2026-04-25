//! MCP 2026 Streamable HTTP transport — scaffold.
//!
//! This module is the **router-side** implementation of the Model Context
//! Protocol's Streamable HTTP transport (spec revision `2025-03-26`,
//! [transports](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports)).
//!
//! ## Status: scaffold (`phase1/mcp-2026-scaffold`)
//!
//! This PR lands type-level + framing-level primitives only. **No router
//! routes are wired yet.** Subsequent branches add:
//!
//! - `phase1/mcp-2026-streamable-http-routes` — POST/GET/DELETE handlers
//!   on `/mcp` mounted under `routes::inference`, with session-id state
//!   tracker and SSE response support.
//! - `phase1/mcp-2026-oauth21` — OAuth 2.1 token verifier (PKCE-aware,
//!   audience-checked, expiry-checked, replay-rejected) gated by
//!   `McpServer.spec.productionMode`.
//! - `phase1/mcp-server-crd` — `McpServer` CRD + reconciler that wires
//!   `spec.{url, auth, productionMode, scopes, allowedTools}` into a
//!   per-sandbox MCP-client runtime.
//!
//! ## Submodules (this PR)
//!
//! - [`jsonrpc`] — JSON-RPC 2.0 frame types (request, notification,
//!   response, error). Tolerant parser that returns structured errors
//!   for tampered/malformed input — no panics, no silent acceptance.
//! - [`streamable_http`] — Streamable HTTP envelope types: session-id
//!   validation per the spec (`MUST` only contain visible ASCII
//!   `0x21..=0x7E`), `Accept` header negotiation, and oversize-frame
//!   gate constants used by future route handlers.
//! - [`error`] — canonical JSON-RPC error code catalogue per the
//!   [JSON-RPC 2.0 spec](https://www.jsonrpc.org/specification#error_object)
//!   plus MCP-2026 reserved range.
//!
//! ## Security posture
//!
//! Per §0.2 #8 ("never roll our own crypto, framing, or wire format")
//! every primitive in this module sits **on top of** existing well-tested
//! layers:
//!
//! - JSON parsing → `serde_json` (no hand-rolled tokenizer).
//! - HTTP transport → `axum` + `hyper` (no hand-rolled framer).
//! - Session-id custody → caller's responsibility; this module only
//!   *validates* the wire format. The actual ID minting belongs to the
//!   forthcoming `streamable_http_routes::initialize` handler and uses
//!   `rand::rngs::OsRng` for cryptographic randomness.
//!
//! All public functions in this module are pure / side-effect-free.
//! No I/O, no logging, no global state. Tests cover:
//!
//! - Tampered JSON-RPC version (`"3.0"`, `"1.0"`, missing) → reject.
//! - Embedded NUL / control chars in session ID → reject.
//! - Session ID outside `0x21..=0x7E` → reject.
//! - Empty JSON-RPC batch → reject (per spec §6).
//! - Oversize frame → reject (config-driven cap, default 4 MiB).
//! - `Accept` header missing one of the required content types → reject.

pub mod error;
pub mod initialize;
pub mod jsonrpc;
pub mod streamable_http;

pub use error::{ErrorCode, JsonRpcError};
pub use initialize::{
    InitializeConfig, InitializeOutcome, OsRngSessionMinter, ServerCapabilities, ServerInfo,
    SessionMinter, handle_initialize,
};
pub use jsonrpc::{Frame, Id, Notification, Request, Response, parse_frame};
pub use streamable_http::{
    AcceptNegotiation, MAX_FRAME_BYTES, MCP_PROTOCOL_VERSION, SessionId, validate_accept_header,
};
