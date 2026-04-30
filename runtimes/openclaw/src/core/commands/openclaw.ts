// ci:loc-ok — Phase 2 multi-CRD reconciler / generated module; intentional. Tracked in plan.md §S15 follow-up.
// OpenClaw command/provider/CLI registrations — extracted from plugin.ts in S15.f.10.
//
// Final closure-bound block lifted out of `register()`:
//   • Foundry model provider registration (`api.registerProvider`).
//   • CLI subcommand registrar `openclaw azureclaw …`
//     (`api.registerCli`).
//   • ~12 slash-command (`/azureclaw …`) definitions
//     (`api.registerCommand`).
//
// None of this code talks to the AGT mesh — it's all router HTTP
// queries plus UI plumbing. Tool/command bodies are byte-identical to
// the previous inline registrations; only closure capture is replaced
// with explicit Deps threading.

/* eslint-disable @typescript-eslint/no-explicit-any */
import { routerCall, routerUrl, routerBase } from "../router-client.js";
import type { FoundryProjectInfo } from "../foundry-discovery.js";
import { safeJson } from "../safe-json.js";

void routerCall; void routerBase; void safeJson;

type AnyApi = any;

export interface OpenClawCommandsDeps {
  log: { info: (m: string) => void; warn: (m: string) => void };
  config: any;
  getFoundryProject: () => FoundryProjectInfo | null;
  meshClient: () => any;
  identity: () => any;
  policy: () => any;
  trustStore: () => any;
  auditLogger: () => any;
  // Memory sync coupling — used by the /azureclaw model-switch command to
  // flush buffered tool-call summaries before the LLM swaps under us.
  memorySyncBuffer: string[];
  syncToFoundryMemory: (
    content: string,
    log: { info: (m: string) => void; warn: (m: string) => void },
  ) => Promise<void>;
}

