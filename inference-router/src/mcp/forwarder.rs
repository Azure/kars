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
use tokio::sync::Mutex;

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
    /// Live MCP session for a STATEFUL upstream — the `Mcp-Session-Id` plus the
    /// negotiated protocol version, established by the `initialize` handshake at
    /// discovery and reused across every `tools/call` so servers that bind state
    /// to the session (e.g. Playwright, whose browser context lives in the
    /// session) keep that state between calls. Re-established on demand when an
    /// upstream reports the session is gone (pod restart / TTL expiry).
    session: Arc<Mutex<McpSession>>,
    /// Background keepalive task for a STATEFUL session (see
    /// [`run_session_keepalive`]). MCP servers such as Playwright run a
    /// server-side heartbeat: they send the client a JSON-RPC `ping` over the
    /// standalone `GET /mcp` SSE stream and, if no response (`pong`) arrives
    /// within a few seconds (Playwright: `PLAYWRIGHT_MCP_PING_TIMEOUT_MS`,
    /// default 5000), they tear the session down. A forwarder that only issues
    /// request/response `tools/call` POSTs never receives those pings, so every
    /// session silently dies ~5s after creation — the next call then 404s with
    /// "Session not found", forcing a re-init onto a brand-new (blank) browser
    /// context. The keepalive task holds the GET stream open and answers pings,
    /// keeping the session — and the agent's live browser page — alive. Holds
    /// the task's [`AbortHandle`] so it can be cancelled when the session is
    /// re-initialized (replaced) or the dispatcher is dropped.
    keepalive: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
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

    // MCP streamable-HTTP servers may be STATEFUL: they require an
    // `initialize` handshake and bind subsequent requests to the
    // `Mcp-Session-Id` they issue. Establish that session up front
    // (best-effort: stateless servers simply return no session id and the
    // `tools/list` below proceeds unauthenticated as before).
    let session = initialize_session(http, &meta.url, bearer_token.as_deref()).await;

    let upstream_tools =
        fetch_upstream_tools(http, &meta.url, bearer_token.as_deref(), &session).await?;

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

    // For a STATEFUL session, start the heartbeat-responder so the upstream
    // doesn't reap the session (and the agent's live browser page) after its
    // ping timeout. Stateless sessions (id: None) need no keepalive.
    let keepalive: Arc<Mutex<Option<tokio::task::AbortHandle>>> = Arc::new(Mutex::new(None));
    if session.id.is_some() {
        let handle = spawn_session_keepalive(
            http.clone(),
            meta.url.clone(),
            session.clone(),
            bearer_token.clone(),
        );
        *keepalive.lock().await = Some(handle);
    }

    Ok((
        ForwarderEntry {
            name: name.to_string(),
            prefix,
            upstream_url: meta.url.clone(),
            tools: tools_map,
            bearer_token,
            session: Arc::new(Mutex::new(session)),
            keepalive,
        },
        namespaced_defs,
    ))
}

/// The MCP protocol version the router advertises in its `initialize`
/// handshake. Servers negotiate to a version they support; this is the
/// version the router proposes and falls back to when the upstream doesn't
/// echo one back.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// A negotiated MCP streamable-HTTP session.
///
/// Two pieces of per-connection state survive the `initialize` handshake and
/// MUST be replayed on every subsequent request for broad server
/// compatibility:
///
/// - **`id`** — the `Mcp-Session-Id` a STATEFUL server issues (Playwright, the
///   official TS-SDK servers, GitHub MCP, …). `None` for stateless servers,
///   which simply don't return the header.
/// - **`protocol_version`** — the version the server negotiated. Per MCP
///   2025-06-18 the client MUST send it back in the `MCP-Protocol-Version`
///   header on every post-initialize HTTP request; SDK-based servers reject
///   requests that omit it with `400 Bad Request`. We default to the version
///   we proposed when the server doesn't echo one (older servers ignore the
///   header).
#[derive(Clone, Debug)]
struct McpSession {
    id: Option<String>,
    protocol_version: String,
}

impl McpSession {
    /// A session for an upstream that did not complete a handshake (stateless,
    /// or handshake unreachable): no session id, default protocol version.
    fn stateless() -> Self {
        Self {
            id: None,
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        }
    }

    /// Apply this session's headers (`Mcp-Session-Id` when present, and always
    /// `MCP-Protocol-Version`) to an outbound request builder.
    fn apply(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req = req.header("mcp-protocol-version", &self.protocol_version);
        if let Some(sid) = self.id.as_deref() {
            req = req.header("mcp-session-id", sid);
        }
        req
    }
}

