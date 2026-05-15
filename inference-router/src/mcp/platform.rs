// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Platform MCP server — Foundry-shim runtime surface.
//!
//! This module ships the **runtime-agnostic platform MCP server** mounted at
//! `/platform/mcp` (see [`crate::routes::platform_mcp`]). It publishes a
//! stable, runtime-agnostic catalog of the 9 Foundry-shim tools that today
//! live inside the OpenClaw plugin (`cli/src/plugin.ts` —
//! `foundry_web_search`, `foundry_code_execute`, `foundry_memory`,
//! `foundry_file_search`, `foundry_image_generation`, `foundry_conversations`,
//! `foundry_evaluations`, `foundry_deployments`, `foundry_agents`).
//!
//! ## Why a separate dispatcher
//!
//! `cli/src/plugin.ts` is a Node.js OpenClaw plugin. By definition, it
//! cannot serve OpenAI Agents Python or Microsoft Agent Framework
//! runtimes — those agents speak Python, not Node, and load tools
//! through their own runtime-native mechanisms. The runtime-agnostic
//! way to expose the same affordances is **MCP**: every modern agent
//! runtime ships an MCP client out of the box. By mounting these tools
//! at `/platform/mcp` and pointing the adapters' MCP client at
//! `127.0.0.1:8443/platform/mcp`, every runtime gets the same Foundry
//! affordances with zero adapter code.
//!
//! ## How tool calls are wired
//!
//! Each `foundry.*` tool routes back into the **same router process**
//! over loopback (`http://127.0.0.1:{ROUTER_INTERNAL_PORT}/...`,
//! default port `8443`). The router itself runs as UID 1001 and is
//! therefore exempt from the egress-guard iptables rules that pin UID
//! 1000 to localhost. Self-calling reuses every existing governance,
//! policy, content-safety, and token-budget enforcement layer
//! automatically — no parallel implementation required.
//!
//! Per-tool URL mapping (full list in [`PlatformDispatcher::invoke`]'s
//! tool dispatch arms):
//!
//! | Tool                       | Method | Path                                                |
//! |----------------------------|--------|-----------------------------------------------------|
//! | `foundry.web_search`       | POST   | `/v1/responses` with `tools=[{type:"web_search"}]`  |
//! | `foundry.code_execute`     | POST   | `/v1/responses` with `code_interpreter` tool        |
//! | `foundry.file_search`      | POST   | `/v1/responses` with `file_search` tool             |
//! | `foundry.memory`           | POST   | `/memory_stores/{id}:search_memories` / `:update_memories` |
//! | `foundry.image_generation` | POST   | `/v1/images/generations`                            |
//! | `foundry.conversations`    | varies | `/openai/conversations[/{id}[/items]]`              |
//! | `foundry.evaluations`      | varies | `/openai/evals[/{id}/runs[/{run}]]`, `/evaluators`  |
//! | `foundry.deployments`      | GET    | `/v1/models`, `/connections`, `/indexes`, `/datasets` |
//! | `foundry.agents`           | GET    | `/agents/v1/assistants[/{id}]`                      |
//!
//! ## Security posture
//!
//! - **Loopback only.** The `/platform/mcp` endpoint listens on
//!   `127.0.0.1:8443`. The egress-guard init container pins UID 1000
//!   to localhost + DNS, so the agent in the same pod is the only
//!   reachable client.
//! - **No OAuth on the platform endpoint.** Customer-facing MCP servers
//!   surfaced via the `McpServer` CRD use [`crate::routes::mcp`] which
//!   wears OAuth 2.1 (production mode). The platform MCP server is
//!   single-tenant by construction and shares the trust boundary of
//!   the router process itself.
//! - **Egress is governed.** Self-calls go through the same Foundry
//!   proxy stack that already enforces InferencePolicy, Content
//!   Safety, token budgets, and audit-chain emission.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::tools::{
    AsyncToolDispatcher, DispatchError, ToolCallOutput, ToolCatalog, ToolContent, ToolDefinition,
    ToolDispatcher,
};
use crate::policy_status::{PolicyKind, PolicyStatusRegistry};

/// Default loopback port the router binds in production. Overridable
/// via `ROUTER_INTERNAL_PORT` so integration tests can target a
/// `wiremock`/axum fake on a free port.
const DEFAULT_ROUTER_PORT: u16 = 8443;

/// Default Memory Store id when callers don't pin one explicitly via
/// `FOUNDRY_MEMORY_STORE_ID`. Foundry projects ship with a
/// project-default store named `default`; production deployments that
/// pin a per-tenant store override the env var.
const DEFAULT_MEMORY_STORE: &str = "default";

/// Sync-path return for any `foundry.*` tool. The async path
/// ([`PlatformDispatcher`] as `AsyncToolDispatcher`) does the real
/// work; calling the sync trait against this dispatcher is always a
/// configuration mistake — surface it explicitly instead of pretending
/// success.
const SYNC_PATH_NOT_SUPPORTED: &str = "PlatformDispatcher does not support synchronous invocation. \
     Mount the dispatcher via AsyncToolDispatcher (the streamable HTTP \
     /platform/mcp route does this automatically). The 9 Foundry-shim \
     tools self-call upstream HTTP services and cannot run inside a \
     synchronous trait method.";

/// Build the canonical Foundry-shim tool catalog. Schemas mirror the
/// OpenClaw plugin definitions in `cli/src/plugin.ts` so existing
/// OpenClaw agents migrating to the platform MCP server do not need to
/// relearn the tool surface.
///
/// Tools are namespaced as `foundry.<name>` so future Class B (mesh /
/// spawn / handoff) and AzureClaw-platform tools sit cleanly alongside.
pub fn foundry_tool_catalog() -> ToolCatalog {
    let tools = vec![
        ToolDefinition {
            name: "foundry.web_search".into(),
            description: "Search the web in real-time via Azure AI Foundry's Bing grounding. \
                Returns answers with inline URL citations. Runs server-side — no egress \
                policy exceptions needed. Use for current events, news, recent changes, \
                verifying facts, or any query needing up-to-date information."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query or question to look up on the web."
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "foundry.code_execute".into(),
            description: "Execute Python code server-side via Azure AI Foundry's \
                code_interpreter. Has pandas, numpy, matplotlib, scipy pre-installed."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Python code to execute." }
                },
                "required": ["code"]
            }),
        },
        ToolDefinition {
            name: "foundry.file_search".into(),
            description: "Search uploaded documents and knowledge bases via Azure AI \
                Foundry's file_search. Requires vector_store_ids."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The search query." },
                    "vector_store_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Vector store IDs to search (required)."
                    }
                },
                "required": ["query", "vector_store_ids"]
            }),
        },
        ToolDefinition {
            name: "foundry.memory".into(),
            description: "Persistent agent memory via Azure AI Foundry Memory Store. Use \
                'search' to recall, 'update' to store new knowledge."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["search", "update"],
                        "description": "Operation: 'search' or 'update'."
                    },
                    "text": {
                        "type": "string",
                        "description": "For 'update': the fact to remember. For 'search': the query."
                    }
                },
                "required": ["operation", "text"]
            }),
        },
        ToolDefinition {
            name: "foundry.image_generation".into(),
            description: "Generate images from text prompts via Azure AI Foundry (gpt-image-1)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "Text prompt." },
                    "quality": {
                        "type": "string",
                        "enum": ["low", "medium", "high"],
                        "description": "Image quality (default: medium)."
                    },
                    "size": {
                        "type": "string",
                        "enum": ["1024x1024", "1024x1536", "1536x1024"],
                        "description": "Image dimensions (default: 1024x1024)."
                    }
                },
                "required": ["prompt"]
            }),
        },
        ToolDefinition {
            name: "foundry.conversations".into(),
            description: "Manage persistent server-side conversations via Azure AI Foundry.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["create", "list", "get", "respond", "add_message", "delete"]
                    },
                    "conversation_id": { "type": "string" },
                    "input": { "type": "string" },
                    "message": { "type": "string" },
                    "role": { "type": "string" },
                    "metadata": { "type": "object" },
                    "model": { "type": "string" }
                },
                "required": ["operation"]
            }),
        },
        ToolDefinition {
            name: "foundry.evaluations".into(),
            description: "Create and run model quality evaluations via Azure AI Foundry.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["list", "create", "run", "get_run", "list_evaluators"]
                    },
                    "eval_id": { "type": "string" },
                    "run_id": { "type": "string" },
                    "name": { "type": "string" },
                    "data_source_config": { "type": "object" },
                    "testing_criteria": { "type": "array", "items": { "type": "object" } },
                    "run_config": { "type": "object" }
                },
                "required": ["operation"]
            }),
        },
        ToolDefinition {
            name: "foundry.deployments".into(),
            description: "Query available Azure AI Foundry resources: models, connections, \
                search indexes, and datasets."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "resource": {
                        "type": "string",
                        "enum": ["models", "connections", "indexes", "datasets"]
                    }
                },
                "required": ["resource"]
            }),
        },
        ToolDefinition {
            name: "foundry.agents".into(),
            description: "List and query Azure AI Foundry hosted agents.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": ["list", "get"] },
                    "agent_id": { "type": "string" }
                },
                "required": ["operation"]
            }),
        },
    ];
    ToolCatalog::new(tools).expect("foundry_tool_catalog: schemas are valid by construction")
}

