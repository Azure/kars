// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Shared credential management for kars CLI.
 * Used by `dev` (auto-prompts if missing) and `credentials` (explicit reconfigure).
 */

import chalk from "chalk";
import ora from "ora";
import inquirer from "inquirer";
import { mkdirSync, writeFileSync, chmodSync, existsSync, readFileSync } from "fs";
import { execFileSync } from "child_process";
import { join } from "path";
import { homedir } from "os";
import {
  GITHUB_MODELS_ENDPOINT,
  fetchCatalog,
  buildCuratedChoices,
  buildAllToolCapableChoices,
  validateModelAgainstCatalog,
  detectGhAccounts,
  getGhToken,
  normalizeSecretValue,
  PAT_CREATE_URL,
  type GhAccount,
} from "./github-models.js";
import {
  COPILOT_API_ENDPOINT,
  COPILOT_MODELS,
  buildCopilotChoices,
  checkCopilotEligibility,
  copilotDeviceLogin,
} from "./github-copilot.js";

const CONFIG_DIR = join(homedir(), ".kars");
const CONFIG_FILE = join(CONFIG_DIR, "config.json");
const CREDENTIALS_FILE = join(CONFIG_DIR, "credentials");
const SECRETS_FILE = join(CONFIG_DIR, "secrets.json");

/** Well-known secret keys and their descriptions */
export const KNOWN_SECRETS: Record<string, { env: string; label: string }> = {
  "azure-openai-key":  { env: "AZURE_OPENAI_API_KEY", label: "Inference API key (Azure OpenAI key OR GitHub PAT)" },
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

export interface KarsConfig {
  endpoint: string;
  model: string;
  apiKey: string;
  foundryProjectEndpoint?: string;
  /**
   * Inference provider. "foundry" (default) routes inference at an Azure AI
   * Foundry / Azure OpenAI resource via api-key or Workload Identity.
   * "github-models" routes at https://models.github.ai/inference using a
   * GitHub PAT — no Azure subscription needed; Foundry-only features
   * (Memory Store, agents, evaluations, indexes, Content Safety inline)
   * are unavailable in this mode.
   * "github-copilot" routes at https://api.githubcopilot.com using a
   * Copilot subscription's gh OAuth/PAT token. Lets agents reach Claude /
   * GPT-5 / Gemini-2.5 with the full upstream context window (no GH
   * Models 16k cap that triggers OpenClaw auto-compaction).
   */
  provider?: "foundry" | "github-models" | "github-copilot";
  /**
   * Set to true once the first-time setup banner has been completed.
   * Used by `kars dev` to decide whether to show the welcome flow.
   * Toggle via `kars config reset --first-run` to retest the UX.
   */
  firstRunCompleted?: boolean;
}

/**
 * Up phases tracked for auto-resume. Persisted in DeploymentContext.phase
 * after each completed phase; cleared (set to "complete") after a successful run.
 */
export type UpPhase =
  | "rg"
  | "infra"
  | "network"
  | "kubectl"
  | "images"
  | "helm"
  | "mesh"
  | "sandbox"
  | "complete";

/** Cached deployment context — saved incrementally during `kars up` */
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
  /** AgentMesh registry mode: "local" (default) or "global" */
  registryMode?: string;
  /** External registry URL when registryMode is "global" */
  globalRegistryUrl?: string;
  /** External relay URL when registryMode is "global" */
  globalRelayUrl?: string;
  /** How the registry was promoted: "port-forward" | "loadbalancer" */
  promoteMode?: string;
  /**
   * Last completed phase of `kars up`. Used by auto-resume to skip
   * already-done phases on a re-run after a failure. Set to "complete"
   * after a fully successful run.
   */
  phase?: UpPhase;
  /** ISO-8601 timestamp when the current run started (for resume staleness). */
  phaseStartedAt?: string;
  /**
   * Sandbox name from --name. Used to detect topology mismatches that
   * should invalidate a partial-run resume.
   */
  sandboxName?: string;
  /**
   * Source ACR (--source-acr) used for the run. Used to detect topology
   * changes that should invalidate a partial-run resume.
   */
  sourceAcr?: string;
}

const CONTEXT_FILE = join(CONFIG_DIR, "context.json");

