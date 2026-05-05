// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Sub-slice S15.d.2 of S15.d phase2-hotspot-up-cli.
//
// Preflight phase extracted verbatim from cli/src/commands/up.ts:
// auto-detect dev mode, prefill from cached context, banner + tool
// checks, Azure auth + subscription, interactive prompts
// (region/name/isolation/backend), RBAC + provider preflight, SKU
// availability check, and the --dry-run plan print.
//
// Contract:
//   - Mutates `options` in place (cached context, interactive prompts).
//   - Returns `null` when --dry-run was taken (caller must `return` and
//     skip deploy).
//   - Returns `{ rg }` when preflight passed and caller should proceed
//     to the production-deploy section.
//
// Symbol surface preserved verbatim:
//   - banner / checkLine / Stepper from "../../stepper.js"
//   - loadContext from "../../config.js"
//   - runPreflightChecks from "../../preflight.js"
//   - chalk / execa / inquirer / existsSync ("fs")
import chalk from "chalk";
import { existsSync } from "fs";
import { banner, checkLine } from "../../stepper.js";
import { loadContext } from "../../config.js";
import { runPreflightChecks } from "../../preflight.js";

export function isValidAzureHost(url: string, expectedSuffix: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.hostname === expectedSuffix || parsed.hostname.endsWith(`.${expectedSuffix}`);
  } catch {
    return false;
  }
}

export interface UpOptionsForPreflight {
  name: string;
  model: string;
  region: string;
  isolation: string;
  resourceGroup?: string;
  foundryEndpoint?: string;
  openaiEndpoint?: string;
  build: boolean;
  sourceAcr: string;
  dryRun: boolean;
  skipInfra: boolean;
  skipPreflight: boolean;
  [key: string]: unknown;
}

export interface PreflightResult {
  /** Resolved resource group name; reused throughout the production deploy. */
  rg: string;
}

/**
 * Run all preflight checks. Returns null when --dry-run was taken (caller
 * must early-return), otherwise returns the derived `rg` for the production
 * deploy. May call `process.exit(1)` on hard failures.
 */