/// MCP `initialize` handshake — negotiates the protocol version and (for
/// STATEFUL upstreams) establishes the `Mcp-Session-Id` reused across every
/// subsequent request.
///
/// Best-effort by design: stateless servers (and servers that don't implement
/// the handshake) return no session header, in which case the returned session
/// has `id: None` and the caller proceeds without one — preserving the original
/// `tools/list` behaviour. Stateful servers (e.g. Playwright MCP, which returns
/// `400 "Server not initialized"` on a bare `tools/list`) issue a session id;
/// for those we also send the required `notifications/initialized` to complete
/// the handshake before any `tools/list` / `tools/call`.
///
/// Never returns an error: a failed handshake degrades to a stateless session
/// and the real signal surfaces from the subsequent `tools/list` (which keeps
/// error reporting in one place).
async fn initialize_session(http: &reqwest::Client, url: &str, bearer: Option<&str>) -> McpSession {
    let init_body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "kars-inference-router",
                "version": env!("CARGO_PKG_VERSION"),
            },
        },
    });

    let mut req = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&init_body);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(url = %url, error = %e, "MCP initialize POST failed; treating upstream as stateless");
            return McpSession::stateless();
        }
    };

    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // The initialize *result* carries the negotiated `protocolVersion`. The
    // body may be plain JSON or an SSE event stream (official TS-SDK servers
    // reply with SSE); `extract_jsonrpc_payload` handles both. If anything is
    // missing we fall back to the version we proposed.
    let protocol_version = match resp.text().await {
        Ok(body) => extract_jsonrpc_payload(&content_type, &body)
            .ok()
            .and_then(|v| {
                v.get("result")
                    .and_then(|r| r.get("protocolVersion"))
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_string()),
        Err(_) => MCP_PROTOCOL_VERSION.to_string(),
    };

    let Some(sid) = session_id else {
        // Stateless server: no session id, but still report the negotiated
        // protocol version so we send the `MCP-Protocol-Version` header.
        return McpSession {
            id: None,
            protocol_version,
        };
    };

    // Stateful server: complete the handshake. Per the MCP spec the client
    // MUST send `notifications/initialized` (carrying the session + protocol
    // headers) before issuing requests. Best-effort: a transient hiccup here
    // surfaces as a clear error on the following `tools/list`.
    let session = McpSession {
        id: Some(sid),
        protocol_version,
    };
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let mut nreq = http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&note);
    nreq = session.apply(nreq);
    if let Some(token) = bearer {
        nreq = nreq.bearer_auth(token);
    }
    if let Err(e) = nreq.send().await {
        tracing::debug!(url = %url, error = %e, "MCP notifications/initialized POST failed");
    }

    tracing::debug!(url = %url, protocol = %session.protocol_version, "Established MCP session with stateful upstream");
    session
}

/// How long the keepalive holds a single `GET /mcp` SSE connection before
/// proactively reconnecting. The connection normally stays open for the life
/// of the session; this bound just guarantees we recycle a wedged TCP
/// connection rather than block forever on a half-open socket.
const KEEPALIVE_GET_TIMEOUT: Duration = Duration::from_secs(3600);

/// Backoff before re-opening the standalone SSE stream after it ends. Short
/// enough that a transient blip doesn't leave a ping unanswered past the
/// upstream's timeout, long enough to avoid a hot loop if the server has
/// actually torn the session down (in which case the next GET 404s and the
/// task exits).
const KEEPALIVE_RECONNECT_BACKOFF: Duration = Duration::from_millis(750);

/// Spawn the [`run_session_keepalive`] task and return its [`AbortHandle`].
fn spawn_session_keepalive(
    http: reqwest::Client,
    url: String,
    session: McpSession,
    bearer: Option<String>,
) -> tokio::task::AbortHandle {
    tokio::spawn(run_session_keepalive(http, url, session, bearer)).abort_handle()
}