/// Outcome of `PlatformDispatcher::ensure_memory_store` — used
/// by the `memory()` 404→provision→retry path (Slice 3c.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnsureOutcome {
    /// Provision POST returned 2xx or 409 ("already exists" — the
    /// idempotent-success case). The caller retries the original
    /// `foundry.memory.{search,update}` call once.
    Ready,
    /// Provision POST hit upstream RBAC (401/403). The caller
    /// records `AuthMisconfigured:` on the Memory PolicyKind and
    /// surfaces the original 404 envelope back to the agent. The
    /// embedded status is informational for log lines.
    AuthFailed(u16),
    /// Transport error, 4xx other than 401/403/409, or 5xx. The
    /// caller falls through to the existing `MemoryStoreMissing:`
    /// record path so the operator still sees something on the CRD.
    Failed,
}

/// Dispatcher backing `/platform/mcp`. Publishes the 9-tool Foundry-shim
/// catalog and forwards each tool call into the same router process
/// over loopback so the existing governance/policy/safety/budget
/// pipeline applies uniformly.
#[derive(Debug, Clone)]
pub struct PlatformDispatcher {
    catalog: ToolCatalog,
    base_url: String,
    sandbox_name: String,
    memory_store_id: String,
    /// Slice 3b.3: optional handle into the ClawMemory binding loaded
    /// by `memory_binding_loader`. When `Some` and the handle resolves
    /// to a binding with a non-empty `store_name`, that value wins
    /// over the chart-fed `FOUNDRY_MEMORY_STORE_ID` env (i.e. CRD
    /// trumps env). When `None`, or when the binding is missing/empty
    /// (mount absent, parse failed), we fall back to
    /// `memory_store_id`. This is the consumer that closes the Slice
    /// 3a `MEMORY_STORE_ID` deferred work without changing any other
    /// behaviour — non-`foundry.memory` tools are unaffected.
    memory_binding: Option<crate::memory_binding_loader::LoadedMemoryBindingHandle>,
    /// Slice 3b.4 — optional handle into the per-process
    /// `PolicyStatusRegistry`. When set, `foundry.memory` records an
    /// `AuthMisconfigured:` prefixed `last_error` on the `Memory`
    /// policy kind any time the upstream Foundry Memory Store
    /// returns 401/403. The controller's ClawMemory reconciler
    /// scans for that prefix and elevates the status to
    /// `Degraded=True / reason=AuthMisconfigured` rather than the
    /// generic `AwaitingRouterEnforcement` (see Slice 3b.4 changelog).
    /// `None` keeps the legacy behaviour: 403s bubble up to the agent
    /// via the normal envelope but never reach the CRD status.
    policy_status: Option<Arc<PolicyStatusRegistry>>,
    http: reqwest::Client,
}

