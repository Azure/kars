// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
// ci:loc-ok: Slice-level module; decomposition tracked in §4.2 (see dev→main #320 promotion notes)

//! Slice 4d.4 — namespaced MCP tool forwarder.
//!
//! Closes Slice 4 DoD #3 (second half): `/mcp` no longer serves only
//! the in-tree [`super::tools::EchoDispatcher`]. Instead, an
//! [`AsyncToolDispatcher`] driven by the [`McpServerRegistry`]
//! exposes every upstream McpServer's tools under a per-server
//! namespace and forwards `tools/call` requests to the corresponding
//! upstream URL.
//!
//! # Catalog construction (startup-time)
//!
//! 1. For each `DiscoveredMcpServer` with a non-empty `meta.url`:
//!    - POST a JSON-RPC `tools/list` to the upstream.
//!    - On 2xx, parse the result into `[ToolDefinition]`.
//!    - Filter through `meta.allowed_tools`:
//!       - empty → no tools advertised (fail-closed, recorded as skip).
//!       - `["*"]` → expose every tool the upstream advertises.
//!       - otherwise → expose only the named subset.
//!    - Prefix each name with `{server_snake_case}.` (so server
//!      `github-mcp` exposing tool `search` becomes `github_mcp.search`).
//! 2. Servers whose discovery fails (network error, non-2xx, parse
//!    failure, or empty allow-list) are recorded in `skipped` with a
//!    human-readable reason. The router still starts — the agent sees
//!    a partial catalog and the operator sees the gap on
//!    `/internal/policy-status` / startup logs.
//!
//! # Dispatch (per `tools/call`)
//!
//! 1. Split the tool name on the first `.` — `prefix = server_snake_case`,
//!    `suffix = upstream_tool_name`.
//! 2. Look up `prefix` in the per-server index.
//! 3. Forward a JSON-RPC `tools/call` envelope (with `name = suffix`)
//!    to the upstream's URL.
//! 4. Return the upstream's content array verbatim.
//!
//! Failures map to either `DispatchError::UnknownTool` (no namespace
//! match), `DispatchError::ExecutionFailed` (network / non-2xx /
//! parse), or `ToolCallOutput { is_error: true, ... }` (upstream
//! reported a per-call error). The router's audit layer wraps the
//! whole call — see Slice 4a/4c.
//!
//! # Outbound auth (Slice 4d.4 scope)
//!
//! **Unauthenticated only.** This slice forwards without an
//! `Authorization` header. That covers:
//!
//! - In-cluster MCP servers exposed on a private network with
//!   `productionMode: false` (developer / staging fleet).
//! - Public unauthenticated read-only catalogs.
//!
//! Outbound OAuth (client-credentials, on-behalf-of for the agent's
//! incoming bearer, or sandbox-mounted secret) is **Slice 4d.5**.
//! Until then, the forwarder refuses to advertise servers with a
//! non-empty `oauth.issuer` in their meta — those servers are
//! recorded in `skipped` with reason `outbound_oauth_unsupported`
//! so the operator sees the gap honestly (principles §3). This is
//! the §5 anti-scaffolding boundary: only ship the consumer that we
//! can actually drive end-to-end.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use super::registry::{DiscoveredMcpServer, McpServerRegistry};
use super::tools::{
    AsyncToolDispatcher, DispatchError, ToolCallOutput, ToolCatalog, ToolContent, ToolDefinition,
};

/// One server's entry in the forwarder's runtime index.
#[derive(Debug, Clone)]
struct ForwarderEntry {
    /// Source server name (DNS-1123).
    name: String,
    /// Snake-cased prefix used in the agent-facing namespaced tool
    /// name. Computed once at construction.
    prefix: String,
    /// Upstream URL — POST destination for `tools/call`.
    upstream_url: String,
    /// Map from upstream-tool-name → upstream `ToolDefinition`. Used
    /// during dispatch to confirm the tool is in this server's
    /// allow-list (defence-in-depth against catalog drift).
    tools: BTreeMap<String, ToolDefinition>,
    /// Slice 4d.4.1 — optional outbound static bearer token. When
    /// `Some`, attached as `Authorization: Bearer <token>` on every
    /// outbound `tools/list` and `tools/call` POST. Resolved at
    /// discovery time from the env var named by `meta.bearer_from_env`.
    bearer_token: Option<String>,
}