export async function runPreflight(options: UpOptionsForPreflight): Promise<PreflightResult | null> {
  const blue = chalk.hex("#0078D4");
  const { default: inquirer } = await import("inquirer");
  const { execa } = await import("execa");

  // Auto-detect developer mode: if running from the repo (Dockerfile exists), default to --build
  if (!options.build && !process.argv.includes("--source-acr")) {
    const repoRoot = new URL("../../../..", import.meta.url).pathname;
    if (existsSync(`${repoRoot}/inference-router/Dockerfile`) || existsSync("inference-router/Dockerfile")) {
      options.build = true;
    }
  }

  // ── Pre-fill from cached deployment context ────────────────────
  // If a previous `azureclaw up` saved context, use those values as
  // defaults so the user isn't re-prompted for everything.
  const cachedCtx = loadContext();
  if (cachedCtx && cachedCtx.region) {
    console.log(chalk.dim(`\n  Using cached deployment context (${cachedCtx.region}/${cachedCtx.resourceGroup || "default"}). Pass explicit flags to override.\n`));
    const hasFlag = (f: string) => process.argv.includes(f);
    if (cachedCtx.region && !hasFlag("--region"))
      options.region = cachedCtx.region;
    if (cachedCtx.resourceGroup && !hasFlag("-g") && !hasFlag("--resource-group"))
      options.resourceGroup = cachedCtx.resourceGroup;
    if (cachedCtx.foundryEndpoint && !hasFlag("--foundry-endpoint") && !hasFlag("--openai-endpoint"))
      options.openaiEndpoint = options.openaiEndpoint || cachedCtx.foundryEndpoint;
    if (cachedCtx.foundryProjectEndpoint && !hasFlag("--foundry-endpoint"))
      options.foundryEndpoint = options.foundryEndpoint || cachedCtx.foundryProjectEndpoint;
  }

  // ══════════════════════════════════════════════════════════════
  //  PREFLIGHT: validate everything before touching Azure
  // ══════════════════════════════════════════════════════════════

  if (!options.dryRun) {
    banner("AzureClaw · Preflight Check", "Validating environment before deployment");
  } else {
    console.log(chalk.dim("  Preflight validation...\n"));
  }

  // ── 1. Check required CLI tools ────────────────────────────────
  const tools: { cmd: string; args: string[]; label: string; required: boolean }[] = [
    { cmd: "az", args: ["--version"], label: "Azure CLI", required: true },
    { cmd: "kubectl", args: ["version", "--client"], label: "kubectl", required: true },
    { cmd: "helm", args: ["version", "--short"], label: "Helm", required: true },
    { cmd: "docker", args: ["info", "--format", "{{.ServerVersion}}"], label: "Docker", required: options.build },
  ];

  {
    let allToolsOk = true;
    for (const tool of tools) {
      try {
        await execa(tool.cmd, tool.args, { stdio: "pipe", timeout: 10000 });
        checkLine(true, `${tool.label} — found`);
      } catch {
        if (tool.required) {
          checkLine(false, `${tool.label} — ${chalk.red("not found")} (required)`);
          allToolsOk = false;
        } else {
          checkLine(false, `${tool.label} — not found (optional${tool.cmd === "docker" ? ", needed for --build" : ""})`);
        }
      }
    }
    if (!allToolsOk) {
      console.log(chalk.red("\n  Missing required tools. Install them and try again.\n"));
      process.exit(1);
    }
  }

  // ── 2. Azure auth + subscription ───────────────────────────────
  {
    let isLoggedIn = false;
    try {
      await execa("az", ["account", "show", "--output", "none"], { stdio: "pipe" });
      isLoggedIn = true;
      checkLine(true, "Azure CLI — logged in");
    } catch { /* not logged in */ }

    if (!isLoggedIn) {
      if (options.dryRun) {
        checkLine(false, "Azure CLI — not logged in");
      } else {
        console.log(chalk.yellow("\n  Not logged into Azure. Opening browser for login...\n"));
        await execa("az", ["login"], { stdio: "inherit" });
      }
    }

    if (isLoggedIn) {
      // Get current subscription (read-only)
      const { stdout: subJson } = await execa("az", [
        "account", "show", "--output", "json",
      ], { stdio: "pipe" });
      const currentSub = JSON.parse(subJson);

      if (options.dryRun) {
        checkLine(true, `Subscription — ${currentSub.name}`);
      } else {
        // List all subscriptions to check for multiples
        const { stdout: subsJson } = await execa("az", [
          "account", "list", "--query", "[?state=='Enabled']", "--output", "json",
        ], { stdio: "pipe" });
        const subs = JSON.parse(subsJson) as { id: string; name: string; isDefault: boolean }[];

        if (subs.length > 1) {
          // Multiple subscriptions — let user confirm or pick
          const subChoices = subs.map((s) => ({
            name: `${s.name} (${s.id.slice(0, 8)}...)${s.isDefault ? chalk.dim(" ← default") : ""}`,
            value: s.id,
          }));

          const { subId } = await inquirer.prompt([{
            type: "list",
            name: "subId",
            message: "Which Azure subscription?",
            choices: subChoices,
            default: currentSub.id,
          }]);

          if (subId !== currentSub.id) {
            await execa("az", ["account", "set", "--subscription", subId], { stdio: "pipe" });
            const selected = subs.find((s) => s.id === subId);
            checkLine(true, `Subscription — ${selected?.name || subId}`);
          } else {
            checkLine(true, `Subscription — ${currentSub.name}`);
          }
        } else {
          checkLine(true, `Subscription — ${currentSub.name}`);
        }
      }
    }
  }

  // ── 3. Interactive prompts for region/name/isolation ────────────
  // If a cached context provided values, treat them as "user-provided"
  // so the user isn't re-prompted for details they already set.
  const userProvidedRegion = process.argv.includes("--region") || !!cachedCtx?.region;
  const userProvidedName = process.argv.includes("--name");
  const userProvidedIsolation = process.argv.includes("--isolation");

  if (!options.dryRun && (!userProvidedRegion || !userProvidedName || !userProvidedIsolation)) {
    console.log();

    if (!userProvidedRegion) {
      const { region } = await inquirer.prompt([{
        type: "list" as const,
        name: "region",
        message: "Azure region:",
        choices: [
          { name: "East US 2 (Recommended)", value: "eastus2" },
          { name: "West US 3", value: "westus3" },
          { name: "Central US", value: "centralus" },
          { name: "West Europe", value: "westeurope" },
          { name: "North Europe", value: "northeurope" },
          { name: "UK South", value: "uksouth" },
          { name: "Southeast Asia", value: "southeastasia" },
          { name: "Australia East", value: "australiaeast" },
          new inquirer.Separator(),
          { name: "Other (type region name)", value: "__other__" },
        ],
        default: options.region,
      }]);

      if (region === "__other__") {
        const { customRegion } = await inquirer.prompt([{
          type: "input" as const,
          name: "customRegion",
          message: "Enter Azure region name:",
          validate: (input: string) => input.length > 0 || "Region is required",
        }]);
        options.region = customRegion;
      } else {
        options.region = region;
      }
    }

    if (!userProvidedName) {
      const { name } = await inquirer.prompt([{
        type: "input" as const,
        name: "name",
        message: "Sandbox agent name:",
        default: options.name,
        validate: (input: string) => /^[a-z0-9][a-z0-9-]*$/.test(input) || "Lowercase letters, numbers, and hyphens only",
      }]);
      options.name = name;
    }

    if (!userProvidedIsolation) {
      const { isolation } = await inquirer.prompt([{
        type: "list" as const,
        name: "isolation",
        message: "Pod isolation level:",
        choices: [
          { name: "Enhanced — runc + strict seccomp + read-only rootfs (Recommended)", value: "enhanced" },
          { name: "Confidential — Kata VM isolation (hardware-backed TEE)", value: "confidential" },
          { name: "Standard — runc with default seccomp (minimal)", value: "standard" },
        ],
        default: options.isolation,
      }]);
      options.isolation = isolation;
    }

    // Ask about existing Foundry endpoint (skip AOAI deployment if provided)
    if (!options.foundryEndpoint && !process.argv.includes("--foundry-endpoint")) {
      const { backendChoice } = await inquirer.prompt([{
        type: "list" as const,
        name: "backendChoice",
        message: "Azure AI backend:",
        choices: [
          { name: "Connect to an existing Foundry project (Recommended)", value: "foundry" },
          { name: "Connect to an existing Azure OpenAI resource (no Foundry)", value: "openai" },
          { name: "Deploy a new Azure OpenAI resource (adds ~5 min)", value: "deploy" },
        ],
      }]);
      if (backendChoice === "foundry") {
        const { endpoint } = await inquirer.prompt([{
          type: "input" as const,
          name: "endpoint",
          message: "Foundry project endpoint (e.g. https://<name>.services.ai.azure.com/api/projects/<project>):",
          validate: (input: string) => {
            if (!input.startsWith("https://")) return "Must be an https:// URL";
            if (!isValidAzureHost(input, "services.ai.azure.com")) return "Expected services.ai.azure.com URL. For openai.azure.com, choose 'Azure OpenAI endpoint only'.";
            return true;
          },
        }]);
        options.foundryEndpoint = endpoint;
        // Derive OpenAI inference endpoint from Foundry resource name
        const match = endpoint.match(/https:\/\/([^.]+)\.services\.ai\.azure\.com/);
        if (match && !options.openaiEndpoint) {
          options.openaiEndpoint = `https://${match[1]}.openai.azure.com`;
          console.log(chalk.dim(`  → Derived OpenAI inference endpoint: ${options.openaiEndpoint}`));
        }
      } else if (backendChoice === "openai") {
        const { endpoint } = await inquirer.prompt([{
          type: "input" as const,
          name: "endpoint",
          message: "Azure OpenAI endpoint (*.openai.azure.com):",
          validate: (input: string) => {
            if (!input.startsWith("https://")) return "Must be an https:// URL";
            if (!isValidAzureHost(input, "openai.azure.com")) return "Expected openai.azure.com URL. For Foundry, choose the Foundry option.";
            return true;
          },
        }]);
        // Treat as the inference endpoint, not a Foundry project
        options.openaiEndpoint = endpoint.replace(/\/openai\/v1\/?$/, "");
        options.foundryEndpoint = endpoint.replace(/\/openai\/v1\/?$/, "");
      }
    }
  }

  const rg = options.resourceGroup || `azureclaw-${options.region}`;

  // ── 3b. RBAC + provider preflight ──────────────────────────────
  // Fails fast (≤30s) if the caller lacks roles / providers / features
  // needed for the ~20-minute `up` flow. Skip on --dry-run and --skip-infra
  // (the latter implies the cluster already exists and the caller already
  // has cluster creds) and when the operator opts out explicitly.
  if (!options.dryRun && !options.skipInfra && !options.skipPreflight) {
    const pf = await runPreflightChecks({
      region: options.region,
      resourceGroup: rg,
      isolation: options.isolation,
      foundryEndpoint: options.foundryEndpoint,
      skipPreflight: options.skipPreflight,
    });
    if (!pf.ok) {
      process.exit(1);
    }
  }

  // ── 4. SKU availability check ──────────────────────────────────
  if (!options.dryRun && !options.skipInfra) {
    console.log();
    console.log(chalk.dim(`  Checking VM SKU availability in ${options.region}...\n`));

    // System pool is always D4s_v5. Agent pool depends on isolation level:
    // - standard/enhanced: D4s_v5 (general purpose, works with runc)
    // - confidential: Standard_DC4as_v5 (AMD SEV-SNP, required for Kata CC)
    const agentSku = options.isolation === "confidential" ? "Standard_DC4as_v5" : "Standard_D4s_v5";
    const agentLabel = options.isolation === "confidential"
      ? `AKS Kata pool (DC4as_v5 — confidential compute)`
      : `AKS agent pool (D4s_v5)`;

    const skuChecks = [
      { sku: "Standard_D4s_v5", label: "AKS system pool (D4s_v5)" },
      { sku: agentSku, label: agentLabel },
    ];
    const checkOne = async (check: { sku: string; label: string }): Promise<{ ok: boolean; line: string; available: boolean }> => {
      try {
        const { stdout: skuJson } = await execa("az", [
          "vm", "list-skus",
          "--location", options.region,
          "--size", check.sku,
          "--query", "[0].restrictions",
          "--output", "json",
        ], { stdio: "pipe", timeout: 10000 });
        const restrictions = JSON.parse(skuJson || "[]");
        const blocked = restrictions.some((r: { type: string }) => r.type === "Location");
        return blocked
          ? { ok: false, available: false, line: `${check.label} — ${chalk.red("not available")} in ${options.region}` }
          : { ok: true, available: true, line: `${check.label} — available` };
      } catch {
        // Network/CLI failure is non-fatal — surface as "could not verify" but don't fail preflight
        return { ok: true, available: true, line: `${check.label} — ${chalk.yellow("could not verify")} (continuing)` };
      }
    };
    const results = await Promise.all(skuChecks.map(checkOne));
    let skuOk = true;
    for (const r of results) {
      checkLine(r.ok, r.line);
      if (!r.available) skuOk = false;
    }
    if (!skuOk) {
      console.log(chalk.yellow(`\n  Some VM SKUs are not available in ${options.region}.`));
      console.log(chalk.yellow(`  Try a different region: ${chalk.cyan("azureclaw up --region westus3")}\n`));
      process.exit(1);
    }

    // Quick check: can we create resources in this sub+region?
    const isolationLabels: Record<string, string> = {
      standard: "Standard (runc)",
      enhanced: "Enhanced (runc + strict seccomp + ro-rootfs)",
      confidential: "Confidential (Kata VM)",
    };
    checkLine(true, `Region — ${options.region}`);
    checkLine(true, `Isolation — ${isolationLabels[options.isolation] || options.isolation}`);
    if (options.foundryEndpoint && isValidAzureHost(options.foundryEndpoint, "services.ai.azure.com")) {
      checkLine(true, `Foundry — ${options.foundryEndpoint}`);
      if (options.openaiEndpoint) {
        checkLine(true, `OpenAI — ${options.openaiEndpoint} (derived)`);
      }
    } else if (options.foundryEndpoint || options.openaiEndpoint) {
      checkLine(true, `AI Backend — ${options.openaiEndpoint || options.foundryEndpoint} (existing)`);
    } else {
      checkLine(true, `AI Backend — new Azure OpenAI resource`);
    }
    checkLine(true, `Resource group — ${rg}`);
    checkLine(true, `Sandbox — ${options.name}`);

    console.log(chalk.green(`\n  ✓ Preflight passed — ready to deploy\n`));
  }

  // ── Dry-run mode: just print the plan ──────────────────────────
  if (options.dryRun) {
    const isolationDesc: Record<string, string> = {
      standard: "standard (runc + RuntimeDefault seccomp)",
      enhanced: "enhanced (runc + azureclaw-strict seccomp)",
      confidential: "confidential (Kata VM isolation)",
    };
    console.log(blue(`\n  AzureClaw · Dry Run\n`));
    console.log(`  Steps that would execute:\n`);
    console.log(`   1. Check Azure credentials (az account show)`);
    console.log(`   2. Create resource group '${rg}' in ${options.region}`);
    console.log(`   3. Detect caller public IP for firewall rules`);
    console.log(`   4. Register features: EncryptionAtHost${options.isolation === "confidential" ? ", KataVMIsolationPreview + aks-preview ext" : ""}`);
    console.log(`   5. Deploy Bicep: AKS + ACR + KV + AOAI + Monitor + WI${options.isolation === "confidential" ? " + katapool" : ""}`);
    console.log(`   6. Add AKS egress IP to ACR + AOAI firewalls`);
    console.log(`   7. Attach ACR to AKS (az aks update --attach-acr)`);
    console.log(`   8. Get AKS credentials`);
    console.log(`   9. ${options.build ? "Build images locally + push to ACR (docker build + docker push)" : `Import images from ${options.sourceAcr}: controller, inference-router, openclaw-sandbox`}`);
    console.log(`  10. Helm install: CRD + controller + seccomp DaemonSet + RBAC`);
    console.log(`  11. Create federated credential for azureclaw-${options.name}:sandbox`);
    console.log(`  12. Create ClawSandbox CR '${options.name}' (isolation: ${options.isolation})`);
    console.log(`  13. Wait for sandbox Running`);
    console.log(`\n  Configuration:`);
    console.log(`    Name:       ${options.name}`);
    console.log(`    Model:      ${options.model}`);
    console.log(`    Isolation:  ${isolationDesc[options.isolation] || options.isolation}`);
    console.log(`    Region:     ${options.region}`);
    console.log(`    RG:         ${rg}`);
    console.log(`    Source ACR: ${options.sourceAcr}`);
    console.log(`    Skip Infra: ${options.skipInfra}`);
    console.log();
    return null;
  }

  return { rg };
}
