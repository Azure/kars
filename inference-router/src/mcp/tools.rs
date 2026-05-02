// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! MCP `tools/list` and `tools/call` method handlers — pure dispatch.
//!
//! Spec: <https://modelcontextprotocol.io/specification/2025-11-25/server/tools>
//!
//! Defines:
//!
//! - [`ToolDefinition`] — wire-format-compliant tool descriptor
//!   (`name`, `description`, `inputSchema` per the JSON-Schema draft
//!   the spec references).
//! - [`ToolCatalog`] — the set of tools the server publishes.
//! - [`ToolDispatcher`] — trait the route handler injects to wire in
//!   tool execution. Defaults to no execution if the caller passes
//!   `None`.
//! - [`EchoDispatcher`] — a minimal real implementation for the
//!   in-tree dev path / smoke testing. **Not a stub** — it is wired
//!   end-to-end and used by tests; the production tool runtime
//!   (AGT-policy-gated) replaces it via injection.
//!
//! `tools/list` and `tools/call` are dispatched purely against this
//! trait. Pagination support per spec §tools/list (opaque `cursor`
//! parameter) is included.
//!
//! # Total function discipline
//!
//! Every handler in this module returns a fully-formed
//! [`super::jsonrpc::Response`]. Validation failures are surfaced as
//! JSON-RPC errors at HTTP 200, never as panics. Tool execution errors
//! are surfaced as `isError: true` content per the MCP spec.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{ErrorCode, JsonRpcError};
use super::jsonrpc::{Request, Response};

/// Default page size for `tools/list` when the catalog has more tools
/// than fit in a single response. Caller can override via
/// [`ToolCatalog::with_page_size`].
pub const DEFAULT_PAGE_SIZE: usize = 64;

/// One published tool. Wire format follows MCP 2025-11-25 §tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the `arguments` shape. Per spec, this
    /// MUST be a valid JSON-Schema object — we validate that it
    /// parses as a JSON object at catalog-construction time.
    pub input_schema: Value,
}

/// Static or dynamic catalog of tools the server publishes. Owns the
/// list; `tools/list` returns a page of these.
#[derive(Debug, Clone)]
pub struct ToolCatalog {
    tools: Vec<ToolDefinition>,
    page_size: usize,
}

impl ToolCatalog {
    /// Build a catalog. Returns `Err` if any tool's `input_schema` is
    /// not a JSON object (per JSON-Schema draft 7+: schemas MUST be
    /// objects or booleans; we reject non-objects to keep the wire
    /// format strict).
    pub fn new(tools: Vec<ToolDefinition>) -> Result<Self, CatalogError> {
        for t in &tools {
            if !t.input_schema.is_object() && !t.input_schema.is_boolean() {
                return Err(CatalogError::InvalidSchema(t.name.clone()));
            }
            if t.name.is_empty() {
                return Err(CatalogError::EmptyName);
            }
        }
        // Detect duplicate tool names — JSON-RPC `tools/call` resolves
        // by name, so duplicates make dispatch ambiguous.
        let mut seen = std::collections::HashSet::new();
        for t in &tools {
            if !seen.insert(t.name.as_str()) {
                return Err(CatalogError::DuplicateName(t.name.clone()));
            }
        }
        Ok(Self {
            tools,
            page_size: DEFAULT_PAGE_SIZE,
        })
    }

    pub fn with_page_size(mut self, n: usize) -> Self {
        self.page_size = n.max(1);
        self
    }

    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    pub fn find(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools.iter().find(|t| t.name == name)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error("tool `{0}` has invalid inputSchema (must be object or boolean)")]
    InvalidSchema(String),
    #[error("tool name must not be empty")]
    EmptyName,
    #[error("duplicate tool name `{0}`")]
    DuplicateName(String),
}

/// One content item returned by a tool call. Spec §tools/call result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text { text: String },
}

/// Successful tool-call result. `is_error: true` indicates the tool
/// ran but returned an error result (per spec — this is distinct from
/// the JSON-RPC error path which is for protocol-level failures).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallOutput {
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