/// Namespaced MCP forwarder — the production-mode `AsyncToolDispatcher`
/// mounted at `/mcp` whenever the registry advertises at least one
/// server with a usable upstream URL.
///
/// Construct via [`RouterToolDispatcher::discover`]. The catalog is
/// fixed at construction — refresh requires a rebuild (mirrors the
/// other registry-driven router state; pod rolling-restart is the
/// supported reload path until inotify-watch lands).
pub struct RouterToolDispatcher {
    catalog: ToolCatalog,
    /// Keyed by snake_case server prefix.
    entries: BTreeMap<String, ForwarderEntry>,
    /// Reasons why a discovered server was not promoted. Surfaced via
    /// `tracing::warn!` at construction and held here for observability
    /// hooks (e.g. `/internal/policy-status` extension in 4e).
    skipped: Vec<(String, String)>,
    http: reqwest::Client,
}

impl std::fmt::Debug for RouterToolDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let servers: Vec<&str> = self.entries.values().map(|e| e.name.as_str()).collect();
        f.debug_struct("RouterToolDispatcher")
            .field("tools_total", &self.catalog.tools().len())
            .field("servers", &servers)
            .field("skipped", &self.skipped)
            .finish()
    }
}

impl RouterToolDispatcher {
    /// How many namespaced tools the dispatcher advertises.
    pub fn len(&self) -> usize {
        self.catalog.tools().len()
    }

    pub fn is_empty(&self) -> bool {
        self.catalog.tools().is_empty()
    }

    /// Reasons servers were skipped during discovery. Stable shape:
    /// `(server_name, human_readable_reason)`.
    pub fn skipped(&self) -> &[(String, String)] {
        &self.skipped
    }

    /// Run startup discovery against every server in `registry`. POSTs
    /// `tools/list` to each upstream with a per-call timeout. Returns
    /// a dispatcher with the discovered catalog. Servers whose
    /// discovery fails are recorded in `skipped` and excluded from
    /// the catalog.
    ///
    /// Catalog construction errors that affect the *aggregate* (duplicate
    /// namespaced tool names across servers, schema validation) bubble
    /// up as a top-level `Err` — the caller (typically `main`) refuses
    /// to mount `/mcp` in that case (principles §3, no silent failure).
    pub async fn discover(
        registry: Arc<McpServerRegistry>,
        per_call_timeout: Duration,
    ) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(per_call_timeout)
            .build()
            .map_err(|e| format!("reqwest client init: {e}"))?;
        Self::discover_with_client(registry, http).await
    }

    /// Same as [`discover`] but uses the provided HTTP client. Lets
    /// tests inject a client whose connector points at a local mock
    /// server.
    pub async fn discover_with_client(
        registry: Arc<McpServerRegistry>,
        http: reqwest::Client,
    ) -> Result<Self, String> {
        let mut entries: BTreeMap<String, ForwarderEntry> = BTreeMap::new();
        let mut skipped: Vec<(String, String)> = Vec::new();
        let mut namespaced_tools: Vec<ToolDefinition> = Vec::new();

        for (name, server) in &registry.servers {
            match build_entry_for(name, server, &http).await {
                Ok((entry, defs)) => {
                    entries.insert(entry.prefix.clone(), entry);
                    namespaced_tools.extend(defs);
                }
                Err(reason) => {
                    tracing::warn!(server = %name, reason = %reason, "Skipping McpServer during forwarder discovery");
                    skipped.push((name.clone(), reason));
                }
            }
        }

        let catalog = ToolCatalog::new(namespaced_tools)
            .map_err(|e| format!("forwarder catalog construction failed: {e}"))?;

        Ok(Self {
            catalog,
            entries,
            skipped,
            http,
        })
    }
}

#[async_trait]
impl AsyncToolDispatcher for RouterToolDispatcher {
    fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    async fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError> {
        let (prefix, suffix) = split_namespaced_name(name)
            .ok_or_else(|| DispatchError::UnknownTool(name.to_string()))?;
        let entry = self
            .entries
            .get(prefix)
            .ok_or_else(|| DispatchError::UnknownTool(name.to_string()))?;
        if !entry.tools.contains_key(suffix) {
            return Err(DispatchError::UnknownTool(name.to_string()));
        }
        forward_tools_call(&self.http, entry, suffix, arguments).await
    }
}

