// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Foundry AGT tool registrations — extracted from plugin.ts in S15.f.8.
//
// Nine tools that proxy through the inference router to Azure AI Foundry
// services. All are read-only / RPC-style: no closure over plugin.ts state,
// no mesh interaction. Safe to extract as a self-contained module.
//
// Tools registered:
//   foundry_code_execute     foundry_image_generation
//   foundry_web_search       foundry_file_search
//   foundry_memory           foundry_conversations
//   foundry_evaluations      foundry_deployments
//   foundry_agents

import { routerCall } from "../router-client.js";
import { safeJson } from "../safe-json.js";
import type { FoundryProjectInfo } from "../foundry-discovery.js";

// _routerCall is the historical alias used inside these blocks; keep it for
// byte-identical bodies.
const _routerCall = routerCall;

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnyApi = any;

export interface FoundryToolsDeps {
  // Logger surface (subset of OpenClaw logger used in tool bodies).
  log: { info: (m: string) => void; warn: (m: string) => void };
  // Read-only access to the resolved plugin config (only `config.model` is
  // referenced inside foundry tools).
  config: { model: string };
  // Late-bound accessor for the Foundry project discovered asynchronously by
  // `initFoundry`; tool bodies reference it through `getFoundryProject()` since
  // discovery may complete after `register()` returns.
  getFoundryProject: () => FoundryProjectInfo | null;
}

