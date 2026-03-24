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

export interface AzureClawConfig {
  endpoint: string;
  model: string;
  apiKey: string;
  foundryProjectEndpoint?: string;
}

/**
 * Load saved config + credentials from ~/.azureclaw/.
 * Returns null if either is missing or corrupt.
 */
export function loadConfig(): AzureClawConfig | null {
  try {
    if (!existsSync(CONFIG_FILE) || !existsSync(CREDENTIALS_FILE)) return null;
    const config = JSON.parse(readFileSync(CONFIG_FILE, "utf-8"));
    const apiKey = readFileSync(CREDENTIALS_FILE, "utf-8").trim();
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

export { CONFIG_DIR, CONFIG_FILE, CREDENTIALS_FILE };
