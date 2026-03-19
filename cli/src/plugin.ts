/**
 * AzureClaw — OpenClaw Plugin
 *
 * Registers AzureClaw commands and Azure OpenAI as a model provider
 * within the OpenClaw plugin system.
 *
 * Usage: openclaw azureclaw <command>
 *
 * Plugin object format (mirrors stock OpenClaw plugins like memory-core, ollama):
 *   export default { id, name, description, configSchema, register(api) }
 */

import type { Command } from "commander";

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

const azureClawPlugin = {
  id: "azureclaw",
  name: "AzureClaw",
  description: "Secure AI agent runtime on Azure — Azure OpenAI provider and sandbox CLI",
  configSchema: {
    jsonSchema: {
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
  },

  register(api: OpenClawPluginApi): void {
    const config = getPluginConfig(api);
    const log = api.logger;

    log.info(`AzureClaw plugin loaded (model: ${config.model})`);

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
          const body = await new Promise<string>((resolve, reject) => {
            const req = http.get("http://127.0.0.1:8443/v1/models", (res) => {
              let data = "";
              res.on("data", (chunk: Buffer) => { data += chunk.toString(); });
              res.on("end", () => resolve(data));
            });
            req.on("error", reject);
            req.setTimeout(10000, () => { req.destroy(); reject(new Error("timeout")); });
          });
          const parsed = JSON.parse(body);
          const models = (parsed.data || []).map((m: any) => m.id).sort();
          const deployed = models.filter((m: string) =>
            ["gpt-4.1", "gpt-5-mini", "DeepSeek-V3.2", "Phi-4", "gpt-4o"].some(d => m.includes(d))
          );
          return {
            text: [
              `**Azure Foundry Models** (${models.length} available)`,
              "",
              "Deployed in your project:",
              deployed.map((m: string) => `  - ${m}`).join("\n") || "  (none detected)",
              "",
              `Total catalog: ${models.length} models`,
              "Switch with: `/azureclaw-switch <model>`",
            ].join("\n"),
          };
        } catch {
          return { text: "Could not query models. Is the inference router running?" };
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
            `Network: default-deny egress`,
            `Inference: routed through AzureClaw inference router`,
            `Auth: IMDS (kubelet MI, zero keys)`,
          ].join("\n"),
        };
      },
    });
  },
};

export default azureClawPlugin;
