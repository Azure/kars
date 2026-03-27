/**
 * AzureClaw — OpenClaw Plugin
 *
 * Registers AzureClaw commands, Azure OpenAI as a model provider,
 * and agent tools (spawn, mesh, inbox, destroy) within the OpenClaw
 * plugin system using the native definePluginEntry SDK.
 *
 * AGT Integration: Uses @agentmesh/sdk for tool-level policy evaluation,
 * trust scoring, and audit logging. AzureClaw's Rust router handles
 * infrastructure-level controls (mesh routing, content safety, token budgets).
 *
 * Usage: openclaw azureclaw <command>
 */

import type { Command } from "commander";

// ---------------------------------------------------------------------------
// OpenClaw Plugin SDK — loaded dynamically at runtime from the host OpenClaw
// installation. definePluginEntry wraps the plugin definition so OpenClaw
// provides the full API (registerTool, registerProvider, registerCommand, etc.).
// ---------------------------------------------------------------------------

let definePluginEntry: (def: any) => any;
try {
  const sdk = require("openclaw/plugin-sdk/plugin-entry");
  definePluginEntry = sdk.definePluginEntry;
} catch {
  // Fallback: identity wrapper — plain object pattern for older OpenClaw
  definePluginEntry = (def: any) => def;
}

// Prevent unhandled rejections from crashing the process.
// The read-only rootfs causes EPERM in chokidar file watchers — those
// are non-fatal but show up as unhandled rejections that kill Node.
process.on("unhandledRejection", (reason: any) => {
  const msg = reason?.message || String(reason);
  // EPERM from file watchers on read-only rootfs — harmless, suppress
  if (msg.includes("EPERM") && msg.includes("watch")) return;
  console.error("[azureclaw] Unhandled rejection (suppressed crash):", msg);
});

// ---------------------------------------------------------------------------
// AGT SDK — AgentMesh (amitayks/agentmesh)
// Full E2E encrypted inter-agent communication via self-hosted relay/registry.
// Also: tool-level policy, trust scoring, audit logging.
// Infrastructure controls (NetworkPolicy, token budgets) stay in Rust router.
// ---------------------------------------------------------------------------

let agtPolicy: any = null;
let agtTrustStore: any = null;
let agtAuditLogger: any = null;
let agtMeshClient: any = null;
let agtIdentity: any = null;
let agtInitialized = false; // Singleton guard — only first plugin load creates the mesh client

// AGT message buffer — filled by onMessage handler, drained by mesh_inbox tool
const agtInbox: Array<{ from_amid: string; from_agent: string; content: any; timestamp: string; id: string }> = [];

// AGT reconnect & heartbeat state
let agtReconnectTimer: ReturnType<typeof setInterval> | null = null;
let agtHeartbeatTimer: ReturnType<typeof setInterval> | null = null;
let agtInboxNotifyTimer: ReturnType<typeof setInterval> | null = null;
let agtConnected = false;
let agtSdk: any = null; // cached SDK module for reconnect

// AMID → agent name mapping (populated during send via registry search)
const amidToName: Map<string, string> = new Map();
const nameToAmid: Map<string, string> = new Map();

// Stored sandbox name for reconnect attempts
let agtSandboxName: string = "unknown";

// Push trust updates to the router's local TrustStore (POST /agt/trust).
// This syncs the plugin's reputation observations with the router for /agt/status display.
// Also writes an audit chain entry on the router side.
async function pushTrustToRouter(agentId: string, scoreDelta: number) {
  try {
    const http = await import("node:http");
    const body = JSON.stringify({
      agent_id: agentId,
      score: Math.round(500 + scoreDelta * 500), // 0.0-1.0 → 0-1000 scale
      interactions: 1,
    });
    await new Promise<void>((resolve, reject) => {
      const req = http.request("http://127.0.0.1:8443/agt/trust", {
        method: "POST",
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) },
        timeout: 5000,
      }, (res: any) => {
        res.resume();
        res.on("end", () => {
          if (res.statusCode && res.statusCode >= 400) {
            reject(new Error(`HTTP ${res.statusCode}`));
          } else {
            resolve();
          }
        });
      });
      req.on("error", reject);
      req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
      req.write(body);
      req.end();
    });
  } catch (e: any) {
    console.error(`[azureclaw] pushTrustToRouter failed for ${agentId}: ${e.message}`);
  }
}

// Attempt to reconnect the AGT mesh client after a disconnect.
async function agtReconnect(log: { info: (m: string) => void; warn: (m: string) => void }) {
  if (!agtMeshClient || agtConnected) return;
  try {
    await agtMeshClient.connect({
      displayName: agtSandboxName,
      capabilities: ["azureclaw-agent", "task-execution", agtSandboxName],
    });
    agtConnected = true;
    log.info("AGT mesh reconnected successfully");
  } catch (e: any) {
    log.warn(`AGT mesh reconnect failed: ${e.message}`);
  }
}

// Write unread inbox messages to a file the LLM can see in its context.
// This is the key mechanism to keep conversations "lively" — the agent sees
// pending messages in MEMORY.md without needing to manually call mesh_inbox.
async function notifyInboxToMemory(log: { info: (m: string) => void; warn: (m: string) => void }) {
  if (agtInbox.length === 0) return;
  try {
    const fs = await import("node:fs/promises");
    const memPath = process.env.MEMORY_FILE_PATH || "/home/user/MEMORY.md";
    const INBOX_MARKER = "<!-- AGT_INBOX_START -->";
    const INBOX_END = "<!-- AGT_INBOX_END -->";

    let existing = "";
    try { existing = await fs.readFile(memPath, "utf-8"); } catch { return; }

    // Build inbox section with pending messages (don't drain — just preview)
    const preview = agtInbox.slice(0, 10).map((m, i) =>
      `${i + 1}. **${m.from_agent}** (${m.timestamp}): ${String(m.content).slice(0, 300)}`
    ).join("\n");
    const section = [
      INBOX_MARKER,
      "",
      `## 📬 Unread Mesh Messages (${agtInbox.length})`,
      "",
      `> You have ${agtInbox.length} unread message(s) from sub-agents. Call \`azureclaw_mesh_inbox\` to read and respond.`,
      "",
      preview,
      "",
      INBOX_END,
    ].join("\n");

    // Replace existing inbox section or append
    if (existing.includes(INBOX_MARKER)) {
      const re = new RegExp(`${INBOX_MARKER}[\\s\\S]*?${INBOX_END}`, "m");
      existing = existing.replace(re, section);
    } else {
      existing = existing + "\n\n" + section;
    }
    await fs.writeFile(memPath, existing, "utf-8");
    log.info(`AGT inbox: wrote ${agtInbox.length} pending message(s) to MEMORY.md`);
  } catch { /* best effort */ }
}
interface FoundryProjectInfo {
  endpoint: string;
  deployments: Array<{ id: string; model: string; sku?: string }>;
  connections: Array<{ name: string; type: string }>;
  indexes: Array<{ name: string }>;
}
let foundryProject: FoundryProjectInfo | null = null;
let foundryInitialized = false;

/**
 * Delegate a task to the native OpenClaw agent loop running in the Gateway.
 * This gives the sub-agent access to ALL OpenClaw tools (exec, process, web_search,
 * web_fetch, browser, cron, read/write, etc.) plus all AzureClaw plugin skills
 * (foundry_memory, foundry_web_search, foundry_code, etc.).
 *
 * The task is sent via `openclaw agent --message` which goes through the Gateway's
 * full agent pipeline (AGENTS.md, SOUL.md, TOOLS.md, skills, tool policy, etc.).
 */