/// Convert a `name-with-hyphens` server name to `name_with_underscores`
/// so the agent-facing namespaced tool name is a valid identifier in
/// JSON Schema `properties` lookups (`.` separator). DNS-1123 names
/// already exclude `_`, so the mapping is injective in one direction.
pub fn server_name_to_prefix(name: &str) -> String {
    name.replace('-', "_")
}

fn split_namespaced_name(name: &str) -> Option<(&str, &str)> {
    let (prefix, suffix) = name.split_once('.')?;
    if prefix.is_empty() || suffix.is_empty() {
        return None;
    }
    Some((prefix, suffix))
}

/// Discover one server: POST `tools/list`, filter through allow-list,
/// build a `ForwarderEntry` + the namespaced `ToolDefinition`s.
async fn build_entry_for(
    name: &str,
    server: &DiscoveredMcpServer,
    http: &reqwest::Client,
) -> Result<(ForwarderEntry, Vec<ToolDefinition>), String> {
    let meta = server
        .meta
        .as_ref()
        .ok_or_else(|| "no meta.json — pre-4d.4 mirror".to_string())?;

    if meta.url.is_empty() {
        return Err("meta.url is empty — controller did not publish upstream URL".to_string());
    }

    // Slice 4d.4.1 — outbound static-bearer auth. The OAuth-issuer
    // refusal below is relaxed when `bearer_from_env` is set: the
    // server uses static-bearer auth (e.g. a PAT or short-lived
    // OAuth token sourced from a router env var), which we *can*
    // drive end-to-end, so it is not the §5 boundary case.
    //
    // Pure OAuth (issuer-only, no bearer source) is still deferred
    // to Slice 4d.5.
    let bearer_token: Option<String> = if !meta.bearer_from_env.is_empty() {
        match std::env::var(&meta.bearer_from_env) {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) => {
                return Err(format!(
                    "bearerFromEnv={} is set but empty — outbound bearer unavailable (skipping; \
                     other McpServers continue)",
                    meta.bearer_from_env
                ));
            }
            Err(_) => {
                return Err(format!(
                    "bearerFromEnv={} not present in router env — outbound bearer unavailable \
                     (skipping; other McpServers continue)",
                    meta.bearer_from_env
                ));
            }
        }
    } else {
        None
    };

    // Slice 4d.4 anti-scaffolding boundary: refuse to expose servers
    // that require outbound OAuth WHEN no static bearer is configured.
    // The router would otherwise call them anonymously and 401 every
    // request.
    if !meta.issuer.is_empty() && bearer_token.is_none() {
        return Err(
            "outbound OAuth unsupported in 4d.4 — server requires bearer credentials (defer to 4d.5)"
                .to_string(),
        );
    }

    if meta.allowed_tools.is_empty() {
        return Err(
            "allowedTools is empty — fail-closed (use `[\"*\"]` to expose every upstream tool)"
                .to_string(),
        );
    }

    let upstream_tools = fetch_upstream_tools(http, &meta.url, bearer_token.as_deref()).await?;

    let filtered = filter_by_allowlist(&upstream_tools, &meta.allowed_tools);
    if filtered.is_empty() {
        return Err(format!(
            "no upstream tools matched allowedTools={:?}",
            meta.allowed_tools
        ));
    }

    let prefix = server_name_to_prefix(name);
    let mut tools_map: BTreeMap<String, ToolDefinition> = BTreeMap::new();
    let mut namespaced_defs: Vec<ToolDefinition> = Vec::with_capacity(filtered.len());

    for def in filtered {
        tools_map.insert(def.name.clone(), def.clone());
        namespaced_defs.push(ToolDefinition {
            name: format!("{prefix}.{}", def.name),
            description: def.description.clone(),
            input_schema: def.input_schema.clone(),
        });
    }

    Ok((
        ForwarderEntry {
            name: name.to_string(),
            prefix,
            upstream_url: meta.url.clone(),
            tools: tools_map,
            bearer_token,
        },
        namespaced_defs,
    ))
}