/// Keep a STATEFUL MCP session alive by behaving as a well-formed MCP client:
/// hold the standalone `GET /mcp` SSE stream open and answer the server's
/// JSON-RPC `ping` requests with a `pong`.
///
/// # Why this is required
///
/// MCP servers may run a server-initiated heartbeat to detect dead clients.
/// Playwright MCP does (HTTP transport, `runHeartbeat = true`): every ~3s it
/// calls `server.ping()` and, if no response arrives within
/// `PLAYWRIGHT_MCP_PING_TIMEOUT_MS` (default 5000), it calls `server.close()`,
/// which deletes the session. A forwarder that only POSTs `tools/call` and
/// reads the immediate response never sees those pings, so the session — and
/// the browser context bound to it — is reaped ~5s after creation. The next
/// `tools/call` then gets `404 "Session not found"`, the forwarder re-inits,
/// and the retry lands on a fresh blank page (`about:blank`). Holding the GET
/// stream and ponging keeps the session (and the agent's page) alive.
///
/// # Behaviour
///
/// Loops for the life of the task (cancelled via its [`AbortHandle`] when the
/// session is re-initialized or the dispatcher is dropped):
///
/// 1. Open `GET {url}` with `Accept: text/event-stream` + the session headers.
/// 2. A non-2xx (e.g. `404 Session not found`, or `405` from a server that
///    doesn't implement the standalone stream) means there's nothing to keep
///    alive this way — exit; the normal `tools/call` path handles re-init.
/// 3. Stream the SSE body. For each server→client JSON-RPC `ping`, POST a
///    `{"result":{}}` response carrying the same id and the session headers.
/// 4. If the stream ends while the session may still be valid, back off briefly
///    and reconnect.
async fn run_session_keepalive(
    http: reqwest::Client,
    url: String,
    session: McpSession,
    bearer: Option<String>,
) {
    use futures::StreamExt;

    loop {
        let mut req = http
            .get(&url)
            .header("accept", "text/event-stream")
            .header("mcp-protocol-version", &session.protocol_version)
            .timeout(KEEPALIVE_GET_TIMEOUT);
        if let Some(sid) = session.id.as_deref() {
            req = req.header("mcp-session-id", sid);
        }
        if let Some(token) = bearer.as_deref() {
            req = req.bearer_auth(token);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(url = %url, error = %e, "MCP keepalive GET failed; retrying");
                tokio::time::sleep(KEEPALIVE_RECONNECT_BACKOFF).await;
                continue;
            }
        };

        if !resp.status().is_success() {
            // 404 → session already gone; 405 → server has no standalone SSE
            // stream (so it can't be heartbeating us either). Either way, stop:
            // there is nothing this task can keep alive.
            tracing::debug!(
                url = %url,
                status = %resp.status(),
                "MCP keepalive GET not available; stopping keepalive for this session"
            );
            return;
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(url = %url, error = %e, "MCP keepalive SSE read error; reconnecting");
                    break;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            // SSE frames are newline-delimited; pull complete lines and act on
            // `data:` lines that carry a server→client `ping` request.
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim().to_string();
                buf.drain(..=nl);
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(msg) = serde_json::from_str::<Value>(payload.trim()) else {
                    continue;
                };
                if msg.get("method").and_then(|m| m.as_str()) != Some("ping") {
                    continue;
                }
                let Some(id) = msg.get("id").cloned() else {
                    // A ping *notification* (no id) needs no response.
                    continue;
                };
                let pong = json!({ "jsonrpc": "2.0", "id": id, "result": {} });
                let mut preq = http
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .json(&pong);
                preq = session.apply(preq);
                if let Some(token) = bearer.as_deref() {
                    preq = preq.bearer_auth(token);
                }
                if let Err(e) = preq.send().await {
                    tracing::debug!(url = %url, error = %e, "MCP keepalive pong POST failed");
                }
            }
        }

        // Stream ended. Back off, then reconnect — if the session was reaped the
        // next GET 404s and we exit; otherwise we resume answering pings.
        tokio::time::sleep(KEEPALIVE_RECONNECT_BACKOFF).await;
    }
}