async function delegateToNativeAgent(
  taskContent: string,
  fromAgent: string,
  log: { info: (m: string) => void; warn: (m: string) => void },
): Promise<string> {
  const { spawn } = await import("node:child_process");

  // Stable session ID per sender → maintains conversation context across tasks
  const sessionId = `agt-task-${fromAgent}`;
  const taskText = typeof taskContent === "string" ? taskContent : JSON.stringify(taskContent);

  log.info(`Delegating task to native OpenClaw agent (session: ${sessionId})`);

  return new Promise<string>((resolve, reject) => {
    const child = spawn("openclaw", [
      "agent",
      "--message", taskText,
      "--session-id", sessionId,
      "--timeout", "300",
      "--json",
    ], { env: process.env, stdio: ["ignore", "pipe", "pipe"] });

    const chunks: Buffer[] = [];
    // OpenClaw writes all output (plugin logs + JSON result) to stderr
    child.stderr.on("data", (chunk: Buffer) => chunks.push(chunk));
    child.stdout.on("data", (chunk: Buffer) => chunks.push(chunk));

    const timer = setTimeout(() => { child.kill("SIGTERM"); }, 120_000);

    child.on("close", () => {
      clearTimeout(timer);
      const output = Buffer.concat(chunks).toString("utf-8");

      // Extract the JSON response by finding the last top-level { ... } block
      const jsonMatch = output.match(/\n(\{[\s\S]*\})\s*$/);
      if (jsonMatch) {
        try {
          const result = JSON.parse(jsonMatch[1]);
          const text = result?.reply?.text || result?.text || "";
          if (text) {
            log.info(`Native agent responded (${text.length} chars, session: ${sessionId})`);
            return resolve(text);
          }
        } catch { /* fall through */ }
      }

      // Fallback: strip log lines and return raw text
      const lines = output.split("\n").filter((l: string) =>
        !l.startsWith("[plugins]") && !l.startsWith("[") && l.trim());
      const response = lines.join("\n").trim();
      if (response) {
        log.info(`Native agent responded (${response.length} chars, session: ${sessionId})`);
        return resolve(response);
      }

      log.warn(`Native agent returned empty response (${output.length} bytes captured)`);
      reject(new Error("Native agent returned empty response"));
    });

    child.on("error", (err: Error) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

/**
 * Fallback: process a task_request with a limited tool-calling loop.
 * Used when native delegation fails (e.g., Gateway not running).
 * Runs an LLM loop with 6 tools, max 10 rounds, 2048 max_tokens.
 */
async function processTaskWithTools(
  taskContent: any,
  log: { info: (m: string) => void; warn: (m: string) => void },
): Promise<string> {
  const http = await import("node:http");
  const { execSync } = await import("node:child_process");
  const model = process.env.MODEL || "gpt-4.1";

  const tools = [
    {
      type: "function" as const,
      function: {
        name: "exec_command",
        description: "Execute a shell command inside the sandbox and return stdout/stderr. Use for system info (uname, hostname, ip addr, cat /etc/os-release, etc.), file operations, or any command-line task. NOTE: Direct internet access (curl to external URLs) is blocked — use http_fetch for external HTTP requests.",
        parameters: {
          type: "object",
          properties: {
            command: { type: "string", description: "The shell command to execute" },
          },
          required: ["command"],
        },
      },
    },
    {
      type: "function" as const,
      function: {
        name: "http_fetch",
        description: "Make an HTTP request to an external URL through the security proxy. The request goes through blocklist checking and allowlist enforcement. Use this for any external API calls (Telegram, HackerNews, web APIs, etc.).",
        parameters: {
          type: "object",
          properties: {
            url: { type: "string", description: "The full URL to fetch (e.g., https://api.telegram.org/...)" },
            method: { type: "string", description: "HTTP method: GET, POST, PUT, DELETE. Default: GET" },
            headers: { type: "object", description: "Optional HTTP headers as key-value pairs" },
            body: { type: "string", description: "Optional request body (for POST/PUT)" },
          },
          required: ["url"],
        },
      },
    },
    {
      type: "function" as const,
      function: {
        name: "foundry_web_search",
        description: "Search the web in real-time via Azure AI Foundry's Bing grounding. Returns answers with inline URL citations. Runs server-side — no egress policy exceptions needed. Use for current events, news, recent changes, verifying facts, or any query needing up-to-date information.",
        parameters: {
          type: "object",
          properties: {
            query: { type: "string", description: "The search query or question to look up on the web." },
          },
          required: ["query"],
        },
      },
    },
    {
      type: "function" as const,
      function: {
        name: "foundry_code_execute",
        description: "Execute Python code server-side via Azure AI Foundry's code_interpreter. Has pandas, numpy, matplotlib, scipy pre-installed. Use for data analysis, charts, complex math, and file processing.",
        parameters: {
          type: "object",
          properties: {
            code: { type: "string", description: "Python code to execute." },
          },
          required: ["code"],
        },
      },
    },
    {
      type: "function" as const,
      function: {
        name: "foundry_file_search",
        description: "Search uploaded documents and knowledge bases via Azure AI Foundry's file_search. Requires vector_store_ids — use foundry_memory instead for general memory/knowledge storage.",
        parameters: {
          type: "object",
          properties: {
            query: { type: "string", description: "The search query." },
            vector_store_ids: { type: "array", items: { type: "string" }, description: "Vector store IDs to search (required)." },
          },
          required: ["query", "vector_store_ids"],
        },
      },
    },
    {
      type: "function" as const,
      function: {
        name: "foundry_memory",
        description: "Persistent agent memory via Azure AI Foundry Memory Store. Store facts, preferences, and context that persists across sessions. Use 'search' to recall, 'update' to store new knowledge.",
        parameters: {
          type: "object",
          properties: {
            operation: { type: "string", enum: ["search", "update"], description: "Operation: 'search' to find relevant memories, 'update' to store new facts." },
            text: { type: "string", description: "For 'update': the fact to remember. For 'search': the query to find relevant memories." },
          },
          required: ["operation", "text"],
        },
      },
    },
  ];

  const messages: Array<{ role: string; content?: string; tool_calls?: any[]; tool_call_id?: string; name?: string }> = [
    {
      role: "system",
      content: "You are a helpful sub-agent running inside an AzureClaw sandbox. You have access to these tools:\n- exec_command: run shell commands (uname, hostname, etc.)\n- http_fetch: make HTTP requests through the security proxy\n- foundry_web_search: real-time web search via Bing grounding (use for news, current events, facts)\n- foundry_code_execute: run Python code server-side (pandas, numpy, matplotlib available)\n- foundry_image_generation: generate images from text prompts (gpt-image-1)\n- foundry_file_search: search documents in vector stores, manage vector stores and files\n- foundry_memory: persistent memory store — use 'search' to recall and 'update' to remember facts\n- foundry_conversations: persistent multi-turn dialogues (create, respond, get history)\n- foundry_evaluations: benchmark model quality (create, run, check results)\nUse the appropriate tool for each task. Use foundry_memory (not foundry_file_search) for storing/recalling knowledge. Be concise and report actual results.",
    },
    {
      role: "user",
      content: typeof taskContent === "string" ? taskContent : JSON.stringify(taskContent),
    },
  ];

  // Tool-calling loop (max 10 rounds to prevent runaway)
  for (let round = 0; round < 10; round++) {
    const postData = JSON.stringify({ model, messages, tools, max_tokens: 2048 });
    const response = await new Promise<any>((resolve, reject) => {
      const req = http.request("http://127.0.0.1:8443/v1/chat/completions", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Content-Length": Buffer.byteLength(postData),
          "x-azureclaw-sandbox": process.env.SANDBOX_NAME || process.env.HOSTNAME || "unknown",
        },
        timeout: 60000,
      }, (res) => {
        let body = "";
        res.on("data", (chunk: Buffer) => { body += chunk.toString(); });
        res.on("end", () => {
          try {
            const parsed = JSON.parse(body);
            if (res.statusCode && res.statusCode >= 400) {
              reject(new Error(`LLM HTTP ${res.statusCode}: ${body.slice(0, 300)}`));
            } else {
              resolve(parsed);
            }
          } catch { reject(new Error(`LLM parse error: ${body.slice(0, 200)}`)); }
        });
        res.on("error", () => {});
      });
      req.on("error", (e) => reject(e));
      req.on("timeout", () => { req.destroy(); reject(new Error("LLM timeout")); });
      req.write(postData);
      req.end();
    });

    const choice = response?.choices?.[0];
    if (!choice) throw new Error("No LLM response");

    const msg = choice.message;

    // If the model wants to call tools, execute them and continue
    if (msg.tool_calls && msg.tool_calls.length > 0) {
      messages.push(msg);
      for (const tc of msg.tool_calls) {
        let result: string;
        try {
          const args = JSON.parse(tc.function.arguments);
          const fnName = tc.function.name;

          if (fnName === "http_fetch") {
            // Route through the egress proxy (blocklist + allowlist checked)
            log.info(`AGT sub-agent http_fetch: ${args.method || "GET"} ${args.url}`);
            const fetchBody = JSON.stringify({
              url: args.url,
              method: args.method || "GET",
              headers: args.headers || {},
              body: args.body || "",
            });
            const fetchResult = execSync(
              `curl -s -X POST http://127.0.0.1:8443/egress/fetch -H "Content-Type: application/json" -d '${fetchBody.replace(/'/g, "'\\''")}'`,
              { timeout: 35000, encoding: "utf8", maxBuffer: 256 * 1024 },
            ).trim();
            result = fetchResult;
          } else if (fnName === "foundry_web_search" || fnName === "foundry_code_execute" || fnName === "foundry_file_search") {
            // Route through the inference router → Foundry Responses API
            log.info(`AGT sub-agent ${fnName}: ${JSON.stringify(args).slice(0, 200)}`);
            let reqBody: any;
            if (fnName === "foundry_web_search") {
              // Discover Bing connection
              let connId: string | undefined;
              try {
                const connsRaw = await new Promise<string>((resolve, reject) => {
                  const r = http.get("http://127.0.0.1:8443/connections?api-version=2025-05-15-preview", { timeout: 10000 }, (res) => {
                    let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
                  });
                  r.on("error", reject); r.on("timeout", () => { r.destroy(); reject(new Error("timeout")); });
                });
                const conns = JSON.parse(connsRaw);
                const bingConn = (conns.value || conns || []).find(
                  (c: any) => c.type === "GroundingWithBingSearch" || c.properties?.category === "GroundingWithBingSearch"
                );
                if (bingConn) connId = bingConn.id;
              } catch { /* fall through */ }
              reqBody = {
                model: model,
                input: args.query,
                tools: [{ type: "bing_grounding", bing_grounding: { search_configurations: [{ project_connection_id: connId }] } }],
                store: false,
              };
            } else if (fnName === "foundry_code_execute") {
              reqBody = {
                model: model,
                input: args.code,
                tools: [{ type: "code_interpreter", container: { type: "auto" } }],
                instructions: "Execute the provided Python code and return the output.",
                store: false,
              };
            } else {
              // foundry_file_search — vector_store_ids is now required
              reqBody = {
                model: model,
                input: args.query,
                tools: [{ type: "file_search", file_search: { vector_store_ids: args.vector_store_ids } }],
                store: false,
              };
            }
            // Use Node HTTP instead of curl to avoid shell escaping issues
            const foundryResult = await new Promise<string>((resolve, reject) => {
              const postBody = JSON.stringify(reqBody);
              const req = http.request("http://127.0.0.1:8443/openai/responses?api-version=2025-11-15-preview", {
                method: "POST",
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postBody) },
                timeout: 60000,
              }, (res) => {
                let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
              });
              req.on("error", reject);
              req.on("timeout", () => { req.destroy(); reject(new Error("Foundry API timeout")); });
              req.write(postBody);
              req.end();
            });
            // Extract text from Responses API output
            try {
              const parsed = JSON.parse(foundryResult);
              if (parsed.error) {
                result = `Foundry API error: ${JSON.stringify(parsed.error)}`;
                log.warn(`AGT sub-agent ${fnName} error: ${result}`);
              } else {
                const output = parsed.output || parsed;
                if (Array.isArray(output)) {
                  const texts = output
                    .filter((item: any) => item.type === "message" && item.content)
                    .flatMap((item: any) => item.content)
                    .filter((c: any) => c.type === "output_text" || c.type === "text")
                    .map((c: any) => c.text)
                    .filter(Boolean);
                  result = texts.join("\n") || foundryResult;
                } else {
                  result = foundryResult;
                }
              }
            } catch {
              result = foundryResult;
            }
            log.info(`AGT sub-agent ${fnName} result: ${result.slice(0, 200)}`);
          } else if (fnName === "foundry_memory") {
            // Route through the inference router → Foundry Memory Store API
            log.info(`AGT sub-agent foundry_memory: ${args.operation} — ${(args.text as string || "").slice(0, 100)}`);
            const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
            const store = `memory-${agentName}`;
            const scope = agentName;
            const apiVer = "api-version=2025-11-15-preview";
            const makeItem = (content: string) => ({
              type: "message", role: "user",
              content: [{ type: "input_text", text: content }],
            });

            let memPath: string;
            let memBody: any;
            if (args.operation === "update") {
              memPath = `/memory_stores/${store}:update_memories?${apiVer}`;
              memBody = { scope, items: [makeItem(args.text as string)], update_delay: 0 };
            } else {
              memPath = `/memory_stores/${store}:search_memories?${apiVer}`;
              memBody = { scope, items: [makeItem(args.text as string)], options: { max_memories: 10 } };
            }

            const memResult = await new Promise<string>((resolve, reject) => {
              const postBody = JSON.stringify(memBody);
              const req = http.request(`http://127.0.0.1:8443${memPath}`, {
                method: "POST",
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postBody) },
                timeout: 30000,
              }, (res) => {
                let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body));
              });
              req.on("error", reject);
              req.on("timeout", () => { req.destroy(); reject(new Error("Memory API timeout")); });
              req.write(postBody);
              req.end();
            });

            try {
              const parsed = JSON.parse(memResult);
              if (parsed.error) {
                // Auto-create store on not_found, then retry
                if ((parsed.error.code === "not_found" || parsed.error.message?.includes("not found")) && args.operation === "update") {
                  log.info(`Creating memory store '${store}'...`);
                  const chatModel = process.env.OPENCLAW_MODEL || model;
                  const createBody = JSON.stringify({
                    name: store,
                    description: "AzureClaw sub-agent persistent memory",
                    definition: { kind: "default", chat_model: chatModel, embedding_model: "text-embedding-3-small",
                      options: { user_profile_enabled: true, chat_summary_enabled: true } },
                  });
                  await new Promise<void>((resolve, reject) => {
                    const req = http.request(`http://127.0.0.1:8443/memory_stores?${apiVer}`, {
                      method: "POST",
                      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(createBody) },
                      timeout: 15000,
                    }, (res) => { let d = ""; res.on("data", (c: Buffer) => { d += c.toString(); }); res.on("end", () => resolve()); });
                    req.on("error", reject);
                    req.write(createBody);
                    req.end();
                  });
                  // Retry the original operation
                  const retryResult = await new Promise<string>((resolve, reject) => {
                    const postBody = JSON.stringify(memBody);
                    const req = http.request(`http://127.0.0.1:8443${memPath}`, {
                      method: "POST",
                      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postBody) },
                      timeout: 30000,
                    }, (res) => { let body = ""; res.on("data", (c: Buffer) => { body += c.toString(); }); res.on("end", () => resolve(body)); });
                    req.on("error", reject);
                    req.write(postBody);
                    req.end();
                  });
                  result = retryResult;
                } else {
                  result = `Memory error: ${JSON.stringify(parsed.error)}`;
                }
              } else {
                if (args.operation === "update") {
                  result = `Memory updated successfully (id: ${parsed.update_id || parsed.id || "ok"})`;
                } else {
                  const memories = parsed.memories || [];
                  result = memories.length > 0
                    ? memories.map((m: any) => `[${m.score?.toFixed(2) || "?"}] ${m.content || m.text || JSON.stringify(m)}`).join("\n")
                    : "No relevant memories found.";
                }
              }
            } catch {
              result = memResult;
            }
            log.info(`AGT sub-agent foundry_memory result: ${result.slice(0, 200)}`);
          } else {
            // exec_command
            const cmd = args.command || "";
            log.info(`AGT sub-agent exec: ${cmd}`);
            result = execSync(cmd, { timeout: 15000, encoding: "utf8", maxBuffer: 64 * 1024 }).trim();
          }
        } catch (e: any) {
          result = e.stderr || e.stdout || e.message || "Command failed";
        }
        messages.push({ role: "tool", tool_call_id: tc.id, content: result });
      }
      continue;
    }

    // No tool calls — return the text response
    return msg.content || "";
  }

  return "Sub-agent reached maximum tool-calling rounds without a final response.";
}

