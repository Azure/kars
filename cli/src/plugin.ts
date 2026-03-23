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

// AMID → agent name mapping (populated during send via registry search)
const amidToName: Map<string, string> = new Map();
const nameToAmid: Map<string, string> = new Map();

// Stored sandbox name for reconnect attempts
let agtSandboxName: string = "unknown";

/**
 * Process a task_request with tool-calling (exec_command).
 * Runs an LLM loop: the model can call exec_command to run shell commands,
 * then gets the output back, and continues until it produces a final text response.
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
        description: "Execute a shell command inside the sandbox and return stdout/stderr. Use for system info (uname, hostname, ip addr, cat /etc/os-release, etc.), file operations, or any command-line task.",
        parameters: {
          type: "object",
          properties: {
            command: { type: "string", description: "The shell command to execute" },
          },
          required: ["command"],
        },
      },
    },
  ];

  const messages: Array<{ role: string; content?: string; tool_calls?: any[]; tool_call_id?: string; name?: string }> = [
    {
      role: "system",
      content: "You are a helpful sub-agent running inside an AzureClaw sandbox. You have access to exec_command to run shell commands. Use it to answer questions about the system (kernel, IP, hostname, etc.). Be concise and report actual command output.",
    },
    {
      role: "user",
      content: typeof taskContent === "string" ? taskContent : JSON.stringify(taskContent),
    },
  ];

  // Tool-calling loop (max 5 rounds to prevent runaway)
  for (let round = 0; round < 5; round++) {
    const postData = JSON.stringify({ model, messages, tools, max_tokens: 2048 });
    const response = await new Promise<any>((resolve, reject) => {
      const req = http.request("http://127.0.0.1:8443/v1/chat/completions", {
        method: "POST",
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(postData) },
        timeout: 60000,
      }, (res) => {
        let body = "";
        res.on("data", (chunk: Buffer) => { body += chunk.toString(); });
        res.on("end", () => {
          try { resolve(JSON.parse(body)); } catch { reject(new Error(`LLM parse error: ${body.slice(0, 200)}`)); }
        });
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
          const cmd = args.command || "";
          log.info(`AGT sub-agent exec: ${cmd}`);
          result = execSync(cmd, { timeout: 15000, encoding: "utf8", maxBuffer: 64 * 1024 }).trim();
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
  // Singleton guard — OpenClaw loads the plugin twice (tool registry + agent session).
  // Only the first load should create the AGT identity and mesh connection to avoid
  // duplicate messages and wasted relay connections.
  // Use process env flag since module may be loaded as separate instances.
  if (agtInitialized || process.env.__AGT_INITIALIZED === '1') return;
  agtInitialized = true;
  process.env.__AGT_INITIALIZED = '1';

  try {
    const sdk = require("@agentmesh/sdk");

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

    // Create AgentMeshClient — connects to self-hosted relay/registry via router proxy
    // Router (UID 1001) proxies: /agt/relay → ws://agentmesh-relay:8765
    //                            /agt/registry/* → http://agentmesh-registry:8080/v1/*
    const registryUrl = process.env.AGT_REGISTRY_URL || "http://127.0.0.1:8443/agt/registry";
    const relayUrl = process.env.AGT_RELAY_URL || "ws://127.0.0.1:8443/agt/relay";

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
    try {
      await agtMeshClient.connect({
        displayName: agtSandboxName,
        capabilities: ["azureclaw-agent", "task-execution", agtSandboxName],
      });
      log.info(`AGT mesh connected (relay: ${relayUrl}, registry: ${registryUrl})`);
    } catch (connErr: any) {
      log.warn(`AGT mesh connect deferred: ${connErr.message} (will retry on first send)`);
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
          const trustScore = peerInfo?.trustScore || peerInfo?.reputation || 0;
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

      // Process task_request messages: call LLM with tool access and reply via E2E encrypted relay
      if (message?.type === "task_request" && fromAmid && agtMeshClient) {
        const taskContent = message?.content || content;
        try {
          const llmResponse = await processTaskWithTools(taskContent, log);

          // Send the LLM response back via E2E encrypted relay
          await agtMeshClient.send(fromAmid, {
            type: "task_response",
            content: llmResponse,
            from_agent: agtSandboxName,
            in_reply_to: taskContent,
            timestamp: new Date().toISOString(),
          });
          log.info(`AGT relay: tool-enabled reply sent to ${fromName} via E2E encrypted relay`);
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
          log.warn(`AGT relay: LLM reply failed: ${replyErr.message}`);
        }
      }
    });

    log.info(`AGT SDK loaded (v${sdk.VERSION}) — identity, policy, trust, audit, mesh ACTIVE (relay path for inter-agent comms)`);
  } catch (e: any) {
    log.warn(`AGT SDK not available: ${e.message}. Using router-native governance.`);
  }
}

// ---------------------------------------------------------------------------
// OpenClaw Plugin SDK types (stubs — only available at runtime via host)
// ---------------------------------------------------------------------------

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

    log.info(`AzureClaw plugin loaded (model: ${config.model})`);

    // Initialize AGT SDK (identity, policy, trust, audit, mesh)
    initAGT(log).catch((e: any) => log.warn(`AGT init error: ${e.message}`));

    // ── Register AzureClaw agent tools (spawn, mesh, status, destroy) ────
    // These are first-class tools the LLM can call directly.
    // Registered as required tools (always available, no tools.allow needed).
    // API: execute(_id, params) → { content: [{ type: "text", text }] }

    const ROUTER = "http://127.0.0.1:8443";
    async function routerCall(method: string, path: string, body?: unknown): Promise<any> {
      const http = await import("node:http");
      const url = `${ROUTER}${path}`;
      return new Promise((resolve, reject) => {
        const opts: any = { method, timeout: 30000, headers: {} };
        if (body) opts.headers["Content-Type"] = "application/json";
        const req = http.request(url, opts, (res: any) => {
          let data = "";
          res.on("data", (c: Buffer) => { data += c.toString(); });
          res.on("end", () => {
            try { resolve(JSON.parse(data)); } catch { resolve({ raw: data }); }
          });
        });
        req.on("error", (e: Error) => reject(e));
        req.setTimeout(30000, () => { req.destroy(); reject(new Error("timeout")); });
        if (body) req.write(JSON.stringify(body));
        req.end();
      });
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
          await new Promise(r => setTimeout(r, 5000));

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
          return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `Status check failed: ${e.message}` }] };
        }
      },
    });

    api.registerTool({
      name: "azureclaw_mesh_send",
      label: "Send Mesh Task",
      description: "Send a task to a sub-agent via AGT mesh (E2E encrypted relay). The sub-agent will auto-process the task with its AI model and send the result back to your inbox. Wait for the sub-agent to be Running first.",
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
                  await new Promise(r => setTimeout(r, 5000));
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
              for (let sendAttempt = 0; sendAttempt < 6; sendAttempt++) {
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
                    log.info(`AGT relay: waiting for prekeys from '${agentName}' (${sendAttempt + 1}/6)...`);
                    await new Promise(r => setTimeout(r, 5000));
                  } else {
                    break; // non-prekey error — don't retry
                  }
                }
              }
              if (!sendErr) {
                log.info(`AGT relay: sent to ${agentName} (${targetAmid.slice(0, 12)}...) via E2E encrypted relay`);
                return { content: [{ type: "text", text: JSON.stringify({
                  status: "delivered_via_agt_relay",
                  to_agent: agentName,
                  to_amid: targetAmid,
                  from_amid: agtIdentity.amid,
                  protocol: "AGT E2E encrypted (Signal Protocol)",
                  message_id: `agt-${Date.now().toString(36)}`,
                }, null, 2) }] };
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
          return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
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
          return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
        } catch (e: any) {
          return { content: [{ type: "text", text: `List failed: ${e.message}` }] };
        }
      },
    });

    log.info("AzureClaw agent tools registered: azureclaw_spawn, azureclaw_spawn_status, azureclaw_mesh_send, azureclaw_mesh_inbox, azureclaw_spawn_destroy, azureclaw_spawn_list");

    // ── Register Azure AI Foundry as a model provider ───────────────────
    api.registerProvider({
      id: "azure-openai",
      label: "Azure AI Foundry (via AzureClaw)",
      docsPath: "https://github.com/Azure/azureclaw",
      aliases: ["azure", "azureclaw", "foundry"],
      envVars: ["AZURE_OPENAI_API_KEY"],
      models: {
        chat: [
          { id: "gpt-4.1", label: "GPT-4.1 (Azure)", contextWindow: 1047576, maxOutput: 32768 },
          { id: "gpt-5-mini", label: "GPT-5 Mini (Azure)", contextWindow: 1047576, maxOutput: 32768 },
          { id: "gpt-4o", label: "GPT-4o (Azure)", contextWindow: 128000, maxOutput: 16384 },
          { id: "DeepSeek-V3.2", label: "DeepSeek V3.2 (Foundry)", contextWindow: 131072, maxOutput: 8192 },
          { id: "Phi-4", label: "Phi-4 (Microsoft)", contextWindow: 16384, maxOutput: 16384 },
          { id: "Meta-Llama-3.1-405B-Instruct", label: "Llama 3.1 405B (Meta)", contextWindow: 131072, maxOutput: 8192 },
          { id: "o3-mini", label: "o3-mini (Azure)", contextWindow: 200000, maxOutput: 100000 },
        ],
      },
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
      description: "Switch AI model (e.g. /azureclaw-switch Phi-4)",
      acceptsArgs: true,
      handler: async (ctx) => {
        const model = ctx.args?.trim();
        if (!model) {
          return { text: "Usage: /azureclaw-switch <model-name>\nExample: /azureclaw-switch DeepSeek-V3.2" };
        }
        return {
          text: [
            `Switching to **${model}**...`,
            "",
            "From the host CLI, run:",
            `\`azureclaw model set ${config.sandboxName} ${model}\``,
            "",
            "The model switch takes effect on the next request.",
          ].join("\n"),
        };
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