/** Load cached deployment context from ~/.kars/context.json */
export function loadContext(): DeploymentContext | null {
  try {
    if (!existsSync(CONTEXT_FILE)) return null;
    return JSON.parse(readFileSync(CONTEXT_FILE, "utf-8"));
  } catch {
    return null;
  }
}

/** Save deployment context to ~/.kars/context.json */
export function saveContext(ctx: DeploymentContext): void {
  mkdirSync(CONFIG_DIR, { recursive: true });
  ctx.savedAt = new Date().toISOString();
  writeFileSync(CONTEXT_FILE, JSON.stringify(ctx, null, 2), "utf-8");
  chmodSync(CONTEXT_FILE, 0o600);
}

/**
 * Load saved config + credentials from ~/.kars/.
 * Returns null if either is missing or corrupt.
 */
export function loadConfig(): KarsConfig | null {
  try {
    if (!existsSync(CONFIG_FILE)) return null;
    const config = JSON.parse(readFileSync(CONFIG_FILE, "utf-8"));
    // API key: secrets.json > legacy credentials file
    const secrets = loadSecrets();
    const apiKey = secrets["azure-openai-key"]
      || (existsSync(CREDENTIALS_FILE) ? readFileSync(CREDENTIALS_FILE, "utf-8").trim() : "");
    if (!config.endpoint || !apiKey) return null;
    const provider: KarsConfig["provider"] =
      config.provider === "github-models"
        ? "github-models"
        : config.provider === "github-copilot"
          ? "github-copilot"
          : "foundry";
    const defaultModel =
      provider === "github-models"
        ? "gpt-4o-mini"
        : provider === "github-copilot"
          ? "claude-opus-4.7"
          : "gpt-4.1";
    return {
      endpoint: config.endpoint,
      model: config.model || defaultModel,
      apiKey,
      foundryProjectEndpoint: config.foundryProjectEndpoint,
      provider,
      firstRunCompleted: config.firstRunCompleted === true,
    };
  } catch {
    return null;
  }
}

/**
 * Interactively prompt for inference credentials, verify them, and save.
 * Leads with a provider choice (Azure AI Foundry / Azure OpenAI vs GitHub
 * Models); the GitHub Models branch only needs a PAT.
 * If existing config is found, shows it as defaults.
 */
export async function promptAndSaveCredentials(options?: {
  /** Skip verification (for offline setup) */
  skipVerify?: boolean;
  /** Heading to show before prompts */
  heading?: string;
  /** Force a specific provider (skip the choice prompt) */
  provider?: "foundry" | "github-models" | "github-copilot";
}): Promise<KarsConfig> {
  const existing = loadConfig();

  if (options?.heading) {
    console.log(chalk.hex("#0078D4")(`\n  ${options.heading}\n`));
  }

  if (existing) {
    const label =
      existing.provider === "github-models"
        ? "GitHub Models"
        : existing.provider === "github-copilot"
          ? "GitHub Copilot"
          : existing.endpoint;
    console.log(chalk.dim(`  Existing config found (${label}). Press Enter to keep.\n`));
  }

  // ── Step 1: provider choice ────────────────────────────────────────
  let provider: "foundry" | "github-models" | "github-copilot";
  if (options?.provider) {
    provider = options.provider;
  } else {
    const providerAnswer = await inquirer.prompt([
      {
        type: "list",
        name: "provider",
        message: "Which inference provider do you want to use?",
        default: "github-copilot",
        choices: [
          {
            name: "GitHub Copilot                    (recommended; needs an active Copilot seat — large context, Claude/GPT/Gemini)",
            value: "github-copilot",
          },
          {
            name: "Azure AI Foundry / Azure OpenAI   (full feature set: Memory Store, agents, Content Safety, etc.)",
            value: "foundry",
          },
          {
            name: "GitHub Models                     (free; just need a GitHub PAT — small context, Foundry features disabled)",
            value: "github-models",
          },
        ],
      },
    ]);
    provider = providerAnswer.provider;
  }

  if (provider === "github-models") {
    return promptGithubModels(existing, options?.skipVerify);
  }
  if (provider === "github-copilot") {
    return promptGithubCopilot(existing, options?.skipVerify);
  }
  return promptFoundry(existing, options?.skipVerify);
}