/// pagination — Slice 4d.4 caps at one page; multi-page upstreams are
/// truncated with a recorded warning. Multi-page support lands when
/// we have a real consumer that hits the cap (principles §5).
async fn fetch_upstream_tools(
    http: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
    session: &McpSession,
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
    req = session.apply(req);
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
/// Outcome of a single `tools/call` POST attempt, distinguishing a stale
/// session (which warrants one re-handshake + retry) from a terminal result.
enum CallAttempt {
    /// Upstream produced a usable response (success or a per-call isError).
    Done(ToolCallOutput),
    /// Upstream reported its session is gone (pod restart / TTL expiry).
    /// The caller re-initializes the session and retries once. Carries the
    /// triggering signal (HTTP status + trimmed body, or the JSON-RPC error)
    /// so the re-init log line records *why* the session was deemed lost —
    /// turning any future false-positive into a one-line diagnosis instead of
    /// a guessing game.
    SessionLost { reason: String },
    /// The request failed in a way that could mean either a stale pooled
    /// connection/session or a genuine tool failure. The caller probes the
    /// existing session with `tools/list`; only a failed probe permits re-init
    /// and retry, preventing duplicate side effects when the tool actually ran.
    AmbiguousFatal {
        error: DispatchError,
        reason: String,
    },
    AmbiguousResult {
        output: ToolCallOutput,
        reason: String,
    },
    /// Terminal failure — surface to the agent as-is.
    Fatal(DispatchError),
}

/// Heuristic: does this upstream response indicate the MCP session is no
/// longer valid (so a single re-handshake + retry is warranted)?
///
/// This MUST be conservative. A false positive is *destructive* for
/// stateful servers: re-initializing throws away live per-session state
/// (e.g. a Playwright server's open browser page) and the retry lands on
/// a brand-new blank context. We therefore only treat a response as
/// session-loss on the canonical, unambiguous signals:
///
///   - the MCP SDK's exact `Server not initialized` 400 (what Playwright
///     and the official TypeScript SDK return once a session is gone), or
///   - a body that explicitly couples "session" with an expiry/not-found
///     qualifier (`expired`, `not found`, `invalid`, `unknown`, …).
///
/// Crucially we do NOT match the bare word "session": large tool
/// responses (browser snapshots, `evaluate` output) routinely embed it
/// (`sessionStorage`, page text), and matching it corrupted healthy
/// sessions mid-flight.
fn is_session_lost(status: reqwest::StatusCode, body: &str) -> bool {
    let code = status.as_u16();
    if code != 400 && code != 404 {
        return false;
    }
    body_signals_session_loss(body)
}

/// Shared session-loss body classifier for both the HTTP-status path and
/// the JSON-RPC-error path. Conservative by design (see [`is_session_lost`]).
fn body_signals_session_loss(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    // Canonical MCP SDK signal — emitted verbatim by Playwright MCP and the
    // official reference servers when the session they issued is gone.
    if b.contains("not initialized") {
        return true;
    }
    // Other servers phrase it as an expired/invalid/missing *session*. Require
    // the word "session" AND an explicit invalidation qualifier so incidental
    // occurrences of "session" in tool output never trigger a re-init.
    if b.contains("session") {
        const QUALIFIERS: [&str; 6] = [
            "expired",
            "not found",
            "invalid",
            "unknown",
            "missing",
            "no longer",
        ];
        return QUALIFIERS.iter().any(|q| b.contains(q));
    }
    false
}

async fn post_tools_call(
    http: &reqwest::Client,
    entry: &ForwarderEntry,
    upstream_name: &str,
    arguments: &Value,
    session: &McpSession,
) -> CallAttempt {
    let tool_label = format!("{}.{}", entry.prefix, upstream_name);
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
    req = session.apply(req);

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let reason = format!("upstream POST failed: {e}");
            return if session.id.is_some() {
                CallAttempt::AmbiguousFatal {
                    error: DispatchError::ExecutionFailed {
                        tool: tool_label,
                        reason: reason.clone(),
                    },
                    reason,
                }
            } else {
                CallAttempt::Fatal(DispatchError::ExecutionFailed {
                    tool: tool_label,
                    reason,
                })
            };
        }
    };

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body_text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            let reason = format!("upstream body read failed: {e}");
            return if session.id.is_some() {
                CallAttempt::AmbiguousFatal {
                    error: DispatchError::ExecutionFailed {
                        tool: tool_label,
                        reason: reason.clone(),
                    },
                    reason,
                }
            } else {
                CallAttempt::Fatal(DispatchError::ExecutionFailed {
                    tool: tool_label,
                    reason,
                })
            };
        }
    };

    if !status.is_success() {
        if is_session_lost(status, &body_text) {
            return CallAttempt::SessionLost {
                reason: format!(
                    "http {} body: {}",
                    status,
                    body_text.chars().take(160).collect::<String>()
                ),
            };
        }
        return CallAttempt::Fatal(DispatchError::ExecutionFailed {
            tool: tool_label,
            reason: format!(
                "upstream non-2xx: {} (body trimmed: {})",
                status,
                body_text.chars().take(120).collect::<String>()
            ),
        });
    }

    let json_payload = match extract_jsonrpc_payload(&content_type, &body_text) {
        Ok(v) => v,
        Err(e) => {
            return CallAttempt::Fatal(DispatchError::ExecutionFailed {
                tool: tool_label,
                reason: format!("upstream response decode failed: {e}"),
            });
        }
    };
    let parsed: ToolsCallResponse = match serde_json::from_value(json_payload) {
        Ok(p) => p,
        Err(e) => {
            return CallAttempt::Fatal(DispatchError::ExecutionFailed {
                tool: tool_label,
                reason: format!("upstream tools/call parse failed: {e}"),
            });
        }
    };

    if let Some(err) = parsed.error {
        // A JSON-RPC session error (code -32000/-32001 with an explicit
        // session-loss message) also means the session is gone — retry once.
        // We reuse the same conservative body classifier so an ordinary
        // tool-level error whose message merely mentions "session" is NOT
        // mistaken for a lost session.
        if (err.code == -32000 || err.code == -32001) && body_signals_session_loss(&err.message) {
            return CallAttempt::SessionLost {
                reason: format!("jsonrpc error code={} message={}", err.code, err.message),
            };
        }
        if err.code == -32603
            && err
                .message
                .to_ascii_lowercase()
                .contains("tool execution failed")
            && session.id.is_some()
        {
            let reason = format!("ambiguous jsonrpc error code={} message={}", err.code, err.message);
            return CallAttempt::AmbiguousResult {
                output: ToolCallOutput {
                    content: vec![ToolContent::Text {
                        text: format!(
                            "upstream JSON-RPC error code={} message={}",
                            err.code, err.message
                        ),
                    }],
                    is_error: true,
                },
                reason,
            };
        }
        // Other upstream protocol error → surface as an isError content
        // entry, not a DispatchError. Per MCP spec, JSON-RPC errors from
        // `tools/call` indicate the *protocol* failed; the semantic "tool
        // ran but errored" path uses isError:true. We collapse them here
        // because the agent-facing surface shouldn't distinguish (the audit
        // layer can see both).
        return CallAttempt::Done(ToolCallOutput {
            content: vec![ToolContent::Text {
                text: format!(
                    "upstream JSON-RPC error code={} message={}",
                    err.code, err.message
                ),
            }],
            is_error: true,
        });
    }

    let result = match parsed.result {
        Some(r) => r,
        None => {
            return CallAttempt::Fatal(DispatchError::ExecutionFailed {
                tool: tool_label,
                reason: "tools/call missing result".to_string(),
            });
        }
    };

    let output = ToolCallOutput {
        content: result.content,
        is_error: result.is_error.unwrap_or(false),
    };
    let ambiguous_execution_failure = output.is_error
        && output.content.iter().any(|content| {
            let ToolContent::Text { text } = content;
            let lower = text.to_ascii_lowercase();
            lower.contains("tool execution failed") || lower.contains("mcp error -32603")
        });
    if ambiguous_execution_failure && session.id.is_some() {
        return CallAttempt::AmbiguousResult {
            output,
            reason: "upstream returned isError tool execution failure on a stateful session"
                .to_string(),
        };
    }
    CallAttempt::Done(output)
}