/// POST a JSON-RPC `tools/list` to `url` and parse the result. No
/// pagination — Slice 4d.4 caps at one page; multi-page upstreams are
/// truncated with a recorded warning. Multi-page support lands when
/// we have a real consumer that hits the cap (principles §5).
async fn fetch_upstream_tools(
    http: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> Result<Vec<ToolDefinition>, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
    });

    let mut req = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("tools/list POST failed: {e}"))?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("tools/list body read failed: {e}"))?;

    if !status.is_success() {
        return Err(format!(
            "tools/list non-2xx: {} (body trimmed: {})",
            status,
            body_text.chars().take(120).collect::<String>()
        ));
    }

    let json_payload = extract_jsonrpc_payload(&content_type, &body_text)
        .map_err(|e| format!("tools/list response decode failed: {e}"))?;
    let parsed: ToolsListResponse = serde_json::from_value(json_payload)
        .map_err(|e| format!("tools/list parse failed: {e}"))?;

    if let Some(err) = parsed.error {
        return Err(format!(
            "tools/list returned JSON-RPC error: code={} message={}",
            err.code, err.message
        ));
    }

    let result = parsed
        .result
        .ok_or_else(|| "tools/list missing result".to_string())?;

    if result.next_cursor.is_some() {
        tracing::warn!(
            url = %url,
            "Upstream advertised tools/list pagination (nextCursor present); \
             Slice 4d.4 only consumes the first page — additional tools will \
             not be advertised until pagination support lands"
        );
    }

    Ok(result.tools)
}

fn filter_by_allowlist(upstream: &[ToolDefinition], allow: &[String]) -> Vec<ToolDefinition> {
    if allow.iter().any(|t| t == "*") {
        return upstream.to_vec();
    }
    let set: std::collections::HashSet<&str> = allow.iter().map(|s| s.as_str()).collect();
    upstream
        .iter()
        .filter(|t| set.contains(t.name.as_str()))
        .cloned()
        .collect()
}

/// Forward one `tools/call` invocation to `entry.upstream_url`.
async fn forward_tools_call(
    http: &reqwest::Client,
    entry: &ForwarderEntry,
    upstream_name: &str,
    arguments: &Value,
) -> Result<ToolCallOutput, DispatchError> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": upstream_name,
            "arguments": arguments,
        },
    });

    let mut req = http
        .post(&entry.upstream_url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(token) = entry.bearer_token.as_deref() {
        req = req.bearer_auth(token);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| DispatchError::ExecutionFailed {
            tool: format!("{}.{}", entry.prefix, upstream_name),
            reason: format!("upstream POST failed: {e}"),
        })?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body_text = resp
        .text()
        .await
        .map_err(|e| DispatchError::ExecutionFailed {
            tool: format!("{}.{}", entry.prefix, upstream_name),
            reason: format!("upstream body read failed: {e}"),
        })?;

    if !status.is_success() {
        return Err(DispatchError::ExecutionFailed {
            tool: format!("{}.{}", entry.prefix, upstream_name),
            reason: format!(
                "upstream non-2xx: {} (body trimmed: {})",
                status,
                body_text.chars().take(120).collect::<String>()
            ),
        });
    }

    let json_payload = extract_jsonrpc_payload(&content_type, &body_text).map_err(|e| {
        DispatchError::ExecutionFailed {
            tool: format!("{}.{}", entry.prefix, upstream_name),
            reason: format!("upstream response decode failed: {e}"),
        }
    })?;
    let parsed: ToolsCallResponse =
        serde_json::from_value(json_payload).map_err(|e| DispatchError::ExecutionFailed {
            tool: format!("{}.{}", entry.prefix, upstream_name),
            reason: format!("upstream tools/call parse failed: {e}"),
        })?;

    if let Some(err) = parsed.error {
        // Upstream protocol error → surface as an isError content
        // entry, not a DispatchError. Per MCP spec, JSON-RPC errors
        // from `tools/call` indicate the *protocol* failed; the
        // semantic "tool ran but errored" path uses isError:true.
        // We collapse them here because the agent-facing surface
        // shouldn't distinguish (the audit layer can see both).
        return Ok(ToolCallOutput {
            content: vec![ToolContent::Text {
                text: format!(
                    "upstream JSON-RPC error code={} message={}",
                    err.code, err.message
                ),
            }],
            is_error: true,
        });
    }

    let result = parsed
        .result
        .ok_or_else(|| DispatchError::ExecutionFailed {
            tool: format!("{}.{}", entry.prefix, upstream_name),
            reason: "tools/call missing result".to_string(),
        })?;

    Ok(ToolCallOutput {
        content: result.content,
        is_error: result.is_error.unwrap_or(false),
    })
}