/**
 * Prompt + verify + save the GitHub Models provider. The PAT lives in the
 * same `azure-openai-key` slot the router reads at /run/secrets/, so no
 * downstream wiring needs to change — only the URL and Bearer-vs-api-key
 * decision (handled in the router based on the endpoint hostname).
 *
 * Flow:
 *   1. Decide PAT source: existing secret → `gh auth` → manual entry.
 *   2. Hit /catalog/models with the candidate PAT to validate scope and
 *      fetch the live model list.
 *   3. Show a curated picker (filtered to tool-capable), with "show all"
 *      and "custom id" escape hatches.
 *   4. Persist config + secret + legacy credentials file (back-compat).
 */
async function promptGithubModels(
  existing: KarsConfig | null,
  skipVerify?: boolean,
): Promise<KarsConfig> {
  const endpoint = GITHUB_MODELS_ENDPOINT;

  // ── Step 1: PAT acquisition ────────────────────────────────────────────
  const apiKey = await acquireGithubPat(existing);

  // ── Step 2: Validate + fetch catalog ───────────────────────────────────
  let catalogModels: ReturnType<typeof Array.prototype.slice> | undefined;
  if (!skipVerify) {
    const spinner = ora({ color: "cyan" }).start("Verifying PAT and fetching model catalog...");
    const result = await fetchCatalog(apiKey);
    if (!result.ok) {
      spinner.fail("GitHub PAT verification failed");
      console.log(chalk.red(`\n  ${result.status || "network"}: ${result.message}\n`));
      if (result.status === 401 || result.status === 403) {
        console.log(chalk.yellow("  The PAT is missing the 'models:read' scope, or has been revoked."));
        console.log(chalk.yellow(`  Create a new token: ${PAT_CREATE_URL}\n`));
      }
      process.exit(1);
    }
    spinner.succeed(`GitHub PAT verified (${result.models.length} models in catalog)`);
    catalogModels = result.models;
  }

  // ── Step 3: Model picker ───────────────────────────────────────────────
  const model = await pickModelInteractive(catalogModels as never, existing?.provider === "github-models" ? existing.model : undefined);

  // ── Step 4: Persist ────────────────────────────────────────────────────
  mkdirSync(CONFIG_DIR, { recursive: true });
  const configObj: Record<string, string | boolean> = {
    endpoint,
    model,
    provider: "github-models",
    version: "0.1.0-alpha.1",
    firstRunCompleted: true,
  };
  writeFileSync(CONFIG_FILE, JSON.stringify(configObj, null, 2), "utf-8");
  chmodSync(CONFIG_FILE, 0o600);
  setSecret("azure-openai-key", apiKey);
  // Legacy credentials file preserved for back-compat with older sandbox
  // images that mount it directly. New `dev` runs build a per-run tempfile
  // so this is a soft compatibility hook only.
  writeFileSync(CREDENTIALS_FILE, apiKey, "utf-8");
  chmodSync(CREDENTIALS_FILE, 0o600);

  return { endpoint, model, apiKey, provider: "github-models", firstRunCompleted: true };
}

/**
 * Prompt + verify + save the GitHub Copilot provider. Reuses the same gh
 * OAuth detection as github-models but probes
 * `https://api.github.com/copilot_internal/v2/token` to confirm an active
 * Copilot subscription before persisting. Stores the gh token in the
 * `azure-openai-key` slot — the router exchanges it for a Copilot JWT at
 * inference time (see `inference-router/src/copilot_auth.rs`).
 */