/// Errors a dispatcher can raise. These map to JSON-RPC errors —
/// distinct from a tool-level "I ran but failed" which is encoded
/// inside [`ToolCallOutput::is_error`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DispatchError {
    #[error("tool `{0}` not found in catalog")]
    UnknownTool(String),
    #[error("invalid arguments for tool `{tool}`: {reason}")]
    InvalidArguments { tool: String, reason: String },
    /// Tool execution itself failed at the protocol level (timeout,
    /// resource exhaustion, etc.). Returns -32000 (server error).
    #[error("tool `{tool}` execution failed: {reason}")]
    ExecutionFailed { tool: String, reason: String },
}

/// Strategy injected by the route handler: how to invoke a named
/// tool with its argument value.
pub trait ToolDispatcher: Send + Sync {
    /// The catalog this dispatcher publishes.
    fn catalog(&self) -> &ToolCatalog;
    /// Invoke `name` with `arguments`. Synchronous: the caller wraps
    /// async execution itself.
    fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError>;
}

/// Async strategy injected by the streamable HTTP MCP route. Tools that
/// fan out to upstream HTTP services (Foundry shim catalogue, governed
/// proxy callbacks, etc.) implement this directly so they don't have
/// to spin a runtime inside a sync trait method.
///
/// Sync dispatchers (the in-tree [`EchoDispatcher`], the customer-facing
/// `McpServer` dispatcher, and any future synchronous one) compose by
/// wrapping in [`SyncToAsync`] — no rewriting required.
#[async_trait::async_trait]
pub trait AsyncToolDispatcher: Send + Sync {
    /// The catalog this dispatcher publishes.
    fn catalog(&self) -> &ToolCatalog;
    /// Invoke `name` with `arguments`. Async: implementations may make
    /// upstream HTTP calls without blocking the router runtime.
    async fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError>;
}

/// Adapter that lifts any [`ToolDispatcher`] into an
/// [`AsyncToolDispatcher`] by delegating to the synchronous `invoke`
/// without spawning a blocking task. The sync dispatchers in tree
/// (`EchoDispatcher`, customer `McpServer` dispatchers) do not perform
/// I/O, so the adapter is essentially free.
///
/// Why a wrapper instead of a blanket impl? `PlatformDispatcher`
/// implements **both** trait flavours directly — sync returns a clear
/// "use the async path" error, async does the real upstream call.
/// A blanket impl would conflict with that explicit async impl. The
/// wrapper makes the lift opt-in at the route-construction site, which
/// also keeps the trait contract honest: anything plugged in as
/// `AsyncToolDispatcher` is genuinely async-capable, not silently
/// downgrading to sync at runtime.
pub struct SyncToAsync<D: ToolDispatcher> {
    inner: D,
}

impl<D: ToolDispatcher> SyncToAsync<D> {
    pub fn new(inner: D) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: ToolDispatcher + std::fmt::Debug> std::fmt::Debug for SyncToAsync<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncToAsync")
            .field("inner", &self.inner)
            .finish()
    }
}

#[async_trait::async_trait]
impl<D: ToolDispatcher> AsyncToolDispatcher for SyncToAsync<D> {
    fn catalog(&self) -> &ToolCatalog {
        self.inner.catalog()
    }

    async fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError> {
        self.inner.invoke(name, arguments)
    }
}

/// Minimal real dispatcher used as the dev / smoke-test default.
///
/// Echoes the arguments back as a single text content item. Wired
/// end-to-end through tests. The production runtime (AGT-policy-gated
/// tool invocation) replaces it via injection — this is **not** a
/// stub.
#[derive(Debug, Clone)]
pub struct EchoDispatcher {
    catalog: ToolCatalog,
}

impl EchoDispatcher {
    /// Single tool named `echo` that accepts `{ "text": string }` and
    /// returns it verbatim.
    pub fn standard() -> Self {
        let echo = ToolDefinition {
            name: "echo".into(),
            description: "Echoes back the `text` argument verbatim.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
        };
        Self {
            catalog: ToolCatalog::new(vec![echo]).expect("echo schema is valid"),
        }
    }