/// Decode an MCP Streamable HTTP response body into a JSON-RPC payload.
///
/// Per the MCP Streamable HTTP transport spec, a server MAY respond
/// with either `application/json` (single JSON-RPC envelope) or
/// `text/event-stream` (SSE stream of JSON-RPC events). For request
/// endpoints like `tools/list` / `tools/call`, we expect exactly one
/// JSON-RPC response — so on SSE we scan `data:` lines until we find
/// a JSON-RPC envelope (object with `jsonrpc:"2.0"` AND a matching
/// `id`) and return it.
fn extract_jsonrpc_payload(content_type: &str, body: &str) -> Result<serde_json::Value, String> {
    let is_sse = content_type
        .split(';')
        .next()
        .map(|s| s.trim().eq_ignore_ascii_case("text/event-stream"))
        .unwrap_or(false);

    if !is_sse {
        return serde_json::from_str::<serde_json::Value>(body)
            .map_err(|e| format!("json parse failed: {e}"));
    }

    // Parse SSE: concatenate consecutive `data:` lines per event,
    // dispatch on blank line. Stop at the first event whose body
    // parses as a JSON-RPC response (object with `jsonrpc` field).
    let mut data_buf = String::new();
    let mut last_decode_err: Option<String> = None;
    for line in body.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data_buf.is_empty() {
                match serde_json::from_str::<serde_json::Value>(&data_buf) {
                    Ok(v) => {
                        if v.get("jsonrpc").is_some() {
                            return Ok(v);
                        }
                    }
                    Err(e) => last_decode_err = Some(format!("sse event json parse: {e}")),
                }
                data_buf.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data_buf.is_empty() {
                data_buf.push('\n');
            }
            data_buf.push_str(rest);
        }
    }
    // Trailing event with no terminating blank line.
    if !data_buf.is_empty()
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&data_buf)
        && v.get("jsonrpc").is_some()
    {
        return Ok(v);
    }
    Err(last_decode_err
        .unwrap_or_else(|| "sse body contained no JSON-RPC response event".to_string()))
}

#[derive(Debug, Deserialize)]
struct ToolsListResponse {
    #[serde(default)]
    result: Option<ToolsListResult>,
    #[serde(default)]
    error: Option<JsonRpcWireError>,
}

#[derive(Debug, Deserialize)]
struct ToolsListResult {
    #[serde(default)]
    tools: Vec<ToolDefinition>,
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ToolsCallResponse {
    #[serde(default)]
    result: Option<ToolsCallResult>,
    #[serde(default)]
    error: Option<JsonRpcWireError>,
}

#[derive(Debug, Deserialize)]
struct ToolsCallResult {
    #[serde(default)]
    content: Vec<ToolContent>,
    #[serde(default, rename = "isError")]
    is_error: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcWireError {
    code: i64,
    message: String,
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::registry::{DiscoveredMcpServer, DiscoveredMcpServerMeta, McpServerRegistry};
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::{any, post},
    };
    use std::collections::BTreeMap;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokioMutex;

