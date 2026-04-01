/**
 * Shared credential management for AzureClaw CLI.
 * Used by `dev` (auto-prompts if missing) and `credentials` (explicit reconfigure).
 */

import chalk from "chalk";
import ora from "ora";
import inquirer from "inquirer";
import { mkdirSync, writeFileSync, chmodSync, existsSync, readFileSync } from "fs";
import { join } from "path";
import { homedir } from "os";

const CONFIG_DIR = join(homedir(), ".azureclaw");
const CONFIG_FILE = join(CONFIG_DIR, "config.json");
const CREDENTIALS_FILE = join(CONFIG_DIR, "credentials");
const SECRETS_FILE = join(CONFIG_DIR, "secrets.json");

/** Well-known secret keys and their descriptions */
export const KNOWN_SECRETS: Record<string, { env: string; label: string }> = {
  "azure-openai-key":  { env: "AZURE_OPENAI_API_KEY", label: "Azure OpenAI API key" },
  "telegram-token":    { env: "TELEGRAM_BOT_TOKEN",   label: "Telegram bot token" },
  "telegram-allow-from": { env: "TELEGRAM_ALLOW_FROM", label: "Telegram allowed user IDs" },
  "slack-token":       { env: "SLACK_BOT_TOKEN",      label: "Slack bot OAuth token" },
  "discord-token":     { env: "DISCORD_BOT_TOKEN",    label: "Discord bot token" },
  "brave-api-key":     { env: "BRAVE_API_KEY",        label: "Brave Search API key" },
  "tavily-api-key":    { env: "TAVILY_API_KEY",       label: "Tavily search API key" },
  "exa-api-key":       { env: "EXA_API_KEY",          label: "Exa search API key" },
  "firecrawl-api-key": { env: "FIRECRAWL_API_KEY",    label: "Firecrawl API key" },
  "perplexity-api-key":{ env: "PERPLEXITY_API_KEY",   label: "Perplexity API key" },
  "openai-api-key":    { env: "OPENAI_API_KEY",       label: "OpenAI API key" },
};

/** Mapping from CLI flag names to secret keys */
export const FLAG_TO_SECRET: Record<string, string> = {
  telegramToken:    "telegram-token",
  telegramAllowFrom:"telegram-allow-from",
  slackToken:       "slack-token",
  discordToken:     "discord-token",
  braveApiKey:      "brave-api-key",
  tavilyApiKey:     "tavily-api-key",
  exaApiKey:        "exa-api-key",
  firecrawlApiKey:  "firecrawl-api-key",
  perplexityApiKey: "perplexity-api-key",
  openaiApiKey:     "openai-api-key",
};

export interface AzureClawConfig {
  endpoint: string;
  model: string;
  apiKey: string;
  foundryProjectEndpoint?: string;
}

/** Cached deployment context — saved after successful `azureclaw up` */
export interface DeploymentContext {
  subscription?: string;
  region?: string;
  resourceGroup?: string;
  aksCluster?: string;
  acrLoginServer?: string;
  acrName?: string;
  keyVaultName?: string;
  wiClientId?: string;
  imdsClientId?: string;
  foundryEndpoint?: string;
  foundryProjectEndpoint?: string;
  identityName?: string;
  identityResourceGroup?: string;
  oidcIssuerUrl?: string;
  savedAt?: string;
}

const CONTEXT_FILE = join(CONFIG_DIR, "context.json");

/** Load cached deployment context from ~/.azureclaw/context.json */
export function loadContext(): DeploymentContext | null {
  try {
    if (!existsSync(CONTEXT_FILE)) return null;
    return JSON.parse(readFileSync(CONTEXT_FILE, "utf-8"));
  } catch {
    return null;
  }
}

/** Save deployment context to ~/.azureclaw/context.json */
export function saveContext(ctx: DeploymentContext): void {
  mkdirSync(CONFIG_DIR, { recursive: true });
  ctx.savedAt = new Date().toISOString();
  writeFileSync(CONTEXT_FILE, JSON.stringify(ctx, null, 2), "utf-8");
  chmodSync(CONTEXT_FILE, 0o600);
}

/**
 * Load saved config + credentials from ~/.azureclaw/.
 * Returns null if either is missing or corrupt.
 */