async function initAGT(log: { info: (m: string) => void; warn: (m: string) => void }) {
  // Singleton guard — only one mesh connection per module instance.
  // Don't use process.env flag: OpenClaw's gateway hot-restart resets module state
  // but inherits env vars, causing initAGT to be skipped with a null agtMeshClient.
  if (agtInitialized && agtMeshClient) return;
  agtInitialized = true;

  try {
    const sdk: any = await import("@agentmesh/sdk");
    agtSdk = sdk;

    // Policy engine — tool allow/deny evaluation
    agtPolicy = new sdk.Policy([
      { action: "web_search", effect: "allow" },
      { action: "file_read", effect: "allow" },
      { action: "file_write", effect: "allow" },
      { action: "shell:ls", effect: "allow" },
      { action: "shell:cat", effect: "allow" },
      { action: "shell:python", effect: "allow" },
      { action: "shell:git", effect: "allow" },
      { action: "shell:curl", effect: "allow" },
      { action: "shell:rm -rf /", effect: "deny" },
      { action: "shell:chmod 777", effect: "deny" },
      { action: "shell:dd", effect: "deny" },
      { action: "shell:mkfs", effect: "deny" },
    ]);

    // Trust store — 0-1000 scoring with tiers
    agtTrustStore = sdk.createTrustStore();
    // Audit logger — hash-chain append-only log
    agtAuditLogger = sdk.createAuditLogger();

    // Generate cryptographic identity (Ed25519 + X25519)
    agtIdentity = await sdk.Identity.generate();
    log.info(`AGT identity: ${agtIdentity.amid}`);

    // Create AgentMeshClient — ALWAYS connect through the router proxy.
    // The plugin (UID 1000) cannot reach external services directly (iptables blocks).
    // The router (UID 1001) proxies: /agt/relay → relay service, /agt/registry/* → registry service.
    // On AKS, router reads AGT_RELAY_URL/AGT_REGISTRY_URL to find the services.
    // In dev, same env vars point to Docker containers on the shared network.
    const registryUrl = "http://127.0.0.1:8443/agt/registry";
    const relayUrl = "ws://127.0.0.1:8443/agt/relay";

    agtMeshClient = new sdk.AgentMeshClient(agtIdentity, {
      storage: new sdk.MemoryStorage(),
      registryUrl,
      relayUrl,
    });

    // Connect to the mesh — registers with registry + connects to relay.
    // Capabilities include the sandbox name so other agents can discover us.
    // SANDBOX_NAME is set on inference-router; for openclaw container extract from HOSTNAME.
    agtSandboxName = process.env.SANDBOX_NAME
      || (process.env.HOSTNAME ? process.env.HOSTNAME.replace(/-[a-f0-9]+-[a-z0-9]+$/, "") : "unknown");

    // Retry connection with exponential backoff — relay/registry may not be ready yet.
    let connected = false;
    for (let attempt = 1; attempt <= 5; attempt++) {
      try {
        await agtMeshClient.connect({
          displayName: agtSandboxName,
          capabilities: ["azureclaw-agent", "task-execution", agtSandboxName],
        });
        log.info(`AGT mesh connected (relay: ${relayUrl}, registry: ${registryUrl})`);
        connected = true;
        agtConnected = true;
        break;
      } catch (connErr: any) {
        const delay = attempt * 2;
        if (attempt < 5) {
          log.warn(`AGT mesh connect attempt ${attempt}/5 failed: ${connErr.message} — retrying in ${delay}s`);
          await new Promise(r => setTimeout(r, delay * 1000));
        } else {
          log.warn(`AGT mesh connect failed after 5 attempts: ${connErr.message}. Mesh tools will be unavailable.`);
        }
      }
    }

    // Set up KNOCK handler — policy-gated session establishment with trust scoring.
    // Each agent evaluates the incoming KNOCK before accepting:
    // - Checks the sender's trust tier (Anonymous=500, Verified=600, Organization=700)
    // - Evaluates the requested intent against AGT policy
    // - Rejects agents below the configured trust threshold
    const AGT_TRUST_THRESHOLD = parseInt(process.env.AGT_TRUST_THRESHOLD || "0", 10); // 0 = accept all (dev)
    agtMeshClient.onKnock(async (fromAmid: string, request: any) => {
      const intent = request?.intent?.capability || '*';
      log.info(`AGT KNOCK from ${fromAmid.slice(0, 12)}... intent=${intent}`);

      // Trust score evaluation (when threshold > 0)
      if (AGT_TRUST_THRESHOLD > 0) {
        try {
          const peerInfo = await agtMeshClient.lookup(fromAmid);
          const trustScore = peerInfo?.reputationScore ?? 0;
          if (trustScore < AGT_TRUST_THRESHOLD) {
            log.warn(`AGT KNOCK rejected: ${fromAmid.slice(0, 12)} trust=${trustScore} < threshold=${AGT_TRUST_THRESHOLD}`);
            return { accept: false, reason: `trust_score_${trustScore}_below_${AGT_TRUST_THRESHOLD}` };
          }
          log.info(`AGT KNOCK trust OK: ${fromAmid.slice(0, 12)} trust=${trustScore}`);
        } catch {
          // Registry lookup failed — accept anyway for mesh agents (trust evaluation best-effort)
        }
      }

      // Policy evaluation
      if (agtPolicy && intent !== '*') {
        const decision = agtPolicy.evaluate({ action: intent });
        if (decision && !decision.allowed) {
          log.warn(`AGT KNOCK rejected by policy: ${fromAmid.slice(0, 12)} intent=${intent}`);
          return { accept: false, reason: 'policy_denied' };
        }
      }

      return { accept: true };
    });

    // Set up message handler — stores received messages in the AGT inbox buffer
    // AND auto-replies to task_request messages via AGT relay (E2E encrypted reply)
    agtMeshClient.onMessage(async (fromAmid: string, message: any) => {
      const fromName = amidToName.get(fromAmid) || fromAmid.slice(0, 12);
      const content = typeof message === "string" ? message : (message?.content || message?.text || JSON.stringify(message));
      const entry = {
        from_amid: fromAmid,
        from_agent: fromName,
        content,
        message_type: message?.type || "message",
        timestamp: new Date().toISOString(),
        id: `agt-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
      };
      agtInbox.push(entry);
      log.info(`AGT relay message from ${fromName} (${fromAmid.slice(0, 12)}...): ${JSON.stringify(content).slice(0, 200)}`);

      // Process task_request messages: delegate to native OpenClaw agent for full tool access,
      // with fallback to the limited processTaskWithTools loop if native delegation fails.
      if (message?.type === "task_request" && fromAmid && agtMeshClient) {
        const taskContent = message?.content || content;
        try {
          let llmResponse: string;
          try {
            // Primary: delegate to native OpenClaw agent (full toolset)
            llmResponse = await delegateToNativeAgent(taskContent, fromName, log);
          } catch (nativeErr: any) {
            // Fallback: limited tool-calling loop (6 tools, 10 rounds)
            log.warn(`Native delegation failed (${nativeErr.message}), falling back to processTaskWithTools`);
            llmResponse = await processTaskWithTools(taskContent, log);
          }

          // Send the response back via E2E encrypted relay
          await agtMeshClient.send(fromAmid, {
            type: "task_response",
            content: llmResponse,
            from_agent: agtSandboxName,
            in_reply_to: taskContent,
            timestamp: new Date().toISOString(),
          });
          log.info(`AGT relay: reply sent to ${fromName} via E2E encrypted relay`);
          // Submit positive reputation after successful task completion
          try {
            const sessionId = `task-${Date.now().toString(36)}`;
            await agtMeshClient.submitReputation(fromAmid, sessionId, 0.8, ["task_completed"]);
            pushTrustToRouter(fromName, 0.8);
            log.info(`AGT reputation: submitted +0.8 for ${fromName}`);
          } catch (repErr: any) { log.warn(`AGT reputation submit failed: ${repErr.message}`); }
        } catch (replyErr: any) {
          // Fallback: send error message back so parent knows what happened
          try {
            await agtMeshClient.send(fromAmid, {
              type: "task_response",
              content: `Error processing task: ${replyErr.message}`,
              from_agent: agtSandboxName,
              timestamp: new Date().toISOString(),
            });
          } catch { /* best effort */ }
          log.warn(`AGT relay: task processing failed: ${replyErr.message}`);
          // Submit negative reputation on failure
          try {
            const sessionId = `task-${Date.now().toString(36)}`;
            await agtMeshClient.submitReputation(fromAmid, sessionId, 0.3, ["task_failed"]);
            pushTrustToRouter(fromName, 0.3);
          } catch (repErr: any) { log.warn(`AGT reputation submit failed: ${repErr.message}`); }
        }
      }
    });

    // ── Disconnect handler + auto-reconnect ──────────────────────────────
    // If the WS connection drops (relay restart, network blip), try to reconnect.
    if (agtMeshClient.onDisconnect) {
      agtMeshClient.onDisconnect(() => {
        agtConnected = false;
        log.warn("AGT mesh disconnected — will attempt reconnect in 15s");
      });
    }

    // Reconnect timer: every 30s, check if disconnected and try to reconnect.
    // Also serves as a keep-alive — if the SDK exposes a ping, use it.
    if (agtReconnectTimer) clearInterval(agtReconnectTimer);
    agtReconnectTimer = setInterval(async () => {
      if (!agtConnected && agtMeshClient) {
        await agtReconnect(log);
      }
      // Heartbeat: ping the relay proxy to keep the connection warm
      try {
        const http = await import("node:http");
        const req = http.request("http://127.0.0.1:8443/agt/status", { timeout: 3000 }, () => {});
        req.on("error", () => {});
        req.end();
      } catch { /* best effort */ }
    }, 30_000);
    // Don't let the timer prevent process exit
    if (agtReconnectTimer.unref) agtReconnectTimer.unref();

    // ── Inbox notification timer ─────────────────────────────────────────
    // Every 10s, if there are unread messages, write a notification section
    // into MEMORY.md so the LLM sees them in its context window without
    // needing to manually call mesh_inbox. This is what keeps conversations
    // "lively" — the agent is proactively told it has messages to process.
    if (agtInboxNotifyTimer) clearInterval(agtInboxNotifyTimer);
    agtInboxNotifyTimer = setInterval(() => {
      notifyInboxToMemory(log).catch(() => {});
    }, 10_000);
    if (agtInboxNotifyTimer.unref) agtInboxNotifyTimer.unref();

    log.info(`AGT SDK loaded (v${sdk.VERSION}) — identity, policy, trust, audit${connected ? ", mesh ACTIVE" : ", mesh OFFLINE (relay unreachable)"}`);
    log.info("AGT timers started: reconnect (30s), inbox notify (10s)");
  } catch (e: any) {
    // Distinguish module-not-found from other errors
    const isModuleError = e.code === 'MODULE_NOT_FOUND' || e.code === 'ERR_MODULE_NOT_FOUND';
    if (isModuleError) {
      log.warn(`AGT SDK not installed: ${e.message}. Install @agentmesh/sdk to enable inter-agent communication.`);
    } else {
      log.warn(`AGT SDK init failed: ${e.message}. Stack: ${e.stack?.split('\n').slice(0, 3).join(' → ')}`);
    }
  }
}

// ---------------------------------------------------------------------------
// Module-level HTTP helper for router calls (used by initFoundry, syncToFoundryMemory)
// ---------------------------------------------------------------------------

const ROUTER_BASE = "http://127.0.0.1:8443";

async function _routerCall(method: string, path: string, body?: unknown): Promise<any> {
  const http = await import("node:http");
  const url = new URL(path, ROUTER_BASE);
  return new Promise((resolve, reject) => {
    const opts: any = {
      hostname: url.hostname,
      port: url.port,
      path: url.pathname + url.search,
      method,
      headers: { "x-azureclaw-sandbox": "self" } as Record<string, string>,
    };
    if (body) {
      opts.headers["content-type"] = "application/json";
    }
    const req = http.request(opts, (res: any) => {
      let data = "";
      res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
      res.on("end", () => {
        try { resolve(JSON.parse(data)); } catch { resolve(data); }
      });
    });
    req.on("error", reject);
    req.setTimeout(15000, () => { req.destroy(); reject(new Error("timeout")); });
    if (body) req.write(JSON.stringify(body));
    req.end();
  });
}

// ---------------------------------------------------------------------------
// Foundry project discovery — query deployments, connections, indexes at init
// ---------------------------------------------------------------------------

async function initFoundry(log: { info: (m: string) => void; warn: (m: string) => void }) {
  // Allow re-initialization per session (register() is called once per session)
  // Only guard within the same register() call to prevent double-init
  if (foundryInitialized) return;
  foundryInitialized = true;

  const apiVer = "api-version=2025-11-15-preview";
  const endpoint = process.env.FOUNDRY_PROJECT_ENDPOINT || process.env.AZURE_OPENAI_ENDPOINT || "";

  foundryProject = {
    endpoint,
    deployments: [],
    connections: [],
    indexes: [],
  };

  // Query deployed models via /openai/deployments (Foundry data plane) with /openai/models fallback,
  // plus Foundry project resources in parallel
  const [deploymentsResult, modelsResult, connResult, idxResult] = await Promise.allSettled([
    _routerCall("GET", `/v1/deployments`),
    _routerCall("GET", `/v1/models`),
    _routerCall("GET", `/connections?${apiVer}`),
    _routerCall("GET", `/indexes?${apiVer}`),
  ]);

  // Priority: 1) FOUNDRY_DEPLOYMENTS env var (from CLI discovery)
  //           2) /v1/deployments (data-plane API, works on AI Services)
  //           3) /v1/models (full catalog, always works)
  const envDeployments = process.env.FOUNDRY_DEPLOYMENTS;
  if (envDeployments) {
    try {
      const deps = JSON.parse(envDeployments);
      if (Array.isArray(deps) && deps.length > 0) {
        foundryProject.deployments = deps.map((d: any) =>
          typeof d === "string"
            ? { id: d, model: d, sku: "active" }
            : { id: d.id || d.name, model: d.model || d.id || d.name || "unknown", sku: d.sku || "active" }
        );
        log.info(`Foundry: ${foundryProject.deployments.length} deployment(s) from FOUNDRY_DEPLOYMENTS env`);
      }
    } catch { /* ignore parse error */ }
  }

  if (foundryProject.deployments.length === 0) {
    const deploymentsData = deploymentsResult.status === "fulfilled"
      ? (deploymentsResult.value?.data || deploymentsResult.value?.value || [])
      : [];
    const modelsData = modelsResult.status === "fulfilled"
      ? (modelsResult.value?.data || modelsResult.value?.value || [])
      : [];

    if (Array.isArray(deploymentsData) && deploymentsData.length > 0) {
      foundryProject.deployments = deploymentsData
        .slice(0, 50)
        .map((d: any) => ({
          id: d.id || d.name || d.deployment_id,
          model: d.model || d.properties?.model?.name || d.id || "unknown",
          sku: d.sku?.name || d.properties?.provisioningState || d.status || "active",
        }));
      log.info(`Foundry: ${foundryProject.deployments.length} deployment(s) discovered via /deployments`);
    } else if (Array.isArray(modelsData) && modelsData.length > 0) {
      // Fall back to models catalog — filter to chat-capable
      foundryProject.deployments = modelsData
        .filter((m: any) => m?.capabilities?.chat_completion || m?.capabilities?.inference || m?.id)
        .slice(0, 50)
        .map((m: any) => ({
          id: m.id || m.name,
          model: m.id || m.name || "unknown",
          sku: m.lifecycle_status || m.status || "available",
        }));
      log.info(`Foundry: ${foundryProject.deployments.length} model(s) discovered via /models catalog`);
    } else {
      log.warn(`Foundry models discovery failed: deployments=${(deploymentsResult as any).reason?.message || "empty"}, models=${(modelsResult as any).reason?.message || "empty"}`);
    }
  }

  if (connResult.status === "fulfilled") {
    const data = connResult.value?.data || connResult.value?.value || connResult.value;
    if (Array.isArray(data)) {
      foundryProject.connections = data.map((c: any) => ({
        name: c.name || c.id,
        type: c.type || c.connection_type || c.category || "unknown",
      }));
      log.info(`Foundry: ${foundryProject.connections.length} connection(s) discovered`);
    }
  }

  if (idxResult.status === "fulfilled") {
    const data = idxResult.value?.data || idxResult.value?.value || idxResult.value;
    if (Array.isArray(data)) {
      foundryProject.indexes = data.map((i: any) => ({
        name: i.name || i.id,
      }));
      log.info(`Foundry: ${foundryProject.indexes.length} search index(es) discovered`);
    }
  }

  if (foundryProject.deployments.length > 0) {
    log.info(`Foundry models: ${foundryProject.deployments.map(d => d.id).join(", ")}`);
  }

  // Write Foundry context to MEMORY.md so the agent knows what's available
  // Write to /tmp/ first, then rename — avoids triggering chokidar mid-write
  try {
    const fs = await import("node:fs");
    const memoryDir = "/sandbox/.openclaw/workspace/memory";
    const memoryFile = "/sandbox/.openclaw/workspace/MEMORY.md";
    const tmpFile = "/tmp/azureclaw-MEMORY.md";
    try { fs.mkdirSync(memoryDir, { recursive: true }); } catch { /* read-only fs */ }

    const sections: string[] = ["# AzureClaw Environment\n"];

    if (endpoint) {
      sections.push(`## Connected Foundry Project\n\nEndpoint: \`${endpoint}\`\n`);
    }

    if (foundryProject.deployments.length > 0) {
      sections.push("## Available Model Deployments\n");
      for (const d of foundryProject.deployments) {
        sections.push(`- **${d.id}** — model: ${d.model}${d.sku ? ` (${d.sku})` : ""}`);
      }
      sections.push("");
    }

    if (foundryProject.connections.length > 0) {
      sections.push("## Configured Connections\n");
      for (const c of foundryProject.connections) {
        sections.push(`- **${c.name}** — type: ${c.type}`);
      }
      sections.push("");
    }

    if (foundryProject.indexes.length > 0) {
      sections.push("## Search Indexes (RAG)\n");
      for (const i of foundryProject.indexes) {
        sections.push(`- **${i.name}**`);
      }
      sections.push("");
    }

    sections.push(
      "## Available Tools\n",
      "- `foundry_code_execute` — Python code execution (server-side, data science libraries)",
      "- `foundry_image_generation` — Generate images from text prompts (gpt-image-1)",
      "- `foundry_web_search` — Real-time web search via Bing grounding",
      "- `foundry_file_search` — RAG over vector stores + vector store CRUD + file upload",
      "- `foundry_memory` — Persistent semantic memory (cross-session, cross-agent)",
      "- `foundry_conversations` — Persistent multi-turn conversations (get/create/respond/list/delete)",
      "- `foundry_evaluations` — Model quality testing and benchmarking",
      "- `foundry_deployments` — Discover models, connections, indexes",
      "- `foundry_agents` — List Foundry-hosted agents",
      "- `http_fetch` — External HTTP via egress proxy (blocklist + allowlist enforced)",
      "- `azureclaw_spawn` / `azureclaw_mesh_send` / `azureclaw_mesh_inbox` — Multi-agent orchestration with E2E encryption",
      "",
    );

    // Write (or replace) the environment section at the top of MEMORY.md
    let existingMemory = "";
    try { existingMemory = fs.readFileSync(memoryFile, "utf8"); } catch { /* first run */ }
    const envMarker = "# AzureClaw Environment";
    const endMarker = "\n---\n";
    const envSection = sections.join("\n") + endMarker;

    let content: string;
    if (existingMemory.includes(envMarker)) {
      const start = existingMemory.indexOf(envMarker);
      const end = existingMemory.indexOf(endMarker, start);
      const before = existingMemory.slice(0, start);
      const after = end >= 0 ? existingMemory.slice(end + endMarker.length) : "";
      content = envSection + after;
    } else {
      content = envSection + existingMemory;
    }
    // Write to /tmp/ first, then atomic rename — reduces chokidar watcher churn
    fs.writeFileSync(tmpFile, content);
    try {
      fs.renameSync(tmpFile, memoryFile);
    } catch {
      // rename across filesystems fails — fall back to direct write
      fs.writeFileSync(memoryFile, content);
    }
    log.info("Foundry project context written to MEMORY.md");

    // Recall prior context from Foundry memory store on startup
    try {
      const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
      const store = `memory-${agentName}`;
      const recallResult = await _routerCall(
        "POST",
        `/memory_stores/${store}:search_memories?api-version=2025-11-15-preview`,
        { query: "key facts, user preferences, prior context, recent work", max_memories: 10 },
      );
      const memories = recallResult?.memories || recallResult?.value || [];
      if (Array.isArray(memories) && memories.length > 0) {
        const recallSection = [
          "\n## Prior Context (Foundry Memory)\n",
          "_Recalled from persistent memory store on startup:_\n",
        ];
        for (const m of memories) {
          const text = m?.content || m?.text || "";
          const kind = m?.kind || m?.type || "memory";
          if (text) recallSection.push(`- [${kind}] ${text}`);
        }
        recallSection.push("");
        // Append recall section to MEMORY.md (before the user content separator)
        let current = "";
        try { current = fs.readFileSync(memoryFile, "utf8"); } catch { /* */ }
        const recallMarker = "## Prior Context (Foundry Memory)";
        if (!current.includes(recallMarker)) {
          // Insert right before the --- separator
          const sepIdx = current.indexOf("\n---\n");
          if (sepIdx >= 0) {
            const updated = current.slice(0, sepIdx) + recallSection.join("\n") + current.slice(sepIdx);
            fs.writeFileSync(tmpFile, updated);
            try { fs.renameSync(tmpFile, memoryFile); } catch { fs.writeFileSync(memoryFile, updated); }
          }
        }
        log.info(`Foundry memory: recalled ${memories.length} memories on startup`);
      }
    } catch {
      // First boot or no memory store yet — silently skip
    }
  } catch (e: any) {
    log.warn(`Failed to write Foundry context to MEMORY.md: ${e.message}`);
  }
}

// ---------------------------------------------------------------------------
// Background Foundry Memory sync — persist conversation summaries
// ---------------------------------------------------------------------------

async function syncToFoundryMemory(
  content: string,
  log: { info: (m: string) => void; warn: (m: string) => void },
) {
  try {
    const agentName = process.env.SANDBOX_NAME || process.env.HOSTNAME || "default";
    const store = `memory-${agentName}`;
    await _routerCall("POST", `/memory_stores/${store}:update_memories?api-version=2025-11-15-preview`, {
      scope: agentName,
      items: [{ role: "assistant", content, type: "message" }],
    });
  } catch {
    // Best effort — don't disrupt agent workflow
  }
}

interface OpenClawConfig {
  [key: string]: unknown;
}

interface PluginLogger {
  info(message: string): void;
  warn(message: string): void;
  error(message: string): void;
}

interface PluginCliContext {
  program: Command;
  config: OpenClawConfig;
  workspaceDir?: string;
  logger: PluginLogger;
}

interface ProviderAuthMethod {
  type: string;
  envVar?: string;
  headerName?: string;
  label?: string;
}

interface ModelProviderEntry {
  id: string;
  label: string;
  contextWindow?: number;
  maxOutput?: number;
}

interface ProviderPlugin {
  id: string;
  label: string;
  docsPath?: string;
  aliases?: string[];
  envVars?: string[];
  models?: {
    chat?: ModelProviderEntry[];
  };
  auth: ProviderAuthMethod[];
}

interface PluginCommandDefinition {
  name: string;
  description: string;
  acceptsArgs?: boolean;
  requireAuth?: boolean;
  handler: (ctx: { args?: string; channel?: string; config: OpenClawConfig }) => Promise<{ text: string }> | { text: string };
}

interface ToolDefinition {
  name: string;
  label: string;
  description: string;
  parameters?: Record<string, unknown>;
  execute: (toolCallId: string, params: Record<string, unknown>, signal?: AbortSignal) => Promise<{ content: Array<{ type: string; text: string }>; details?: unknown }>;
}

interface OpenClawPluginApi {
  id: string;
  name: string;
  version?: string;
  config: OpenClawConfig;
  pluginConfig?: Record<string, unknown>;
  logger: PluginLogger;
  registerCommand: (command: PluginCommandDefinition) => void;
  registerCli: (registrar: (ctx: PluginCliContext) => void, opts?: { commands?: string[] }) => void;
  registerProvider: (provider: ProviderPlugin) => void;
  registerTool: (tool: ToolDefinition) => void;
  resolvePath: (input: string) => string;
}

// ---------------------------------------------------------------------------
// Plugin config
// ---------------------------------------------------------------------------

interface AzureClawConfig {
  endpoint: string;
  model: string;
  sandboxName: string;
}

const DEFAULT_CONFIG: AzureClawConfig = {
  endpoint: "",
  model: "gpt-4.1",
  sandboxName: "dev-agent",
};

function getPluginConfig(api: OpenClawPluginApi): AzureClawConfig {
  const raw = api.pluginConfig ?? {};
  return {
    endpoint: (raw.endpoint as string) || DEFAULT_CONFIG.endpoint,
    model: (raw.model as string) || DEFAULT_CONFIG.model,
    sandboxName: (raw.sandboxName as string) || DEFAULT_CONFIG.sandboxName,
  };
}

// ---------------------------------------------------------------------------
// Plugin object (OpenClaw expects: { id, name, description, configSchema, register })
// ---------------------------------------------------------------------------

const azureClawPlugin = definePluginEntry({
  id: "azureclaw",
  name: "AzureClaw",
  description: "Secure AI agent runtime on Azure — Azure OpenAI provider, agent tools, and sandbox CLI",
  configSchema: {
    type: "object" as const,
    additionalProperties: false,
    properties: {
      endpoint: {
        type: "string",
        description: "Azure OpenAI endpoint URL (e.g. https://my-resource.openai.azure.com)",
      },
      model: {
        type: "string",
        description: "Default model deployment name (default: gpt-4.1)",
      },
      sandboxName: {
        type: "string",
        description: "Docker sandbox container name (default: dev-agent)",
      },
    },
  },

  register(api: OpenClawPluginApi): void {
    const config = getPluginConfig(api);
    const log = api.logger;

    // ── Startup banner ─────────────────────────────────────────────────
    const foundryEndpoint = process.env.FOUNDRY_PROJECT_ENDPOINT || process.env.AZURE_OPENAI_ENDPOINT || "";
    const projectName = foundryEndpoint
      ? foundryEndpoint.replace(/^https?:\/\//, "").replace(/\..*$/, "")
      : "direct";
    const sandbox = process.env.SANDBOX_NAME || process.env.HOSTNAME || "local";

    log.info([
      "",
      "  ╔══════════════════════════════════════════════════════════╗",
      "  ║  🔒 AzureClaw — Secure AI Agent Runtime                 ║",
      "  ╠══════════════════════════════════════════════════════════╣",
      `  ║  Sandbox:  ${(sandbox).padEnd(43)}║`,
      `  ║  Model:    ${(config.model).padEnd(43)}║`,
      `  ║  Foundry:  ${(projectName).padEnd(43)}║`,
      "  ║                                                          ║",
      "  ║  Security: kata-vm · seccomp · rootfs-ro · uid-guard     ║",
      "  ║  Egress:   blocklist · allowlist · learn · pending        ║",
      "  ║  Comms:    Signal Protocol E2E · AGT mesh                 ║",
      "  ╚══════════════════════════════════════════════════════════╝",
      "",
    ].join("\n"));

    // Reset per-session initialization guards so new sessions rediscover state
    foundryInitialized = false;

    // Initialize AGT SDK (identity, policy, trust, audit, mesh)
    initAGT(log).catch((e: any) => log.warn(`AGT init error: ${e.message}`));

    // Initialize Foundry project discovery (models, connections, indexes)
    initFoundry(log).catch((e: any) => log.warn(`Foundry init error: ${e.message}`));

    // ── Register AzureClaw agent tools (spawn, mesh, status, destroy) ────
    // These are first-class tools the LLM can call directly.
    // Registered as required tools (always available, no tools.allow needed).
    // API: execute(_id, params) → { content: [{ type: "text", text }] }

    const ROUTER = "http://127.0.0.1:8443";
    async function routerCall(method: string, path: string, body?: unknown, extraHeaders?: Record<string, string>): Promise<any> {
      const http = await import("node:http");
      const url = `${ROUTER}${path}`;
      return new Promise((resolve, reject) => {
        const opts: any = {
          method,
          timeout: 15000,
          headers: { "x-azureclaw-sandbox": process.env.SANDBOX_NAME || "self", ...extraHeaders } as Record<string, string>,
        };
        if (body) opts.headers["Content-Type"] = "application/json";
        let settled = false;
        const settle = (fn: () => void) => { if (!settled) { settled = true; fn(); } };
        const req = http.request(url, opts, (res: any) => {
          let data = "";
          const maxLen = 64 * 1024; // 64 KB safety cap
          // Must handle response stream errors (e.g. from req.destroy() on timeout)
          res.on("error", () => {});
          res.on("data", (c: Buffer) => {
            if (data.length < maxLen) data += c.toString();
          });
          res.on("end", () => {
            if (res.statusCode && res.statusCode >= 400) {
              settle(() => reject(new Error(`HTTP ${res.statusCode}: ${data.slice(0, 500)}`)));
              return;
            }
            settle(() => { try { resolve(JSON.parse(data)); } catch { resolve({ raw: data.slice(0, 2000) }); } });
          });
        });
        req.on("error", (e: Error) => settle(() => reject(e)));
        req.setTimeout(15000, () => { req.destroy(); settle(() => reject(new Error("timeout"))); });
        if (body) req.write(JSON.stringify(body));
        req.end();
      });
    }

    // Safe JSON response for tool output — truncate to avoid blowing WebSocket frames
    function safeJson(obj: unknown, maxLen = 8000): string {
      try {
        const s = JSON.stringify(obj, null, 2);
        return s.length > maxLen ? s.slice(0, maxLen) + "\n...(truncated)" : s;
      } catch {
        return String(obj).slice(0, maxLen);
      }
    }

    // ── Register AzureClaw agent tools (spawn, mesh, status, destroy) ────
    // These are first-class tools the LLM can call directly.
    // Registered as required tools (always available, no tools.allow needed).
    // API: execute(_id, params) → { content: [{ type: "text", text }] }

    api.registerTool({
      name: "azureclaw_spawn",
      label: "Spawn Sub-Agent",
      description: "Spawn a secure isolated sub-agent on AKS with E2E encrypted communication (Signal Protocol). The sub-agent automatically connects to the AGT relay mesh for encrypted inter-agent messaging — no special configuration needed. Use azureclaw_mesh_send to communicate and azureclaw_mesh_inbox to receive replies.",
      parameters: {
        type: "object",
        properties: {
          name: { type: "string", description: "DNS-safe name for the sub-agent (lowercase alphanumeric + hyphens, e.g. 'auditor', 'analyst')" },
          model: { type: "string", description: "AI model deployment (default: gpt-4.1)" },
          governance: { type: "boolean", description: "Enable AGT governance + mesh communication (default: true)" },
        },
        required: ["name"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const result = await routerCall("POST", "/sandbox/spawn", {
            name: params.name,
            model: params.model || "gpt-4.1",
            governance: params.governance !== false,
            trust_threshold: 500,
          });

          // Auto-wait until the sub-agent is Running (poll spawn_status)
          const agentName = params.name as string;
          log.info(`Waiting for sub-agent '${agentName}' to reach Running state...`);
          let phase = "Pending";
          for (let i = 0; i < 24; i++) { // 24 × 5s = 120s max
            await new Promise(r => setTimeout(r, 5000));
            try {
              const status = await routerCall("GET", `/sandbox/${encodeURIComponent(agentName)}`);
              phase = status?.phase || "Pending";
              log.info(`Sub-agent '${agentName}' phase: ${phase} (${i + 1}/24)`);
              if (phase === "Running") break;
            } catch {
              // Status endpoint not ready yet — keep polling
            }
          }

          if (phase !== "Running") {
            return { content: [{ type: "text", text: JSON.stringify({
              ...result,
              warning: `Sub-agent created but not yet Running (phase: ${phase}). It may still be booting. Use azureclaw_spawn_status to check.`,
            }, null, 2) }] };
          }

          // Give the sub-agent a few more seconds to register with the AGT relay
          await new Promise(r => setTimeout(r, 3000));

          // Pre-discover the sub-agent's AMID so mesh_send doesn't need to search
          if (agtMeshClient) {
            try {
              const searchResult = await routerCall("GET",
                `/agt/registry/registry/search?capability=${encodeURIComponent(agentName)}`);
              const agents = searchResult?.results || [];
              const match = agents.find((a: any) =>
                a.display_name === agentName || a.capabilities?.includes(agentName)
              );
              if (match?.amid) {
                nameToAmid.set(agentName, match.amid);
                amidToName.set(match.amid, agentName);
                log.info(`AGT pre-discovery: cached AMID for '${agentName}' (${match.amid.slice(0, 12)}...)`);
              }
            } catch { /* best effort — mesh_send will retry */ }
          }

          return { content: [{ type: "text", text: JSON.stringify({
            ...result,
            phase: "Running",
            message: `Sub-agent '${agentName}' is Running and ready for mesh communication. Use azureclaw_mesh_send to send it a task.`,
          }, null, 2) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Spawn failed: ${e.message}` }] };
        }
      },
    });

    api.registerTool({
      name: "azureclaw_spawn_status",
      label: "Sub-Agent Status",
      description: "Check the status of a spawned sub-agent. Returns phase (Pending/Running/Terminating), namespace, and readiness. Poll this after spawning until phase is 'Running' before sending mesh messages.",
      parameters: {
        type: "object",
        properties: {
          name: { type: "string", description: "Name of the sub-agent to check" },
        },
        required: ["name"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const result = await routerCall("GET", `/sandbox/${encodeURIComponent(params.name as string)}/status`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Status check failed: ${e.message}` }] };
        }
      },
    });

    api.registerTool({
      name: "azureclaw_mesh_send",
      label: "Send Mesh Task",
      description: "Send a task to a sub-agent via AGT mesh (E2E encrypted relay). Automatically waits up to 5.5 minutes for the sub-agent's reply and returns it inline. If no reply arrives within the wait window, check azureclaw_mesh_inbox later. Wait for the sub-agent to be Running first.",
      parameters: {
        type: "object",
        properties: {
          to_agent: { type: "string", description: "Name of the target sub-agent" },
          content: { type: "string", description: "Task description or message to send" },
        },
        required: ["to_agent", "content"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        const agentName = params.to_agent as string;
        const msgContent = params.content as string;

        // ── Primary path: AGT SDK relay (E2E encrypted) ──
        if (agtMeshClient && agtIdentity) {
          // Ensure we're connected (reconnect if initial connect was deferred)
          if (!agtMeshClient.isConnected) {
            try {
              log.info("AGT relay: reconnecting before send...");
              await agtMeshClient.connect({
                displayName: agtSandboxName,
                capabilities: ["azureclaw-agent", "task-execution", agtSandboxName],
              });
              log.info("AGT relay: reconnected successfully");
            } catch (reconErr: any) {
              log.warn(`AGT relay: reconnect failed: ${reconErr.message}`);
            }
          }
          try {
            // 1. Discover target agent's AMID via registry search (with retry for boot timing)
            let targetAmid = nameToAmid.get(agentName);
            if (!targetAmid) {
              for (let attempt = 0; attempt < 12 && !targetAmid; attempt++) {
                if (attempt > 0) {
                  log.info(`AGT relay: waiting for '${agentName}' to register (${attempt}/11)...`);
                  await new Promise(r => setTimeout(r, 2000));
                }

                // Try direct registry HTTP search by capability (most reliable)
                try {
                  const http = await import("node:http");
                  const regResult: any = await new Promise((resolve, reject) => {
                    const req = http.get(
                      `http://127.0.0.1:8443/agt/registry/registry/search?capability=${encodeURIComponent(agentName)}`,
                      (res: any) => {
                        let data = "";
                        res.on("data", (c: Buffer) => { data += c.toString(); });
                        res.on("end", () => { try { resolve(JSON.parse(data)); } catch { resolve(null); } });
                      },
                    );
                    req.on("error", reject);
                    req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
                  });
                  if (regResult && Array.isArray(regResult.results) && regResult.results.length > 0) {
                    const match = regResult.results.find((a: any) =>
                      a.display_name === agentName || a.capabilities?.includes(agentName)
                    ) || regResult.results[0];
                    targetAmid = match.amid;
                  }
                } catch (regErr: any) {
                  if (attempt === 0) log.warn(`AGT registry search: ${regErr.message}`);
                }
              }

              if (targetAmid) {
                nameToAmid.set(agentName, targetAmid);
                amidToName.set(targetAmid, agentName);
              }
            }

            if (targetAmid) {
              // 2. Send via AGT relay (E2E encrypted, Signal Protocol)
              // Retry loop: target may need time to upload prekeys after registering
              let sendErr: Error | null = null;
              for (let sendAttempt = 0; sendAttempt < 8; sendAttempt++) {
                try {
                  await agtMeshClient.send(targetAmid, {
                    type: "task_request",
                    content: msgContent,
                    from_agent: process.env.SANDBOX_NAME || "unknown",
                    timestamp: new Date().toISOString(),
                  });
                  sendErr = null;
                  break;
                } catch (e: any) {
                  sendErr = e;
                  if (e.message?.includes("prekeys") || e.message?.includes("prekey")) {
                    log.info(`AGT relay: waiting for prekeys from '${agentName}' (${sendAttempt + 1}/8)...`);
                    await new Promise(r => setTimeout(r, 2000));
                  } else {
                    break; // non-prekey error — don't retry
                  }
                }
              }
              if (!sendErr) {
                log.info(`AGT relay: sent to ${agentName} (${targetAmid.slice(0, 12)}...) via E2E encrypted relay`);
                const messageId = `agt-${Date.now().toString(36)}`;

                // Auto-wait for reply: poll agtInbox for a response from this agent
                const waitMaxMs = 60_000; // 60 seconds — prevents blocking the agent loop too long
                const pollIntervalMs = 500; // 500ms — fast polling for responsive feel
                const waitStart = Date.now();
                let replyContent: string | null = null;
                log.info(`AGT relay: waiting up to ${waitMaxMs / 1000}s for reply from '${agentName}'...`);

                while (Date.now() - waitStart < waitMaxMs) {
                  // Check inbox for a reply from this target
                  const replyIdx = agtInbox.findIndex(
                    (m) => m.from_amid === targetAmid || m.from_agent === agentName,
                  );
                  if (replyIdx >= 0) {
                    const reply = agtInbox.splice(replyIdx, 1)[0];
                    replyContent = typeof reply.content === "string"
                      ? reply.content
                      : JSON.stringify(reply.content);
                    log.info(`AGT relay: got reply from '${agentName}' after ${((Date.now() - waitStart) / 1000).toFixed(1)}s`);
                    break;
                  }
                  await new Promise((r) => setTimeout(r, pollIntervalMs));
                }

                const result: any = {
                  status: replyContent ? "delivered_and_replied" : "delivered_via_agt_relay",
                  to_agent: agentName,
                  to_amid: targetAmid,
                  from_amid: agtIdentity.amid,
                  protocol: "AGT E2E encrypted (Signal Protocol)",
                  message_id: messageId,
                };
                if (replyContent) {
                  result.reply = replyContent;
                  // Submit positive reputation — peer completed the task
                  try {
                    await agtMeshClient.submitReputation(targetAmid, messageId, 0.9, ["task_responded"]);
                    pushTrustToRouter(agentName, 0.9);
                    log.info(`AGT reputation: submitted +0.9 for '${agentName}'`);
                  } catch (repErr: any) { log.warn(`AGT reputation submit failed: ${repErr.message}`); }
                } else {
                  result.note = "No reply within timeout — use azureclaw_mesh_inbox to check later.";
                }
                return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
              }
              log.warn(`AGT relay send failed after retries: ${sendErr?.message}, falling back to router`);
            } else {
              log.warn(`AGT relay: target '${agentName}' not found in registry, falling back to router`);
            }
          } catch (agtErr: any) {
            log.warn(`AGT relay send failed: ${agtErr.message}, falling back to router`);
          }
        }

        // ── Fallback: router HTTP (K8s DNS, no encryption) ──
        try {
          const result = await routerCall("POST", "/agt/mesh/send", {
            to_agent: agentName,
            content: msgContent,
            type: "task_request",
          });
          return { content: [{ type: "text", text: JSON.stringify({
            ...result,
            protocol: "router_http_fallback (no E2E encryption)",
          }, null, 2) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Mesh send failed: ${e.message}` }] };
        }
      },
    });

    api.registerTool({
      name: "azureclaw_mesh_inbox",
      label: "Check Mesh Inbox",
      description: "Check your AGT mesh inbox for responses from sub-agents. Returns messages received via the E2E encrypted AGT relay and any router-level messages.",
      parameters: { type: "object", properties: {} },
      async execute(_id: string, _params: Record<string, unknown>) {
        try {
          // Collect messages from both sources
          const agtMessages = agtInbox.splice(0, agtInbox.length); // drain AGT buffer

          // Clear the MEMORY.md inbox notification since we've drained messages
          if (agtMessages.length > 0) {
            try {
              const fs = await import("node:fs/promises");
              const memPath = process.env.MEMORY_FILE_PATH || "/home/user/MEMORY.md";
              const INBOX_MARKER = "<!-- AGT_INBOX_START -->";
              const INBOX_END = "<!-- AGT_INBOX_END -->";
              let mem = await fs.readFile(memPath, "utf-8");
              if (mem.includes(INBOX_MARKER)) {
                const re = new RegExp(`\\n*${INBOX_MARKER}[\\s\\S]*?${INBOX_END}\\n*`, "m");
                mem = mem.replace(re, "\n");
                await fs.writeFile(memPath, mem, "utf-8");
              }
            } catch { /* best effort */ }
          }

          // Also get any router-level messages (fallback / auto-reply)
          let routerMessages: any[] = [];
          try {
            const routerResult = await routerCall("GET", "/agt/mesh/inbox");
            routerMessages = routerResult.messages || [];
          } catch {
            // Router inbox unavailable — use AGT only
          }

          // Merge and deduplicate (prefer AGT source)
          const allMessages = [
            ...agtMessages.map((m: any) => ({ ...m, source: "agt_relay_e2e" })),
            ...routerMessages.map((m: any) => ({ ...m, source: "router_http" })),
          ];

          return { content: [{ type: "text", text: JSON.stringify({
            count: allMessages.length,
            agt_relay_count: agtMessages.length,
            router_count: routerMessages.length,
            messages: allMessages,
          }, null, 2) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Inbox check failed: ${e.message}` }] };
        }
      },
    });

    api.registerTool({
      name: "azureclaw_spawn_destroy",
      label: "Destroy Sub-Agent",
      description: "Destroy a spawned sub-agent sandbox. Tears down the K8s namespace, deployment, and all resources. Use this to clean up after the sub-agent has completed its task.",
      parameters: {
        type: "object",
        properties: {
          name: { type: "string", description: "Name of the sub-agent to destroy" },
        },
        required: ["name"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const result = await routerCall("DELETE", `/sandbox/${encodeURIComponent(params.name as string)}`);
          return { content: [{ type: "text", text: safeJson(result) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Destroy failed: ${e.message}` }] };
        }
      },
    });

    api.registerTool({
      name: "azureclaw_spawn_list",
      label: "List Sub-Agents",
      description: "List all sub-agents spawned from this sandbox. Returns name, phase, model, and governance status for each.",
      parameters: { type: "object", properties: {} },
      async execute(_id: string, _params: Record<string, unknown>) {
        try {
          const result = await routerCall("GET", "/sandbox/list");
          return { content: [{ type: "text", text: safeJson(result) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `List failed: ${e.message}` }] };
        }
      },
    });

    api.registerTool({
      name: "azureclaw_discover",
      label: "Discover Agents",
      description: "Search the AgentMesh registry for other agents by name or capability. Returns their AMID, display name, tier, capabilities, and reputation score. Use this to find agents to communicate with via azureclaw_mesh_send or azureclaw_relay.",
      parameters: {
        type: "object",
        properties: {
          query: { type: "string", description: "Agent name or capability to search for. Use '*' to list all known agents." },
        },
        required: ["query"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        const query = (params.query as string) || "*";
        try {
          const searchUrl = query === "*"
            ? "/agt/registry/registry/search?capability=azureclaw-agent"
            : `/agt/registry/registry/search?capability=${encodeURIComponent(query)}`;
          const result = await routerCall("GET", searchUrl);
          const agents = (result as any)?.results || [];
          const summary = agents.map((a: any) => ({
            amid: a.amid,
            name: a.display_name,
            tier: a.tier,
            capabilities: a.capabilities,
            reputation: a.reputation_score,
            status: a.status,
            last_seen: a.last_seen,
          }));
          return { content: [{ type: "text", text: safeJson({ agents: summary, count: summary.length }) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Discovery failed: ${e.message}` }] };
        }
      },
    });

    log.info("AzureClaw agent tools registered: azureclaw_spawn, azureclaw_spawn_status, azureclaw_mesh_send, azureclaw_mesh_inbox, azureclaw_spawn_destroy, azureclaw_spawn_list, azureclaw_discover, http_fetch");

    // ── http_fetch: routed through the inference router's egress proxy ──
    // The sandbox (UID 1000) cannot reach the internet directly (iptables).
    // This tool routes requests through the router (UID 1001) which enforces
    // blocklist, allowlist, and learn mode before proxying the request.
    api.registerTool({
      name: "http_fetch",
      label: "HTTP Fetch (Egress Proxy)",
      description:
        "Make an HTTP request to an external URL. The request is routed through the AzureClaw security proxy which enforces blocklist (51K+ malicious domains blocked), allowlist, and learn mode. Use this for ANY external API call (Telegram, HackerNews, web APIs, etc.). Direct internet access via curl/fetch is blocked by the egress guard.",
      parameters: {
        type: "object",
        properties: {
          url: { type: "string", description: "The full URL to fetch (e.g., https://api.telegram.org/bot.../getMe)" },
          method: { type: "string", description: "HTTP method: GET, POST, PUT, DELETE. Default: GET" },
          headers: { type: "object", description: "Optional HTTP headers as key-value pairs" },
          body: { type: "string", description: "Optional request body (for POST/PUT)" },
        },
        required: ["url"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const result = await routerCall("POST", "/egress/fetch", {
            url: params.url,
            method: (params.method as string) || "GET",
            headers: params.headers || {},
            body: params.body || undefined,
          });
          return { content: [{ type: "text", text: safeJson(result) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Fetch failed: ${e.message}` }] };
        }
      },
    });

    // ── Foundry Code Interpreter: server-side Python execution ──────────
    // Runs in Azure AI Foundry's managed sandbox with pre-installed data
    // science libraries (pandas, numpy, matplotlib, etc.). No egress needed.
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
        "Uses gpt-image-1 model. Returns base64-encoded image data. Use when the user " +
        "asks to create, draw, or generate an image, diagram, or visual.",
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
          model: {
            type: "string",
            description: "Orchestrator model (default: gpt-4.1). Coordinates the image generation.",
          },
        },
        required: ["prompt"],
      },
      async execute(_id: string, params: Record<string, unknown>) {
        try {
          const imgModel = "gpt-image-1";
          const quality = (params.quality as string) || "medium";
          const size = (params.size as string) || "1024x1024";
          const result = await routerCall("POST", "/openai/responses?api-version=2025-11-15-preview", {
            model: (params.model as string) || "gpt-4.1",
            input: params.prompt,
            tools: [{ type: "image_generation", image_generation: { model: imgModel, quality, size } }],
            store: false,
          }, { "x-ms-oai-image-generation-deployment": imgModel });
          const output = result.output || result;
          const parts: string[] = [];
          if (Array.isArray(output)) {
            for (const item of output) {
              if (item.type === "image_generation_call" && item.result) {
                parts.push(`[Generated image — base64 data ${item.result.length} chars]`);
              } else if (item.type === "message" && item.content) {
                for (const c of item.content) {
                  if (c.type === "output_text" || c.type === "text") parts.push(c.text);
                }
              }
            }
          }
          return { content: [{ type: "text", text: parts.length > 0 ? parts.join("\n\n") : safeJson(output) }] };
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
                const embeddingModel = foundryProject?.deployments?.find(
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
            const result = await routerCall("POST", `/memory_stores/${store}:delete_scope?${apiVer}`, { scope });
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
            // Priority: 1) pre-discovered deployments (from FOUNDRY_DEPLOYMENTS env / initFoundry)
            //           2) /v1/deployments data-plane (usually 404 — no Azure API)
            //           3) /v1/models catalog fallback
            if (foundryProject?.deployments && foundryProject.deployments.length > 0) {
              return { content: [{ type: "text", text: safeJson({
                source: "discovered_deployments",
                total: foundryProject.deployments.length,
                models: foundryProject.deployments,
              }) }] };
            }

            let deployments: any[] = [];
            let source = "deployments";
            try {
              const depResult = await routerCall("GET", "/v1/deployments");
              deployments = depResult?.data || depResult?.value || [];
            } catch { /* fall through to models */ }

            if (!Array.isArray(deployments) || deployments.length === 0) {
              source = "models_catalog";
              const result = await routerCall("GET", "/v1/models");
              deployments = result?.data || result?.value || [];
            }

            const models = Array.isArray(deployments) ? deployments.map((m: any) => ({
              id: m.id || m.name || m.deployment_id,
              model: m.model || m.properties?.model?.name || m.id,
              status: m.status || m.properties?.provisioningState || m.lifecycle_status || "active",
              sku: m.sku?.name || "",
            })) : [];

            return { content: [{ type: "text", text: safeJson({
              source,
              total: models.length,
              models,
            }) }] };
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

    log.info("Foundry tools registered: foundry_code_execute, foundry_image_generation, foundry_web_search, foundry_file_search, foundry_memory, foundry_conversations, foundry_evaluations, foundry_deployments, foundry_agents");

    // ── Register Azure AI Foundry as a model provider ───────────────────
    // Use dynamically discovered deployments when available, fall back to defaults
    const defaultModels = [
      { id: "gpt-4.1", label: "GPT-4.1 (Azure)", contextWindow: 1047576, maxOutput: 32768 },
      { id: "gpt-5-mini", label: "GPT-5 Mini (Azure)", contextWindow: 1047576, maxOutput: 32768 },
      { id: "gpt-4o", label: "GPT-4o (Azure)", contextWindow: 128000, maxOutput: 16384 },
      { id: "DeepSeek-V3.2", label: "DeepSeek V3.2 (Foundry)", contextWindow: 131072, maxOutput: 8192 },
      { id: "Phi-4", label: "Phi-4 (Microsoft)", contextWindow: 16384, maxOutput: 16384 },
      { id: "Meta-Llama-3.1-405B-Instruct", label: "Llama 3.1 405B (Meta)", contextWindow: 131072, maxOutput: 8192 },
      { id: "o3-mini", label: "o3-mini (Azure)", contextWindow: 200000, maxOutput: 100000 },
    ];

    // If Foundry discovery populated deployments, build models from those
    const chatModels = (foundryProject?.deployments?.length)
      ? foundryProject.deployments.map(d => ({
          id: d.id,
          label: `${d.model || d.id} (Azure Foundry)`,
          contextWindow: 128000,
          maxOutput: 16384,
        }))
      : defaultModels;

    api.registerProvider({
      id: "azure-openai",
      label: "Azure AI Foundry (via AzureClaw)",
      docsPath: "https://github.com/Azure/azureclaw",
      aliases: ["azure", "azureclaw", "foundry"],
      envVars: ["AZURE_OPENAI_API_KEY"],
      models: { chat: chatModels },
      auth: [
        {
          type: "api-key",
          envVar: "AZURE_OPENAI_API_KEY",
          headerName: "api-key",
          label: "Azure API Key (or 'routed-via-inference-router' for AzureClaw)",
        },
      ],
    });

    // ── Register CLI subcommands: openclaw azureclaw <cmd> ────────────────
    api.registerCli(
      (ctx: PluginCliContext) => {
        const azureclaw = ctx.program
          .command("azureclaw")
          .description("AzureClaw — secure AI agent runtime on Azure");

        azureclaw
          .command("status")
          .description("Show sandbox health, security, and inference metrics")
          .action(async () => {
            const http = await import("node:http");
            try {
              const body = await new Promise<string>((resolve, reject) => {
                const req = http.get("http://127.0.0.1:8443/metrics", (res) => {
                  let data = "";
                  res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                  res.on("end", () => resolve(data));
                });
                req.on("error", reject);
                req.setTimeout(3000, () => { req.destroy(); reject(new Error("timeout")); });
              });
              console.log("AzureClaw Inference Router — Prometheus Metrics\n");
              console.log(body);
            } catch {
              console.log("AzureClaw Inference Router: not reachable (http://127.0.0.1:8443/metrics)");
            }
          });

        azureclaw
          .command("connect")
          .description("Connect to the sandbox (host-side only)")
          .action(async () => {
            console.log("'connect' is a host-side command. Inside the sandbox, you're already connected.");
            console.log("From the host, run: azureclaw connect");
          });

        azureclaw
          .command("dev")
          .description("Start a local sandbox (host-side only)")
          .action(async () => {
            console.log("'dev' is a host-side command. This sandbox is already running.");
            console.log("From the host, run: azureclaw dev");
          });

        azureclaw
          .command("logs")
          .option("-f, --follow", "Follow log output")
          .description("Stream sandbox logs (host-side only)")
          .action(async () => {
            console.log("'logs' is a host-side command.");
            console.log("From the host, run: azureclaw logs");
          });
      },
      { commands: ["azureclaw"] }
    );

    // ── Register /azureclaw slash command ─────────────────────────────────
    api.registerCommand({
      name: "azureclaw",
      description: "Show AzureClaw sandbox status, models, and security info",
      handler: async () => {
        return {
          text: [
            "**AzureClaw Sandbox** (Foundry-integrated)",
            `Model: ${config.model}`,
            `Sandbox: ${config.sandboxName}`,
            `Endpoint: ${config.endpoint || "(configured via Foundry)"}`,
            "",
            "**Slash Commands:**",
            "- `/azureclaw` — this help",
            "- `/azureclaw-models` — list available Foundry models",
            "- `/azureclaw-switch <model>` — switch AI model live",
            "- `/azureclaw-agents` — list Foundry agents",
            "- `/azureclaw-memory <agent-id>` — view agent memory (threads)",
            "- `/azureclaw-security` — show isolation level + security posture",
            "",
            "**CLI Commands (from host):**",
            "- `azureclaw model list foundry-agent` — live model catalog",
            "- `azureclaw model set foundry-agent Phi-4` — switch model",
            "- `azureclaw policy get foundry-agent` — show network policy",
            "- `azureclaw approve --list` — pending egress requests",
            "- `azureclaw trace foundry-agent --exec` — eBPF tracing",
          ].join("\n"),
        };
      },
    });

    // ── /azureclaw-models — list available models from Foundry ────────────
    api.registerCommand({
      name: "azureclaw-models",
      description: "List available AI models from Azure Foundry",
      handler: async () => {
        try {
          const http = await import("node:http");
          // Query actual Foundry deployments (not the full catalog)
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(
              "http://127.0.0.1:8443/deployments?api-version=2025-11-15-preview",
              { headers: { "x-azureclaw-sandbox": "self" } },
              (res) => {
                let data = "";
                res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                res.on("end", () => resolve(data));
              },
            );
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          const deployments = parsed.data || parsed.value || [];
          const lines = deployments.map((d: any) => {
            const name = d.id || d.name || "?";
            const model = d.model?.name || d.model || d.properties?.model?.name || "";
            const status = d.status || d.properties?.provisioningState || "?";
            return `  - **${name}**${model ? ` (${model})` : ""} — ${status}`;
          });
          return {
            text: [
              `**Foundry Deployments** (${deployments.length})`,
              "",
              ...lines,
              "",
              "Switch with: `/azureclaw-switch <model>`",
            ].join("\n"),
          };
        } catch {
          return { text: "Could not query deployments. Is the inference router running?" };
        }
      },
    });

    // ── /azureclaw-switch — switch model live ─────────────────────────────
    api.registerCommand({
      name: "azureclaw-switch",
      description: "Switch AI model (e.g. /azureclaw-switch gpt-5-mini)",
      acceptsArgs: true,
      handler: async (ctx) => {
        const model = ctx.args?.trim();
        if (!model) {
          const available = foundryProject?.deployments?.map(d => d.id).join(", ") || "unknown";
          return { text: `Usage: /azureclaw-switch <model-name>\nAvailable: ${available}` };
        }
        // Update plugin env + inference router
        process.env.OPENCLAW_MODEL = model;
        config.model = model;

        // Update OpenClaw's model config files so session_status accepts the new model
        try {
          const fs = await import("node:fs");
          const modelsPath = "/sandbox/.openclaw/agents/main/agent/models.json";
          const oclawPath = "/sandbox/.openclaw/openclaw.json";
          const sandbox = process.env.HOSTNAME || "dev-agent";

          // Build models array with all deployments + the new model
          const allModels = new Set<string>();
          allModels.add(model);
          if (foundryProject?.deployments) {
            for (const d of foundryProject.deployments) {
              if (!d.id.includes("embedding")) allModels.add(d.id);
            }
          }
          const modelsArr = [...allModels].map(id => ({
            id, name: `${id} (Azure via AzureClaw)`, reasoning: false,
            input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
            contextWindow: 200000, maxTokens: 8192, api: "openai-completions",
          }));

          // Update models.json
          try {
            const mj = JSON.parse(fs.readFileSync(modelsPath, "utf8"));
            if (mj.providers?.["azure-openai"]) {
              mj.providers["azure-openai"].models = modelsArr;
            }
            fs.writeFileSync(modelsPath, JSON.stringify(mj, null, 2));
          } catch { /* non-critical */ }

          // Update openclaw.json — both models list and default primary
          try {
            const oc = JSON.parse(fs.readFileSync(oclawPath, "utf8"));
            if (oc.models?.providers?.["azure-openai"]) {
              oc.models.providers["azure-openai"].models = modelsArr.map(m => ({ id: m.id, name: m.name }));
            }
            if (oc.agents?.defaults?.model) {
              oc.agents.defaults.model.primary = `azure-openai/${model}`;
            }
            fs.writeFileSync(oclawPath, JSON.stringify(oc, null, 2));
          } catch { /* non-critical */ }
        } catch { /* read-only fs — skip config update */ }

        try {
          const result = await routerCall("PUT", "/admin/model", { model });
          const prev = (result as any)?.previous || "unknown";
          return { text: `Switched from **${prev}** → **${model}**. Takes effect on next request.` };
        } catch {
          return { text: `Plugin updated to **${model}**, but router admin endpoint not available. Model may need a restart.` };
        }
      },
    });

    // ── /azureclaw-security — show security posture ───────────────────────
    api.registerCommand({
      name: "azureclaw-security",
      description: "Show sandbox security posture",
      handler: async () => {
        const uname = await import("node:child_process");
        let kernel = "unknown";
        let user = "unknown";
        try {
          kernel = uname.execSync("uname -r", { encoding: "utf-8" }).trim();
          user = uname.execSync("whoami", { encoding: "utf-8" }).trim();
        } catch {}

        const isKata = kernel.includes("mshv");
        return {
          text: [
            "**AzureClaw Security Posture**",
            "",
            `Kernel: ${kernel}`,
            `User: ${user}`,
            `Isolation: ${isKata ? "confidential (Kata VM)" : "enhanced (runc + seccomp)"}`,
            `Root filesystem: read-only`,
            `Capabilities: ALL dropped`,
            `Seccomp: ${isKata ? "RuntimeDefault (VM boundary)" : "Localhost (azureclaw-strict)"}`,
            `Network: default-deny egress + iptables UID guard`,
            `Inference: routed through AzureClaw inference router`,
            `Foundry Agent API: proxied via localhost:8443/agents/*`,
            `Auth: IMDS (kubelet MI, zero keys)`,
          ].join("\n"),
        };
      },
    });

    // ── /azureclaw-agt — AGT governance status + policy evaluation ────────
    api.registerCommand({
      name: "azureclaw-agt",
      description: "AGT governance status. /azureclaw-agt check <action> to evaluate policy",
      acceptsArgs: true,
      handler: async (ctx) => {
        const args = ctx.args?.trim() || "";

        // Policy check mode: /azureclaw-agt check shell:rm -rf /
        if (args.startsWith("check ")) {
          const action = args.slice(6).trim();
          if (agtPolicy) {
            const decision = agtPolicy.evaluate(action);
            return {
              text: [
                `**AGT Policy Check** (via @agentmesh/sdk)`,
                `Action: \`${action}\``,
                `Decision: **${decision.effect}**`,
                decision.effect === "deny" ? "Blocked by AGT policy" : "Allowed",
              ].join("\n"),
            };
          }
          // Fallback to router-native policy
          try {
            const http = await import("node:http");
            const postData = JSON.stringify({ action });
            const body = await new Promise<string>((resolve, reject) => {
              const req = http.request({ hostname: "127.0.0.1", port: 8443, path: "/agt/evaluate", method: "POST", headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postData) } }, (res) => {
                let data = ""; res.on("data", (c: Buffer) => { data += c.toString(); }); res.on("end", () => resolve(data));
              });
              req.on("error", reject); req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
              req.write(postData); req.end();
            });
            const parsed = JSON.parse(body);
            return { text: `**Policy Check** (router-native)\nAction: \`${action}\`\nDecision: **${parsed.decision || parsed.error}**` };
          } catch {
            return { text: "Could not evaluate policy. Is the router running?" };
          }
        }

        // Status mode
        const sdkStatus = agtPolicy ? "active (@agentmesh/sdk)" : "unavailable (using router-native)";
        const trustStatus = agtTrustStore ? "active (Ed25519, 0-1000 scale)" : "unavailable";
        const auditStatus = agtAuditLogger ? "active (hash-chain)" : "unavailable";
        const meshStatus = agtMeshClient
          ? (agtMeshClient.isConnected ? "connected (E2E encrypted)" : "initialized (not connected)")
          : "unavailable";
        const identityStatus = agtIdentity ? `AMID: ${agtIdentity.amid}` : "not generated";

        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get("http://127.0.0.1:8443/agt/status", (res) => {
              let data = ""; res.on("data", (c: Buffer) => { data += c.toString(); }); res.on("end", () => resolve(data));
            });
            req.on("error", reject); req.setTimeout(5000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          return {
            text: [
              "**AzureClaw AGT Governance**",
              "",
              "**Application Layer** (plugin, @agentmesh/sdk):",
              `  Identity: ${identityStatus}`,
              `  Mesh client: ${meshStatus}`,
              `  Policy engine: ${sdkStatus}`,
              `  Trust store: ${trustStatus}`,
              `  Audit logger: ${auditStatus}`,
              "",
              "**Infrastructure Layer** (Rust router):",
              `  Governance: ${parsed.governance_enabled ? "enabled" : "disabled"}`,
              `  Sandbox: ${parsed.sandbox_name}`,
              `  Trust threshold: ${parsed.trust_threshold}`,
              `  Audit entries: ${parsed.audit_entries}`,
              `  Mesh inbox: ${parsed.mesh_inbox_count} messages`,
              parsed.blocklist_domains ? `  Blocklist: ${parsed.blocklist_domains} domains` : "",
              "",
              "**Overlap resolution:**",
              "  Tool policy → AGT SDK (plugin)",
              "  Mesh routing → Rust router (K8s DNS)",
              "  Content safety → AzureClaw (Azure AI)",
              "  Token budgets → AzureClaw (router)",
              "  Network/FS → AzureClaw (iptables/seccomp)",
              "",
              "Check policy: `/azureclaw-agt check shell:rm -rf /`",
            ].filter(Boolean).join("\n"),
          };
        } catch {
          return {
            text: [
              "**AzureClaw AGT Governance**",
              `Policy engine: ${sdkStatus}`,
              `Trust store: ${trustStatus}`,
              `Audit logger: ${auditStatus}`,
              "",
              "Router unreachable — showing SDK-only status.",
            ].join("\n"),
          };
        }
      },
    });

    // ── /azureclaw-agents — list Foundry agents via proxied API ───────────
    api.registerCommand({
      name: "azureclaw-agents",
      description: "List Foundry agents available in this sandbox",
      handler: async () => {
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get("http://127.0.0.1:8443/agents", (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          const agents = parsed.data || [];
          if (agents.length === 0) {
            return {
              text: [
                "**Foundry Agents**: none created yet",
                "",
                "Create an agent via the Foundry Agent API:",
                "```",
                "POST http://localhost:8443/agents",
                '{"name": "my-agent", "model": "gpt-4.1", "instructions": "You are a helpful assistant"}',
                "```",
                "",
                "The router authenticates and proxies to Foundry automatically.",
              ].join("\n"),
            };
          }
          return {
            text: [
              `**Foundry Agents** (${agents.length})`,
              "",
              ...agents.map((a: any) => `- **${a.name || a.id}** (model: ${a.model || "default"}, id: ${a.id})`),
              "",
              "Use `/azureclaw-memory <agent-id>` to view threads.",
            ].join("\n"),
          };
        } catch {
          return { text: "Could not query Foundry agents. Is the inference router running?" };
        }
      },
    });

    // ── /azureclaw-memory — list Foundry threads (agent memory) ───────────
    api.registerCommand({
      name: "azureclaw-memory",
      description: "List Foundry threads (agent memory) — /azureclaw-memory [agent-id]",
      acceptsArgs: true,
      handler: async (ctx) => {
        const agentId = ctx.args?.trim();
        if (!agentId) {
          return { text: "Usage: `/azureclaw-memory <agent-id>`\n\nUse `/azureclaw-agents` to list agents first." };
        }
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(`http://127.0.0.1:8443/agents/${agentId}/threads`, (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          const threads = parsed.data || [];
          if (threads.length === 0) {
            return { text: `No threads found for agent ${agentId}. Memory is created when the agent processes messages.` };
          }
          return {
            text: [
              `**Agent Memory** (${threads.length} threads for ${agentId})`,
              "",
              ...threads.slice(0, 10).map((t: any) =>
                `- Thread ${t.id} (created: ${t.created_at || "unknown"})`
              ),
              threads.length > 10 ? `\n... and ${threads.length - 10} more` : "",
            ].join("\n"),
          };
        } catch {
          return { text: `Could not query threads for agent ${agentId}. Check that the agent exists.` };
        }
      },
    });

    // ── /azureclaw-spawn — spawn a sub-agent sandbox via router ────────────
    api.registerCommand({
      name: "azureclaw-spawn",
      description: "Spawn a sub-agent sandbox — /azureclaw-spawn <name> [--model X] [--governance] [--learn-egress]",
      acceptsArgs: true,
      handler: async (ctx) => {
        const raw = ctx.args?.trim() || "";
        if (!raw) {
          return {
            text: [
              "**Usage:** `/azureclaw-spawn <name> [options]`",
              "",
              "**Options:**",
              "  `--model <name>` — model deployment (default: gpt-4.1)",
              "  `--governance` — enable AGT governance + mesh",
              "  `--trust-threshold <n>` — AGT trust threshold (default: 500)",
              "  `--learn-egress` — enable egress learn mode",
              "  `--token-budget-daily <n>` — daily token limit",
              "",
              "**Examples:**",
              "  `/azureclaw-spawn sub-analyst --model gpt-4.1 --governance`",
              "  `/azureclaw-spawn sub-coder --model DeepSeek-V3.2 --learn-egress`",
              "",
              "**After spawning:**",
              "  `/azureclaw-spawn-list` — list your sub-agents",
              "  Use AGT mesh to communicate: `POST localhost:8443/agt/mesh/send`",
            ].join("\n"),
          };
        }

        // Parse args: first token is name, rest are flags
        const tokens = raw.split(/\s+/);
        const name = tokens[0];
        const body: Record<string, unknown> = { name };

        for (let i = 1; i < tokens.length; i++) {
          switch (tokens[i]) {
            case "--model":
              body.model = tokens[++i];
              break;
            case "--governance":
              body.governance = true;
              break;
            case "--trust-threshold":
              body.trust_threshold = parseInt(tokens[++i], 10);
              break;
            case "--learn-egress":
              body.learn_egress = true;
              break;
            case "--token-budget-daily":
              body.token_budget_daily = parseInt(tokens[++i], 10);
              break;
            case "--token-budget-per-request":
              body.token_budget_per_request = parseInt(tokens[++i], 10);
              break;
            case "--isolation":
              body.isolation = tokens[++i];
              break;
          }
        }

        try {
          const http = await import("node:http");
          const postData = JSON.stringify(body);
          const result = await new Promise<string>((resolve, reject) => {
            const req = http.request(
              {
                hostname: "127.0.0.1",
                port: 8443,
                path: "/sandbox/spawn",
                method: "POST",
                headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postData) },
              },
              (res) => {
                let data = "";
                res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                res.on("end", () => resolve(data));
              },
            );
            req.on("error", reject);
            req.setTimeout(15000, () => { req.destroy(); reject(new Error("timeout")); });
            req.write(postData);
            req.end();
          });
          const parsed = JSON.parse(result);
          if (parsed.error) {
            return { text: `**Spawn failed:** ${parsed.error}` };
          }
          return {
            text: [
              `**Sub-agent spawned:** ${parsed.name}`,
              `Namespace: ${parsed.namespace || "pending"}`,
              `Phase: ${parsed.phase || "Pending"}`,
              parsed.message || "",
              "",
              "**Next steps:**",
              body.governance
                ? "- Send tasks via AGT mesh: `POST localhost:8443/agt/mesh/send`"
                : "- Enable governance for inter-agent communication",
              "- Check status: `/azureclaw-spawn-list`",
              "- Tear down: `/azureclaw-spawn-destroy " + name + "`",
            ].join("\n"),
          };
        } catch (e) {
          return { text: `**Spawn error:** Could not reach the inference router. Is it running?` };
        }
      },
    });

    // ── /azureclaw-spawn-list — list spawned sub-agents ───────────────────
    api.registerCommand({
      name: "azureclaw-spawn-list",
      description: "List sub-agents spawned from this sandbox",
      handler: async () => {
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get("http://127.0.0.1:8443/sandbox/list", (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          if (parsed.error) {
            return { text: `**Error:** ${parsed.error}` };
          }
          const sandboxes = parsed.sandboxes || [];
          if (sandboxes.length === 0) {
            return { text: "No sub-agents spawned yet. Use `/azureclaw-spawn <name>` to create one." };
          }
          return {
            text: [
              `**Sub-Agents** (${sandboxes.length})`,
              "",
              ...sandboxes.map((s: any) =>
                `- **${s.name}** — ${s.phase || "unknown"} (model: ${s.model || "default"}, governance: ${s.governance ? "on" : "off"})`
              ),
              "",
              "Communicate via AGT mesh: `POST localhost:8443/agt/mesh/send`",
              "Destroy: `/azureclaw-spawn-destroy <name>`",
            ].join("\n"),
          };
        } catch {
          return { text: "Could not list sub-agents. Is the inference router running?" };
        }
      },
    });

    // ── /azureclaw-spawn-destroy — tear down a sub-agent ──────────────────
    api.registerCommand({
      name: "azureclaw-spawn-destroy",
      description: "Destroy a spawned sub-agent — /azureclaw-spawn-destroy <name>",
      acceptsArgs: true,
      handler: async (ctx) => {
        const name = ctx.args?.trim();
        if (!name) {
          return { text: "Usage: `/azureclaw-spawn-destroy <name>`\n\nUse `/azureclaw-spawn-list` to see your sub-agents." };
        }
        try {
          const http = await import("node:http");
          const result = await new Promise<string>((resolve, reject) => {
            const req = http.request(
              {
                hostname: "127.0.0.1",
                port: 8443,
                path: `/sandbox/${encodeURIComponent(name)}`,
                method: "DELETE",
              },
              (res) => {
                let data = "";
                res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
                res.on("end", () => resolve(data));
              },
            );
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
            req.end();
          });
          const parsed = JSON.parse(result);
          if (parsed.error) {
            return { text: `**Delete failed:** ${parsed.error}` };
          }
          return { text: `**Destroyed:** ${parsed.name} — ${parsed.message || "teardown in progress"}` };
        } catch {
          return { text: `Could not delete sub-agent '${name}'. Is the inference router running?` };
        }
      },
    });

    // ── /azureclaw-spawn-status — check status of a sub-agent ─────────────
    api.registerCommand({
      name: "azureclaw-spawn-status",
      description: "Check status of a spawned sub-agent — /azureclaw-spawn-status <name>",
      acceptsArgs: true,
      handler: async (ctx) => {
        const name = ctx.args?.trim();
        if (!name) {
          return { text: "Usage: `/azureclaw-spawn-status <name>`\n\nUse `/azureclaw-spawn-list` to see your sub-agents." };
        }
        try {
          const http = await import("node:http");
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get(`http://127.0.0.1:8443/sandbox/${encodeURIComponent(name)}/status`, (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          if (parsed.error) {
            return { text: `**Not found:** ${parsed.error}` };
          }
          const ready = parsed.phase === "Running";
          return {
            text: [
              `**Sub-Agent: ${parsed.name}**`,
              `Phase: ${parsed.phase || "unknown"} ${ready ? "(ready for mesh)" : "(not ready yet)"}`,
              parsed.namespace ? `Namespace: ${parsed.namespace}` : "",
              "",
              ready
                ? "Send a task: `POST localhost:8443/agt/mesh/send` with `to_agent: \"" + name + "\"`"
                : "Wait for phase=Running before sending mesh messages.",
            ].filter(Boolean).join("\n"),
          };
        } catch {
          return { text: `Could not check status of '${name}'. Is the inference router running?` };
        }
      },
    });
  },
});

export default azureClawPlugin;