async function promptGithubCopilot(
  existing: KarsConfig | null,
  skipVerify?: boolean,
): Promise<KarsConfig> {
  const endpoint = COPILOT_API_ENDPOINT;

  // ── Step 1: token acquisition ──────────────────────────────────────────
  // We deliberately do NOT reuse a stock `gh auth login` PAT here — those
  // are missing the Copilot integration scope and 404 on the
  // `copilot_internal/v2/token` exchange. Only an existing Copilot token
  // saved in this CLI's secrets is reusable. Otherwise go straight to the
  // Copilot device-code OAuth flow.
  let apiKey: string | undefined;

  if (existing?.provider === "github-copilot" && existing.apiKey) {
    const { reuse } = await inquirer.prompt([
      {
        type: "confirm",
        name: "reuse",
        message: "Reuse existing Copilot token from previous session?",
        default: true,
      },
    ]);
    if (reuse) apiKey = existing.apiKey;
  }

  if (!apiKey) {
    console.log(chalk.cyan("\n  Copilot uses a one-time browser login (device-code OAuth) — your `gh` PAT can't be reused"));
    console.log(chalk.dim("  because the standard `gh auth login` scope doesn't include the Copilot integration.\n"));

    const { confirm } = await inquirer.prompt([
      {
        type: "confirm",
        name: "confirm",
        message: "Open a browser to authorize kars with your Copilot account?",
        default: true,
      },
    ]);
    if (!confirm) {
      console.log(chalk.yellow("\n  Cancelled. Re-run `kars credentials` when you're ready.\n"));
      process.exit(1);
    }

    try {
      apiKey = await copilotDeviceLogin(async ({ userCode, verificationUri }) => {
        console.log(chalk.cyan("\n  ┌─ GitHub device login ─────────────────────────────────"));
        console.log(chalk.cyan(`  │  1. Open: ${chalk.bold.white(verificationUri)}`));
        console.log(chalk.cyan(`  │  2. Enter code: ${chalk.bold.white(userCode)}`));
        console.log(chalk.cyan("  │  3. Approve the GitHub for VS Code (Copilot) integration"));
        console.log(chalk.cyan("  └───────────────────────────────────────────────────────\n"));
        try {
          const opener = process.platform === "darwin" ? "open" : process.platform === "win32" ? "start" : "xdg-open";
          execFileSync(opener, [verificationUri], { stdio: "ignore" });
        } catch { /* user can copy/paste */ }
      });
    } catch (e) {
      console.log(chalk.red(`\n  Device login failed: ${e instanceof Error ? e.message : String(e)}\n`));
      process.exit(1);
    }
  }

  // ── Step 2: Verify Copilot eligibility ─────────────────────────────────
  if (!skipVerify) {
    const spinner = ora({ color: "cyan" }).start("Verifying Copilot subscription...");
    const result = await checkCopilotEligibility(apiKey);
    if (!result.ok) {
      spinner.fail("Copilot eligibility check failed");
      console.log(chalk.red(`\n  ${result.status ?? "network"}: ${result.message}\n`));
      console.log(chalk.yellow("  Copilot needs an active seat on this GitHub account."));
      console.log(chalk.yellow(`  Subscribe: https://github.com/settings/copilot\n`));
      process.exit(1);
    }
    if (!result.chatEnabled) {
      spinner.warn("Copilot subscription is active but Chat is disabled");
      console.log(chalk.yellow("\n  Inference calls require Copilot Chat. Enable it at https://github.com/settings/copilot/features\n"));
      process.exit(1);
    }
    spinner.succeed(`Copilot verified (${result.plan} plan, ${COPILOT_MODELS.length} models available)`);
  }

  // ── Step 3: Model picker ───────────────────────────────────────────────
  const currentModel =
    existing?.provider === "github-copilot" ? existing.model : COPILOT_MODELS.find((m) => m.recommended)?.id;
  const { model } = await inquirer.prompt([
    {
      type: "list",
      name: "model",
      message: "Pick a Copilot model:",
      default: currentModel,
      pageSize: COPILOT_MODELS.length + 2,
      choices: buildCopilotChoices(currentModel),
    },
  ]);

  // ── Step 4: Persist ────────────────────────────────────────────────────
  mkdirSync(CONFIG_DIR, { recursive: true });
  const configObj: Record<string, string | boolean> = {
    endpoint,
    model,
    provider: "github-copilot",
    version: "0.1.0-alpha.1",
    firstRunCompleted: true,
  };
  writeFileSync(CONFIG_FILE, JSON.stringify(configObj, null, 2), "utf-8");
  chmodSync(CONFIG_FILE, 0o600);
  setSecret("azure-openai-key", apiKey);
  writeFileSync(CREDENTIALS_FILE, apiKey, "utf-8"); // lgtm[js/http-to-file-access] — OAuth token persisted to 0o600 user-private file by design
  chmodSync(CREDENTIALS_FILE, 0o600);

  return { endpoint, model, apiKey, provider: "github-copilot", firstRunCompleted: true };
}