export function loadConfig(): AzureClawConfig | null {
  try {
    if (!existsSync(CONFIG_FILE)) return null;
    const config = JSON.parse(readFileSync(CONFIG_FILE, "utf-8"));
    // API key: secrets.json > legacy credentials file
    const secrets = loadSecrets();
    const apiKey = secrets["azure-openai-key"]
      || (existsSync(CREDENTIALS_FILE) ? readFileSync(CREDENTIALS_FILE, "utf-8").trim() : "");
    if (!config.endpoint || !apiKey) return null;
    return { endpoint: config.endpoint, model: config.model || "gpt-4.1", apiKey, foundryProjectEndpoint: config.foundryProjectEndpoint };
  } catch {
    return null;
  }
}

/**
 * Interactively prompt for Azure OpenAI credentials, verify them, and save.
 * If existing config is found, shows it as defaults.
 */
export async function promptAndSaveCredentials(options?: {
  /** Skip verification (for offline setup) */
  skipVerify?: boolean;
  /** Heading to show before prompts */
  heading?: string;
}): Promise<AzureClawConfig> {
  const existing = loadConfig();

  if (options?.heading) {
    console.log(chalk.hex("#0078D4")(`\n  ${options.heading}\n`));
  }

  if (existing) {
    console.log(chalk.dim(`  Existing config found (${existing.endpoint}). Press Enter to keep.\n`));
  }

  const answers = await inquirer.prompt([
    {
      type: "input",
      name: "endpoint",
      message: "Azure OpenAI endpoint:",
      default: existing?.endpoint || "https://your-resource.openai.azure.com",
      validate: (input: string) => {
        if (!input.startsWith("https://")) return "Endpoint must start with https://";
        return true;
      },
      filter: (input: string) =>
        input.replace(/\/openai\/v1\/?$/, "").replace(/\/openai\/?$/, "").replace(/\/+$/, ""),
    },
    {
      type: "input",
      name: "model",
      message: "Model deployment name:",
      default: existing?.model || "gpt-4.1",
    },
    {
      type: "password",
      name: "apiKey",
      message: "API key:",
      mask: "•",
      validate: (input: string) => {
        if (!input || input.length < 10) return "API key is required";
        return true;
      },
    },
  ]);

  // Verify
  if (!options?.skipVerify) {
    const spinner = ora({ color: "cyan" }).start("Verifying credentials...");
    try {
      const response = await fetch(
        `${answers.endpoint}/openai/deployments/${answers.model}/chat/completions?api-version=2024-12-01-preview`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json", "api-key": answers.apiKey },
          body: JSON.stringify({ messages: [{ role: "user", content: "hello" }], max_tokens: 5 }),
        }
      );
      if (!response.ok) {
        const body = await response.text();
        spinner.fail("Credential verification failed");
        console.log(chalk.red(`\n  ${response.status}: ${body}\n`));
        console.log(chalk.yellow("  Check your endpoint, model deployment name, and API key.\n"));
        process.exit(1);
      }
      spinner.succeed("Credentials verified");
    } catch (error) {
      spinner.fail("Could not reach endpoint");
      console.log(chalk.red(`\n  ${error instanceof Error ? error.message : String(error)}\n`));
      process.exit(1);
    }
  }

  // Auto-derive Foundry project endpoint from OpenAI endpoint hostname
  // e.g. https://foo.openai.azure.com → https://foo.services.ai.azure.com
  let foundryProjectEndpoint = "";
  const hostname = new URL(answers.endpoint).hostname;
  const accountName = hostname.split(".")[0];
  if (hostname.endsWith(".openai.azure.com") || hostname.endsWith(".services.ai.azure.com")) {
    const { default: inquirerFoundry } = await import("inquirer");
    const foundryAnswer = await inquirerFoundry.prompt([
      {
        type: "input",
        name: "foundryProjectEndpoint",
        message: "Foundry project endpoint (optional, for memory/agents/indexes):",
        default: existing?.foundryProjectEndpoint || `https://${accountName}.services.ai.azure.com/api/projects/YOUR_PROJECT`,
        filter: (input: string) => input.replace(/\/+$/, ""),
      },
    ]);
    if (foundryAnswer.foundryProjectEndpoint && !foundryAnswer.foundryProjectEndpoint.includes("YOUR_PROJECT")) {
      foundryProjectEndpoint = foundryAnswer.foundryProjectEndpoint;
    }
  }

  // Save
  mkdirSync(CONFIG_DIR, { recursive: true });
  const configObj: Record<string, string> = { endpoint: answers.endpoint, model: answers.model, version: "0.1.0-alpha.1" };
  if (foundryProjectEndpoint) configObj.foundryProjectEndpoint = foundryProjectEndpoint;
  writeFileSync(CONFIG_FILE, JSON.stringify(configObj, null, 2), "utf-8");
  chmodSync(CONFIG_FILE, 0o600);
  // Store API key in unified secrets.json (also keeps legacy file for compat)
  setSecret("azure-openai-key", answers.apiKey);
  writeFileSync(CREDENTIALS_FILE, answers.apiKey, "utf-8");
  chmodSync(CREDENTIALS_FILE, 0o600);

  return { endpoint: answers.endpoint, model: answers.model, apiKey: answers.apiKey, foundryProjectEndpoint: foundryProjectEndpoint || undefined };
}

