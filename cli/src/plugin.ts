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

    // ── Register Azure OpenAI as a model provider ─────────────────────────
    api.registerProvider({
      id: "azure-openai",
      label: "Azure OpenAI (via AzureClaw)",
      docsPath: "https://github.com/Azure/azureclaw",
      aliases: ["azure", "azureclaw"],
      envVars: ["AZURE_OPENAI_API_KEY"],
      models: {
        chat: [
          {
            id: "gpt-4.1",
            label: "GPT-4.1 (Azure OpenAI)",
            contextWindow: 1047576,
            maxOutput: 32768,
          },
          {
            id: "gpt-4o",
            label: "GPT-4o (Azure OpenAI)",
            contextWindow: 128000,
            maxOutput: 16384,
          },
          {
            id: "o3-mini",
            label: "o3-mini (Azure OpenAI)",
            contextWindow: 200000,
            maxOutput: 100000,
          },
        ],
      },
      auth: [
        {
          type: "api-key",
          envVar: "AZURE_OPENAI_API_KEY",
          headerName: "api-key",
          label: "Azure OpenAI API Key",
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
      description: "Show AzureClaw sandbox status and security info",
      handler: async () => {
        return {
          text: [
            "**AzureClaw Sandbox**",
            `Model: azure-openai/${config.model}`,
            `Sandbox: ${config.sandboxName}`,
            `Endpoint: ${config.endpoint || "(configured via azureclaw onboard)"}`,
            "",
            "Commands:",
            "- `openclaw azureclaw status` — health + metrics",
            "- `openclaw azureclaw connect` — shell into sandbox",
            "- `openclaw azureclaw dev` — start local sandbox",
            "- `openclaw azureclaw logs -f` — stream logs",
          ].join("\n"),
        };
      },
    });
  },
};

export default azureClawPlugin;
