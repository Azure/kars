//! Platform MCP server — Foundry-shim discovery surface.
//!
//! This module ships the **runtime-agnostic platform MCP server** mounted at
//! `/platform/mcp` (see [`crate::routes::platform_mcp`]). Its sole purpose
//! at this stage is to publish a stable, runtime-agnostic catalog of the
//! 9 Foundry-shim tools that today live inside the OpenClaw plugin
//! (`cli/src/plugin.ts` — `foundry_web_search`, `foundry_code_execute`,
//! `foundry_memory`, `foundry_file_search`, `foundry_image_generation`,
//! `foundry_conversations`, `foundry_evaluations`, `foundry_deployments`,
//! `foundry_agents`).
//!
//! ## Why a separate dispatcher
//!
//! `cli/src/plugin.ts` is a Node.js OpenClaw plugin. By definition, it
//! cannot serve OpenAI Agents Python (S10.A3) or Microsoft Agent Framework
//! (S10.A4) runtimes — those agents speak Python, not Node, and load
//! tools through their own runtime-native mechanisms. The runtime-agnostic
//! way to expose the same affordances is **MCP**: every modern agent
//! runtime ships an MCP client out of the box. By mounting these tools at
//! `/platform/mcp` and pointing the adapters' MCP client at
//! `127.0.0.1:8443/platform/mcp`, every runtime gets the same Foundry
//! affordances with zero adapter code.
//!
//! This is **Class A** of the OpenClaw-plugin three-class survey
//! (see `docs/internal/agt-upstream-asks.md` §4 and the S10-runtime-
//! agnostic-rule note in `plan.md` S10): pure HTTP shims with no E2E
//! concern, no AGT crypto, no per-runtime crypto state. Class B (mesh /
//! spawn / handoff) explicitly **does not** belong here — those stay
//! per-runtime, registered natively against the appropriate-language
//! AgentMesh SDK.
//!
//! ## Status: discovery surface only
//!
//! This slice (S10.B) ships **catalog + dispatch seam**. Every
//! `tools/call` returns `is_error: true` with a deferred-wiring message;
//! `tools/list` returns the full 9-tool catalog with the exact same
//! input schemas the OpenClaw plugin publishes today. This shape is
//! deliberately analogous to S10.A2 (controller dispatch seam without
//! BYO/MAF/OpenAI runtime wiring) — runtime adapters can validate
//! discovery against this surface immediately while per-tool wiring lands
//! in follow-up slices `S10.B.{1..9}`.
//!
//! ## Why discovery-only is the right shape for this slice
//!
//! - Each tool's actual upstream call (`POST /openai/responses`,
//!   `GET /memory_stores/...`, etc.) requires async HTTP. The current
//!   [`ToolDispatcher::invoke`](crate::mcp::tools::ToolDispatcher::invoke)
//!   trait method is **synchronous** by design (the trait predates this
//!   slice; every test and dispatcher in tree relies on it). Migrating
//!   to async is a separate (worthwhile) refactor that would gate this
//!   slice on a 30+ test rewrite. Per the slice philosophy, ship the
//!   architectural seam first; wire up tools after.
//! - The runtime-adapter validation surface (S10.A3 / S10.A4) needs
//!   exactly the catalog — adapter authors can confirm their MCP client
//!   negotiates correctly, sees all 9 tools, and would route calls to
//!   the right server. They cannot make tool calls succeed yet, but
//!   that's transparent: any call returns a structured deferred-error
//!   that the adapter can surface to the user.
//!
//! ## Security posture
//!
//! - **Loopback only.** The `/platform/mcp` endpoint listens on
//!   `127.0.0.1:8443` like every other router route. The egress-guard
//!   init container restricts the agent (UID 1000) to `127.0.0.1` plus
//!   DNS, so this endpoint is reachable by exactly one process: the
//!   agent in the same pod.
//! - **No OAuth on the platform endpoint.** Customer-facing MCP servers
//!   surfaced via the `McpServer` CRD use [`crate::routes::mcp`] which
//!   wears OAuth 2.1 (production mode) for tenant isolation. The
//!   platform MCP server has a different threat model — it is
//!   single-tenant by construction (one agent per pod, loopback only)
//!   and shares the trust boundary of the router process itself.
//! - **No per-tool egress.** This slice does not yet make any upstream
//!   HTTP call. Follow-up slices that wire individual tools will route
//!   through the existing Foundry-proxy layer that already enforces
//!   InferencePolicy, Content Safety, token budgets, and audit chain
//!   emission.

use serde_json::{Value, json};

use super::tools::{
    DispatchError, ToolCallOutput, ToolCatalog, ToolContent, ToolDefinition, ToolDispatcher,
};