/**
 * Acquire a GitHub PAT for Models access. Tries in order:
 *   1. Existing config (offer to reuse)
 *   2. `gh auth` accounts (single match → auto, multiple → pick)
 *   3. Manual paste
 *
 * Returns the chosen PAT — does NOT validate scope (that's the catalog GET).
 */
async function acquireGithubPat(existing: KarsConfig | null): Promise<string> {
  // The same gh OAuth token (or compatible PAT) works for both Models and
  // Copilot providers — both endpoints accept `Authorization: Bearer <gh>`.
  // Allow reuse across either provider transition.
  const existingPat =
    existing?.provider === "github-models" || existing?.provider === "github-copilot"
      ? existing.apiKey
      : undefined;

  // Probe gh accounts up-front so we can show the right defaults.
  const ghAccounts: GhAccount[] = await detectGhAccounts();

  const choices: Array<{ name: string; value: string }> = [];
  if (existingPat) {
    choices.push({ name: chalk.green("Reuse the PAT already in your config"), value: "existing" });
  }
  if (ghAccounts.length === 1) {
    choices.push({
      name: `Use the OAuth token from gh CLI (${chalk.cyan(ghAccounts[0]!.login)})`,
      value: `gh:${ghAccounts[0]!.login}`,
    });
  } else if (ghAccounts.length > 1) {
    for (const acc of ghAccounts) {
      const tag = acc.active ? chalk.cyan(" (active)") : "";
      choices.push({ name: `Use gh token for ${acc.login}${tag}`, value: `gh:${acc.login}` });
    }
  }
  choices.push({ name: "Paste a personal access token", value: "manual" });

  let answer: string;
  if (choices.length === 1) {
    // No existing PAT, no gh — straight to manual entry.
    answer = "manual";
  } else {
    console.log(chalk.dim(
      `\n  GitHub Models docs: https://docs.github.com/github-models — token needs 'models:read' scope.`,
    ));
    if (ghAccounts.length === 0) {
      console.log(chalk.dim(`  Tip: install GitHub CLI and run \`gh auth login\` to skip this step.\n`));
    } else {
      console.log("");
    }
    const r = await inquirer.prompt([{
      type: "list",
      name: "source",
      message: "Where should the PAT come from?",
      choices,
    }]);
    answer = r.source;
  }

  if (answer === "existing" && existingPat) return existingPat;

  if (answer.startsWith("gh:")) {
    const login = answer.slice(3);
    const tok = await getGhToken(login);
    if (tok) return tok;
    console.log(chalk.yellow(`\n  Could not retrieve token for ${login} — falling back to manual entry.\n`));
  }

  const r = await inquirer.prompt([{
    type: "password",
    name: "apiKey",
    message: "GitHub PAT (must have 'models:read' scope):",
    mask: "•",
    validate: (input: string) => (!input || input.length < 20 ? "PAT is required" : true),
  }]);
  return r.apiKey;
}

/**
 * Show the curated picker, with "show all" and "custom id" escape hatches.
 * `catalog` is undefined when skipVerify was set — fall back to free-text.
 */
async function pickModelInteractive(
  catalog: import("./github-models.js").CatalogModel[] | undefined,
  current?: string,
): Promise<string> {
  if (!catalog || catalog.length === 0) {
    const r = await inquirer.prompt([{
      type: "input",
      name: "model",
      message: "Model id (catalog unavailable — enter manually):",
      default: current ?? "openai/gpt-4.1",
      validate: (v: string) => (v.trim() ? true : "Model id required"),
    }]);
    return r.model.trim();
  }

  const choices = buildCuratedChoices(catalog, current);
  const ans = await inquirer.prompt([{
    type: "list",
    name: "model",
    message: "Pick an inference model:",
    pageSize: 14,
    default: current ?? "openai/gpt-4.1",
    choices: choices.map(c => ({
      name: c.label,
      value: c.value,
      disabled: c.isDivider ? " " : false,
    })),
  }]);

  if (ans.model === "__show_all__") {
    const all = buildAllToolCapableChoices(catalog, current);
    const r = await inquirer.prompt([{
      type: "list",
      name: "model",
      message: `All ${all.filter(c => !c.isDivider).length} tool-capable models:`,
      pageSize: 20,
      choices: all.map(c => ({ name: c.label, value: c.value, disabled: c.isDivider ? " " : false })),
    }]);
    return r.model;
  }

  if (ans.model === "__custom__") {
    const r = await inquirer.prompt([{
      type: "input",
      name: "model",
      message: "Custom model id (e.g. openai/gpt-4.1-mini):",
      default: current,
      validate: (v: string) => {
        const trimmed = v.trim();
        if (!trimmed) return "Model id required";
        const result = validateModelAgainstCatalog(trimmed, catalog);
        if (result.ok) return true;
        if (result.reason === "not-found") {
          return result.suggestion
            ? `Not in catalog. Did you mean: ${result.suggestion}?`
            : `Not in the catalog. Run \`kars config model\` to pick from the list.`;
        }
        return `${trimmed} doesn't support tool calling — agents will fail. Pick a tool-capable model.`;
      },
    }]);
    return r.model.trim();
  }

  return ans.model;
}