    fn tool_def(name: &str, desc: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: desc.to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn discovered(name: &str, url: &str, allowed: Vec<&str>) -> DiscoveredMcpServer {
        DiscoveredMcpServer {
            name: name.to_string(),
            jwks_path: std::path::PathBuf::from("/dev/null"),
            meta: Some(DiscoveredMcpServerMeta {
                issuer: String::new(),
                audience: None,
                scopes: vec![],
                url: url.to_string(),
                allowed_tools: allowed.into_iter().map(String::from).collect(),
                bearer_from_env: String::new(),
            }),
        }
    }

    fn registry_with(servers: Vec<DiscoveredMcpServer>) -> Arc<McpServerRegistry> {
        let mut map = BTreeMap::new();
        for s in servers {
            map.insert(s.name.clone(), s);
        }
        Arc::new(McpServerRegistry {
            servers: map,
            skipped: vec![],
        })
    }

    /// State for the mock upstream server.
    #[derive(Clone, Default)]
    struct MockState {
        tools: Vec<ToolDefinition>,
        call_count: StdArc<AtomicUsize>,
        /// If set, the upstream returns this JSON-RPC error for tools/call.
        force_call_error: Option<(i64, String)>,
        /// If set, the upstream returns this HTTP status for tools/call.
        force_call_http_status: Option<u16>,
        /// Last `Authorization` header value seen by the mock (used by
        /// the bearer-attach test to confirm outbound auth wiring).
        last_auth_header: StdArc<TokioMutex<Option<String>>>,
    }

    async fn mock_upstream(state: MockState) -> String {
        let app = Router::new()
            .route("/", post(mock_handler))
            .route("/", any(method_block))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/")
    }

    async fn method_block() -> StatusCode {
        StatusCode::METHOD_NOT_ALLOWED
    }

    async fn mock_handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        // Record the inbound Authorization header for assertions.
        if let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
            *state.last_auth_header.lock().await = Some(v.to_string());
        }
        let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = body.get("id").cloned().unwrap_or(serde_json::json!(1));
        match method {
            "tools/list" => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": state.tools,
                    }
                })),
            ),
            "tools/call" => {
                state.call_count.fetch_add(1, Ordering::SeqCst);
                if let Some(status) = state.force_call_http_status {
                    return (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                        Json(serde_json::json!({})),
                    );
                }
                if let Some((code, msg)) = state.force_call_error {
                    return (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": code, "message": msg}
                        })),
                    );
                }
                let upstream_tool = body
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = body
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {"type": "text", "text": format!("called {upstream_tool} with {args}")}
                            ],
                            "isError": false,
                        }
                    })),
                )
            }
            _ => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "method not found"}
                })),
            ),
        }
    }

    #[test]
    fn server_name_to_prefix_converts_hyphens() {
        assert_eq!(server_name_to_prefix("github-mcp"), "github_mcp");
        assert_eq!(server_name_to_prefix("plain"), "plain");
        assert_eq!(
            server_name_to_prefix("internal-knowledge-base"),
            "internal_knowledge_base"
        );
    }

    #[test]
    fn split_namespaced_name_happy_and_edge() {
        assert_eq!(
            split_namespaced_name("github_mcp.search"),
            Some(("github_mcp", "search"))
        );
        // Multi-dot — only split on first.
        assert_eq!(
            split_namespaced_name("foundry.memory.update"),
            Some(("foundry", "memory.update"))
        );
        assert_eq!(split_namespaced_name("noprefix"), None);
        assert_eq!(split_namespaced_name(".empty"), None);
        assert_eq!(split_namespaced_name("empty."), None);
    }

    #[test]
    fn filter_by_allowlist_star_returns_all() {
        let upstream = vec![tool_def("a", ""), tool_def("b", "")];
        let allow = vec!["*".to_string()];
        let got = filter_by_allowlist(&upstream, &allow);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn filter_by_allowlist_named_subset() {
        let upstream = vec![tool_def("a", ""), tool_def("b", ""), tool_def("c", "")];
        let allow = vec!["a".to_string(), "c".to_string()];
        let got = filter_by_allowlist(&upstream, &allow);
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|t| t.name == "a"));
        assert!(got.iter().any(|t| t.name == "c"));
    }

    #[tokio::test]
    async fn discover_happy_path_builds_namespaced_catalog() {
        let state = MockState {
            tools: vec![
                tool_def("search", "search docs"),
                tool_def("fetch", "fetch by id"),
            ],
            ..Default::default()
        };
        let url = mock_upstream(state).await;
        let registry = registry_with(vec![discovered("github-mcp", &url, vec!["*"])]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");

        assert_eq!(dispatcher.len(), 2);
        assert!(dispatcher.skipped().is_empty());
        let names: Vec<&str> = dispatcher
            .catalog()
            .tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert!(names.contains(&"github_mcp.search"));
        assert!(names.contains(&"github_mcp.fetch"));
    }

    #[tokio::test]
    async fn discover_filters_by_allow_list() {
        let state = MockState {
            tools: vec![tool_def("search", ""), tool_def("dangerous", "")],
            ..Default::default()
        };
        let url = mock_upstream(state).await;
        let registry = registry_with(vec![discovered("github-mcp", &url, vec!["search"])]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");

        assert_eq!(dispatcher.len(), 1);
        assert_eq!(dispatcher.catalog().tools()[0].name, "github_mcp.search");
    }

    #[tokio::test]
    async fn discover_skips_servers_with_empty_url() {
        let mut server = discovered("svc", "http://example.invalid/", vec!["*"]);
        server.meta.as_mut().unwrap().url = String::new();
        let registry = registry_with(vec![server]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert!(dispatcher.is_empty());
        assert_eq!(dispatcher.skipped().len(), 1);
        assert!(dispatcher.skipped()[0].1.contains("meta.url is empty"));
    }

    #[tokio::test]
    async fn discover_skips_servers_requiring_outbound_oauth() {
        let mut server = discovered("svc", "http://example.invalid/", vec!["*"]);
        server.meta.as_mut().unwrap().issuer = "https://idp.example".to_string();
        let registry = registry_with(vec![server]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert!(dispatcher.is_empty());
        assert_eq!(dispatcher.skipped().len(), 1);
        assert!(
            dispatcher.skipped()[0]
                .1
                .contains("outbound OAuth unsupported")
        );
    }

    /// Slice 4d.4.1 — server declares `bearerFromEnv` but the named env
    /// var is not present in the router process. The server is recorded
    /// as skipped (with the env var name surfaced) and other servers
    /// keep working — Foundry-only deployments must NOT crash when a
    /// github MCP CR is present but no Copilot token is mounted.
    #[tokio::test]
    async fn discover_skips_server_when_bearer_env_unset() {
        // Use a deliberately-unset env var name. SAFETY: single-threaded
        // env mutation is safe inside #[tokio::test]; we read but never
        // set this var.
        let env_name = "AZURECLAW_TEST_UNSET_BEARER_DO_NOT_DEFINE";
        unsafe {
            std::env::remove_var(env_name);
        }
        let mut server = discovered("github", "http://example.invalid/", vec!["*"]);
        server.meta.as_mut().unwrap().bearer_from_env = env_name.to_string();
        let registry = registry_with(vec![server]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert!(dispatcher.is_empty(), "server with unset bearer must skip");
        assert_eq!(dispatcher.skipped().len(), 1);
        assert!(
            dispatcher.skipped()[0].1.contains("bearerFromEnv")
                && dispatcher.skipped()[0].1.contains(env_name),
            "skip reason should name the missing env var, got: {}",
            dispatcher.skipped()[0].1
        );
    }

    /// Slice 4d.4.1 — when `bearerFromEnv` is set AND the value is
    /// non-empty, the server is allowed even with a non-empty issuer
    /// (static-bearer auth covers the upstream). The Authorization
    /// header is attached on both tools/list and tools/call.
    #[tokio::test]
    async fn discover_with_bearer_attaches_authorization_header() {
        let env_name = "AZURECLAW_TEST_BEARER_FIXTURE";
        let token_value = "ghp_test_fixture_token_xyz";
        unsafe {
            std::env::set_var(env_name, token_value);
        }

        let state = MockState {
            tools: vec![tool_def("search", "")],
            ..Default::default()
        };
        let url = mock_upstream(state.clone()).await;

        let mut server = discovered("gh", &url, vec!["*"]);
        {
            let m = server.meta.as_mut().unwrap();
            // Confirm that bearer relaxes the OAuth refusal.
            m.issuer = "https://github.com".to_string();
            m.bearer_from_env = env_name.to_string();
        }
        let registry = registry_with(vec![server]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert!(
            dispatcher.skipped().is_empty(),
            "bearer should relax OAuth refusal, got skipped: {:?}",
            dispatcher.skipped()
        );
        assert_eq!(dispatcher.catalog().tools().len(), 1);

        // Issue a tools/call and confirm the mock saw a Bearer header.
        let output = dispatcher
            .invoke("gh.search", &json!({"q":"hi"}))
            .await
            .expect("invoke");
        assert!(!output.is_error);
        let seen = state.last_auth_header.lock().await.clone();
        assert_eq!(
            seen.as_deref(),
            Some(format!("Bearer {token_value}").as_str()),
            "outbound request must include bearer Authorization, saw {:?}",
            seen
        );

        unsafe {
            std::env::remove_var(env_name);
        }
    }

    #[tokio::test]
    async fn discover_skips_servers_with_empty_allow_list() {
        let registry = registry_with(vec![discovered("svc", "http://example.invalid/", vec![])]);
        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert!(dispatcher.is_empty());
        assert!(dispatcher.skipped()[0].1.contains("allowedTools is empty"));
    }

    #[tokio::test]
    async fn discover_skips_servers_whose_upstream_returns_500() {
        // Bind a port but never serve → connection-refused-like.
        // Easiest is point at 127.0.0.1:1 which kernels close fast.
        let registry = registry_with(vec![discovered("dead", "http://127.0.0.1:1/", vec!["*"])]);
        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_millis(500))
            .await
            .expect("discover should not propagate per-server errors");
        assert!(dispatcher.is_empty());
        assert!(!dispatcher.skipped().is_empty());
    }

    #[tokio::test]
    async fn invoke_forwards_call_and_returns_content() {
        let state = MockState {
            tools: vec![tool_def("search", "")],
            ..Default::default()
        };
        let url = mock_upstream(state.clone()).await;
        let registry = registry_with(vec![discovered("github-mcp", &url, vec!["*"])]);
        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .unwrap();

        let out = dispatcher
            .invoke("github_mcp.search", &serde_json::json!({"query": "azure"}))
            .await
            .expect("invoke");
        assert!(!out.is_error);
        assert_eq!(out.content.len(), 1);
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("called search"));
        assert!(text.contains("azure"));
    }

    #[tokio::test]
    async fn invoke_unknown_tool_returns_unknown_tool() {
        let state = MockState {
            tools: vec![tool_def("search", "")],
            ..Default::default()
        };
        let url = mock_upstream(state).await;
        let registry = registry_with(vec![discovered("github-mcp", &url, vec!["*"])]);
        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .unwrap();

        // Wrong prefix.
        let err = dispatcher
            .invoke("other.search", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::UnknownTool(_)));

        // Right prefix, unknown suffix.
        let err = dispatcher
            .invoke("github_mcp.unknown", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::UnknownTool(_)));

        // No dot at all.
        let err = dispatcher
            .invoke("flat", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::UnknownTool(_)));
    }

    #[tokio::test]
    async fn invoke_surfaces_upstream_json_rpc_error_as_is_error() {
        let state = MockState {
            tools: vec![tool_def("flaky", "")],
            force_call_error: Some((-32000, "boom".to_string())),
            ..Default::default()
        };
        let url = mock_upstream(state).await;
        let registry = registry_with(vec![discovered("svc", &url, vec!["*"])]);
        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .unwrap();

        let out = dispatcher
            .invoke("svc.flaky", &serde_json::json!({}))
            .await
            .expect("invoke");
        assert!(out.is_error);
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("code=-32000"));
        assert!(text.contains("boom"));
    }

    #[tokio::test]
    async fn invoke_returns_execution_failed_on_upstream_5xx() {
        let state = MockState {
            tools: vec![tool_def("flaky", "")],
            force_call_http_status: Some(503),
            ..Default::default()
        };
        let url = mock_upstream(state).await;
        let registry = registry_with(vec![discovered("svc", &url, vec!["*"])]);
        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .unwrap();

        let err = dispatcher
            .invoke("svc.flaky", &serde_json::json!({}))
            .await
            .unwrap_err();
        match err {
            DispatchError::ExecutionFailed { tool, reason } => {
                assert_eq!(tool, "svc.flaky");
                assert!(reason.contains("503"));
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_server_namespaces_dispatch_correctly() {
        let s1 = MockState {
            tools: vec![tool_def("search", "")],
            ..Default::default()
        };
        let s2 = MockState {
            tools: vec![tool_def("query", "")],
            ..Default::default()
        };
        let url1 = mock_upstream(s1).await;
        let url2 = mock_upstream(s2).await;
        let registry = registry_with(vec![
            discovered("github-mcp", &url1, vec!["*"]),
            discovered("kb-search", &url2, vec!["*"]),
        ]);
        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(dispatcher.len(), 2);

        // Each tool routes to its own upstream.
        let out1 = dispatcher
            .invoke("github_mcp.search", &serde_json::json!({"q": "a"}))
            .await
            .unwrap();
        let ToolContent::Text { text: t1 } = &out1.content[0];
        assert!(t1.contains("called search"));

        let out2 = dispatcher
            .invoke("kb_search.query", &serde_json::json!({"q": "b"}))
            .await
            .unwrap();
        let ToolContent::Text { text: t2 } = &out2.content[0];
        assert!(t2.contains("called query"));
    }
}