impl PlatformDispatcher {
    /// Default dispatcher: catalog of 9 tools, base URL derived from
    /// `ROUTER_INTERNAL_PORT` (default `8443`), sandbox identifier from
    /// `SANDBOX_NAME` (default `unknown`), memory store id from
    /// `FOUNDRY_MEMORY_STORE_ID` (default `default`).
    pub fn standard() -> Self {
        let port = std::env::var("ROUTER_INTERNAL_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(DEFAULT_ROUTER_PORT);
        let base_url = format!("http://127.0.0.1:{port}");
        let sandbox_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());
        let memory_store_id = std::env::var("FOUNDRY_MEMORY_STORE_ID")
            .unwrap_or_else(|_| DEFAULT_MEMORY_STORE.into());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            catalog: foundry_tool_catalog(),
            base_url,
            sandbox_name,
            memory_store_id,
            memory_binding: None,
            policy_status: None,
            http,
        }
    }

    /// Custom-base-URL constructor for integration tests that point the
    /// dispatcher at a fake upstream instead of the live router.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let mut d = Self::standard();
        d.base_url = base_url.into();
        d
    }

    /// Override the sandbox name used in the `x-azureclaw-sandbox`
    /// header on self-calls. Mainly for tests.
    pub fn with_sandbox_name(mut self, name: impl Into<String>) -> Self {
        self.sandbox_name = name.into();
        self
    }

    /// Override the memory store id used by `foundry.memory`.
    pub fn with_memory_store_id(mut self, id: impl Into<String>) -> Self {
        self.memory_store_id = id.into();
        self
    }

    /// Slice 3b.3: attach a `ClawMemory` binding handle. When the
    /// handle resolves to a binding with a non-empty `store_name`,
    /// `foundry.memory` calls use that store id instead of the
    /// env-fed `memory_store_id`. Lets the CRD-driven binding take
    /// precedence over the legacy chart env without breaking the
    /// env-only fallback path (sandboxes with no `spec.memoryRef`
    /// keep working exactly as before).
    pub fn with_memory_binding(
        mut self,
        handle: crate::memory_binding_loader::LoadedMemoryBindingHandle,
    ) -> Self {
        self.memory_binding = Some(handle);
        self
    }

    /// Slice 3b.4: attach the per-process `PolicyStatusRegistry`.
    /// Enables the `foundry.memory` 401/403 path to surface an
    /// `AuthMisconfigured:` prefixed `last_error` for the `Memory`
    /// policy kind so the controller can elevate the ClawMemory CRD
    /// to `Degraded=True / reason=AuthMisconfigured`. Without this
    /// handle the dispatcher still works end-to-end — 403s just stay
    /// in-band on the agent envelope and never reach the CRD.
    pub fn with_policy_status(mut self, registry: Arc<PolicyStatusRegistry>) -> Self {
        self.policy_status = Some(registry);
        self
    }

    /// Resolve the effective Memory Store id for a `foundry.memory`
    /// call. Returns the ClawMemory binding's `store_name` when one
    /// is loaded with a non-empty value; otherwise the configured
    /// env-fed `memory_store_id`. The CRD-driven binding wins.
    async fn effective_memory_store_id(&self) -> String {
        if let Some(handle) = self.memory_binding.as_ref() {
            let guard = handle.read().await;
            if let Some(binding) = guard.as_ref() {
                let name = binding.store_name.trim();
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
        self.memory_store_id.clone()
    }

    pub fn with_catalog(mut self, catalog: ToolCatalog) -> Self {
        self.catalog = catalog;
        self
    }

    /// Direct accessor for tests/inspection — the canonical 9-tool
    /// catalogue this dispatcher publishes.
    pub fn catalog_ref(&self) -> &ToolCatalog {
        &self.catalog
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    /// Dispatch one tool call to the upstream router. Returns a
    /// `ToolCallOutput` whose `is_error: false` indicates a 2xx
    /// upstream response, `is_error: true` indicates either an upstream
    /// 4xx/5xx (mapped with status text) or a transport-layer error
    /// (`reqwest` failure).
    async fn dispatch(&self, tool: &str, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        match tool {
            "foundry.web_search" => self.web_search(args).await,
            "foundry.code_execute" => self.code_execute(args).await,
            "foundry.file_search" => self.file_search(args).await,
            "foundry.memory" => self.memory(args).await,
            "foundry.image_generation" => self.image_generation(args).await,
            "foundry.conversations" => self.conversations(args).await,
            "foundry.evaluations" => self.evaluations(args).await,
            "foundry.deployments" => self.deployments(args).await,
            "foundry.agents" => self.agents(args).await,
            other => Err(DispatchError::UnknownTool(other.to_string())),
        }
    }

    fn require_str<'a>(
        &self,
        args: &'a Value,
        key: &str,
        tool: &str,
    ) -> Result<&'a str, DispatchError> {
        args.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| DispatchError::InvalidArguments {
                tool: tool.to_string(),
                reason: format!("missing required string argument `{key}`"),
            })
    }

    async fn web_search(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let query = self.require_str(args, "query", "foundry.web_search")?;
        let body = json!({
            "input": query,
            "tools": [{"type": "web_search"}]
        });
        self.post_json("foundry.web_search", "/v1/responses", &body)
            .await
    }

    async fn code_execute(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let code = self.require_str(args, "code", "foundry.code_execute")?;
        let body = json!({
            "input": code,
            "tools": [{"type": "code_interpreter"}]
        });
        self.post_json("foundry.code_execute", "/v1/responses", &body)
            .await
    }

    async fn file_search(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let query = self.require_str(args, "query", "foundry.file_search")?;
        let ids = args
            .get("vector_store_ids")
            .and_then(|v| v.as_array())
            .filter(|arr| !arr.is_empty())
            .ok_or_else(|| DispatchError::InvalidArguments {
                tool: "foundry.file_search".into(),
                reason: "missing required non-empty array `vector_store_ids`".into(),
            })?;
        let body = json!({
            "input": query,
            "tools": [{
                "type": "file_search",
                "vector_store_ids": ids
            }]
        });
        self.post_json("foundry.file_search", "/v1/responses", &body)
            .await
    }

    async fn memory(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let op = self.require_str(args, "operation", "foundry.memory")?;
        let text = self.require_str(args, "text", "foundry.memory")?;
        let (suffix, body) = match op {
            "search" => (":search_memories", json!({"query": text, "top_k": 10})),
            "update" => (
                ":update_memories",
                json!({"messages": [{"role": "user", "content": text}]}),
            ),
            other => {
                return Err(DispatchError::InvalidArguments {
                    tool: "foundry.memory".into(),
                    reason: format!("operation must be 'search' or 'update', got '{other}'"),
                });
            }
        };
        let store_id = self.effective_memory_store_id().await;
        let path = format!("/memory_stores/{store_id}{suffix}");
        let (status, output) = self
            .post_json_with_status("foundry.memory", &path, &body)
            .await;

        // Slice 3c.1 — router-side ensure-on-404. When the Foundry
        // upstream returns 404 ("store does not exist"), attempt to
        // auto-provision the store via POST `/memory_stores` (the
        // same body shape the runtime `ensureMemoryStore` uses) and
        // retry the original call exactly once.
        //
        // Retry succeeds → return the retry envelope; the original
        // 404 is forgotten and no `MemoryStoreMissing` lands on the
        // CRD status (the store now exists, so the condition would
        // be a lie). Provisioning fails with 401/403 → record
        // `AuthMisconfigured:` against the POST path and return the
        // original 404 envelope (the controller's pre-scan will
        // elevate Degraded with the right reason). Provisioning
        // fails for any other reason → fall through to the existing
        // `MemoryStoreMissing:` record path so the operator still
        // gets a signal.
        if status == Some(404) {
            match self.ensure_memory_store(&store_id).await {
                EnsureOutcome::Ready => {
                    let (retry_status, retry_output) = self
                        .post_json_with_status("foundry.memory", &path, &body)
                        .await;
                    if !matches!(retry_status, Some(404)) {
                        return Ok(retry_output);
                    }
                    // Defensive: provisioning claimed success but the
                    // store still 404s. Fall through to record
                    // MemoryStoreMissing so the operator notices.
                }
                EnsureOutcome::AuthFailed(s) => {
                    if let Some(registry) = self.policy_status.as_ref() {
                        let source_path = registry
                            .get(PolicyKind::Memory)
                            .map(|e| e.source_path)
                            .unwrap_or_else(|| "/etc/azureclaw/memory/binding.json".to_string());
                        let msg = format!(
                            "AuthMisconfigured: foundry.memory auto-provision returned HTTP {s} \
                             from /memory_stores (verify the project managed identity has \
                             the Azure AI User role on the resource group hosting the \
                             Foundry account)",
                        );
                        registry.record_error(PolicyKind::Memory, &source_path, &msg);
                    }
                    return Ok(output);
                }
                EnsureOutcome::Failed => {
                    // Provisioning failed for a non-auth reason
                    // (transport, 5xx, malformed response). Fall
                    // through to the original 404 record path so
                    // MemoryStoreMissing surfaces — the operator
                    // can then look at router logs to find the
                    // provision failure detail.
                }
            }
        }

        // Slice 3b.4 — surface upstream auth failures from the
        // Foundry Memory Store on the CRD. The router records an
        // `AuthMisconfigured:` prefixed `last_error` on
        // `PolicyKind::Memory` so the ClawMemory reconciler's pre-scan
        // (see `controller/src/claw_memory_reconciler.rs::
        // first_auth_misconfigured_message`) lifts the condition to
        // `Degraded=True / reason=AuthMisconfigured`. The wire prefix
        // is pinned by the controller side as
        // `conditions::AUTH_MISCONFIGURED_PREFIX`; we replicate the
        // string literal here rather than depend on the controller
        // crate (no cross-binary dep allowed). 401 and 403 both map
        // — both are pure-RBAC problems against the upstream.
        if matches!(status, Some(401) | Some(403))
            && let Some(registry) = self.policy_status.as_ref()
        {
            let source_path = registry
                .get(PolicyKind::Memory)
                .map(|e| e.source_path)
                .unwrap_or_else(|| "/etc/azureclaw/memory/binding.json".to_string());
            let msg = format!(
                "AuthMisconfigured: foundry.memory:{} returned HTTP {} from {} \
                 (verify the project managed identity has the Azure AI User \
                 role on the resource group hosting the Foundry account)",
                op,
                status.unwrap_or(0),
                path,
            );
            registry.record_error(PolicyKind::Memory, &source_path, &msg);
        }
        // Slice 3b.5 — surface "store does not exist on the upstream"
        // as a Degraded condition. Only reached now if Slice 3c.1's
        // ensure-on-404 path either failed for a non-auth reason or
        // claimed success but the store still 404s (defensive).
        // Otherwise the retry above already returned 2xx and we
        // never get here. The wire prefix is pinned controller-side
        // as `conditions::MEMORY_STORE_MISSING_PREFIX`; replicated
        // here as a literal because no cross-binary dep is allowed.
        if status == Some(404)
            && let Some(registry) = self.policy_status.as_ref()
        {
            let source_path = registry
                .get(PolicyKind::Memory)
                .map(|e| e.source_path)
                .unwrap_or_else(|| "/etc/azureclaw/memory/binding.json".to_string());
            let msg = format!(
                "MemoryStoreMissing: foundry.memory:{op} returned HTTP 404 from {path} \
                 (auto-provision also failed; check the router logs for the \
                 POST /memory_stores response, or pre-create the store via the \
                 Foundry portal)",
            );
            registry.record_error(PolicyKind::Memory, &source_path, &msg);
        }
        Ok(output)
    }

    /// Attempt to auto-provision a Foundry Memory Store named
    /// `store_id`. Mirrors the body shape the openclaw runtime
    /// uses in `ensureMemoryStore` (runtimes/openclaw/dist/core/
    /// agt-tools/foundry.js): kind `default`, chat_model from
    /// `OPENCLAW_MODEL` env (default `gpt-4.1`), embedding_model
    /// `text-embedding-3-small`, full options block enabled.
    ///
    /// Returns:
    /// - `Ready` on any 2xx **or** 409 ("conflict — already exists").
    ///   409 is the idempotent-success outcome when two replicas
    ///   race or when the store happens to have been created out-of-
    ///   band between the original 404 and our POST.
    /// - `AuthFailed(status)` on 401/403 — fed into the CRD as
    ///   `AuthMisconfigured` by the caller.
    /// - `Failed` for transport errors, 4xx (except 401/403/409),
    ///   and 5xx. The caller records the original 404 as
    ///   `MemoryStoreMissing` so the operator gets a signal.
    async fn ensure_memory_store(&self, store_id: &str) -> EnsureOutcome {
        let chat_model = std::env::var("OPENCLAW_MODEL").unwrap_or_else(|_| "gpt-4.1".to_string());
        let body = json!({
            "name": store_id,
            "description": "AzureClaw agent persistent memory",
            "definition": {
                "kind": "default",
                "chat_model": chat_model,
                "embedding_model": "text-embedding-3-small",
                "options": {
                    "user_profile_enabled": true,
                    "user_profile_details": "Store user preferences, decisions, and project context",
                    "chat_summary_enabled": true,
                },
            },
        });
        let (status, _envelope) = self
            .post_json_with_status("foundry.memory.ensure", "/memory_stores", &body)
            .await;
        match status {
            Some(s) if (200..300).contains(&s) || s == 409 => {
                tracing::info!(
                    target: "foundry.memory.ensure",
                    store_id = %store_id,
                    status = s,
                    "Foundry Memory Store auto-provisioned (or already existed)"
                );
                EnsureOutcome::Ready
            }
            Some(401) | Some(403) => {
                tracing::warn!(
                    target: "foundry.memory.ensure",
                    store_id = %store_id,
                    status = ?status,
                    "Foundry Memory Store auto-provision rejected by upstream RBAC"
                );
                EnsureOutcome::AuthFailed(status.unwrap_or(0))
            }
            _ => {
                tracing::warn!(
                    target: "foundry.memory.ensure",
                    store_id = %store_id,
                    status = ?status,
                    "Foundry Memory Store auto-provision failed"
                );
                EnsureOutcome::Failed
            }
        }
    }

    async fn image_generation(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let prompt = self.require_str(args, "prompt", "foundry.image_generation")?;
        let quality = args
            .get("quality")
            .and_then(|v| v.as_str())
            .unwrap_or("medium");
        let size = args
            .get("size")
            .and_then(|v| v.as_str())
            .unwrap_or("1024x1024");
        let body = json!({
            "model": "gpt-image-1",
            "prompt": prompt,
            "quality": quality,
            "size": size
        });
        self.post_json("foundry.image_generation", "/v1/images/generations", &body)
            .await
    }

    async fn conversations(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let op = self.require_str(args, "operation", "foundry.conversations")?;
        match op {
            "list" => {
                self.get("foundry.conversations", "/openai/conversations")
                    .await
            }
            "create" => {
                let mut body = json!({});
                if let Some(meta) = args.get("metadata") {
                    body["metadata"] = meta.clone();
                }
                self.post_json("foundry.conversations", "/openai/conversations", &body)
                    .await
            }
            "get" => {
                let id = self.require_str(args, "conversation_id", "foundry.conversations")?;
                let path = format!("/openai/conversations/{id}");
                self.get("foundry.conversations", &path).await
            }
            "delete" => {
                let id = self.require_str(args, "conversation_id", "foundry.conversations")?;
                let path = format!("/openai/conversations/{id}");
                self.delete("foundry.conversations", &path).await
            }
            "add_message" => {
                let id = self.require_str(args, "conversation_id", "foundry.conversations")?;
                let message = self.require_str(args, "message", "foundry.conversations")?;
                let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                let path = format!("/openai/conversations/{id}/items");
                let body = json!({
                    "items": [{"type": "message", "role": role, "content": message}]
                });
                self.post_json("foundry.conversations", &path, &body).await
            }
            "respond" => {
                let id = self.require_str(args, "conversation_id", "foundry.conversations")?;
                let input = self.require_str(args, "input", "foundry.conversations")?;
                let model = args
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("gpt-4.1");
                let body = json!({
                    "model": model,
                    "input": input,
                    "conversation": id
                });
                self.post_json("foundry.conversations", "/v1/responses", &body)
                    .await
            }
            other => Err(DispatchError::InvalidArguments {
                tool: "foundry.conversations".into(),
                reason: format!("unknown operation '{other}'"),
            }),
        }
    }

    async fn evaluations(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let op = self.require_str(args, "operation", "foundry.evaluations")?;
        match op {
            "list" => self.get("foundry.evaluations", "/openai/evals").await,
            "list_evaluators" => self.get("foundry.evaluations", "/evaluators").await,
            "create" => {
                let name = self.require_str(args, "name", "foundry.evaluations")?;
                let dsc = args.get("data_source_config").cloned().unwrap_or(json!({}));
                let tc = args
                    .get("testing_criteria")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                let body = json!({
                    "name": name,
                    "data_source_config": dsc,
                    "testing_criteria": tc
                });
                self.post_json("foundry.evaluations", "/openai/evals", &body)
                    .await
            }
            "run" => {
                let eval_id = self.require_str(args, "eval_id", "foundry.evaluations")?;
                let run_cfg = args.get("run_config").cloned().unwrap_or(json!({}));
                let path = format!("/openai/evals/{eval_id}/runs");
                self.post_json("foundry.evaluations", &path, &run_cfg).await
            }
            "get_run" => {
                let eval_id = self.require_str(args, "eval_id", "foundry.evaluations")?;
                let run_id = self.require_str(args, "run_id", "foundry.evaluations")?;
                let path = format!("/openai/evals/{eval_id}/runs/{run_id}");
                self.get("foundry.evaluations", &path).await
            }
            other => Err(DispatchError::InvalidArguments {
                tool: "foundry.evaluations".into(),
                reason: format!("unknown operation '{other}'"),
            }),
        }
    }

    async fn deployments(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let resource = self.require_str(args, "resource", "foundry.deployments")?;
        let path = match resource {
            "models" => "/v1/models",
            "connections" => "/connections",
            "indexes" => "/indexes",
            "datasets" => "/datasets",
            other => {
                return Err(DispatchError::InvalidArguments {
                    tool: "foundry.deployments".into(),
                    reason: format!(
                        "resource must be one of models|connections|indexes|datasets, got '{other}'"
                    ),
                });
            }
        };
        self.get("foundry.deployments", path).await
    }

    async fn agents(&self, args: &Value) -> Result<ToolCallOutput, DispatchError> {
        let op = self.require_str(args, "operation", "foundry.agents")?;
        match op {
            "list" => self.get("foundry.agents", "/agents/v1/assistants").await,
            "get" => {
                let id = self.require_str(args, "agent_id", "foundry.agents")?;
                let path = format!("/agents/v1/assistants/{id}");
                self.get("foundry.agents", &path).await
            }
            other => Err(DispatchError::InvalidArguments {
                tool: "foundry.agents".into(),
                reason: format!("operation must be 'list' or 'get', got '{other}'"),
            }),
        }
    }

    async fn post_json(
        &self,
        tool: &'static str,
        path: &str,
        body: &Value,
    ) -> Result<ToolCallOutput, DispatchError> {
        let (_status, output) = self.post_json_with_status(tool, path, body).await;
        Ok(output)
    }

    /// Variant of [`post_json`] that returns the upstream HTTP status
    /// alongside the envelope. Returns `None` for the status when the
    /// transport itself failed (DNS, TCP, TLS, body serialise) — those
    /// paths cannot reasonably be classified as auth issues. Used by
    /// `foundry.memory` (Slice 3b.4) to detect 401/403 and lift the
    /// CRD `Degraded` condition.
    async fn post_json_with_status(
        &self,
        tool: &'static str,
        path: &str,
        body: &Value,
    ) -> (Option<u16>, ToolCallOutput) {
        let url = self.url(path);
        let bytes = match serde_json::to_vec(body) {
            Ok(b) => b,
            Err(e) => {
                return (
                    None,
                    ToolCallOutput {
                        content: vec![ToolContent::Text {
                            text: format!("{tool} body serialise: {e}"),
                        }],
                        is_error: true,
                    },
                );
            }
        };
        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .header("x-azureclaw-sandbox", &self.sandbox_name)
            .header("x-azureclaw-platform-mcp", "1")
            .body(bytes)
            .send()
            .await;
        self.envelope_with_status(tool, resp).await
    }

    async fn get(&self, tool: &'static str, path: &str) -> Result<ToolCallOutput, DispatchError> {
        let url = self.url(path);
        let resp = self
            .http
            .get(&url)
            .header("x-azureclaw-sandbox", &self.sandbox_name)
            .header("x-azureclaw-platform-mcp", "1")
            .send()
            .await;
        Ok(self.envelope(tool, resp).await)
    }

    async fn delete(
        &self,
        tool: &'static str,
        path: &str,
    ) -> Result<ToolCallOutput, DispatchError> {
        let url = self.url(path);
        let resp = self
            .http
            .delete(&url)
            .header("x-azureclaw-sandbox", &self.sandbox_name)
            .header("x-azureclaw-platform-mcp", "1")
            .send()
            .await;
        Ok(self.envelope(tool, resp).await)
    }

    async fn envelope(
        &self,
        tool: &'static str,
        resp: Result<reqwest::Response, reqwest::Error>,
    ) -> ToolCallOutput {
        self.envelope_with_status(tool, resp).await.1
    }

    /// Variant of [`envelope`] that returns the upstream HTTP status
    /// alongside the rendered `ToolCallOutput`. `None` when the
    /// transport itself failed (DNS, TCP, TLS, timeout). Used by
    /// `post_json_with_status` so the `foundry.memory` 401/403 hook
    /// (Slice 3b.4) can classify auth failures without losing the
    /// existing envelope shape.
    async fn envelope_with_status(
        &self,
        tool: &'static str,
        resp: Result<reqwest::Response, reqwest::Error>,
    ) -> (Option<u16>, ToolCallOutput) {
        match resp {
            Ok(r) => {
                let status = r.status();
                let code = status.as_u16();
                let text = r.text().await.unwrap_or_default();
                if status.is_success() {
                    (
                        Some(code),
                        ToolCallOutput {
                            content: vec![ToolContent::Text { text }],
                            is_error: false,
                        },
                    )
                } else {
                    (
                        Some(code),
                        ToolCallOutput {
                            content: vec![ToolContent::Text {
                                text: format!("{tool} upstream returned HTTP {code}: {text}"),
                            }],
                            is_error: true,
                        },
                    )
                }
            }
            Err(e) => (
                None,
                ToolCallOutput {
                    content: vec![ToolContent::Text {
                        text: format!("{tool} transport error: {e}"),
                    }],
                    is_error: true,
                },
            ),
        }
    }
}