/**
 * Validate a stored config's model id against the live catalog. Used by
 * `kars dev` and `config model` to surface invalid entries from a
 * stale config (e.g. user hand-edited config.json or upstream renamed a
 * model).
 *
 * Kept separate from `loadConfig()` so the latter stays sync + offline.
 */
export async function validateGithubModelsConfig(
  config: KarsConfig,
): Promise<{ ok: true } | { ok: false; reason: string; suggestion?: string }> {
  if (config.provider !== "github-models") return { ok: true };
  const result = await fetchCatalog(config.apiKey);
  if (!result.ok) {
    return { ok: false, reason: `Catalog fetch failed (${result.status}): ${result.message}` };
  }
  const v = validateModelAgainstCatalog(config.model, result.models);
  if (v.ok) return { ok: true };
  if (v.reason === "not-found") {
    return { ok: false, reason: `Model '${config.model}' is not in the GitHub Models catalog.`, suggestion: v.suggestion };
  }
  return { ok: false, reason: `Model '${config.model}' does not support tool calling.` };
}

/** Prompt + verify + save the Azure AI Foundry / Azure OpenAI provider. */
async function promptFoundry(
  existing: KarsConfig | null,
  skipVerify?: boolean,
): Promise<KarsConfig> {
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
      message: "Model deployment name (must already exist in your Azure OpenAI resource):",
      default: existing?.model || "gpt-4.1",
    },
    {
      type: "password",
      name: "apiKey",
      message: "Azure OpenAI resource API key (resource-level key, not a per-model key):",
      mask: "•",
      validate: (input: string) => {
        if (!input || input.length < 10) return "API key is required";
        return true;
      },
    },
  ]);

  // Verify (best-effort).
  //
  // We probe the classic Azure OpenAI shape `${endpoint}/openai/deployments/
  // {model}/chat/completions`, which works against both `*.openai.azure.com`
  // and `*.services.ai.azure.com`. It does NOT work against project-scoped
  // Foundry endpoints (`/api/projects/<proj>`), and may fail for users whose
  // model deployment name doesn't match `creds.model` even when their creds
  // are valid for the runtime. We must not `process.exit(1)` here: the
  // unique first-run UX bug is that aborting on verify failure leaves
  // nothing saved, so `kars dev` re-prompts forever for users with
  // non-classic endpoint shapes. Save what we have, warn loudly, and let
  // the runtime surface the real error against the real model at use time.
  if (!skipVerify) {
    const spinner = ora({ color: "cyan" }).start("Verifying credentials...");
    try {
      const response = await fetch(
        `${answers.endpoint}/openai/deployments/${answers.model}/chat/completions?api-version=2024-12-01-preview`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json", "api-key": answers.apiKey },
          body: JSON.stringify({ messages: [{ role: "user", content: "hello" }], max_completion_tokens: 5 }),
        }
      );
      if (!response.ok) {
        const body = await response.text();
        spinner.warn(`Credential verification returned ${response.status} — saving anyway`);
        console.log(chalk.dim(`    ${body.length > 240 ? body.slice(0, 240) + "…" : body}`));
        console.log(
          chalk.yellow(
            "  Note: the verify probe uses the classic AOAI deployments path. " +
              "If your endpoint is a Foundry project URL (.../api/projects/<name>) " +
              "this 404/401 is expected — your creds may still be valid at runtime.",
          ),
        );
      } else {
        spinner.succeed("Credentials verified");
      }
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      spinner.warn(`Could not reach endpoint — saving anyway (${msg})`);
      console.log(
        chalk.yellow(
          "  Note: network/TLS error during verify. Creds were saved; re-run " +
            "`kars credentials` if you need to correct them.",
        ),
      );
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
  const configObj: Record<string, string | boolean> = {
    endpoint: answers.endpoint,
    model: answers.model,
    provider: "foundry",
    version: "0.1.0-alpha.1",
    firstRunCompleted: true,
  };
  if (foundryProjectEndpoint) configObj.foundryProjectEndpoint = foundryProjectEndpoint;
  writeFileSync(CONFIG_FILE, JSON.stringify(configObj, null, 2), "utf-8");
  chmodSync(CONFIG_FILE, 0o600);
  // Store API key in unified secrets.json (also keeps legacy file for compat)
  setSecret("azure-openai-key", answers.apiKey);
  writeFileSync(CREDENTIALS_FILE, answers.apiKey, "utf-8");
  chmodSync(CREDENTIALS_FILE, 0o600);

  return {
    endpoint: answers.endpoint,
    model: answers.model,
    apiKey: answers.apiKey,
    foundryProjectEndpoint: foundryProjectEndpoint || undefined,
    provider: "foundry",
    firstRunCompleted: true,
  };
}

