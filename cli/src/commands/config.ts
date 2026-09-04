// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * `kars config` — local CLI configuration management.
 *
 * Distinct from `kars model set <sandbox> <model>`, which hot-swaps
 * the inference model on a *running AKS sandbox* via InferencePolicy CR.
 * This command tree only touches local files under `~/.kars/`.
 *
 * Subcommands:
 *   - `config show`            Print effective config + provider info.
 *   - `config model [id]`      Pick or set the local default model. Validates
 *                              against the live GitHub Models catalog.
 *   - `config reset [--flag]`  Wipe local config; optionally also wipe
 *                              secrets and/or context. Always confirms first.
 */

import { Command } from "commander";
import chalk from "chalk";
import inquirer from "inquirer";
import { existsSync, unlinkSync, writeFileSync, chmodSync, mkdirSync, readFileSync } from "fs";

import {
  loadConfig, saveContext, saveSecrets, resetFirstRunFlag,
  CONFIG_DIR, CONFIG_FILE, CREDENTIALS_FILE, SECRETS_FILE,
  type DeploymentContext,
  validateGithubModelsConfig,
} from "../config.js";
import {
  fetchCatalog, buildCuratedChoices, buildAllToolCapableChoices,
  validateModelAgainstCatalog,
} from "../github-models.js";
import { buildCopilotChoices } from "../github-copilot.js";

export function configCommand(): Command {
  const cmd = new Command("config")
    .description("Manage local CLI configuration (~/.kars/)");

  cmd.addCommand(showCmd());
  cmd.addCommand(modelCmd());
  cmd.addCommand(adoptAksCmd());
  cmd.addCommand(resetCmd());

  return cmd;
}

function showCmd(): Command {
  return new Command("show")
    .description("Display the effective local configuration")
    .action(async () => {
      const cfg = loadConfig();
      console.log();
      if (!cfg) {
        console.log(chalk.yellow("  No configuration found."));
        console.log(chalk.dim(`  Run ${chalk.cyan("kars dev")} or ${chalk.cyan("kars credentials")} to set one up.\n`));
        return;
      }
      console.log(chalk.bold("  Local configuration"));
      console.log(`    Provider:  ${chalk.cyan(cfg.provider ?? "foundry")}`);
      console.log(`    Endpoint:  ${cfg.endpoint}`);
      console.log(`    Model:     ${cfg.model}`);
      if (cfg.foundryProjectEndpoint) {
        console.log(`    Project:   ${cfg.foundryProjectEndpoint}`);
      }
      console.log(chalk.dim(`    File:      ${CONFIG_FILE}`));
      console.log();

      // For GitHub Models, validate that the saved model is still in the
      // catalog and tool-capable. Dev-time UX: catches stale configs early.
      if (cfg.provider === "github-models") {
        process.stdout.write(chalk.dim("  Validating against live catalog... "));
        const result = await validateGithubModelsConfig(cfg);
        if (result.ok) {
          console.log(chalk.green("ok"));
        } else {
          console.log(chalk.red("invalid"));
          console.log(chalk.yellow(`    ${result.reason}`));
          if ("suggestion" in result && result.suggestion) {
            console.log(chalk.dim(`    Suggestion: ${result.suggestion}`));
          }
          console.log(chalk.dim(`    Fix with: ${chalk.cyan("kars config model")}`));
        }
        console.log();
      }
    });
}