/**
 * Ensure credentials are available — load from disk or prompt interactively.
 * This is the main entry point for commands that need creds.
 */
export async function ensureCredentials(): Promise<AzureClawConfig> {
  const existing = loadConfig();
  if (existing) return existing;

  console.log(chalk.yellow("\n  No Azure OpenAI credentials found. Let's set them up:\n"));
  return promptAndSaveCredentials();
}

export { CONFIG_DIR, CONFIG_FILE, CREDENTIALS_FILE, SECRETS_FILE };

// ─── Secrets store (~/.azureclaw/secrets.json) ────────────────────────────────

/** Load all secrets from secrets.json, migrating from legacy credentials file if needed. */
export function loadSecrets(): Record<string, string> {
  let secrets: Record<string, string> = {};

  // Load secrets.json if it exists
  try {
    if (existsSync(SECRETS_FILE)) {
      secrets = JSON.parse(readFileSync(SECRETS_FILE, "utf-8"));
    }
  } catch { /* corrupt file — start fresh */ }

  // Migrate legacy credentials file (plain API key) if secrets.json doesn't have it
  if (!secrets["azure-openai-key"]) {
    try {
      if (existsSync(CREDENTIALS_FILE)) {
        const legacyKey = readFileSync(CREDENTIALS_FILE, "utf-8").trim();
        if (legacyKey && legacyKey.length >= 10) {
          secrets["azure-openai-key"] = legacyKey;
          // Save migrated secrets and clean up legacy file
          saveSecrets(secrets);
        }
      }
    } catch { /* ignore */ }
  }

  return secrets;
}

/** Save secrets to ~/.azureclaw/secrets.json with mode 600. */
export function saveSecrets(secrets: Record<string, string>): void {
  mkdirSync(CONFIG_DIR, { recursive: true });
  writeFileSync(SECRETS_FILE, JSON.stringify(secrets, null, 2), "utf-8");
  chmodSync(SECRETS_FILE, 0o600);
}

/** Get a single secret value by key. */
export function getSecret(key: string): string | undefined {
  return loadSecrets()[key];
}

/** Set a single secret value by key. */
export function setSecret(key: string, value: string): void {
  const secrets = loadSecrets();
  secrets[key] = value;
  saveSecrets(secrets);
}

/** Delete a secret by key. */
export function deleteSecret(key: string): boolean {
  const secrets = loadSecrets();
  if (!(key in secrets)) return false;
  delete secrets[key];
  saveSecrets(secrets);
  return true;
}

/**
 * Resolve a secret value with priority: CLI flag > secrets.json > host env var.
 * Returns undefined if not found anywhere.
 */
export function resolveSecret(flagValue: string | undefined, secretKey: string): string | undefined {
  if (flagValue) return flagValue;
  const stored = getSecret(secretKey);
  if (stored) return stored;
  const envVar = KNOWN_SECRETS[secretKey]?.env;
  if (envVar) return process.env[envVar];
  return undefined;
}

/**
 * Find all stored secrets matching a base key (supports dot-suffixed variants).
 * E.g. listSecretVariants("telegram-token") returns:
 *   [{ key: "telegram-token", label: "default", value: "bot123..." },
 *    { key: "telegram-token.cloud", label: "cloud", value: "bot456..." }]
 */
export function listSecretVariants(baseKey: string): Array<{ key: string; label: string; value: string }> {
  const secrets = loadSecrets();
  const results: Array<{ key: string; label: string; value: string }> = [];
  for (const [k, v] of Object.entries(secrets)) {
    if (k === baseKey) {
      results.push({ key: k, label: "default", value: v });
    } else if (k.startsWith(baseKey + ".")) {
      results.push({ key: k, label: k.slice(baseKey.length + 1), value: v });
    }
  }
  return results;
}