/**
 * Ensure credentials are available — load from disk or prompt interactively.
 * This is the main entry point for commands that need creds.
 */
export async function ensureCredentials(): Promise<KarsConfig> {
  const existing = loadConfig();
  if (existing) return existing;

  console.log(chalk.yellow("\n  No inference credentials found. Let's set them up:\n"));
  return promptAndSaveCredentials();
}

export { CONFIG_DIR, CONFIG_FILE, CREDENTIALS_FILE, SECRETS_FILE };

/**
 * Reset the `firstRunCompleted` flag so `kars dev` re-shows the
 * welcome banner on the next run. Leaves credentials and other config
 * in place. Returns true if the flag was cleared, false if no config
 * file exists.
 */
export function resetFirstRunFlag(): boolean {
  let raw: string;
  try {
    raw = readFileSync(CONFIG_FILE, "utf-8");
  } catch {
    return false;
  }
  try {
    const config = JSON.parse(raw);
    delete config.firstRunCompleted;
    writeFileSync(CONFIG_FILE, JSON.stringify(config, null, 2), "utf-8");
    chmodSync(CONFIG_FILE, 0o600);
    return true;
  } catch {
    return false;
  }
}

/**
 * Set the `firstRunCompleted` flag without re-prompting. Used when the
 * user pre-configured credentials via `kars credentials` and then
 * runs `kars dev` for the first time — we want to skip the
 * credentials sub-prompt of the welcome flow but mark first-run done
 * so subsequent runs don't re-display the welcome.
 */
export function markFirstRunCompleted(): boolean {
  let raw: string;
  try {
    raw = readFileSync(CONFIG_FILE, "utf-8");
  } catch {
    return false;
  }
  try {
    const config = JSON.parse(raw);
    config.firstRunCompleted = true;
    writeFileSync(CONFIG_FILE, JSON.stringify(config, null, 2), "utf-8");
    chmodSync(CONFIG_FILE, 0o600);
    return true;
  } catch {
    return false;
  }
}

// ─── Secrets store (~/.kars/secrets.json) ────────────────────────────────

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

/** Save secrets to ~/.kars/secrets.json with mode 600. */
export function saveSecrets(secrets: Record<string, string>): void {
  mkdirSync(CONFIG_DIR, { recursive: true });
  writeFileSync(SECRETS_FILE, JSON.stringify(secrets, null, 2), "utf-8");
  chmodSync(SECRETS_FILE, 0o600);
}

/** Get a single secret value by key. */
export function getSecret(key: string): string | undefined {
  return loadSecrets()[key];
}

/** Set a single secret value by key. Applies key-specific normalization. */
export function setSecret(key: string, value: string): void {
  const secrets = loadSecrets();
  secrets[key] = normalizeSecretValue(key, value);
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
  // If no exact match, check for dot-suffixed variants (e.g. telegram-token.dev)
  const variants = listSecretVariants(secretKey);
  if (variants.length > 0) return variants[0].value;
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