async fn forward_tools_call(
    http: &reqwest::Client,
    entry: &ForwarderEntry,
    upstream_name: &str,
    arguments: &Value,
) -> Result<ToolCallOutput, DispatchError> {
    // Reuse the session established at discovery so stateful upstreams keep
    // their per-session state (e.g. the open browser page) across calls.
    let session = entry.session.lock().await.clone();

    let attempt = post_tools_call(http, entry, upstream_name, arguments, &session).await;
    let reason = match attempt {
        CallAttempt::Done(out) => return Ok(out),
        CallAttempt::Fatal(error) => {
            if session.id.is_none()
                || fetch_upstream_tools(
                    http,
                    &entry.upstream_url,
                    entry.bearer_token.as_deref(),
                    &session,
                )
                .await
                .is_ok()
            {
                return Err(error);
            }
            format!(
                "fatal tools/call failure followed by failed existing-session tools/list probe: {error}"
            )
        }
        CallAttempt::SessionLost { reason } => reason,
        CallAttempt::AmbiguousFatal { error, reason } => {
            if fetch_upstream_tools(
                http,
                &entry.upstream_url,
                entry.bearer_token.as_deref(),
                &session,
            )
            .await
            .is_ok()
            {
                return Err(error);
            }
            format!("{reason}; existing-session tools/list probe failed")
        }
        CallAttempt::AmbiguousResult { output, reason } => {
            if fetch_upstream_tools(
                http,
                &entry.upstream_url,
                entry.bearer_token.as_deref(),
                &session,
            )
            .await
            .is_ok()
            {
                return Ok(output);
            }
            format!("{reason}; existing-session tools/list probe failed")
        }
    };
    {
            // Re-establish the session once and retry. Covers upstream pod
            // restarts and session TTL expiry without failing the agent's call.
            tracing::info!(
                tool = %format!("{}.{}", entry.prefix, upstream_name),
                reason = %reason,
                "MCP upstream session lost; re-initializing and retrying once"
            );
            let new_session =
                initialize_session(http, &entry.upstream_url, entry.bearer_token.as_deref()).await;
            *entry.session.lock().await = new_session.clone();

            // The old keepalive (if any) was bound to the now-dead session;
            // cancel it and start a fresh one for the re-established session so
            // the new session is likewise protected from the upstream's
            // heartbeat reaper.
            {
                let mut guard = entry.keepalive.lock().await;
                if let Some(old) = guard.take() {
                    old.abort();
                }
                if new_session.id.is_some() {
                    *guard = Some(spawn_session_keepalive(
                        http.clone(),
                        entry.upstream_url.clone(),
                        new_session.clone(),
                        entry.bearer_token.clone(),
                    ));
                }
            }

            match post_tools_call(http, entry, upstream_name, arguments, &new_session).await {
                CallAttempt::Done(out) => Ok(out),
                CallAttempt::Fatal(e) => Err(e),
                CallAttempt::AmbiguousFatal { error, .. } => Err(error),
                CallAttempt::AmbiguousResult { output, .. } => Ok(output),
                CallAttempt::SessionLost { .. } => Err(DispatchError::ExecutionFailed {
                    tool: format!("{}.{}", entry.prefix, upstream_name),
                    reason: "upstream session could not be re-established after retry".to_string(),
                }),
            }
    }
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
        routing::{any, get, post},
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
        let env_name = "KARS_TEST_UNSET_BEARER_DO_NOT_DEFINE";
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
        let env_name = "KARS_TEST_BEARER_FIXTURE";
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

    /// A configurable STATEFUL mock upstream covering the transport variants of
    /// the popular MCP servers:
    ///
    /// - **Playwright MCP** — JSON `initialize` body, `Mcp-Session-Id`, rejects
    ///   bare `tools/list` with `400 "Server not initialized"`.
    /// - **Official TS-SDK servers / GitHub MCP** — SSE `initialize` body, a
    ///   negotiated `protocolVersion`, and a hard requirement that every
    ///   post-initialize request carries the `MCP-Protocol-Version` header
    ///   (returns `400` otherwise).
    /// - **Session expiry / pod restart** — one request fails with a session
    ///   error, forcing a re-initialize + retry.
    #[derive(Clone)]
    struct StatefulState {
        tools: Vec<ToolDefinition>,
        session_id: String,
        negotiated_version: String,
        /// Return the `initialize` result as a `text/event-stream` body.
        sse_init: bool,
        /// Reject `tools/*` requests that omit the `MCP-Protocol-Version` header.
        require_protocol_header: bool,
        init_count: StdArc<AtomicUsize>,
        call_count: StdArc<AtomicUsize>,
        /// When set, the next `tools/call` returns a `400` session error once
        /// (then clears the flag), simulating session expiry / pod restart.
        fail_next_with_session_lost: StdArc<std::sync::atomic::AtomicBool>,
        /// Return a 200 `isError:true` generic execution failure once and mark
        /// the session invalid, matching the official Everything server after
        /// its pod restarts behind an existing router.
        fail_next_with_ambiguous_execution: StdArc<std::sync::atomic::AtomicBool>,
        session_invalidated: StdArc<std::sync::atomic::AtomicBool>,
        /// When set, every successful `tools/call` result embeds the word
        /// "session" in its text (mimics Playwright `browser_evaluate` output
        /// that references `sessionStorage`). A healthy 200 like this must
        /// NEVER be misread as a lost session.
        result_mentions_session: bool,
    }

    impl StatefulState {
        fn new(session_id: &str, tools: Vec<ToolDefinition>) -> Self {
            Self {
                tools,
                session_id: session_id.to_string(),
                negotiated_version: MCP_PROTOCOL_VERSION.to_string(),
                sse_init: false,
                require_protocol_header: false,
                init_count: StdArc::new(AtomicUsize::new(0)),
                call_count: StdArc::new(AtomicUsize::new(0)),
                fail_next_with_session_lost: StdArc::new(std::sync::atomic::AtomicBool::new(false)),
                fail_next_with_ambiguous_execution: StdArc::new(
                    std::sync::atomic::AtomicBool::new(false),
                ),
                session_invalidated: StdArc::new(std::sync::atomic::AtomicBool::new(false)),
                result_mentions_session: false,
            }
        }
    }

    async fn stateful_mock_handler(
        State(state): State<StatefulState>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = body.get("id").cloned().unwrap_or(serde_json::json!(1));
        let sid = headers
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let has_proto = headers.contains_key("mcp-protocol-version");

        let session_lost = || {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": {"code": -32000, "message": "Bad Request: Server not initialized"}
                })),
            )
                .into_response()
        };

        match method {
            "initialize" => {
                state.init_count.fetch_add(1, Ordering::SeqCst);
                state.session_invalidated.store(false, Ordering::SeqCst);
                let result = serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {"protocolVersion": state.negotiated_version, "capabilities": {}}
                });
                if state.sse_init {
                    let sse = format!("event: message\ndata: {result}\n\n");
                    (
                        StatusCode::OK,
                        [
                            ("mcp-session-id", state.session_id.clone()),
                            ("content-type", "text/event-stream".to_string()),
                        ],
                        sse,
                    )
                        .into_response()
                } else {
                    (
                        StatusCode::OK,
                        [("mcp-session-id", state.session_id.clone())],
                        Json(result),
                    )
                        .into_response()
                }
            }
            "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
            "tools/list" | "tools/call"
                if state.session_invalidated.load(Ordering::SeqCst) =>
            {
                session_lost()
            }
            "tools/list" | "tools/call" if sid != state.session_id => session_lost(),
            "tools/list" | "tools/call" if state.require_protocol_header && !has_proto => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": {"code": -32600, "message": "Missing MCP-Protocol-Version header"}
                })),
            )
                .into_response(),
            "tools/list" => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "result": {"tools": state.tools}
                })),
            )
                .into_response(),
            "tools/call" => {
                state.call_count.fetch_add(1, Ordering::SeqCst);
                if state
                    .fail_next_with_session_lost
                    .swap(false, Ordering::SeqCst)
                {
                    return session_lost();
                }
                if state
                    .fail_next_with_ambiguous_execution
                    .swap(false, Ordering::SeqCst)
                {
                    state.session_invalidated.store(true, Ordering::SeqCst);
                    return (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{"type": "text", "text": "MCP error -32603: tool execution failed: do_thing"}],
                                "isError": true
                            }
                        })),
                    )
                        .into_response();
                }
                let tool = body
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let text = if state.result_mentions_session {
                    format!("called {tool}; page has sessionStorage keys: [token]")
                } else {
                    format!("called {tool}")
                };
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {"content": [{"type": "text", "text": text}], "isError": false}
                    })),
                )
                    .into_response()
            }
            _ => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": "method not found"}
                })),
            )
                .into_response(),
        }
    }

    async fn stateful_mock_upstream(state: StatefulState) -> String {
        let app = Router::new()
            .route("/", post(stateful_mock_handler))
            .route("/", any(method_block))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/")
    }

    /// Playwright-MCP shape: JSON `initialize`, session id, bare `tools/list`
    /// rejected. Discovery + call must succeed and reuse one session.
    #[tokio::test]
    async fn stateful_playwright_shape_discovers_and_calls_with_reused_session() {
        let state = StatefulState::new(
            "sess-abc-123",
            vec![tool_def("browser_navigate", "open a url")],
        );
        let init_count = state.init_count.clone();
        let url = stateful_mock_upstream(state).await;
        let registry = registry_with(vec![discovered("playwright", &url, vec!["*"])]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert_eq!(
            dispatcher.len(),
            1,
            "stateful server tools must be discovered"
        );
        assert!(
            dispatcher.skipped().is_empty(),
            "stateful server must not be skipped"
        );

        let out = dispatcher
            .invoke(
                "playwright.browser_navigate",
                &serde_json::json!({"url": "https://x"}),
            )
            .await
            .expect("invoke");
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("called browser_navigate"));
        assert!(!out.is_error);

        // Exactly one initialize: the session is reused, not re-created per call.
        assert_eq!(
            init_count.load(Ordering::SeqCst),
            1,
            "session must be reused across calls"
        );
    }

    /// Official TS-SDK / GitHub-MCP shape: SSE `initialize` body, a negotiated
    /// protocol version, and a hard requirement that the `MCP-Protocol-Version`
    /// header is replayed on every request. Discovery succeeding proves the
    /// router parses the SSE-negotiated version and sends the header.
    #[tokio::test]
    async fn stateful_sse_init_with_required_protocol_header() {
        let mut state = StatefulState::new("gh-session-xyz", vec![tool_def("search_repos", "")]);
        state.sse_init = true;
        state.require_protocol_header = true;
        state.negotiated_version = "2025-03-26".to_string();
        let url = stateful_mock_upstream(state).await;
        let registry = registry_with(vec![discovered("github-mcp", &url, vec!["*"])]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert_eq!(
            dispatcher.len(),
            1,
            "SSE-init server with required protocol header must be discovered"
        );
        assert!(dispatcher.skipped().is_empty());

        let out = dispatcher
            .invoke("github_mcp.search_repos", &serde_json::json!({"q": "kars"}))
            .await
            .expect("invoke");
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("called search_repos"));
    }

    /// Session expiry / upstream pod restart: the first `tools/call` fails with
    /// a session error; the router must re-initialize and retry transparently.
    #[tokio::test]
    async fn stateful_session_expiry_triggers_reinit_and_retry() {
        let state = StatefulState::new("sess-1", vec![tool_def("do_thing", "")]);
        let init_count = state.init_count.clone();
        let fail_flag = state.fail_next_with_session_lost.clone();
        let url = stateful_mock_upstream(state).await;
        let registry = registry_with(vec![discovered("svc", &url, vec!["*"])]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert_eq!(
            init_count.load(Ordering::SeqCst),
            1,
            "one initialize at discovery"
        );

        // Arm the upstream to drop the session on the next call.
        fail_flag.store(true, Ordering::SeqCst);
        let out = dispatcher
            .invoke("svc.do_thing", &serde_json::json!({}))
            .await
            .expect("invoke should succeed after transparent re-init + retry");
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("called do_thing"));

        // The retry path re-established the session exactly once more.
        assert_eq!(
            init_count.load(Ordering::SeqCst),
            2,
            "session must be re-initialized once on expiry"
        );
    }

    #[tokio::test]
    async fn generic_execution_failure_probes_then_recovers_dead_session() {
        let state = StatefulState::new("sess-generic", vec![tool_def("do_thing", "")]);
        let init_count = state.init_count.clone();
        let call_count = state.call_count.clone();
        let fail_flag = state.fail_next_with_ambiguous_execution.clone();
        let url = stateful_mock_upstream(state).await;
        let registry = registry_with(vec![discovered("svc", &url, vec!["*"])]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        fail_flag.store(true, Ordering::SeqCst);

        let out = dispatcher
            .invoke("svc.do_thing", &serde_json::json!({}))
            .await
            .expect("dead session should be probed, reinitialized, and retried");
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("called do_thing"));
        assert!(!out.is_error);
        assert_eq!(init_count.load(Ordering::SeqCst), 2);
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    /// Regression: a HEALTHY `tools/call` whose 200 result merely mentions the
    /// word "session" (e.g. Playwright `browser_evaluate` returning page text
    /// that references `sessionStorage`) must NOT be misread as a lost session.
    /// Re-initializing here is destructive — it drops the live browser context
    /// and the retry lands on a blank `about:blank` page. The session must be
    /// reused with ZERO re-initializations across many such calls.
    #[tokio::test]
    async fn healthy_call_mentioning_session_does_not_reinitialize() {
        let mut state =
            StatefulState::new("sess-keep", vec![tool_def("browser_evaluate", "run js")]);
        state.result_mentions_session = true;
        let init_count = state.init_count.clone();
        let url = stateful_mock_upstream(state).await;
        let registry = registry_with(vec![discovered("pw", &url, vec!["*"])]);

        let dispatcher = RouterToolDispatcher::discover(registry, Duration::from_secs(5))
            .await
            .expect("discover");
        assert_eq!(
            init_count.load(Ordering::SeqCst),
            1,
            "one init at discovery"
        );

        // Several calls in a row, each returning "session" in the result text.
        for _ in 0..5 {
            let out = dispatcher
                .invoke(
                    "pw.browser_evaluate",
                    &serde_json::json!({"function": "() => 1"}),
                )
                .await
                .expect("invoke");
            let ToolContent::Text { text } = &out.content[0];
            assert!(text.contains("sessionStorage"), "result text preserved");
            assert!(!out.is_error);
        }

        // The single discovery session must have been reused for every call —
        // no false-positive session-loss re-init.
        assert_eq!(
            init_count.load(Ordering::SeqCst),
            1,
            "healthy 'session'-mentioning results must not trigger re-initialization"
        );
    }

    #[test]
    fn session_loss_classifier_is_conservative() {
        use reqwest::StatusCode;

        // Canonical MCP SDK signal — Playwright / official servers.
        assert!(is_session_lost(
            StatusCode::BAD_REQUEST,
            "Bad Request: Server not initialized"
        ));
        // Explicit session invalidation phrasings.
        assert!(is_session_lost(StatusCode::NOT_FOUND, "session not found"));
        assert!(is_session_lost(
            StatusCode::BAD_REQUEST,
            "Mcp-Session-Id expired"
        ));
        assert!(is_session_lost(
            StatusCode::BAD_REQUEST,
            "this session is no longer valid"
        ));

        // Incidental "session" mentions in error bodies must NOT match — this
        // is exactly the false positive that corrupted live browser sessions.
        assert!(!is_session_lost(
            StatusCode::BAD_REQUEST,
            "Error: page.evaluate failed; sessionStorage is empty"
        ));
        assert!(!is_session_lost(
            StatusCode::BAD_REQUEST,
            "invalid arguments: missing 'url'"
        ));
        // Non-4xx never qualifies regardless of body.
        assert!(!is_session_lost(StatusCode::OK, "Server not initialized"));
        assert!(!is_session_lost(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session expired"
        ));
    }

    /// Mock upstream that mimics a heartbeating MCP server (Playwright shape):
    /// it issues a session on `initialize`, serves a `tools/list`, and — on the
    /// standalone `GET /` SSE stream — pushes a server→client `ping` then holds
    /// the stream open. A correct client MUST answer that ping with a `pong`
    /// POST or the real server would reap the session after its ping timeout.
    #[derive(Clone)]
    struct HeartbeatState {
        session_id: String,
        tools: Vec<ToolDefinition>,
        get_hits: StdArc<AtomicUsize>,
        pong_hits: StdArc<AtomicUsize>,
    }

    async fn heartbeat_post(
        State(state): State<HeartbeatState>,
        Json(body): Json<serde_json::Value>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = body.get("id").cloned().unwrap_or(serde_json::json!(1));
        match method {
            "initialize" => (
                StatusCode::OK,
                [("mcp-session-id", state.session_id.clone())],
                Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {"protocolVersion": MCP_PROTOCOL_VERSION, "capabilities": {}}
                })),
            )
                .into_response(),
            "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
            "tools/list" => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "result": {"tools": state.tools}
                })),
            )
                .into_response(),
            // A `pong` is a JSON-RPC *response*: it has `result` + `id` and no
            // `method`. That's the heartbeat reply we're asserting on.
            "" if body.get("result").is_some() && body.get("id").is_some() => {
                state.pong_hits.fetch_add(1, Ordering::SeqCst);
                StatusCode::ACCEPTED.into_response()
            }
            _ => StatusCode::ACCEPTED.into_response(),
        }
    }

    async fn heartbeat_get(State(state): State<HeartbeatState>) -> axum::response::Response {
        use axum::response::IntoResponse;
        use futures::stream::{self, StreamExt};
        state.get_hits.fetch_add(1, Ordering::SeqCst);
        // Push one server→client ping, then hold the stream open forever.
        let ping = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"ping\"}\n\n";
        let body = axum::body::Body::from_stream(
            stream::once(async move {
                Ok::<_, std::io::Error>(axum::body::Bytes::from_static(ping.as_bytes()))
            })
            .chain(stream::pending()),
        );
        (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            body,
        )
            .into_response()
    }

    /// A heartbeating upstream must not reap our session: the forwarder has to
    /// hold the standalone `GET` SSE stream open and answer server pings with a
    /// `pong`. This is the fix for the Playwright `about:blank` regression
    /// (session reaped after the 5s ping timeout → re-init onto a blank page).
    #[tokio::test]
    async fn keepalive_holds_get_stream_and_pongs_server_ping() {
        let state = HeartbeatState {
            session_id: "hb-session-1".to_string(),
            tools: vec![tool_def("browser_navigate", "go")],
            get_hits: StdArc::new(AtomicUsize::new(0)),
            pong_hits: StdArc::new(AtomicUsize::new(0)),
        };
        let get_hits = state.get_hits.clone();
        let pong_hits = state.pong_hits.clone();

        let app = Router::new()
            .route("/", post(heartbeat_post))
            .route("/", get(heartbeat_get))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{addr}/");

        let registry = registry_with(vec![discovered("browser", &url, vec!["*"])]);
        let dispatcher =
            RouterToolDispatcher::discover_with_client(registry, reqwest::Client::new())
                .await
                .unwrap();
        assert_eq!(dispatcher.len(), 1, "stateful server should be discovered");

        // The keepalive runs in the background; give it a moment to open the
        // GET stream, receive the ping, and POST the pong.
        for _ in 0..50 {
            if pong_hits.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert!(
            get_hits.load(Ordering::SeqCst) >= 1,
            "keepalive must open the standalone GET SSE stream"
        );
        assert!(
            pong_hits.load(Ordering::SeqCst) >= 1,
            "keepalive must answer the server's ping with a pong (else the upstream reaps the session)"
        );
    }
}
