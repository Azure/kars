// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! MCP 2026 Streamable HTTP transport — production code path.
//!
//! This module is the **router-side** implementation of the Model Context
//! Protocol's Streamable HTTP transport (spec revision `2025-03-26`,
//! [transports](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports)).
//!
//! ## Status: implemented
//!
//! The PR series `phase1/mcp-2026-*` landed the full pipeline:
//!
//! - [`jsonrpc`] — JSON-RPC 2.0 frame types + tolerant parser.
//! - [`streamable_http`] — Streamable HTTP envelope + session-id validator.
//! - [`error`] — canonical JSON-RPC + MCP-2026 error code catalogue.
//! - [`initialize`] — `initialize` handler with [`OsRngSessionMinter`].
//! - [`tools`] — `tools/list` + `tools/call` dispatch surface, in-tree
//!   [`EchoDispatcher`], pluggable [`ToolDispatcher`] trait.
//! - [`pipeline`] — request → frame → method → response pipeline.
//! - [`oauth`] — OAuth 2.1 access-token verifier (RFC 9700 / RFC 7515).
//! - [`oauth_layer`] — tower middleware that gates [`crate::routes::mcp`]
//!   in production mode.
//!
//! Production routes live in [`crate::routes::mcp`] (`POST /mcp`,
//! `GET /mcp` → 405). The `phase1/mcp-server-crd` reconciler that maps
//! `McpServer.spec.{url, auth, productionMode, scopes, allowedTools}`
//! onto a per-sandbox MCP-client runtime is a Phase 2 deliverable
//! (`phase2-full-crds`).
//!
//! ## Security posture
//!
//! Per §0.2 #8 ("never roll our own crypto, framing, or wire format")
//! every primitive in this module sits **on top of** existing well-tested
//! layers:
//!
//! - JSON parsing → `serde_json` (no hand-rolled tokenizer).
//! - HTTP transport → `axum` + `hyper` (no hand-rolled framer).
//! - OAuth crypto → `jsonwebtoken` (RSA/EdDSA) verified against the
//!   project-internal allowlist; no hand-rolled JWS parser.
//! - Session-id minting → `rand::rngs::OsRng` (cryptographic randomness).
//!
//! Negative tests cover: tampered JSON-RPC version, embedded NUL /
//! control chars in session id, non-ASCII session id, empty batch,
//! oversize frame, missing `Accept` content types, `alg=none`, `alg`
//! confusion, kid mismatch, expired token, replayed token.

pub mod error;
pub mod initialize;
pub mod jsonrpc;
pub mod oauth;
pub mod oauth_layer;
pub mod pipeline;
pub mod platform;
pub mod streamable_http;
pub mod tools;

pub use error::{ErrorCode, JsonRpcError};
pub use initialize::{
    InitializeConfig, InitializeOutcome, OsRngSessionMinter, ServerCapabilities, ServerInfo,
    SessionMinter, handle_initialize,
};
pub use jsonrpc::{Frame, Id, Notification, Request, Response, parse_frame};
pub use oauth::{OAuthError, OAuthVerifierConfig, VerifiedToken, verify_access_token};
pub use oauth_layer::{OAuthLayer, OAuthService, verified_token};
pub use pipeline::{ProcessOutcome, process_request, process_request_async};
pub use platform::{PlatformDispatcher, foundry_tool_catalog};
pub use streamable_http::{
    AcceptNegotiation, MAX_FRAME_BYTES, MCP_PROTOCOL_VERSION, SessionId, validate_accept_header,
};
pub use tools::{
    AsyncToolDispatcher, CatalogError, DispatchError, EchoDispatcher, SyncToAsync, ToolCallOutput,
    ToolCatalog, ToolContent, ToolDefinition, ToolDispatcher, handle_tools_call,
    handle_tools_call_async, handle_tools_list, handle_tools_list_async,
};