    pub fn with_catalog(catalog: ToolCatalog) -> Self {
        Self { catalog }
    }
}

impl ToolDispatcher for EchoDispatcher {
    fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError> {
        if name != "echo" {
            return Err(DispatchError::UnknownTool(name.to_string()));
        }
        let text = arguments
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DispatchError::InvalidArguments {
                tool: name.to_string(),
                reason: "missing required string property `text`".to_string(),
            })?;
        Ok(ToolCallOutput {
            content: vec![ToolContent::Text {
                text: text.to_string(),
            }],
            is_error: false,
        })
    }
}

/// Handle `tools/list`. Optional `cursor` parameter selects the page;
/// `nextCursor` returned when more pages remain.
pub fn handle_tools_list(req: &Request, dispatcher: &dyn ToolDispatcher) -> Response {
    let cursor = req
        .params
        .as_ref()
        .and_then(|p| p.get("cursor"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let catalog = dispatcher.catalog();
    let start = parse_cursor(cursor);
    let page_size = catalog.page_size;
    let end = (start + page_size).min(catalog.tools.len());

    let page = if start >= catalog.tools.len() {
        Vec::new()
    } else {
        catalog.tools[start..end].to_vec()
    };

    let mut result = serde_json::json!({
        "tools": page,
    });
    if end < catalog.tools.len() {
        result["nextCursor"] = serde_json::Value::String(format_cursor(end));
    }

    Response {
        jsonrpc: "2.0".into(),
        result: Some(result),
        error: None,
        id: req.id.clone(),
    }
}

/// Handle `tools/call`. `params` MUST contain `name` (string) and
/// `arguments` (object). The dispatcher invokes the named tool.
pub fn handle_tools_call(req: &Request, dispatcher: &dyn ToolDispatcher) -> Response {
    let params = match req.params.as_ref() {
        Some(p) => p,
        None => return invalid_params(req, "params required for tools/call"),
    };

    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return invalid_params(req, "params.name required (string)"),
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !arguments.is_object() {
        return invalid_params(req, "params.arguments must be an object");
    }

    match dispatcher.invoke(&name, &arguments) {
        Ok(output) => Response {
            jsonrpc: "2.0".into(),
            result: Some(serde_json::to_value(output).unwrap_or(Value::Null)),
            error: None,
            id: req.id.clone(),
        },
        Err(DispatchError::UnknownTool(t)) => Response {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code: ErrorCode::MethodNotFound.code(),
                message: format!("tool not found: {t}"),
                data: Some(serde_json::json!({"tool": t})),
            }),
            id: req.id.clone(),
        },
        Err(DispatchError::InvalidArguments { tool, reason }) => Response {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code: ErrorCode::InvalidParams.code(),
                message: format!("invalid arguments for {tool}: {reason}"),
                data: Some(serde_json::json!({"tool": tool, "reason": reason})),
            }),
            id: req.id.clone(),
        },
        Err(DispatchError::ExecutionFailed { tool, reason }) => Response {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code: ErrorCode::InternalError.code(),
                message: format!("tool execution failed: {tool}"),
                data: Some(serde_json::json!({"tool": tool, "reason": reason})),
            }),
            id: req.id.clone(),
        },
    }
}

fn invalid_params(req: &Request, reason: &str) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(JsonRpcError {
            code: ErrorCode::InvalidParams.code(),
            message: reason.into(),
            data: None,
        }),
        id: req.id.clone(),
    }
}