impl Default for PlatformDispatcher {
    fn default() -> Self {
        Self::standard()
    }
}

impl ToolDispatcher for PlatformDispatcher {
    fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    fn invoke(&self, name: &str, _arguments: &Value) -> Result<ToolCallOutput, DispatchError> {
        if self.catalog.find(name).is_none() {
            return Err(DispatchError::UnknownTool(name.to_string()));
        }
        Ok(ToolCallOutput {
            content: vec![ToolContent::Text {
                text: format!("{SYNC_PATH_NOT_SUPPORTED}\n\nTool: {name}"),
            }],
            is_error: true,
        })
    }
}

#[async_trait::async_trait]
impl AsyncToolDispatcher for PlatformDispatcher {
    fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    async fn invoke(&self, name: &str, arguments: &Value) -> Result<ToolCallOutput, DispatchError> {
        if self.catalog.find(name).is_none() {
            return Err(DispatchError::UnknownTool(name.to_string()));
        }
        self.dispatch(name, arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ----- catalog shape -----

    #[test]
    fn standard_catalog_contains_all_nine_foundry_tools() {
        let cat = foundry_tool_catalog();
        let names: Vec<&str> = cat.tools().iter().map(|t| t.name.as_str()).collect();
        let expected = [
            "foundry.web_search",
            "foundry.code_execute",
            "foundry.file_search",
            "foundry.memory",
            "foundry.image_generation",
            "foundry.conversations",
            "foundry.evaluations",
            "foundry.deployments",
            "foundry.agents",
        ];
        for name in expected {
            assert!(names.contains(&name), "expected {name} in catalog");
        }
        assert_eq!(names.len(), expected.len());
    }

    #[test]
    fn every_schema_is_an_object_with_required_array() {
        for tool in foundry_tool_catalog().tools() {
            let schema = &tool.input_schema;
            assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
            assert!(schema.get("required").is_some_and(|v| v.is_array()));
            assert!(schema.get("properties").is_some_and(|v| v.is_object()));
        }
    }

    // ----- sync path: returns a clear "use async" error, not silent success -----

    #[test]
    fn sync_invoke_returns_use_async_path_error() {
        let d = PlatformDispatcher::with_base_url("http://example.invalid");
        let out = ToolDispatcher::invoke(&d, "foundry.web_search", &json!({"query": "x"}))
            .expect("known tool dispatches");
        assert!(out.is_error, "sync path must surface is_error=true");
        let ToolContent::Text { text } = &out.content[0];
        assert!(
            text.contains("does not support synchronous"),
            "sync error must mention async path requirement, got: {text}"
        );
        assert!(text.contains("foundry.web_search"));
    }

    #[test]
    fn sync_invoke_unknown_tool_returns_unknown_tool_error() {
        let d = PlatformDispatcher::with_base_url("http://example.invalid");
        let err = ToolDispatcher::invoke(&d, "foundry.does_not_exist", &json!({})).unwrap_err();
        assert!(matches!(err, DispatchError::UnknownTool(ref n) if n == "foundry.does_not_exist"));
    }

    #[tokio::test]
    async fn async_invoke_unknown_tool_returns_unknown_tool_error() {
        let d = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        let err = AsyncToolDispatcher::invoke(&d, "foundry.does_not_exist", &json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::UnknownTool(_)));
    }

    // ----- helpers for HTTP-driven tests -----

    fn dispatcher_for(server: &MockServer) -> PlatformDispatcher {
        PlatformDispatcher::with_base_url(server.uri())
            .with_sandbox_name("test-sandbox")
            .with_memory_store_id("memstore-test")
    }

    async fn assert_ok_text(out: ToolCallOutput, expected_substr: &str) {
        assert!(!out.is_error, "expected success, got error: {out:?}");
        let ToolContent::Text { text } = &out.content[0];
        assert!(
            text.contains(expected_substr),
            "missing {expected_substr:?} in {text:?}"
        );
    }

    // ----- per-tool real dispatch tests -----

    #[tokio::test]
    async fn web_search_posts_to_v1_responses_with_web_search_tool() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("x-azureclaw-sandbox", "test-sandbox"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": "websearch"})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.web_search",
            &json!({"query": "weather in Seattle"}),
        )
        .await
        .unwrap();
        assert_ok_text(out, "websearch").await;

        // body shape verification
        let req = &server.received_requests().await.unwrap()[0];
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["input"], json!("weather in Seattle"));
        assert_eq!(body["tools"][0]["type"], json!("web_search"));
    }

    #[tokio::test]
    async fn web_search_missing_query_invalid_arguments() {
        let d = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        let err = AsyncToolDispatcher::invoke(&d, "foundry.web_search", &json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn code_execute_posts_to_v1_responses_with_code_interpreter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": "code"})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(&d, "foundry.code_execute", &json!({"code": "1+1"}))
            .await
            .unwrap();
        let req = &server.received_requests().await.unwrap()[0];
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["input"], json!("1+1"));
        assert_eq!(body["tools"][0]["type"], json!("code_interpreter"));
    }

    #[tokio::test]
    async fn file_search_requires_non_empty_vector_store_ids() {
        let d = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        let err = AsyncToolDispatcher::invoke(
            &d,
            "foundry.file_search",
            &json!({"query": "q", "vector_store_ids": []}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn file_search_posts_with_file_search_tool() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": "fs"})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.file_search",
            &json!({"query": "policies", "vector_store_ids": ["vs_1"]}),
        )
        .await
        .unwrap();
        let req = &server.received_requests().await.unwrap()[0];
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["tools"][0]["type"], json!("file_search"));
        assert_eq!(body["tools"][0]["vector_store_ids"], json!(["vs_1"]));
    }

    #[tokio::test]
    async fn memory_search_targets_memory_stores_search_memories() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/memstore-test:search_memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "what did I say"}),
        )
        .await
        .unwrap();
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn memory_update_targets_update_memories_with_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/memstore-test:update_memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"updated": 1})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "update", "text": "user prefers concise replies"}),
        )
        .await
        .unwrap();
        let req = &server.received_requests().await.unwrap()[0];
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(
            body["messages"][0]["content"],
            json!("user prefers concise replies")
        );
    }

    #[tokio::test]
    async fn memory_unknown_operation_invalid_arguments() {
        let d = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        let err = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "wipe", "text": "x"}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidArguments { .. }));
    }

    /// Slice 3b.3: when a ClawMemory binding is attached and carries
    /// a non-empty `store_name`, that name wins over the env-fed
    /// `memory_store_id`. The compiled CRD is the source of truth.
    #[tokio::test]
    async fn memory_binding_store_name_overrides_env_store_id() {
        use crate::memory_binding_loader::{LoadedMemoryBinding, empty_handle};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/from-crd:search_memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
            .mount(&server)
            .await;
        let handle = empty_handle();
        {
            let mut g = handle.write().await;
            *g = Some(LoadedMemoryBinding {
                digest: "sha256:deadbeef".into(),
                source_path: "/etc/azureclaw/memory/binding.json".into(),
                store_name: "from-crd".into(),
                scope: "agent:test".into(),
                raw: json!({"storeName": "from-crd"}),
            });
        }
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("from-env")
            .with_memory_binding(handle);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        // Mock was registered for the CRD-driven path; if the env id
        // had won we'd see zero matched requests and the mock would
        // 404.
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "foundry.memory must route to the CRD-driven store name"
        );
    }

    /// Slice 3b.3: when the binding handle resolves to a binding
    /// whose `store_name` is empty (or whitespace), we fall back to
    /// the env-fed id. Avoids tripping on a malformed compiled
    /// binding.
    #[tokio::test]
    async fn memory_empty_binding_store_name_falls_back_to_env() {
        use crate::memory_binding_loader::{LoadedMemoryBinding, empty_handle};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/from-env:update_memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"updated": 1})))
            .mount(&server)
            .await;
        let handle = empty_handle();
        {
            let mut g = handle.write().await;
            *g = Some(LoadedMemoryBinding {
                digest: "sha256:deadbeef".into(),
                source_path: "/etc/azureclaw/memory/binding.json".into(),
                store_name: "   ".into(),
                scope: "agent:test".into(),
                raw: json!({}),
            });
        }
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("from-env")
            .with_memory_binding(handle);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "update", "text": "remember"}),
        )
        .await
        .unwrap();
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "empty store_name in binding must fall back to env"
        );
    }

    /// Slice 3b.3: when no binding handle is attached at all, the
    /// dispatcher behaves exactly as before (env-fed id wins). This
    /// is the path sandboxes without `spec.memoryRef` follow.
    #[tokio::test]
    async fn memory_without_binding_handle_uses_env_store_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/from-env:search_memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
            .mount(&server)
            .await;
        let d = PlatformDispatcher::with_base_url(server.uri()).with_memory_store_id("from-env");
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn image_generation_posts_to_v1_images_generations_with_defaults() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.image_generation",
            &json!({"prompt": "a red square"}),
        )
        .await
        .unwrap();
        let req = &server.received_requests().await.unwrap()[0];
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["prompt"], json!("a red square"));
        assert_eq!(body["model"], json!("gpt-image-1"));
        assert_eq!(body["size"], json!("1024x1024"));
        assert_eq!(body["quality"], json!("medium"));
    }

    #[tokio::test]
    async fn conversations_list_uses_get() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openai/conversations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        let out =
            AsyncToolDispatcher::invoke(&d, "foundry.conversations", &json!({"operation": "list"}))
                .await
                .unwrap();
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn conversations_respond_routes_to_responses_with_conversation_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output_text": "hi"})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.conversations",
            &json!({"operation": "respond", "conversation_id": "conv_1", "input": "hello"}),
        )
        .await
        .unwrap();
        let req = &server.received_requests().await.unwrap()[0];
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["conversation"], json!("conv_1"));
        assert_eq!(body["input"], json!("hello"));
    }

    #[tokio::test]
    async fn conversations_add_message_targets_items_subpath() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/conversations/conv_xyz/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.conversations",
            &json!({"operation": "add_message", "conversation_id": "conv_xyz", "message": "ping"}),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn conversations_unknown_operation_invalid_arguments() {
        let d = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        let err = AsyncToolDispatcher::invoke(
            &d,
            "foundry.conversations",
            &json!({"operation": "telepathy"}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn evaluations_list_uses_openai_evals() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openai/evals"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(&d, "foundry.evaluations", &json!({"operation": "list"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn evaluations_run_targets_runs_subpath() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/evals/eval_42/runs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "run_1"})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.evaluations",
            &json!({"operation": "run", "eval_id": "eval_42"}),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn evaluations_get_run_targets_runs_run_id_subpath() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openai/evals/eval_42/runs/run_7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "completed"})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.evaluations",
            &json!({"operation": "get_run", "eval_id": "eval_42", "run_id": "run_7"}),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn deployments_models_uses_v1_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(&d, "foundry.deployments", &json!({"resource": "models"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn deployments_unknown_resource_invalid_arguments() {
        let d = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        let err =
            AsyncToolDispatcher::invoke(&d, "foundry.deployments", &json!({"resource": "bananas"}))
                .await
                .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn agents_list_uses_assistants_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agents/v1/assistants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(&d, "foundry.agents", &json!({"operation": "list"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn agents_get_uses_assistants_id_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agents/v1/assistants/asst_99"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "asst_99"})))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.agents",
            &json!({"operation": "get", "agent_id": "asst_99"}),
        )
        .await
        .unwrap();
    }

    // ----- error mapping -----

    #[tokio::test]
    async fn upstream_4xx_yields_is_error_with_status_in_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        let out = AsyncToolDispatcher::invoke(&d, "foundry.web_search", &json!({"query": "x"}))
            .await
            .unwrap();
        assert!(out.is_error);
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("403"), "expected status 403 in: {text}");
        assert!(text.contains("forbidden"));
    }

    #[tokio::test]
    async fn upstream_5xx_yields_is_error_with_status_in_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .mount(&server)
            .await;
        let d = dispatcher_for(&server);
        let out = AsyncToolDispatcher::invoke(&d, "foundry.web_search", &json!({"query": "x"}))
            .await
            .unwrap();
        assert!(out.is_error);
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("503"));
    }

    #[tokio::test]
    async fn transport_error_yields_is_error_with_transport_message() {
        // Unroutable address → reqwest connect error.
        let d = PlatformDispatcher::with_base_url("http://127.0.0.1:1");
        let out =
            AsyncToolDispatcher::invoke(&d, "foundry.deployments", &json!({"resource": "models"}))
                .await
                .unwrap();
        assert!(out.is_error);
        let ToolContent::Text { text } = &out.content[0];
        assert!(
            text.contains("transport error"),
            "expected transport error in: {text}"
        );
    }

    // ----- dyn-safety guard -----

    #[test]
    fn dispatcher_implements_async_tool_dispatcher_trait_object_safe() {
        let d: Box<dyn AsyncToolDispatcher> =
            Box::new(PlatformDispatcher::with_base_url("http://example.invalid"));
        assert_eq!(d.catalog().tools().len(), 9);
    }

    #[test]
    fn default_matches_standard_catalog_size() {
        // Default reads SANDBOX_NAME / ROUTER_INTERNAL_PORT env, which
        // may be unset; we only assert the catalog shape is intact.
        let d = PlatformDispatcher::default();
        assert_eq!(d.catalog_ref().tools().len(), 9);
    }

    // ----- Slice 3b.4: foundry.memory 401/403 → AuthMisconfigured -----

    /// 403 from the upstream Memory Store records an
    /// `AuthMisconfigured:` prefixed `last_error` on
    /// `PolicyKind::Memory` while preserving the prior digest. This
    /// is the producer half of the controller-side scan added in
    /// `controller/src/claw_memory_reconciler::first_auth_misconfigured_message`.
    #[tokio::test]
    async fn memory_403_records_auth_misconfigured_on_policy_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        let registry = Arc::new(PolicyStatusRegistry::new());
        // Pre-seed the registry as if the memory binding had loaded
        // successfully — `record_error` must preserve this digest so
        // the controller can still tell "loaded once, now broken" from
        // "never loaded".
        registry.record_success(
            PolicyKind::Memory,
            "/etc/azureclaw/memory/binding.json",
            b"binding",
        );
        let prior_digest = registry
            .get(PolicyKind::Memory)
            .and_then(|e| e.digest)
            .unwrap();

        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        // Agent-facing envelope still surfaces the upstream error.
        assert!(out.is_error);

        let entry = registry.get(PolicyKind::Memory).expect("entry present");
        let err = entry.last_error.expect("last_error recorded");
        assert!(
            err.starts_with("AuthMisconfigured:"),
            "expected AuthMisconfigured: prefix, got {err}"
        );
        assert!(err.contains("HTTP 403"), "should mention status: {err}");
        assert!(err.contains("search"), "should mention operation: {err}");
        // Digest preserved through the error record.
        assert_eq!(entry.digest, Some(prior_digest));
    }

    /// 401 maps the same way — it's a credentials problem at the
    /// upstream, equally a misconfig signal.
    #[tokio::test]
    async fn memory_401_records_auth_misconfigured_on_policy_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:update_memories"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;
        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "update", "text": "x"}),
        )
        .await
        .unwrap();
        let entry = registry.get(PolicyKind::Memory).expect("entry present");
        let err = entry.last_error.unwrap();
        assert!(err.starts_with("AuthMisconfigured:"));
        assert!(err.contains("HTTP 401"));
        assert!(err.contains("update"));
    }

    /// 500 from the upstream is **not** an auth issue and must not
    /// trip the AuthMisconfigured surface — the controller would
    /// over-promote a transient outage to a hard Degraded.
    #[tokio::test]
    async fn memory_500_does_not_record_auth_misconfigured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        assert!(
            registry.get(PolicyKind::Memory).is_none(),
            "500 must not record any Memory entry"
        );
    }

    /// 200 success leaves the registry untouched — only the
    /// `memory_binding_loader` startup path is allowed to call
    /// `record_success` for the Memory kind.
    #[tokio::test]
    async fn memory_200_does_not_touch_policy_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
            .mount(&server)
            .await;
        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        assert!(registry.get(PolicyKind::Memory).is_none());
    }

    /// Without a `PolicyStatusRegistry` handle, the dispatcher still
    /// surfaces 403 to the agent envelope but cannot reach the CRD.
    /// This is the legacy path — must not panic or misbehave.
    #[tokio::test]
    async fn memory_403_without_policy_status_handle_is_silent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        let d = PlatformDispatcher::with_base_url(server.uri()).with_memory_store_id("store-xyz");
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        assert!(out.is_error);
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("HTTP 403"));
    }

    // ----- Slice 3b.5: foundry.memory 404 → MemoryStoreMissing -----

    /// 404 from the upstream Memory Store records a
    /// `MemoryStoreMissing:` prefixed `last_error` on
    /// `PolicyKind::Memory` while preserving the prior digest. The
    /// controller's `first_memory_store_missing_message` pre-scan
    /// (claw_memory_reconciler) elevates this to
    /// `Degraded=True / reason=MemoryStoreMissing`. 404 has lower
    /// precedence than 401/403 (RBAC dominates), so callers must
    /// check AuthMisconfigured first — which they do.
    #[tokio::test]
    async fn memory_404_records_memory_store_missing_on_policy_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let registry = Arc::new(PolicyStatusRegistry::new());
        registry.record_success(
            PolicyKind::Memory,
            "/etc/azureclaw/memory/binding.json",
            b"binding",
        );
        let prior_digest = registry
            .get(PolicyKind::Memory)
            .and_then(|e| e.digest)
            .unwrap();

        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        assert!(out.is_error);

        let entry = registry.get(PolicyKind::Memory).expect("entry present");
        let err = entry.last_error.expect("last_error recorded");
        assert!(
            err.starts_with("MemoryStoreMissing:"),
            "expected MemoryStoreMissing: prefix, got {err}"
        );
        assert!(err.contains("HTTP 404"), "should mention status: {err}");
        assert!(err.contains("search"), "should mention operation: {err}");
        // Digest preserved through the error record (matches the
        // 3b.4 design — record_error never wipes the digest).
        assert_eq!(entry.digest, Some(prior_digest));
    }

    /// 404 must NOT trip the AuthMisconfigured surface — the
    /// controller would attribute a missing store to RBAC and
    /// mislead the operator. The `MemoryStoreMissing:` prefix is
    /// the only signal recorded.
    #[tokio::test]
    async fn memory_404_does_not_record_auth_misconfigured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:update_memories"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "update", "text": "x"}),
        )
        .await
        .unwrap();
        let err = registry
            .get(PolicyKind::Memory)
            .and_then(|e| e.last_error)
            .expect("404 must record an error");
        assert!(
            !err.starts_with("AuthMisconfigured:"),
            "404 must not be classified as AuthMisconfigured: {err}"
        );
        assert!(err.starts_with("MemoryStoreMissing:"));
    }

    /// Without a `PolicyStatusRegistry` handle, the dispatcher still
    /// surfaces 404 to the agent envelope but cannot reach the CRD.
    /// Legacy path — must not panic.
    #[tokio::test]
    async fn memory_404_without_policy_status_handle_is_silent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let d = PlatformDispatcher::with_base_url(server.uri()).with_memory_store_id("store-xyz");
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        assert!(out.is_error);
        let ToolContent::Text { text } = &out.content[0];
        assert!(text.contains("HTTP 404"));
    }

    // ----- Slice 3c.1: foundry.memory 404 → auto-provision → retry -----

    /// Happy path: upstream 404 on the search call → router POSTs
    /// `/memory_stores` to provision → upstream returns 201 → retry
    /// of the original call returns 200. The agent sees success and
    /// **no** `MemoryStoreMissing:` lands on the Memory PolicyKind.
    /// This is the slice's whole point: routine 404s self-heal.
    #[tokio::test]
    async fn memory_404_then_provision_then_retry_success() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let server = MockServer::start().await;
        // The original call mock cycles through responses by using
        // an external counter — first call 404, subsequent calls 200.
        // wiremock's `respond_with` is stateless, so we use a custom
        // responder backed by a shared AtomicUsize.
        static SEARCH_CALLS: AtomicUsize = AtomicUsize::new(0);
        SEARCH_CALLS.store(0, Ordering::SeqCst);

        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(|_: &wiremock::Request| {
                let n = SEARCH_CALLS.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(404).set_body_string("not found")
                } else {
                    ResponseTemplate::new(200).set_body_string(r#"{"results":[]}"#)
                }
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/memory_stores"))
            .respond_with(ResponseTemplate::new(201).set_body_string(r#"{"name":"store-xyz"}"#))
            .mount(&server)
            .await;

        let registry = Arc::new(PolicyStatusRegistry::new());
        registry.record_success(
            PolicyKind::Memory,
            "/etc/azureclaw/memory/binding.json",
            b"binding",
        );
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        // Retry succeeded → agent sees success.
        assert!(!out.is_error, "retry should have succeeded: {out:?}");
        // No MemoryStoreMissing recorded — the store now exists.
        let entry = registry.get(PolicyKind::Memory).expect("entry present");
        assert!(
            entry.last_error.is_none(),
            "auto-heal must not record an error: {:?}",
            entry.last_error
        );
        // Original call invoked twice (initial + retry).
        assert_eq!(SEARCH_CALLS.load(Ordering::SeqCst), 2);
    }

    /// 409 ("already exists") from the provision POST is the
    /// idempotent-success case — two replicas racing or out-of-band
    /// creation. The router still retries the original call.
    #[tokio::test]
    async fn memory_404_then_provision_409_treated_as_ready_and_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let server = MockServer::start().await;
        static SEARCH_CALLS: AtomicUsize = AtomicUsize::new(0);
        SEARCH_CALLS.store(0, Ordering::SeqCst);

        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:update_memories"))
            .respond_with(|_: &wiremock::Request| {
                let n = SEARCH_CALLS.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(404).set_body_string("not found")
                } else {
                    ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#)
                }
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/memory_stores"))
            .respond_with(ResponseTemplate::new(409).set_body_string("conflict"))
            .mount(&server)
            .await;

        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "update", "text": "x"}),
        )
        .await
        .unwrap();
        assert!(!out.is_error);
        // Success path doesn't touch the registry — no entry exists,
        // and that's the correct "no error" surface for the controller.
        let no_err = registry
            .get(PolicyKind::Memory)
            .and_then(|e| e.last_error)
            .is_none();
        assert!(no_err, "successful retry must not record an error");
    }

    /// Provision POST hit 403 (RBAC against the upstream control
    /// plane). The router records `AuthMisconfigured:` against the
    /// `/memory_stores` POST path so the controller's pre-scan
    /// elevates Degraded with the correct reason. The original 404
    /// envelope is what surfaces back to the agent.
    #[tokio::test]
    async fn memory_404_then_provision_403_records_auth_misconfigured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/memory_stores"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        // Original 404 envelope flows to the agent.
        assert!(out.is_error);

        let entry = registry.get(PolicyKind::Memory).expect("entry present");
        let err = entry.last_error.expect("auth misconfigured recorded");
        assert!(
            err.starts_with("AuthMisconfigured:"),
            "expected AuthMisconfigured: prefix, got {err}"
        );
        assert!(err.contains("HTTP 403"), "should mention status: {err}");
        assert!(
            err.contains("/memory_stores"),
            "should mention the provision path: {err}"
        );
        // MemoryStoreMissing must NOT also be recorded (RBAC
        // dominates the 404 signal — see precedence comment in
        // claw_memory_reconciler).
        assert!(
            !err.starts_with("MemoryStoreMissing:"),
            "RBAC must dominate 404: {err}"
        );
    }

    /// Provision POST returned 500 — transient upstream failure or
    /// genuine plane outage. We fall through to the existing
    /// MemoryStoreMissing record path so the operator still sees
    /// something on the CRD (and the router logs carry the 500
    /// details for triage).
    #[tokio::test]
    async fn memory_404_then_provision_500_falls_through_to_memory_store_missing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/memory_stores"))
            .respond_with(ResponseTemplate::new(500).set_body_string("upstream boom"))
            .mount(&server)
            .await;

        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        let entry = registry.get(PolicyKind::Memory).expect("entry present");
        let err = entry.last_error.expect("error recorded");
        assert!(
            err.starts_with("MemoryStoreMissing:"),
            "non-auth provision failure must surface MemoryStoreMissing: {err}"
        );
    }

    /// Initial 200 from the upstream → no provision attempted, no
    /// error recorded. This guards against a regression where the
    /// ensure path fires unconditionally and races against healthy
    /// traffic.
    #[tokio::test]
    async fn memory_200_skips_provision_path() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let server = MockServer::start().await;
        static ENSURE_CALLS: AtomicUsize = AtomicUsize::new(0);
        ENSURE_CALLS.store(0, Ordering::SeqCst);

        Mock::given(method("POST"))
            .and(path("/memory_stores/store-xyz:search_memories"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"results":[]}"#))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/memory_stores"))
            .respond_with(|_: &wiremock::Request| {
                ENSURE_CALLS.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(201)
            })
            .mount(&server)
            .await;

        let registry = Arc::new(PolicyStatusRegistry::new());
        let d = PlatformDispatcher::with_base_url(server.uri())
            .with_memory_store_id("store-xyz")
            .with_policy_status(registry.clone());
        let out = AsyncToolDispatcher::invoke(
            &d,
            "foundry.memory",
            &json!({"operation": "search", "text": "q"}),
        )
        .await
        .unwrap();
        assert!(!out.is_error);
        assert_eq!(
            ENSURE_CALLS.load(Ordering::SeqCst),
            0,
            "ensure must not fire on the happy path"
        );
        assert!(
            registry
                .get(PolicyKind::Memory)
                .and_then(|e| e.last_error)
                .is_none()
        );
    }
}
