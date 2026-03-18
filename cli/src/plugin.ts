/**
 * AzureClaw — OpenClaw Plugin
 *
 * Registers AzureClaw commands and Azure OpenAI as a model provider
 * within the OpenClaw plugin system.
 *
 * Usage: openclaw azureclaw <command>
 *
 * This file mirrors NemoClaw's plugin structure:
 * - registerCli: adds `openclaw azureclaw` subcommands
 * - registerProvider: registers Azure OpenAI as a model provider
 * - registerCommand: adds /azureclaw slash command
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
  command: string;
  description: string;
  handler: (args: { message: string; config: OpenClawConfig }) => Promise<{ text: string }>;
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
// Plugin registration (called by OpenClaw host)
// ---------------------------------------------------------------------------

export default function register(api: OpenClawPluginApi): void {
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
          const { execa } = await import("execa");
          try {
            await execa("azureclaw", ["status", config.sandboxName], {
              stdio: "inherit",
            });
          } catch {
            ctx.logger.error("azureclaw CLI not found — install with: npm install -g @azure/azureclaw");
          }
        });

      azureclaw
        .command("connect")
        .description("Connect to the sandbox")
        .action(async () => {
          const { execa } = await import("execa");
          try {
            await execa("azureclaw", ["connect", config.sandboxName, "--shell"], {
              stdio: "inherit",
            });
          } catch {
            ctx.logger.error("azureclaw CLI not found");
          }
        });

      azureclaw
        .command("dev")
        .description("Start a local sandbox")
        .action(async () => {
          const { execa } = await import("execa");
          try {
            await execa("azureclaw", ["dev"], { stdio: "inherit" });
          } catch {
            ctx.logger.error("azureclaw CLI not found");
          }
        });

      azureclaw
        .command("logs")
        .option("-f, --follow", "Follow log output")
        .description("Stream sandbox logs")
        .action(async (opts: { follow?: boolean }) => {
          const { execa } = await import("execa");
          const args = ["logs", config.sandboxName];
          if (opts.follow) args.push("-f");
          try {
            await execa("azureclaw", args, { stdio: "inherit" });
          } catch {
            ctx.logger.error("azureclaw CLI not found");
          }
        });
    },
    { commands: ["azureclaw"] }
  );

  // ── Register /azureclaw slash command ─────────────────────────────────
  api.registerCommand({
    command: "azureclaw",
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
}