/// Async counterpart to [`handle_tools_list`]. `tools/list` doesn't
/// invoke the dispatcher — it only walks the catalog — but routing
/// through [`AsyncToolDispatcher`] keeps the type story consistent on
/// the streamable HTTP path.
pub fn handle_tools_list_async(req: &Request, dispatcher: &dyn AsyncToolDispatcher) -> Response {
    let cursor = req
        .params
        .as_ref()
        .and_then(|p| p.get("cursor"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let catalog = dispatcher.catalog();
    let start = parse_cursor(cursor);
    let page_size = catalog.page_size;
    let end = (start + page_size).min(catalog.tools.len());

    let page = if start >= catalog.tools.len() {
        Vec::new()
    } else {
        catalog.tools[start..end].to_vec()
    };

    let mut result = serde_json::json!({ "tools": page });
    if end < catalog.tools.len() {
        result["nextCursor"] = serde_json::Value::String(format_cursor(end));
    }

    Response {
        jsonrpc: "2.0".into(),
        result: Some(result),
        error: None,
        id: req.id.clone(),
    }
}

/// Async counterpart to [`handle_tools_call`]. Awaits the dispatcher's
/// invocation (which may make upstream HTTP calls) and maps errors to
/// JSON-RPC envelopes the same way as the sync variant.
pub async fn handle_tools_call_async(
    req: &Request,
    dispatcher: &dyn AsyncToolDispatcher,
) -> Response {
    let params = match req.params.as_ref() {
        Some(p) => p,
        None => return invalid_params(req, "params required for tools/call"),
    };

    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return invalid_params(req, "params.name required (string)"),
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !arguments.is_object() {
        return invalid_params(req, "params.arguments must be an object");
    }

    match dispatcher.invoke(&name, &arguments).await {
        Ok(output) => Response {
            jsonrpc: "2.0".into(),
            result: Some(serde_json::to_value(output).unwrap_or(Value::Null)),
            error: None,
            id: req.id.clone(),
        },
        Err(DispatchError::UnknownTool(t)) => Response {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code: ErrorCode::MethodNotFound.code(),
                message: format!("tool not found: {t}"),
                data: Some(serde_json::json!({"tool": t})),
            }),
            id: req.id.clone(),
        },
        Err(DispatchError::InvalidArguments { tool, reason }) => Response {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code: ErrorCode::InvalidParams.code(),
                message: format!("invalid arguments for {tool}: {reason}"),
                data: Some(serde_json::json!({"tool": tool, "reason": reason})),
            }),
            id: req.id.clone(),
        },
        Err(DispatchError::ExecutionFailed { tool, reason }) => Response {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(JsonRpcError {
                code: ErrorCode::InternalError.code(),
                message: format!("tool execution failed: {tool}"),
                data: Some(serde_json::json!({"tool": tool, "reason": reason})),
            }),
            id: req.id.clone(),
        },
    }
}

fn parse_cursor(cursor: &str) -> usize {
    // Cursors are opaque to the client. We use simple "offset:N" form
    // (still opaque from the client's POV — they can't construct one
    // themselves without seeing it). Empty / unparseable cursor =
    // start at 0.
    cursor
        .strip_prefix("offset:")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
}