export function buildAdoptedAksContext(input: {
  subscription: string;
  region: string;
  resourceGroup: string;
  cluster: string;
  acrLoginServer: string;
  wiClientId?: string;
  identityName?: string;
  identityResourceGroup?: string;
  oidcIssuerUrl?: string;
  foundryEndpoint?: string;
  foundryProjectEndpoint?: string;
  keyVaultName?: string;
}): DeploymentContext {
  const acrLoginServer = input.acrLoginServer.trim().replace(/^https?:\/\//, "").replace(/\/+$/, "");
  if (!acrLoginServer.endsWith(".azurecr.io")) {
    throw new Error("--acr-login-server must be an Azure Container Registry host (*.azurecr.io)");
  }
  return {
    subscription: input.subscription.trim(),
    region: input.region.trim(),
    resourceGroup: input.resourceGroup.trim(),
    aksCluster: input.cluster.trim(),
    acrLoginServer,
    acrName: acrLoginServer.slice(0, -".azurecr.io".length),
    wiClientId: input.wiClientId?.trim() || undefined,
    identityName: input.identityName?.trim() || undefined,
    identityResourceGroup: input.identityResourceGroup?.trim() || input.resourceGroup.trim(),
    oidcIssuerUrl: input.oidcIssuerUrl?.trim() || undefined,
    foundryEndpoint: input.foundryEndpoint?.trim() || undefined,
    foundryProjectEndpoint: input.foundryProjectEndpoint?.trim() || undefined,
    keyVaultName: input.keyVaultName?.trim() || undefined,
    registryMode: "local",
    phase: "complete",
  };
}

function adoptAksCmd(): Command {
  return new Command("adopt-aks")
    .description("Register an existing Helm-installed AKS cluster with this CLI")
    .requiredOption("--subscription <id>", "Azure subscription ID")
    .requiredOption("--region <region>", "AKS region")
    .requiredOption("--resource-group <name>", "AKS resource group")
    .requiredOption("--cluster <name>", "AKS cluster name")
    .requiredOption("--acr-login-server <host>", "ACR login server, e.g. contoso.azurecr.io")
    .option("--context <name>", "Kubernetes context (defaults to current context)")
    .option("--wi-client-id <id>", "Kars controller Workload Identity client ID")
    .option("--identity-name <name>", "Kars managed identity name")
    .option("--identity-resource-group <name>", "Managed identity resource group")
    .option("--oidc-issuer-url <url>", "AKS OIDC issuer URL")
    .option("--foundry-endpoint <url>", "Foundry/Azure OpenAI endpoint")
    .option("--foundry-project-endpoint <url>", "Foundry project endpoint")
    .option("--key-vault-name <name>", "Key Vault name")
    .action(async (options) => {
      const { execa } = await import("execa");
      const kubectlContext = options.context ? ["--context", options.context] : [];
      await execa(
        "kubectl",
        [
          ...kubectlContext,
          "get",
          "crd",
          "karssandboxes.kars.azure.com",
          "--output",
          "name",
        ],
        { stdio: "pipe" },
      );
      const helmContext = options.context ? ["--kube-context", options.context] : [];
      await execa("helm", [...helmContext, "status", "kars", "--namespace", "kars-system"], {
        stdio: "pipe",
      });

      const context = buildAdoptedAksContext({
        subscription: options.subscription,
        region: options.region,
        resourceGroup: options.resourceGroup,
        cluster: options.cluster,
        acrLoginServer: options.acrLoginServer,
        wiClientId: options.wiClientId,
        identityName: options.identityName,
        identityResourceGroup: options.identityResourceGroup,
        oidcIssuerUrl: options.oidcIssuerUrl,
        foundryEndpoint: options.foundryEndpoint,
        foundryProjectEndpoint: options.foundryProjectEndpoint,
        keyVaultName: options.keyVaultName,
      });
      saveContext(context);
      console.log(chalk.green("\n  ✔ Existing AKS installation registered with the Kars CLI."));
      console.log(chalk.dim(`    Cluster: ${context.aksCluster} (${context.resourceGroup})`));
      console.log(chalk.dim(`    ACR:     ${context.acrLoginServer}`));
      console.log(chalk.dim(`    Context: ~/.kars/context.json\n`));
    });
}

function modelCmd(): Command {
  return new Command("model")
    .description("Pick or set the local default inference model")
    .argument("[model-id]", "Model id to set (e.g. openai/gpt-4.1). Omit to pick interactively.")
    .action(async (modelId: string | undefined) => {
      const cfg = loadConfig();
      if (!cfg) {
        console.log(chalk.yellow("\n  No configuration found. Run `kars credentials` first.\n"));
        process.exit(1);
      }
      if (cfg.provider === "github-copilot") {
        console.log(chalk.yellow("\n  `config model` for Copilot: pick from the curated catalog.\n"));
        const { model } = await inquirer.prompt([
          {
            type: "list",
            name: "model",
            message: "Pick a Copilot model:",
            default: cfg.model,
            pageSize: 12,
            choices: buildCopilotChoices(cfg.model),
          },
        ]);
        const next = { ...cfg, model };
        delete (next as Record<string, unknown>).apiKey;
        mkdirSync(CONFIG_DIR, { recursive: true });
        writeFileSync(CONFIG_FILE, JSON.stringify({ ...next, version: "0.1.0-alpha.1" }, null, 2), "utf-8");
        chmodSync(CONFIG_FILE, 0o600);
        console.log(chalk.green(`  ✔ Default model set to ${chalk.cyan(model)}`));
        console.log(chalk.dim(`    File: ${CONFIG_FILE}\n`));
        return;
      }
      if (cfg.provider !== "github-models") {
        console.log(chalk.yellow("\n  `config model` currently only manages GitHub Models / GitHub Copilot entries."));
        console.log(chalk.dim("  For Foundry models, edit the deployment in your Foundry project."));
        console.log(chalk.dim("  For running sandboxes, use `kars model set <sandbox> <model>`.\n"));
        process.exit(1);
      }

      // Always re-fetch the catalog — model availability changes upstream.
      console.log();
      const result = await fetchCatalog(cfg.apiKey);
      if (!result.ok) {
        console.log(chalk.red(`  Catalog fetch failed (${result.status}): ${result.message}`));
        if (result.status === 401 || result.status === 403) {
          console.log(chalk.yellow("  Your PAT no longer has 'models:read'. Re-run `kars credentials`."));
        }
        console.log();
        process.exit(1);
      }

      let chosen: string;
      if (modelId) {
        const v = validateModelAgainstCatalog(modelId, result.models);
        if (!v.ok) {
          console.log(chalk.red(`  ${modelId}: ${v.reason === "not-found" ? "not in catalog" : "doesn't support tool calling"}`));
          if (v.suggestion) console.log(chalk.dim(`  Did you mean: ${chalk.cyan(v.suggestion)}?`));
          console.log();
          process.exit(1);
        }
        chosen = modelId;
      } else {
        chosen = await pickModel(result.models, cfg.model);
      }

      const next = { ...cfg, model: chosen };
      delete (next as Record<string, unknown>).apiKey;
      mkdirSync(CONFIG_DIR, { recursive: true });
      writeFileSync(CONFIG_FILE, JSON.stringify({ ...next, version: "0.1.0-alpha.1" }, null, 2), "utf-8");
      chmodSync(CONFIG_FILE, 0o600);
      console.log(chalk.green(`  ✔ Default model set to ${chalk.cyan(chosen)}`));
      console.log(chalk.dim(`    File: ${CONFIG_FILE}\n`));
    });
}

async function pickModel(catalog: import("../github-models.js").CatalogModel[], current: string): Promise<string> {
  const choices = buildCuratedChoices(catalog, current);
  const ans = await inquirer.prompt([{
    type: "list",
    name: "model",
    message: "Pick a model:",
    pageSize: 14,
    default: current,
    choices: choices.map(c => ({
      name: c.label, value: c.value, disabled: c.isDivider ? " " : false,
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
      message: "Custom model id:",
      default: current,
      validate: (v: string) => {
        const t = v.trim();
        if (!t) return "Required";
        const result = validateModelAgainstCatalog(t, catalog);
        if (result.ok) return true;
        return result.reason === "not-found"
          ? `Not in catalog${result.suggestion ? ` — did you mean ${result.suggestion}?` : ""}`
          : `${t} doesn't support tool calling`;
      },
    }]);
    return r.model.trim();
  }
  return ans.model;
}

function resetCmd(): Command {
  return new Command("reset")
    .description("Wipe local configuration. Default scope: inference config only.")
    .option("--first-run", "Only clear the first-run-completed flag (re-show the welcome banner). Keeps creds.", false)
    .option("--credentials", "Also wipe stored secrets (channels, search APIs, PAT)", false)
    .option("--all", "Wipe everything: config, secrets, and deployment context", false)
    .option("-y, --yes", "Skip confirmation prompt", false)
    .action(async (opts: { firstRun?: boolean; credentials?: boolean; all?: boolean; yes?: boolean }) => {
      // --first-run: lightweight toggle, no destructive cleanup.
      if (opts.firstRun) {
        const ok = resetFirstRunFlag();
        if (!ok) {
          console.log(chalk.dim("\n  No config file found — nothing to reset.\n"));
          return;
        }
        console.log(chalk.green(`\n  ✔ First-run flag cleared.`));
        console.log(chalk.dim(`  Next ${chalk.cyan("kars dev")} will re-show the welcome banner.\n`));
        return;
      }

      const wipeSecrets = !!opts.credentials || !!opts.all;
      const wipeContext = !!opts.all;

      const targets: string[] = [];
      if (existsSync(CONFIG_FILE)) targets.push("config.json (inference config)");
      if (existsSync(CREDENTIALS_FILE)) targets.push("credentials (legacy file)");
      if (wipeSecrets && existsSync(SECRETS_FILE)) {
        // Show count to make the destructive scope concrete.
        try {
          const n = Object.keys(JSON.parse(readFileSync(SECRETS_FILE, "utf-8"))).length;
          targets.push(`secrets.json (${n} secret${n === 1 ? "" : "s"})`);
        } catch {
          targets.push("secrets.json");
        }
      }
      if (wipeContext && existsSync(`${CONFIG_DIR}/context.json`)) targets.push("context.json (deployment context)");

      if (targets.length === 0) {
        console.log(chalk.dim("\n  Nothing to remove — config files don't exist.\n"));
        return;
      }

      console.log(chalk.yellow("\n  This will permanently delete:"));
      for (const t of targets) console.log(`    - ${t}`);
      console.log();

      if (!opts.yes) {
        const { confirm } = await inquirer.prompt([{
          type: "confirm",
          name: "confirm",
          message: "Continue?",
          default: false,
        }]);
        if (!confirm) {
          console.log(chalk.dim("  Cancelled.\n"));
          return;
        }
      }

      const removed: string[] = [];
      try { if (existsSync(CONFIG_FILE))      { unlinkSync(CONFIG_FILE);      removed.push(CONFIG_FILE); } } catch { /* ignore */ }
      try { if (existsSync(CREDENTIALS_FILE)) { unlinkSync(CREDENTIALS_FILE); removed.push(CREDENTIALS_FILE); } } catch { /* ignore */ }
      if (wipeSecrets) {
        try { if (existsSync(SECRETS_FILE)) { unlinkSync(SECRETS_FILE); removed.push(SECRETS_FILE); } } catch { /* ignore */ }
        // Also clear in-memory shape so subsequent calls in the same process see []
        try { saveSecrets({}); unlinkSync(SECRETS_FILE); } catch { /* file already gone */ }
      }
      if (wipeContext) {
        const ctx = `${CONFIG_DIR}/context.json`;
        try { if (existsSync(ctx)) { unlinkSync(ctx); removed.push(ctx); } } catch { /* ignore */ }
      }

      console.log(chalk.green(`  ✔ Removed ${removed.length} file${removed.length === 1 ? "" : "s"}.`));
      console.log(chalk.dim(`  Re-run ${chalk.cyan("kars dev")} to start fresh.\n`));
    });
}