/// Shared "tool wiring deferred" message body. Returned as the text
/// content of every tool call until follow-up slices wire individual
/// tools end-to-end. A structured marker lets adapter test assertions
/// distinguish "the dispatcher fired correctly but the tool wiring is
/// pending" from "tool was unknown" or "arguments were invalid".
const DEFERRED_WIRING_MESSAGE: &str = concat!(
    "Platform MCP server discovery surface (slice S10.B). ",
    "Tool catalog and dispatch are shipped; per-tool upstream wiring to ",
    "Azure AI Foundry lands in follow-up slices S10.B.1 through S10.B.9. ",
    "See docs/internal/phase-2-story.md S10 and ",
    "docs/internal/agt-upstream-asks.md §4 for the runtime-agnostic ",
    "platform MCP design."
);

/// Build the canonical Foundry-shim tool catalog. Schemas mirror the
/// OpenClaw plugin definitions in `cli/src/plugin.ts` (lines 662–735,
/// 6104–6347) so existing OpenClaw agents migrating to the platform
/// MCP server do not need to relearn the tool surface.
///
/// Tools are namespaced as `foundry.<name>` so that follow-up slices
/// adding mesh / spawn / handoff platform tools (Class B), or
/// AzureClaw-platform tools (`platform.attest_self`, etc.) sit cleanly
/// alongside.
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
                code_interpreter. Has pandas, numpy, matplotlib, scipy pre-installed. \
                Use for data analysis, charts, complex math, and file processing."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "Python code to execute."
                    }
                },
                "required": ["code"]
            }),
        },
        ToolDefinition {
            name: "foundry.file_search".into(),
            description: "Search uploaded documents and knowledge bases via Azure AI \
                Foundry's file_search. Requires vector_store_ids — use foundry.memory \
                instead for general memory/knowledge storage."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query."
                    },
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
            description: "Persistent agent memory via Azure AI Foundry Memory Store. Store \
                facts, preferences, and context that persists across sessions. Use 'search' \
                to recall, 'update' to store new knowledge."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["search", "update"],
                        "description": "Operation: 'search' to find relevant memories, 'update' to store new facts."
                    },
                    "text": {
                        "type": "string",
                        "description": "For 'update': the fact to remember. For 'search': the query to find relevant memories."
                    }
                },
                "required": ["operation", "text"]
            }),
        },
        ToolDefinition {
            name: "foundry.image_generation".into(),
            description: "Generate images from text prompts via Azure AI Foundry \
                (gpt-image-1). Returns the saved image as a tool result."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Text description of the image to generate."
                    },
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
            description: "Manage persistent server-side conversations via Azure AI Foundry. \
                Use cases: maintain long-running multi-turn dialogues across sessions, \
                build research threads that survive restarts, keep separate conversation \
                contexts for different tasks/topics."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["create", "list", "get", "respond", "add_message", "delete"],
                        "description": "Operation to perform. 'get' retrieves full message history."
                    },
                    "conversation_id": {
                        "type": "string",
                        "description": "Conversation ID (for get/respond/add_message/delete)."
                    },
                    "input": {
                        "type": "string",
                        "description": "User input (for 'respond' — generates AI response in conversation context)."
                    },
                    "message": {
                        "type": "string",
                        "description": "Message text to add (for 'add_message')."
                    },
                    "role": {
                        "type": "string",
                        "description": "Message role: 'user' or 'assistant' (for 'add_message', default: 'user')."
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Metadata for new conversation (for 'create')."
                    },
                    "model": {
                        "type": "string",
                        "description": "Model to use for responses (default: gpt-4.1)."
                    }
                },
                "required": ["operation"]
            }),
        },
        ToolDefinition {
            name: "foundry.evaluations".into(),
            description: "Create and run model quality evaluations via Azure AI Foundry \
                Evals API. Use cases: benchmark prompt quality before/after changes, \
                validate output against golden answers, run regression tests on model \
                responses, compare different models."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["list", "create", "run", "get_run", "list_evaluators"],
                        "description": "Operation: 'list' evals, 'create' one, 'run' it, 'get_run' status/results, or 'list_evaluators'."
                    },
                    "eval_id": { "type": "string", "description": "Eval ID (for 'run')." },
                    "run_id": { "type": "string", "description": "Run ID (for 'get_run')." },
                    "name": { "type": "string", "description": "Eval name (for 'create')." },
                    "data_source_config": { "type": "object", "description": "Data source config (for 'create')." },
                    "testing_criteria": {
                        "type": "array",
                        "items": { "type": "object" },
                        "description": "Testing criteria array (for 'create')."
                    },
                    "run_config": { "type": "object", "description": "Run configuration (for 'run')." }
                },
                "required": ["operation"]
            }),
        },
        ToolDefinition {
            name: "foundry.deployments".into(),
            description: "Query available Azure AI Foundry resources: models, connections, \
                search indexes, and datasets. Use 'models' to see all available AI models, \
                'connections' for data connections, 'indexes' for search indexes."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "resource": {
                        "type": "string",
                        "enum": ["models", "connections", "indexes", "datasets"],
                        "description": "Resource type to query."
                    }
                },
                "required": ["resource"]
            }),
        },
        ToolDefinition {
            name: "foundry.agents".into(),
            description: "List and query Azure AI Foundry hosted agents. Discover \
                available agents, their capabilities, and configurations. These are \
                server-side Foundry agents (different from AzureClaw sub-agent sandboxes)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["list", "get"],
                        "description": "Operation: 'list' all agents or 'get' a specific agent."
                    },
                    "agent_id": { "type": "string", "description": "Agent ID (for 'get')." }
                },
                "required": ["operation"]
            }),
        },
    ];
    ToolCatalog::new(tools).expect("foundry_tool_catalog: schemas are valid by construction")
}