export function registerFoundryTools(api: AnyApi, deps: FoundryToolsDeps): void {
  const { log, config } = deps;
  const getFoundryProject = deps.getFoundryProject;
  api.registerTool({
    name: "foundry_code_execute",
    label: "Foundry Code Interpreter",
    description:
      "Execute Python code server-side via Azure AI Foundry's code_interpreter. " +
      "Has pandas, numpy, matplotlib, scipy pre-installed. Use for data analysis, " +
      "charts, complex math, and file processing. Runs in a managed Foundry sandbox " +
      "(not the local sandbox). No egress policy needed.",
    parameters: {
      type: "object",
      properties: {
        input: {
          type: "string",
          description: "Natural language instruction or Python code to execute. " +
            "The model will write and run Python code to fulfill the request.",
        },
        model: {
          type: "string",
          description: "Model to use (default: gpt-4.1). Must support code_interpreter.",
        },
      },
      required: ["input"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const result = await routerCall("POST", "/openai/responses?api-version=2025-11-15-preview", {
          model: (params.model as string) || "gpt-4.1",
          input: params.input,
          tools: [{ type: "code_interpreter", container: { type: "auto" } }],
          store: false,
        });
        // Extract text output from Responses API format
        const output = result.output || result;
        const textParts: string[] = [];
        if (Array.isArray(output)) {
          for (const item of output) {
            if (item.type === "message" && item.content) {
              for (const c of item.content) {
                if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
              }
            } else if (item.type === "code_interpreter_call") {
              textParts.push(`\`\`\`python\n${item.code}\n\`\`\`\nOutput: ${item.output || "(no output)"}`);
            }
          }
        }
        return {
          content: [{
            type: "text",
            text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2),
          }],
        };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry code execution failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Image Generation: create images from text ───────────────
  api.registerTool({
    name: "foundry_image_generation",
    label: "Foundry Image Generation",
    description:
      "Generate images from text prompts via Azure AI Foundry's image_generation tool. " +
      "Supports any deployed image model (gpt-image-1, FLUX.2-pro, etc.). Returns base64-encoded image data. " +
      "Use when the user asks to create, draw, or generate an image, diagram, or visual.",
    parameters: {
      type: "object",
      properties: {
        prompt: {
          type: "string",
          description: "Text description of the image to generate.",
        },
        quality: {
          type: "string",
          enum: ["low", "medium", "high"],
          description: "Image quality (default: 'medium'). Higher = slower + more detailed.",
        },
        size: {
          type: "string",
          enum: ["1024x1024", "1024x1536", "1536x1024"],
          description: "Image dimensions (default: '1024x1024').",
        },
        image_model: {
          type: "string",
          description: "Image generation model deployment name (default: 'gpt-image-1').",
        },
      },
      required: ["prompt"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const imgModel = (params.image_model as string) || "gpt-image-1";
        const quality = (params.quality as string) || "medium";
        const size = (params.size as string) || "1024x1024";
        const n = 1;

        // Use the standard OpenAI Images API (POST /images/generations)
        // The router proxies this to Azure OpenAI: /openai/deployments/{model}/images/generations
        const result = await _routerCall("POST",
          `/openai/deployments/${encodeURIComponent(imgModel)}/images/generations?api-version=2025-04-01-preview`,
          { prompt: params.prompt, n, size, quality },
          90000,
        );

        // Response format: { data: [{ b64_json: "...", revised_prompt: "..." }] }
        const images = result?.data || [];
        const parts: string[] = [];
        const fs = await import("node:fs");
        const nodePath = await import("node:path");
        const os = await import("node:os");
        const imgDir = nodePath.join(os.tmpdir(), "azureclaw-images");
        fs.mkdirSync(imgDir, { recursive: true });

        for (const img of images) {
          if (img.b64_json) {
            // Save image to temp file so user can view it
            const ts = Date.now();
            const imgFile = nodePath.join(imgDir, `image-${ts}.png`);
            fs.writeFileSync(imgFile, Buffer.from(img.b64_json, "base64"));
            parts.push(`📁 Image saved: ${imgFile}`);
            if (img.revised_prompt) parts.push(`Revised prompt: ${img.revised_prompt}`);
          } else if (img.url) {
            parts.push(`![Generated Image](${img.url})`);
            parts.push(`Image URL: ${img.url}`);
          }
        }
        if (parts.length === 0) parts.push(safeJson(result));
        return { content: [{ type: "text", text: parts.join("\n\n") }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry image generation failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Web Search: real-time Bing-grounded search ──────────────
  // Server-side web search via Bing grounding — no egress policy needed.
  // Results include inline URL citations.
  api.registerTool({
    name: "foundry_web_search",
    label: "Foundry Web Search",
    description:
      "Search the web in real-time via Azure AI Foundry's Bing grounding. " +
      "Returns answers with inline URL citations. Runs server-side — no egress " +
      "policy exceptions needed. Use for current events, news, recent changes, " +
      "verifying facts, or any query needing up-to-date information.",
    parameters: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description: "The search query or question to look up on the web.",
        },
        model: {
          type: "string",
          description: "Model to use (default: gpt-4.1).",
        },
      },
      required: ["query"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        // Connection ID: env var override → auto-discover first GroundingWithBingSearch connection.
        // The Responses API requires the FULL resource ID, not short /connections/name.
        let connId = process.env.BING_CONNECTION_ID;
        if (!connId) {
          try {
            const conns = await routerCall("GET", "/connections?api-version=2025-05-15-preview");
            const bingConn = (conns.value || conns || []).find(
              (c: any) => c.type === "GroundingWithBingSearch" ||
                c.properties?.category === "GroundingWithBingSearch"
            );
            if (bingConn) connId = bingConn.id; // full resource ID
          } catch { /* fall through to default */ }
        }

        const result = await routerCall("POST", "/openai/responses?api-version=2025-11-15-preview", {
          model: (params.model as string) || "gpt-4.1",
          input: params.query,
          tools: [{
            type: "bing_grounding",
            bing_grounding: {
              search_configurations: [{ project_connection_id: connId }],
            },
          }],
          store: false,
        });
        const output = result.output || result;
        const textParts: string[] = [];
        if (Array.isArray(output)) {
          for (const item of output) {
            if (item.type === "message" && item.content) {
              for (const c of item.content) {
                if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
              }
            }
          }
        }
        return {
          content: [{
            type: "text",
            text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2),
          }],
        };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry web search failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry File Search: RAG over uploaded documents ─────────────────
  // Knowledge retrieval from vector stores via Foundry's file_search tool.
  api.registerTool({
    name: "foundry_file_search",
    label: "Foundry File Search (RAG)",
    description:
      "Search documents and manage vector stores via Azure AI Foundry's file_search. " +
      "Operations: 'search' for RAG queries, 'create_vector_store' to create a store, " +
      "'list_vector_stores' to list stores, 'delete_vector_store' to remove one, " +
      "'upload_file' to add a file to a store. Use search for document Q&A.",
    parameters: {
      type: "object",
      properties: {
        operation: {
          type: "string",
          enum: ["search", "create_vector_store", "list_vector_stores", "delete_vector_store", "upload_file"],
          description: "Operation: 'search' (default), or manage vector stores/files.",
        },
        query: {
          type: "string",
          description: "The question or search query (for 'search').",
        },
        vector_store_ids: {
          type: "array",
          items: { type: "string" },
          description: "Vector store IDs to search (for 'search'). Omit to search all.",
        },
        store_name: {
          type: "string",
          description: "Name for the vector store (for 'create_vector_store').",
        },
        vector_store_id: {
          type: "string",
          description: "Vector store ID (for 'delete_vector_store' or 'upload_file').",
        },
        file_id: {
          type: "string",
          description: "File ID to add to vector store (for 'upload_file' — upload file via foundry_code_execute first).",
        },
        model: {
          type: "string",
          description: "Model to use for search (default: gpt-4.1).",
        },
      },
      required: [],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const op = (params.operation as string) || "search";
        const apiVer = "api-version=2025-11-15-preview";

        if (op === "list_vector_stores") {
          const result = await routerCall("GET", `/openai/vector_stores?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "create_vector_store") {
          const result = await routerCall("POST", `/openai/vector_stores?${apiVer}`, {
            name: params.store_name || "azureclaw-store",
          });
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "delete_vector_store") {
          await routerCall("DELETE", `/openai/vector_stores/${params.vector_store_id}?${apiVer}`);
          return { content: [{ type: "text", text: `Vector store ${params.vector_store_id} deleted.` }] };
        } else if (op === "upload_file") {
          const result = await routerCall("POST",
            `/openai/vector_stores/${params.vector_store_id}/files?${apiVer}`,
            { file_id: params.file_id });
          return { content: [{ type: "text", text: safeJson(result) }] };
        }

        // Default: search operation
        const fileSearchTool: any = { type: "file_search" };
        if (params.vector_store_ids) {
          fileSearchTool.file_search = { vector_store_ids: params.vector_store_ids };
        }
        const result = await routerCall("POST", `/openai/responses?${apiVer}`, {
          model: (params.model as string) || "gpt-4.1",
          input: params.query,
          tools: [fileSearchTool],
          store: false,
        });
        const output = result.output || result;
        const textParts: string[] = [];
        if (Array.isArray(output)) {
          for (const item of output) {
            if (item.type === "message" && item.content) {
              for (const c of item.content) {
                if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
              }
            } else if (item.type === "file_search_call" && item.results) {
              for (const r of item.results) {
                textParts.push(`[${r.filename || "source"}] ${r.text || ""}`);
              }
            }
          }
        }
        return {
          content: [{
            type: "text",
            text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2),
          }],
        };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry file search failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Memory: persistent semantic memory store ────────────────
  api.registerTool({
    name: "foundry_memory",
    label: "Foundry Memory Store",
    description:
      "Manage persistent agent memory via Azure AI Foundry Memory Store. " +
      "Store facts, preferences, and context that persists across conversations. " +
      "Supports semantic search over stored memories.",
    parameters: {
      type: "object",
      properties: {
        operation: {
          type: "string",
          enum: ["search", "update", "delete_scope"],
          description: "Operation: 'search' to find relevant memories, 'update' to store new facts/preferences, 'delete_scope' to clear all memories in a scope.",
        },
        text: {
          type: "string",
          description: "For 'update': the fact or preference to remember (e.g. 'User prefers dark roast coffee'). For 'search': the query to find relevant memories (e.g. 'coffee preferences').",
        },
        scope: { type: "string", description: "Memory scope (default: sandbox name). Use to partition memories by user." },
        store_name: { type: "string", description: "Memory store name (default: 'memory-{agent}')." },
      },
      required: ["operation", "text"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
        const store = (params.store_name as string) || `memory-${agentName}`;
        const scope = (params.scope as string) || agentName;
        const op = params.operation as string;
        const text = (params.text as string) || "";
        const apiVer = "api-version=2025-11-15-preview";

        // Build Foundry-format conversation item (same for both update and search)
        const makeItem = (content: string) => ({
          type: "message",
          role: "user",
          content: [{ type: "input_text", text: content }],
        });

        // Poll an update operation until complete (LRO)
        const pollUpdate = async (updateId: string, maxWaitMs = 60000) => {
          const start = Date.now();
          while (Date.now() - start < maxWaitMs) {
            await new Promise(r => setTimeout(r, 2000));
            try {
              const status = await routerCall("GET", `/memory_stores/${store}/updates/${updateId}?${apiVer}`);
              const state = status?.status || status?.state;
              if (state === "completed" || state === "succeeded") return status;
              if (state === "failed" || state === "error") throw new Error(`Memory update failed: ${safeJson(status)}`);
            } catch (e: any) {
              if (!e.message?.includes("404")) throw e;
            }
          }
          return { status: "timeout", message: "Memory update still processing. It will complete in the background." };
        };

        // Auto-create memory store if it doesn't exist yet
        const ensureStore = async () => {
          try {
            await routerCall("GET", `/memory_stores/${store}?${apiVer}`);
          } catch (e: any) {
            if (e.message?.includes("404") || e.message?.includes("not_found") || e.message?.includes("not found")) {
              const chatModel = process.env.OPENCLAW_MODEL || "gpt-4.1";
              const embeddingModel = getFoundryProject()?.deployments?.find(
                (d: any) => d.id?.includes("embedding") || d.model?.includes("embedding")
              )?.id || "text-embedding-3-small";
              log.info(`Creating memory store '${store}' (chat=${chatModel}, embedding=${embeddingModel})`);
              await routerCall("POST", `/memory_stores?${apiVer}`, {
                name: store,
                description: "AzureClaw agent persistent memory",
                definition: {
                  kind: "default",
                  chat_model: chatModel,
                  embedding_model: embeddingModel,
                  options: {
                    user_profile_enabled: true,
                    user_profile_details: "Store user preferences, decisions, and project context",
                    chat_summary_enabled: true,
                  },
                },
              });
              log.info(`Memory store '${store}' created successfully`);
            }
          }
        };

        if (op === "search") {
          const body = {
            scope,
            items: [makeItem(text)],
            options: { max_memories: 10 },
          };
          try {
            const result = await routerCall("POST", `/memory_stores/${store}:search_memories?${apiVer}`, body);
            return { content: [{ type: "text", text: safeJson(result) }] };
          } catch (e: any) {
            if (e.message?.includes("not found") || e.message?.includes("not_found")) {
              try {
                await ensureStore();
                // Retry search after store creation
                const result = await routerCall("POST", `/memory_stores/${store}:search_memories?${apiVer}`, body);
                return { content: [{ type: "text", text: safeJson(result) }] };
              } catch {
                return { content: [{ type: "text", text: "Memory store just created — no memories stored yet. Try saving something first." }] };
              }
            }
            // Don't crash session on memory errors — return graceful message
            log.warn(`Memory search failed: ${e.message}`);
            return { content: [{ type: "text", text: `Memory search failed: ${e.message}. The memory service may still be initializing.` }] };
          }
        } else if (op === "update") {
          const body = {
            scope,
            items: [makeItem(text)],
            update_delay: 0,
          };
          const doUpdate = async () => {
            const result = await routerCall("POST", `/memory_stores/${store}:update_memories?${apiVer}`, body);
            // update_memories is a LRO — log completion in background, don't block chat
            const updateId = result?.update_id || result?.id;
            if (updateId && (result?.status === "queued" || result?.status === "running")) {
              pollUpdate(updateId).then(
                (r) => log.info(`Memory update ${updateId} completed: ${JSON.stringify(r?.memory_operations?.length ?? 0)} ops`),
                (e) => log.warn(`Memory update ${updateId} failed: ${e.message}`),
              );
            }
            return result;
          };
          try {
            const result = await doUpdate();
            const status = result?.status || "submitted";
            return { content: [{ type: "text", text: `Memory update ${status}. The memory will be available shortly.` }] };
          } catch (e: any) {
            if (e.message?.includes("not found") || e.message?.includes("not_found")) {
              try {
                await ensureStore();
                const result = await doUpdate();
                const status = result?.status || "submitted";
                return { content: [{ type: "text", text: `Memory update ${status}. The memory will be available shortly.` }] };
              } catch (retryErr: any) {
                log.warn(`Memory update failed after store creation: ${retryErr.message}`);
                return { content: [{ type: "text", text: `Memory update failed: ${retryErr.message}` }] };
              }
            }
            log.warn(`Memory update failed: ${e.message}`);
            return { content: [{ type: "text", text: `Memory update failed: ${e.message}. The memory service may still be initializing.` }] };
          }
        } else if (op === "delete_scope") {
          await routerCall("POST", `/memory_stores/${store}:delete_scope?${apiVer}`, { scope });
          return { content: [{ type: "text", text: `Scope '${scope}' deleted from memory store '${store}'.` }] };
        }
        return { content: [{ type: "text", text: `Unknown operation: ${op}` }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry memory failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Conversations: persistent multi-turn state ──────────────
  api.registerTool({
    name: "foundry_conversations",
    label: "Foundry Conversations",
    description:
      "Manage persistent server-side conversations via Azure AI Foundry. " +
      "Use cases: maintain long-running multi-turn dialogues across sessions, " +
      "build research threads that survive restarts, keep separate conversation " +
      "contexts for different tasks/topics. Operations: create, list, get, respond, " +
      "add_message, delete.",
    parameters: {
      type: "object",
      properties: {
        operation: {
          type: "string",
          enum: ["create", "list", "get", "respond", "add_message", "delete"],
          description: "Operation to perform. 'get' retrieves full message history for a conversation.",
        },
        conversation_id: { type: "string", description: "Conversation ID (for get/respond/add_message/delete)." },
        input: { type: "string", description: "User input (for 'respond' — generates AI response in conversation context)." },
        message: { type: "string", description: "Message text to add (for 'add_message')." },
        role: { type: "string", description: "Message role: 'user' or 'assistant' (for 'add_message', default: 'user')." },
        metadata: { type: "object", description: "Metadata for new conversation (for 'create')." },
        model: { type: "string", description: "Model to use for responses (default: gpt-4.1)." },
      },
      required: ["operation"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const op = params.operation as string;
        const apiVer = "api-version=2025-11-15-preview";

        if (op === "create") {
          const result = await routerCall("POST", `/openai/conversations?${apiVer}`, {
            metadata: params.metadata || { user: process.env.SANDBOX_NAME || "agent" },
          });
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "list") {
          const result = await routerCall("GET", `/openai/conversations?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "get") {
          const result = await routerCall("GET", `/openai/conversations/${params.conversation_id}?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "respond") {
          const result = await routerCall("POST", `/openai/responses?${apiVer}`, {
            model: (params.model as string) || "gpt-4.1",
            input: params.input,
            conversation: params.conversation_id,
            store: true,
          });
          const output = result.output || result;
          const textParts: string[] = [];
          if (Array.isArray(output)) {
            for (const item of output) {
              if (item.type === "message" && item.content) {
                for (const c of item.content) {
                  if (c.type === "output_text" || c.type === "text") textParts.push(c.text);
                }
              }
            }
          }
          return { content: [{ type: "text", text: textParts.length > 0 ? textParts.join("\n\n") : JSON.stringify(output, null, 2) }] };
        } else if (op === "add_message") {
          const result = await routerCall("POST", `/openai/conversations/${params.conversation_id}/items?${apiVer}`, {
            items: [{
              type: "message",
              role: (params.role as string) || "user",
              content: [{ type: "input_text", text: params.message }],
            }],
          });
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "delete") {
          await routerCall("DELETE", `/openai/conversations/${params.conversation_id}?${apiVer}`);
          return { content: [{ type: "text", text: `Conversation ${params.conversation_id} deleted.` }] };
        }
        return { content: [{ type: "text", text: `Unknown operation: ${op}` }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry conversations failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Evaluations: model quality testing ──────────────────────
  api.registerTool({
    name: "foundry_evaluations",
    label: "Foundry Evaluations",
    description:
      "Create and run model quality evaluations via Azure AI Foundry Evals API. " +
      "Use cases: benchmark prompt quality before/after changes, validate output " +
      "against golden answers, run regression tests on model responses, compare " +
      "different models. Operations: list, create, run, get_run, list_evaluators.",
    parameters: {
      type: "object",
      properties: {
        operation: {
          type: "string",
          enum: ["list", "create", "run", "get_run", "list_evaluators"],
          description: "Operation: 'list' evals, 'create' one, 'run' it, 'get_run' status/results, or 'list_evaluators'.",
        },
        eval_id: { type: "string", description: "Eval ID (for 'run')." },
        run_id: { type: "string", description: "Run ID (for 'get_run' — check status and results)." },
        name: { type: "string", description: "Eval name (for 'create')." },
        data_source_config: { type: "object", description: "Data source config (for 'create')." },
        testing_criteria: { type: "array", items: { type: "object" }, description: "Testing criteria array (for 'create')." },
        run_config: { type: "object", description: "Run configuration (for 'run')." },
      },
      required: ["operation"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const op = params.operation as string;
        const apiVer = "api-version=2025-11-15-preview";

        if (op === "list") {
          const result = await routerCall("GET", `/openai/evals?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "create") {
          const result = await routerCall("POST", `/openai/evals?${apiVer}`, {
            name: params.name,
            data_source_config: params.data_source_config,
            testing_criteria: params.testing_criteria,
          });
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "run") {
          const result = await routerCall("POST", `/openai/evals/${params.eval_id}/runs?${apiVer}`,
            params.run_config || {});
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "get_run") {
          const result = await routerCall("GET", `/openai/evals/${params.eval_id}/runs/${params.run_id}?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } else if (op === "list_evaluators") {
          const result = await routerCall("GET", `/evaluators?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        }
        return { content: [{ type: "text", text: `Unknown operation: ${op}` }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry evaluations failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Deployments: discover available models and connections ───
  api.registerTool({
    name: "foundry_deployments",
    label: "Foundry Deployments & Connections",
    description:
      "Query available Azure AI Foundry resources: models, connections, " +
      "search indexes, and datasets. Use 'models' to see all available AI models, " +
      "'connections' for data connections, 'indexes' for search indexes.",
    parameters: {
      type: "object",
      properties: {
        resource: {
          type: "string",
          enum: ["models", "connections", "indexes", "datasets"],
          description: "Resource type to query. Use 'models' to list available AI models.",
        },
      },
      required: ["resource"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const resource = params.resource as string;

        if (resource === "models") {
          // Query live Foundry project deployments — returns actual deployed models
          const apiVer = "api-version=2025-11-15-preview";
          try {
            const result = await routerCall("GET", `/deployments?${apiVer}`);
            const deps = result?.value || result?.data || [];
            if (Array.isArray(deps) && deps.length > 0) {
              const currentModel = process.env.OPENCLAW_MODEL || config.model || "gpt-4.1";
              const models = deps.map((d: any) => ({
                id: d.name || d.id,
                model: d.modelName || d.model || d.name || "unknown",
                version: d.modelVersion || "",
                publisher: d.modelPublisher || "",
                capabilities: d.capabilities || {},
                sku: d.sku?.name || "unknown",
                capacity: d.sku?.capacity || 0,
                current: (d.name || d.id) === currentModel,
              }));
              return { content: [{ type: "text", text: safeJson({
                source: "foundry_project_deployments",
                current_model: currentModel,
                total: models.length,
                models,
              }) }] };
            }
          } catch { /* fall through to cached */ }

          // Fallback to cached discovery from startup
          const cachedFp = getFoundryProject();
          if (cachedFp?.deployments && cachedFp.deployments.length > 0) {
            return { content: [{ type: "text", text: safeJson({
              source: "cached_discovery",
              total: cachedFp.deployments.length,
              models: cachedFp.deployments,
            }) }] };
          }

          return { content: [{ type: "text", text: "No deployments found. Check Foundry project configuration." }] };
        }

        // Other resources: try Foundry API first, fall back gracefully
        const apiVer = "api-version=2025-11-15-preview";
        const result = await routerCall("GET", `/${resource}?${apiVer}`);
        return { content: [{ type: "text", text: safeJson(result) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry query failed: ${e.message}` }] };
      }
    },
  });

  // ── Foundry Agents: list and query Foundry-hosted agents ────────────
  api.registerTool({
    name: "foundry_agents",
    label: "Foundry Agents",
    description:
      "List and query Azure AI Foundry hosted agents. Discover available agents, " +
      "their capabilities, and configurations. These are server-side Foundry agents " +
      "(different from AzureClaw sub-agent sandboxes).",
    parameters: {
      type: "object",
      properties: {
        operation: {
          type: "string",
          enum: ["list", "get"],
          description: "Operation: 'list' all agents or 'get' a specific agent.",
        },
        agent_id: { type: "string", description: "Agent ID (for 'get')." },
      },
      required: ["operation"],
    },
    async execute(_id: string, params: Record<string, unknown>) {
      try {
        const apiVer = "api-version=2025-11-15-preview";
        if (params.operation === "get" && params.agent_id) {
          const result = await routerCall("GET", `/agents/${params.agent_id}?${apiVer}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        }
        const result = await routerCall("GET", `/agents?${apiVer}`);
        return { content: [{ type: "text", text: safeJson(result) }] };
      } catch (e: any) {
        return { content: [{ type: "text", text: `Foundry agents query failed: ${e.message}` }] };
      }
    },
  });
}