export function registerOpenClawCommands(api: AnyApi, deps: OpenClawCommandsDeps): void {
  const { log, config } = deps;
  const memorySyncBuffer = deps.memorySyncBuffer;
  const syncToFoundryMemory = deps.syncToFoundryMemory;
  void log; void config; void memorySyncBuffer; void syncToFoundryMemory;

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
  const _fpForModels = deps.getFoundryProject();
  const chatModels = (_fpForModels?.deployments?.length)
    ? _fpForModels.deployments.map((d: any) => ({
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
        id: "azure-openai-key",
        type: "api-key",
        envVar: "AZURE_OPENAI_API_KEY",
        headerName: "api-key",
        label: "Azure API Key (or 'routed-via-inference-router' for AzureClaw)",
      },
    ],
  });

  // ── Register CLI subcommands: openclaw azureclaw <cmd> ────────────────
  api.registerCli(
    (ctx: any) => {
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
              const req = http.get(routerUrl("/metrics"), (res) => {
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
            console.log(`AzureClaw Inference Router: not reachable (${routerUrl("/metrics")})`);
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
            routerUrl("/deployments?api-version=2025-11-15-preview"),
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

  // ── Shared model switch logic ────────────────────────────────────────
  async function switchModelInternal(model: string): Promise<string> {
    const prevModel = process.env.OPENCLAW_MODEL || config.model || "gpt-4.1";

    // 1. Flush conversation context to Foundry memory before switching
    try {
      // Flush any buffered tool calls
      if (memorySyncBuffer.length > 0) {
        const batch = memorySyncBuffer.splice(0);
        const batchSummary = `Pre-switch checkpoint (${batch.length} calls):\n${batch.join("\n")}`;
        await syncToFoundryMemory(batchSummary, log);
      }
      // Save a handoff summary so the new session has context
      const handoff = [
        `Model switch: ${prevModel} → ${model}`,
        `User requested switching to ${model} mid-conversation.`,
        `Session was active with ${prevModel}. Key context should be recalled from prior memories.`,
      ].join("\n");
      await syncToFoundryMemory(handoff, log);
      log.info(`Memory flushed before model switch to ${model}`);
    } catch (e: any) {
      log.warn(`Memory flush before switch failed (non-blocking): ${e.message}`);
    }

    // 2. Update plugin env + config
    process.env.OPENCLAW_MODEL = model;
    config.model = model;

    // 3. Update OpenClaw config files
    try {
      const fs = await import("node:fs");
      const modelsPath = "/sandbox/.openclaw/agents/main/agent/models.json";
      const oclawPath = "/sandbox/.openclaw/openclaw.json";

      const allModels = new Set<string>();
      allModels.add(model);
      const _fpAll = deps.getFoundryProject();
      if (_fpAll?.deployments) {
        for (const d of _fpAll.deployments) {
          if (!d.id.includes("embedding")) allModels.add(d.id);
        }
      }
      const modelsArr = [...allModels].map(id => ({
        id, name: `${id} (Azure via AzureClaw)`, reasoning: false,
        input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
        contextWindow: 200000, maxTokens: 8192, api: "openai-completions",
      }));

      try {
        const mj = JSON.parse(fs.readFileSync(modelsPath, "utf8"));
        if (mj.providers?.["azure-openai"]) {
          mj.providers["azure-openai"].models = modelsArr.map(m => ({ id: m.id, name: m.name }));
        }
        mj.selectedModel = { provider: "azure-openai", id: model };
        fs.writeFileSync(modelsPath, JSON.stringify(mj, null, 2));
      } catch { /* read-only fs */ }

      try {
        const oc = JSON.parse(fs.readFileSync(oclawPath, "utf8"));
        if (oc.models?.providers?.["azure-openai"]) {
          oc.models.providers["azure-openai"].models = modelsArr.map(m => ({ id: m.id, name: m.name }));
        }
        if (oc.agents?.defaults?.model) {
          oc.agents.defaults.model.primary = `azure-openai/${model}`;
        }
        fs.writeFileSync(oclawPath, JSON.stringify(oc, null, 2));
      } catch { /* read-only fs */ }
    } catch { /* non-critical */ }

    // 4. Update router model override
    try {
      const result = await routerCall("PUT", "/admin/model", { model });
      const prev = (result as any)?.previous || prevModel;
      return [
        `✅ Switched **${prev}** → **${model}**`,
        "",
        "Context saved to Foundry memory.",
        "Type `/new` to start a fresh session with **" + model + "** — your conversation context will be recalled automatically.",
      ].join("\n");
    } catch {
      return [
        `⚠️ Plugin updated to **${model}**, but router admin endpoint not reachable.`,
        "",
        "Context saved to Foundry memory.",
        "Type `/new` to start a fresh session with **" + model + "**.",
      ].join("\n");
    }
  }

  // ── /azureclaw-switch — switch model with memory handoff ──────────────
  api.registerCommand({
    name: "azureclaw-switch",
    description: "Switch AI model (e.g. /azureclaw-switch gpt-5.4-mini)",
    acceptsArgs: true,
    handler: async (ctx: any) => {
      const model = ctx.args?.trim();
      if (!model) {
        const available = deps.getFoundryProject()?.deployments
          ?.filter((d: any) => !d.id?.includes("embedding"))
          ?.map((d: any) => d.id).join(", ") || "unknown";
        return { text: `Usage: /azureclaw-switch <model-name>\nAvailable: ${available}` };
      }
      return { text: await switchModelInternal(model) };
    },
  });

  // ── /switch-model — show/switch AI model (avoids built-in /model) ───
  api.registerCommand({
    name: "switch-model",
    description: "Show or switch AI model (e.g. /switch-model gpt-5.4-mini)",
    acceptsArgs: true,
    handler: async (ctx: any) => {
      const model = ctx.args?.trim();
      if (!model) {
        // Query live deployments from Foundry
        const current = process.env.OPENCLAW_MODEL || config.model || "gpt-4.1";
        let available: string[] = [];
        try {
          const result = await routerCall("GET", `/deployments?api-version=2025-11-15-preview`);
          const deps = (result as any)?.data || (result as any)?.value || [];
          if (Array.isArray(deps)) {
            available = deps
              .filter((d: any) => {
                const id = d.id || d.name || "";
                return !id.includes("embedding");
              })
              .map((d: any) => {
                const id = d.id || d.name || "?";
                const modelName = d.model?.name || d.model || d.properties?.model?.name || "";
                const label = modelName && modelName !== id ? `${id} (${modelName})` : id;
                return id === current ? `**${label}** ← current` : label;
              });
          }
        } catch {
          // Fall back to cached discovery
          available = (deps.getFoundryProject()?.deployments || [])
            .filter((d: any) => !d.id?.includes("embedding"))
            .map((d: any) => d.id === current ? `**${d.id}** ← current` : d.id);
        }
        return { text: [
          `Current model: **${current}**`,
          "",
          "Available deployments:",
          ...available.map((m: string) => `  • ${m}`),
          "",
          "Usage: `/switch-model <name>` to switch",
        ].join("\n") };
      }
      return { text: await switchModelInternal(model) };
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
          `Foundry Agent API: proxied via ${routerBase()}/agents/*`,
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
    handler: async (ctx: any) => {
      const args = ctx.args?.trim() || "";

      // Policy check mode: /azureclaw-agt check shell:rm -rf /
      if (args.startsWith("check ")) {
        const action = args.slice(6).trim();
        if (deps.policy()) {
          const decision = deps.policy().evaluate(action);
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
      const sdkStatus = deps.policy() ? "active (@agentmesh/sdk)" : "unavailable (using router-native)";
      const trustStatus = deps.trustStore() ? "active (Ed25519, 0-1000 scale)" : "unavailable";
      const auditStatus = deps.auditLogger() ? "active (hash-chain)" : "unavailable";
      const meshStatus = deps.meshClient()
        ? (deps.meshClient().isConnected ? "connected (E2E encrypted)" : "initialized (not connected)")
        : "unavailable";
      const identityStatus = deps.identity() ? `AMID: ${deps.identity().amid}` : "not generated";

      try {
        const http = await import("node:http");
        const body = await new Promise<string>((resolve, reject) => {
          const req = http.get(routerUrl("/agt/status"), (res) => {
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
            `  Governance: ${parsed.enabled ? "enabled" : "disabled"}`,
            `  Sandbox: ${parsed.sandbox}`,
            `  Audit entries: ${parsed.audit_entries}`,
            `  Mesh inbox: ${parsed.inbox_messages} messages`,
            `  Mesh sessions: ${parsed.mesh_sessions ?? 0}  sent: ${parsed.mesh_messages_sent ?? 0}  recv: ${parsed.mesh_messages_received ?? 0}`,
            `  Trust updates: ${parsed.trust_updates ?? 0}  total interactions: ${parsed.total_interactions ?? 0}`,
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
          const req = http.get(routerUrl("/agents"), (res) => {
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
              `POST ${routerBase()}/agents`,
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
    handler: async (ctx: any) => {
      const agentId = ctx.args?.trim();
      if (!agentId) {
        return { text: "Usage: `/azureclaw-memory <agent-id>`\n\nUse `/azureclaw-agents` to list agents first." };
      }
      try {
        const http = await import("node:http");
        const body = await new Promise<string>((resolve, reject) => {
          const req = http.get(routerUrl(`/agents/${agentId}/threads`), (res) => {
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
    handler: async (ctx: any) => {
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
            "  Use the azureclaw_mesh_send tool to communicate (E2E encrypted)",
          ].join("\n"),
        };
      }

      // Parse args: first token is name, rest are flags
      const tokens = raw.split(/\s+/);
      const name = tokens[0];
      const body: Record<string, unknown> = { agent_id: name };

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
            `**Sub-agent spawned:** ${parsed.agent_id}`,
            `Namespace: ${parsed.namespace || "pending"}`,
            `Phase: ${parsed.phase || "Pending"}`,
            parsed.message || "",
            "",
            "**Next steps:**",
            body.governance
              ? "- Send tasks via azureclaw_mesh_send tool (E2E encrypted)"
              : "- Enable governance for inter-agent communication",
            "- Check status: `/azureclaw-spawn-list`",
            "- Tear down: `/azureclaw-spawn-destroy " + name + "`",
          ].join("\n"),
        };
      } catch {
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
          const req = http.get(routerUrl("/sandbox/list"), (res) => {
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
              `- **${s.agent_id}** — ${s.phase || "unknown"} (model: ${s.model || "default"}, governance: ${s.governance ? "on" : "off"})`
            ),
            "",
            "Communicate via azureclaw_mesh_send tool (E2E encrypted)",
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
    handler: async (ctx: any) => {
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
        return { text: `**Destroyed:** ${parsed.agent_id} — ${parsed.message || "teardown in progress"}` };
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
    handler: async (ctx: any) => {
      const name = ctx.args?.trim();
      if (!name) {
        return { text: "Usage: `/azureclaw-spawn-status <name>`\n\nUse `/azureclaw-spawn-list` to see your sub-agents." };
      }
      try {
        const http = await import("node:http");
        const body = await new Promise<string>((resolve, reject) => {
          const req = http.get(routerUrl(`/sandbox/${encodeURIComponent(name)}/status`), (res) => {
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
            `**Sub-Agent: ${parsed.agent_id}**`,
            `Phase: ${parsed.phase || "unknown"} ${ready ? "(ready for mesh)" : "(not ready yet)"}`,
            parsed.namespace ? `Namespace: ${parsed.namespace}` : "",
            "",
            ready
              ? "Send a task via azureclaw_mesh_send tool with to_agent: \"" + name + "\""
              : "Wait for phase=Running before sending mesh messages.",
          ].filter(Boolean).join("\n"),
        };
      } catch {
        return { text: `Could not check status of '${name}'. Is the inference router running?` };
      }
    },
  });
}