/// Dispatcher backing `/platform/mcp`. Publishes the 9-tool Foundry-shim
/// catalog and returns a structured deferred-wiring response from
/// `tools/call` for any catalogued tool. Unknown tool names return
/// [`DispatchError::UnknownTool`] (mapped to JSON-RPC by the caller).
#[derive(Debug, Clone)]
pub struct PlatformDispatcher {
    catalog: ToolCatalog,
}

impl PlatformDispatcher {
    /// Default dispatcher with the canonical 9-tool Foundry catalog.
    pub fn standard() -> Self {
        Self {
            catalog: foundry_tool_catalog(),
        }
    }

    pub fn with_catalog(catalog: ToolCatalog) -> Self {
        Self { catalog }
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
                text: format!(
                    "{DEFERRED_WIRING_MESSAGE}\n\n\
                     Tool: {name}\n\
                     Status: catalogued, wiring deferred"
                ),
            }],
            is_error: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(
                names.contains(&name),
                "expected {name} in catalog, got {names:?}"
            );
        }
        assert_eq!(
            names.len(),
            expected.len(),
            "no extra tools beyond the 9 Foundry shims"
        );
    }

    #[test]
    fn every_schema_is_an_object_with_required_array() {
        for tool in foundry_tool_catalog().tools() {
            let schema = &tool.input_schema;
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "tool {} input_schema must be of type=object",
                tool.name
            );
            assert!(
                schema.get("required").is_some_and(|v| v.is_array()),
                "tool {} input_schema must declare a required array",
                tool.name
            );
            assert!(
                schema.get("properties").is_some_and(|v| v.is_object()),
                "tool {} input_schema must declare a properties object",
                tool.name
            );
        }
    }

    #[test]
    fn invoke_known_tool_returns_deferred_wiring_error() {
        let d = PlatformDispatcher::standard();
        let out = d
            .invoke("foundry.web_search", &json!({"query": "anything"}))
            .expect("known tool dispatches successfully");
        assert!(out.is_error, "deferred-wiring response is_error=true");
        let first = out.content.first().expect("at least one content item");
        let ToolContent::Text { text } = first;
        assert!(
            text.contains("S10.B"),
            "deferred-wiring text mentions slice id, got {text:?}"
        );
        assert!(
            text.contains("foundry.web_search"),
            "deferred-wiring text echoes tool name, got {text:?}"
        );
    }

    #[test]
    fn invoke_unknown_tool_returns_unknown_tool_error() {
        let d = PlatformDispatcher::standard();
        let err = d
            .invoke("foundry.nonexistent", &json!({}))
            .expect_err("unknown tool returns error");
        match err {
            DispatchError::UnknownTool(name) => assert_eq!(name, "foundry.nonexistent"),
            other => panic!("expected UnknownTool, got {other:?}"),
        }
    }

    #[test]
    fn invoke_does_not_touch_arguments() {
        // Defensive: deferred-wiring response must be argument-agnostic
        // until upstream wiring lands. If a future patch starts
        // validating arguments here, it should bump the catalog version
        // first.
        let d = PlatformDispatcher::standard();
        let out_with_args = d
            .invoke(
                "foundry.memory",
                &json!({"operation": "search", "text": "hi"}),
            )
            .unwrap();
        let out_without_args = d.invoke("foundry.memory", &json!({})).unwrap();
        assert!(out_with_args.is_error);
        assert!(out_without_args.is_error);
    }

    #[test]
    fn dispatcher_implements_tool_dispatcher_trait_object_safe() {
        // Guards against a future refactor accidentally adding a
        // generic method that breaks dyn ToolDispatcher.
        let d: Box<dyn ToolDispatcher> = Box::new(PlatformDispatcher::standard());
        assert_eq!(d.catalog().tools().len(), 9);
    }

    #[test]
    fn default_matches_standard() {
        let d1 = PlatformDispatcher::default();
        let d2 = PlatformDispatcher::standard();
        assert_eq!(d1.catalog().tools().len(), d2.catalog().tools().len());
    }
}