fn format_cursor(n: usize) -> String {
    format!("offset:{n}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::jsonrpc::{Id, Request};

    fn req(method: &str, params: Option<Value>) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id: Id::Number(7),
        }
    }

    fn three_tool_catalog() -> ToolCatalog {
        ToolCatalog::new(vec![
            ToolDefinition {
                name: "alpha".into(),
                description: "first".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "beta".into(),
                description: "second".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "gamma".into(),
                description: "third".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
        ])
        .unwrap()
    }

    #[test]
    fn echo_dispatcher_round_trip() {
        let d = EchoDispatcher::standard();
        let out = d
            .invoke("echo", &serde_json::json!({"text": "hello"}))
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content.len(), 1);
        match &out.content[0] {
            ToolContent::Text { text } => assert_eq!(text, "hello"),
        }
    }

    #[test]
    fn echo_dispatcher_unknown_tool() {
        let d = EchoDispatcher::standard();
        let err = d.invoke("nope", &serde_json::json!({})).unwrap_err();
        assert!(matches!(err, DispatchError::UnknownTool(ref s) if s == "nope"));
    }

    #[test]
    fn echo_dispatcher_invalid_args() {
        let d = EchoDispatcher::standard();
        let err = d.invoke("echo", &serde_json::json!({})).unwrap_err();
        assert!(matches!(err, DispatchError::InvalidArguments { .. }));
    }

    #[test]
    fn catalog_rejects_invalid_schema() {
        let err = ToolCatalog::new(vec![ToolDefinition {
            name: "bad".into(),
            description: "x".into(),
            input_schema: serde_json::json!("not-an-object"),
        }])
        .unwrap_err();
        assert!(matches!(err, CatalogError::InvalidSchema(ref s) if s == "bad"));
    }

    #[test]
    fn catalog_rejects_empty_name() {
        let err = ToolCatalog::new(vec![ToolDefinition {
            name: "".into(),
            description: "x".into(),
            input_schema: serde_json::json!({}),
        }])
        .unwrap_err();
        assert!(matches!(err, CatalogError::EmptyName));
    }

    #[test]
    fn catalog_rejects_duplicate_name() {
        let err = ToolCatalog::new(vec![
            ToolDefinition {
                name: "a".into(),
                description: "x".into(),
                input_schema: serde_json::json!({}),
            },
            ToolDefinition {
                name: "a".into(),
                description: "y".into(),
                input_schema: serde_json::json!({}),
            },
        ])
        .unwrap_err();
        assert!(matches!(err, CatalogError::DuplicateName(_)));
    }

    #[test]
    fn tools_list_returns_all_tools_when_under_page_size() {
        let d = EchoDispatcher::with_catalog(three_tool_catalog());
        let r = handle_tools_list(&req("tools/list", None), &d);
        assert!(r.error.is_none());
        let result = r.result.unwrap();
        let tools = result.get("tools").and_then(|t| t.as_array()).unwrap();
        assert_eq!(tools.len(), 3);
        assert!(result.get("nextCursor").is_none());
    }

    #[test]
    fn tools_list_paginates_when_over_page_size() {
        let cat = three_tool_catalog().with_page_size(2);
        let d = EchoDispatcher::with_catalog(cat);

        let r1 = handle_tools_list(&req("tools/list", None), &d);
        let res1 = r1.result.unwrap();
        let tools1 = res1.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools1.len(), 2);
        let cursor = res1
            .get("nextCursor")
            .and_then(|v| v.as_str())
            .expect("next cursor present");

        let r2 = handle_tools_list(
            &req("tools/list", Some(serde_json::json!({"cursor": cursor}))),
            &d,
        );
        let res2 = r2.result.unwrap();
        let tools2 = res2.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools2.len(), 1);
        assert!(res2.get("nextCursor").is_none(), "no more pages");
    }

    #[test]
    fn tools_list_unknown_cursor_resets_to_first_page() {
        let d = EchoDispatcher::with_catalog(three_tool_catalog().with_page_size(2));
        let r = handle_tools_list(
            &req("tools/list", Some(serde_json::json!({"cursor": "garbage"}))),
            &d,
        );
        let tools = r
            .result
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap()
            .len();
        assert_eq!(tools, 2);
    }

    #[test]
    fn tools_list_offset_past_end_yields_empty_no_next_cursor() {
        let d = EchoDispatcher::with_catalog(three_tool_catalog().with_page_size(2));
        let r = handle_tools_list(
            &req(
                "tools/list",
                Some(serde_json::json!({"cursor": "offset:1000"})),
            ),
            &d,
        );
        let res = r.result.unwrap();
        let tools = res.get("tools").unwrap().as_array().unwrap();
        assert!(tools.is_empty());
        assert!(res.get("nextCursor").is_none());
    }

    #[test]
    fn tools_call_happy_path_via_echo() {
        let d = EchoDispatcher::standard();
        let r = handle_tools_call(
            &req(
                "tools/call",
                Some(serde_json::json!({
                    "name": "echo",
                    "arguments": {"text": "ping"}
                })),
            ),
            &d,
        );
        assert!(r.error.is_none());
        let result = r.result.unwrap();
        assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
        let content = result.get("content").and_then(|c| c.as_array()).unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(
            content[0].get("type").and_then(|v| v.as_str()),
            Some("text")
        );
        assert_eq!(
            content[0].get("text").and_then(|v| v.as_str()),
            Some("ping")
        );
    }

    #[test]
    fn tools_call_missing_params_invalid_params_error() {
        let d = EchoDispatcher::standard();
        let r = handle_tools_call(&req("tools/call", None), &d);
        let err = r.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn tools_call_missing_name_invalid_params_error() {
        let d = EchoDispatcher::standard();
        let r = handle_tools_call(
            &req("tools/call", Some(serde_json::json!({"arguments": {}}))),
            &d,
        );
        let err = r.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn tools_call_arguments_not_object_invalid_params() {
        let d = EchoDispatcher::standard();
        let r = handle_tools_call(
            &req(
                "tools/call",
                Some(serde_json::json!({"name": "echo", "arguments": "string"})),
            ),
            &d,
        );
        let err = r.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn tools_call_unknown_tool_method_not_found() {
        let d = EchoDispatcher::standard();
        let r = handle_tools_call(
            &req(
                "tools/call",
                Some(serde_json::json!({"name": "nope", "arguments": {}})),
            ),
            &d,
        );
        let err = r.error.unwrap();
        assert_eq!(err.code, ErrorCode::MethodNotFound.code());
        assert!(err.message.contains("tool not found"));
    }

    #[test]
    fn tools_call_invalid_arguments_invalid_params() {
        let d = EchoDispatcher::standard();
        let r = handle_tools_call(
            &req(
                "tools/call",
                Some(serde_json::json!({"name": "echo", "arguments": {}})),
            ),
            &d,
        );
        let err = r.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }

    /// Test dispatcher that always fails execution — exercises the
    /// InternalError branch.
    struct FailingDispatcher(ToolCatalog);
    impl ToolDispatcher for FailingDispatcher {
        fn catalog(&self) -> &ToolCatalog {
            &self.0
        }
        fn invoke(&self, name: &str, _args: &Value) -> Result<ToolCallOutput, DispatchError> {
            Err(DispatchError::ExecutionFailed {
                tool: name.into(),
                reason: "synthetic".into(),
            })
        }
    }

    #[test]
    fn tools_call_execution_failed_internal_error() {
        let d = FailingDispatcher(
            ToolCatalog::new(vec![ToolDefinition {
                name: "explode".into(),
                description: "x".into(),
                input_schema: serde_json::json!({}),
            }])
            .unwrap(),
        );
        let r = handle_tools_call(
            &req(
                "tools/call",
                Some(serde_json::json!({"name": "explode", "arguments": {}})),
            ),
            &d,
        );
        let err = r.error.unwrap();
        assert_eq!(err.code, ErrorCode::InternalError.code());
    }

    #[test]
    fn tools_list_emits_camel_case_input_schema_field() {
        // Wire-format regression guard: the spec-defined field is
        // `inputSchema` (camelCase). serde rename must hold.
        let d = EchoDispatcher::standard();
        let r = handle_tools_list(&req("tools/list", None), &d);
        let s = serde_json::to_string(&r.result.unwrap()).unwrap();
        assert!(s.contains("\"inputSchema\""), "got: {s}");
        assert!(!s.contains("\"input_schema\""));
    }

    #[test]
    fn tools_call_output_emits_camel_case_is_error_field() {
        let d = EchoDispatcher::standard();
        let r = handle_tools_call(
            &req(
                "tools/call",
                Some(serde_json::json!({"name": "echo", "arguments": {"text": "x"}})),
            ),
            &d,
        );
        let s = serde_json::to_string(&r.result.unwrap()).unwrap();
        assert!(s.contains("\"isError\""), "got: {s}");
        assert!(!s.contains("\"is_error\""));
    }

    #[test]
    fn tools_list_id_preserved() {
        let d = EchoDispatcher::standard();
        let mut r = req("tools/list", None);
        r.id = Id::String("tracker-42".into());
        let resp = handle_tools_list(&r, &d);
        assert_eq!(resp.id, Id::String("tracker-42".into()));
    }
}
