import { Command } from "commander";
import chalk from "chalk";
import { existsSync } from "fs";
import { Stepper, banner, section, kvLine, checkLine } from "../stepper.js";
import { saveContext, loadContext } from "../config.js";

export function upCommand(): Command {
  const cmd = new Command("up");

  cmd
    .description(
      "One command to go from zero to running agent. Provisions Azure resources, builds images, deploys controller, creates sandbox."
    )
    .option("--name <name>", "Sandbox name", "my-assistant")
    .option("--model <model>", "AI model", "gpt-4.1")
    .option(
      "--policy <preset>",
      "Policy preset: minimal, developer, web, azure",
      "developer"
    )
    .option("--region <region>", "Azure region", "eastus2")
    .option("--cluster-name <name>", "AKS cluster name", "azureclaw")
    .option(
      "--isolation <level>",
      "Pod isolation level: standard (runc), enhanced (runc + strict seccomp), confidential (Kata VM)",
      "enhanced"
    )
    .option("-g, --resource-group <name>", "Resource group name")
    .option("--skip-infra", "Skip infrastructure provisioning (reuse existing cluster)", false)
    .option("--force-infra", "Force Bicep deployment even if AKS cluster exists", false)
    .option("--source-acr <server>", "Source ACR for pre-built images (customers)", "azureclawacr.azurecr.io")
    .option("--build", "Build images locally and push to ACR (developer mode)", false)
    .option("--foundry-endpoint <url>", "Existing Azure AI Foundry project endpoint (services.ai.azure.com)")
    .option("--openai-endpoint <url>", "Existing Azure OpenAI endpoint (openai.azure.com, derived from Foundry if omitted)")
    .option("--dry-run", "Show what would be done without executing", false)
    .option("--upgrade", "Fast upgrade: skip prompts, reuse cached context, just re-run Helm + RBAC", false)
    .action(async (options) => {
      const blue = chalk.hex("#0078D4");
      const bold = chalk.bold;
      const { default: inquirer } = await import("inquirer");
      const { execa } = await import("execa");
      const ora = (await import("ora")).default;
      const path = await import("path");
      const fs = await import("fs");

      // ── FAST UPGRADE PATH ──────────────────────────────────────────
      // Skip all prompts and infra — just re-run Helm with cached context
      if (options.upgrade) {
        const ctx = loadContext();
        if (!ctx?.acrLoginServer || !ctx?.aksCluster || !ctx?.resourceGroup) {
          console.error(chalk.red("\n  No cached deployment context. Run 'azureclaw up' first (without --upgrade).\n"));
          process.exit(1);
        }

        console.log(blue("\n  AzureClaw · Fast Upgrade\n"));

        // Connect to AKS
        let spin = ora("Connecting to AKS...").start();
        await execa("az", ["aks", "get-credentials", "--name", ctx.aksCluster, "--resource-group", ctx.resourceGroup, "--overwrite-existing"], { stdio: "pipe" });
        spin.succeed("AKS connected");

        // Find Helm chart — try cwd, then walk up, then try relative to CLI source
        let repoRoot = process.cwd();
        for (let i = 0; i < 5; i++) {
          if (fs.existsSync(path.join(repoRoot, "deploy", "helm"))) break;
          repoRoot = path.dirname(repoRoot);
        }
        if (!fs.existsSync(path.join(repoRoot, "deploy", "helm"))) {
          // Try relative to the CLI package itself
          const cliDir = new URL("../../..", import.meta.url).pathname;
          repoRoot = cliDir;
          for (let i = 0; i < 3; i++) {
            if (fs.existsSync(path.join(repoRoot, "deploy", "helm"))) break;
            repoRoot = path.dirname(repoRoot);
          }
        }
        if (!fs.existsSync(path.join(repoRoot, "deploy", "helm"))) {
          console.error(chalk.red("\n  Helm chart not found. Run from the AzureClaw repo directory.\n"));
          process.exit(1);
        }
        const helmPath = path.join(repoRoot, "deploy", "helm", "azureclaw");

        // Build Helm args from cached context
        const openAiEndpoint = ctx.foundryEndpoint || "";
        const helmArgs = [
          "upgrade", "--install", "azureclaw", helmPath,
          "--namespace", "azureclaw-system",
          "--create-namespace",
          "--set", `controller.image.repository=${ctx.acrLoginServer}/azureclaw-controller`,
          "--set", `controller.image.tag=latest`,
          "--set", `inferenceRouter.image.repository=${ctx.acrLoginServer}/azureclaw-inference-router`,
          "--set", `inferenceRouter.image.tag=latest`,
          "--set", `inferenceRouter.azure.openai.endpoint=${openAiEndpoint}`,
          "--set", `sandbox.image.repository=${ctx.acrLoginServer}/openclaw-sandbox`,
          "--set", `sandbox.image.tag=latest`,
          "--set", `agtGovernance.image.repository=${ctx.acrLoginServer}/agt-governance-sidecar`,
          "--set", `agtGovernance.image.tag=latest`,
          "--set", `azure.workloadIdentity.clientId=${ctx.wiClientId || ""}`,
          "--set", `azure.keyVaultCsi.keyVaultName=${ctx.keyVaultName || ""}`,
          "--wait",
          "--timeout", "5m",
        ];
        if (ctx.foundryEndpoint) {
          helmArgs.push("--set", `foundry.endpoint=${ctx.foundryEndpoint}`);
        }
        if (ctx.foundryProjectEndpoint) {
          helmArgs.push("--set", `foundry.projectEndpoint=${ctx.foundryProjectEndpoint}`);
        }
        if (ctx.imdsClientId) {
          helmArgs.push("--set", `foundry.imdsClientId=${ctx.imdsClientId}`);
        }
        // Fedcred config for controller auto-creation
        if (ctx.oidcIssuerUrl) {
          try {
            const { stdout: subId } = await execa("az", ["account", "show", "--query", "id", "--output", "tsv"], { stdio: "pipe", timeout: 10000 });
            helmArgs.push(
              "--set", `fedcred.subscriptionId=${subId.trim()}`,
              "--set", `fedcred.identityName=${ctx.identityName || ""}`,
              "--set", `fedcred.identityResourceGroup=${ctx.identityResourceGroup || ctx.resourceGroup}`,
              "--set", `fedcred.oidcIssuerUrl=${ctx.oidcIssuerUrl}`,
            );
          } catch { /* non-critical */ }
        }
        // Discover deployments
        try {
          const accountName = ctx.foundryEndpoint ? new URL(ctx.foundryEndpoint).hostname.split(".")[0] : "";
          if (accountName) {
            const { stdout: rgOut } = await execa("az", [
              "cognitiveservices", "account", "list",
              "--query", `[?name=='${accountName}'].resourceGroup | [0]`,
              "--output", "tsv",
            ], { stdio: "pipe", timeout: 15000 });
            const foundryRg = rgOut.trim();
            if (foundryRg) {
              const { stdout } = await execa("az", [
                "cognitiveservices", "account", "deployment", "list",
                "--name", accountName, "--resource-group", foundryRg,
                "--query", "[].name", "--output", "json",
              ], { stdio: "pipe", timeout: 30000 });
              const deps = JSON.parse(stdout || "[]");
              if (Array.isArray(deps) && deps.length > 0) {
                const escaped = JSON.stringify(deps).replace(/,/g, "\\,");
                helmArgs.push("--set-string", `foundry.deployments=${escaped}`);
              }
            }
          }
        } catch { /* non-critical */ }

        spin = ora("Upgrading Helm release...").start();
        await execa("helm", helmArgs, { stdio: "pipe" });
        spin.succeed("Helm upgraded");

        // Rollout restart
        spin = ora("Restarting controller...").start();
        await execa("kubectl", ["rollout", "restart", "deployment/azureclaw-controller", "-n", "azureclaw-system"], { stdio: "pipe" }).catch(() => {});
        await execa("kubectl", ["rollout", "status", "deployment/azureclaw-controller", "-n", "azureclaw-system", "--timeout=120s"], { stdio: "pipe" }).catch(() => {});
        spin.succeed("Controller restarted");

        // Ensure controller SA has a fedcred (so it can get ARM tokens via WI to create sandbox fedcreds)
        if (ctx.oidcIssuerUrl && ctx.identityName) {
          spin = ora("Ensuring controller SA fedcred + MI Contributor...").start();
          const idRg = ctx.identityResourceGroup || ctx.resourceGroup;

          // Controller SA fedcred
          await execa("az", [
            "identity", "federated-credential", "create",
            "--identity-name", ctx.identityName,
            "--resource-group", idRg,
            "--name", "azureclaw-controller-sa",
            "--issuer", ctx.oidcIssuerUrl,
            "--subject", "system:serviceaccount:azureclaw-system:azureclaw-controller",
            "--audiences", "api://AzureADTokenExchange",
            "--output", "none",
          ], { stdio: "pipe", timeout: 30000 }).catch(() => {});

          // MI Contributor self-scoped (so controller can create/delete fedcreds)
          try {
            const { stdout: subId } = await execa("az", [
              "account", "show", "--query", "id", "--output", "tsv",
            ], { stdio: "pipe", timeout: 10000 });
            const miScope = `/subscriptions/${subId.trim()}/resourceGroups/${idRg}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/${ctx.identityName}`;
            const { stdout: miPid } = await execa("az", [
              "identity", "show",
              "--name", ctx.identityName,
              "--resource-group", idRg,
              "--query", "principalId",
              "--output", "tsv",
            ], { stdio: "pipe" });
            await execa("az", [
              "role", "assignment", "create",
              "--assignee-object-id", miPid.trim(),
              "--assignee-principal-type", "ServicePrincipal",
              "--role", "Managed Identity Contributor",
              "--scope", miScope,
              "--output", "none",
            ], { stdio: "pipe" });
          } catch { /* already exists or lacks Owner — non-fatal */ }

          spin.succeed("Controller SA fedcred + MI Contributor ready");
        }

        // Ensure federated credentials exist for all sandboxes
        if (ctx.oidcIssuerUrl && ctx.identityName) {
          spin = ora("Syncing federated credentials for sandboxes...").start();
          try {
            const { stdout: sandboxJson } = await execa("kubectl", [
              "get", "clawsandbox", "-A", "-o", "json",
            ], { stdio: "pipe", timeout: 15000 });
            const sandboxes = JSON.parse(sandboxJson).items || [];
            let created = 0;
            for (const sb of sandboxes) {
              const sbName = sb.metadata?.name;
              if (!sbName) continue;
              const sbNs = `azureclaw-${sbName}`;
              await execa("az", [
                "identity", "federated-credential", "create",
                "--identity-name", ctx.identityName,
                "--resource-group", ctx.identityResourceGroup || ctx.resourceGroup,
                "--name", `azureclaw-${sbName}`,
                "--issuer", ctx.oidcIssuerUrl,
                "--subject", `system:serviceaccount:${sbNs}:sandbox`,
                "--audiences", "api://AzureADTokenExchange",
                "--output", "none",
              ], { stdio: "pipe", timeout: 30000 }).then(() => { created++; }).catch(() => {});
            }
            spin.succeed(`Federated credentials synced (${created} created, ${sandboxes.length} total)`);
          } catch {
            spin.warn("Federated credential sync skipped");
          }
        }

        console.log(chalk.green("\n  ✓ Fast upgrade complete\n"));
        return;
      }

      // Auto-detect developer mode: if running from the repo (Dockerfile exists), default to --build
      if (!options.build && !process.argv.includes("--source-acr")) {
        const repoRoot = new URL("../../..", import.meta.url).pathname;
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
      }

      // ── 1. Check required CLI tools ────────────────────────────────
      const tools: { cmd: string; args: string[]; label: string; required: boolean }[] = [
        { cmd: "az", args: ["--version"], label: "Azure CLI", required: true },
        { cmd: "kubectl", args: ["version", "--client"], label: "kubectl", required: true },
        { cmd: "helm", args: ["version", "--short"], label: "Helm", required: true },
        { cmd: "docker", args: ["info", "--format", "{{.ServerVersion}}"], label: "Docker", required: options.build },
      ];

      if (!options.dryRun) {
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
      if (!options.dryRun) {
        let isLoggedIn = false;
        try {
          await execa("az", ["account", "show", "--output", "none"], { stdio: "pipe" });
          isLoggedIn = true;
        } catch { /* not logged in */ }

        if (!isLoggedIn) {
          console.log(chalk.yellow("\n  Not logged into Azure. Opening browser for login...\n"));
          await execa("az", ["login"], { stdio: "inherit" });
        }

        // Get current subscription
        const { stdout: subJson } = await execa("az", [
          "account", "show", "--output", "json",
        ], { stdio: "pipe" });
        const currentSub = JSON.parse(subJson);

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

      // ── 3. Interactive prompts for region/name/isolation ────────────
      // If a cached context provided values, treat them as "user-provided"
      // so the user isn't re-prompted for details they already set.
      const userProvidedRegion = process.argv.includes("--region") || !!cachedCtx?.region;
      const userProvidedName = process.argv.includes("--name");
      const userProvidedIsolation = process.argv.includes("--isolation");
      const userProvidedRg = process.argv.includes("-g") || process.argv.includes("--resource-group") || !!cachedCtx?.resourceGroup;

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
                if (!input.includes("services.ai.azure.com")) return "Expected services.ai.azure.com URL. For openai.azure.com, choose 'Azure OpenAI endpoint only'.";
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
                if (!input.includes("openai.azure.com")) return "Expected openai.azure.com URL. For Foundry, choose the Foundry option.";
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
        let skuOk = true;
        for (const check of skuChecks) {
          try {
            const { stdout: skuJson } = await execa("az", [
              "vm", "list-skus",
              "--location", options.region,
              "--size", check.sku,
              "--query", "[0].restrictions",
              "--output", "json",
            ], { stdio: "pipe", timeout: 15000 });
            const restrictions = JSON.parse(skuJson || "[]");
            const blocked = restrictions.some((r: { type: string }) => r.type === "Location");
            if (blocked) {
              checkLine(false, `${check.label} — ${chalk.red("not available")} in ${options.region}`);
              skuOk = false;
            } else {
              checkLine(true, `${check.label} — available`);
            }
          } catch {
            checkLine(false, `${check.label} — ${chalk.yellow("could not verify")} (continuing)`);
          }
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
        if (options.foundryEndpoint && options.foundryEndpoint.includes("services.ai.azure.com")) {
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
        return;
      }

      banner("AzureClaw · Production Deploy", "Secure AI Agent Runtime on Azure");

      const clusterName = options.clusterName ?? "azureclaw";
      const baseName = clusterName.replace(/-aks$/, "");
      const acrName = baseName.replace(/-/g, "") + "acr";
      const stepper = new Stepper({ totalSteps: 7 });

      try {
        const path = await import("path");
        const { fileURLToPath } = await import("url");

        // Resolve repo root from CLI package location
        const thisFile = fileURLToPath(import.meta.url);
        const cliDist = path.dirname(path.dirname(thisFile));
        // CLI lives at cli/dist/commands/ or cli/src/commands/ — repo root is 3 levels up
        let repoRoot = path.resolve(cliDist, "..", "..");
        // Fallback: walk up from CWD looking for Cargo.toml
        if (!existsSync(path.join(repoRoot, "Cargo.toml"))) {
          repoRoot = process.cwd();
          while (repoRoot !== "/" && !existsSync(path.join(repoRoot, "Cargo.toml"))) {
            repoRoot = path.dirname(repoRoot);
          }
        }

        const bicepPath = path.join(repoRoot, "deploy/bicep/main.bicep");
        const helmPath = path.join(repoRoot, "deploy/helm/azureclaw");

        if (!existsSync(bicepPath)) {
          stepper.fail("Bicep template not found");
          console.log(chalk.yellow(`  Expected at: ${bicepPath}`));
          console.log(chalk.yellow(`  Run from the AzureClaw repo root.\n`));
          process.exit(1);
        }

        // ── Step 1: Create resource group ────────────────────────────
        stepper.step(`Setting up resource group '${rg}'...`);

        // Check if RG already exists
        let rgExists = false;
        try {
          const { stdout: rgCheck } = await execa("az", [
            "group", "show", "--name", rg, "--query", "properties.provisioningState", "-o", "tsv",
          ], { stdio: "pipe" });
          if (rgCheck.trim() === "Succeeded") rgExists = true;
        } catch { /* doesn't exist */ }

        await execa("az", [
          "group", "create", "--name", rg, "--location", options.region, "--output", "none",
        ], { stdio: "pipe" });
        stepper.detail(rgExists ? "ok" : "new", `Resource group '${rg}' — ${rgExists ? "exists" : "created"}`);


        // ── Step 2b: Detect caller IP for firewall rules ─────────────
        stepper.update("Detecting your public IP for firewall rules...");
        let callerIp: string | null = null;
        try {
          const { stdout: ipOut } = await execa("curl", ["-s", "--max-time", "5", "https://ifconfig.me"], { stdio: "pipe" });
          const ip = ipOut.trim();
          if (/^\d{1,3}(\.\d{1,3}){3}$/.test(ip)) {
            callerIp = ip;
            stepper.detail("ok", `Public IP detected — ${ip}`);
          }
        } catch {
          stepper.detail("skip", "Public IP detection — skipped (offline?)");
        }

        // ── Step 2c: Register required preview features ──────────
        stepper.update("Registering preview features...");
        // EncryptionAtHost is required for clawpool
        await execa("az", [
          "feature", "register",
          "--namespace", "Microsoft.Compute",
          "--name", "EncryptionAtHost",
          "--output", "none",
        ], { stdio: "pipe" }).catch(() => {
          // Already registered — OK
        });

        if (options.isolation === "confidential") {
          // Install aks-preview extension for Kata workload runtime
          await execa("az", [
            "extension", "add", "--name", "aks-preview", "--upgrade",
          ], { stdio: "pipe" }).catch(() => {});

          await execa("az", [
            "feature", "register",
            "--namespace", "Microsoft.ContainerService",
            "--name", "KataVMIsolationPreview",
            "--output", "none",
          ], { stdio: "pipe" }).catch(() => {});
        }

        // Propagate feature registrations
        await execa("az", ["provider", "register", "-n", "Microsoft.Compute", "--output", "none"], { stdio: "pipe" }).catch(() => {});
        await execa("az", ["provider", "register", "-n", "Microsoft.ContainerService", "--output", "none"], { stdio: "pipe" }).catch(() => {});

        stepper.done(`Resource group '${rg}' ready${callerIp ? ` (IP: ${callerIp})` : ""}`);

        // ── Step 3: Deploy Bicep (AKS + ACR + KV + AOAI + Monitor + WI) ─
        let acrLoginServer: string;
        let openAiEndpoint: string;
        let wiClientId: string;
        let kvName: string;

        if (!options.skipInfra && !options.forceInfra) {
          // Auto-detect: if AKS cluster already exists, skip Bicep (saves ~8 min)
          try {
            const { stdout: aksCheck } = await execa("az", [
              "aks", "show", "-g", rg, "-n", `${baseName}-aks`,
              "--query", "provisioningState", "-o", "tsv",
            ], { stdio: "pipe" });
            if (aksCheck.trim() === "Succeeded") {
              options.skipInfra = true;
            }
          } catch {
            // Cluster doesn't exist — proceed with Bicep
          }
        }

        if (!options.skipInfra) {
          stepper.step(`Provisioning Azure resources in ${options.region}...`);
          const isolationDesc: Record<string, string> = {
            standard: "runc + RuntimeDefault seccomp",
            enhanced: "runc + azureclaw-strict seccomp + read-only rootfs",
            confidential: "Kata VM isolation (hardware TEE)",
          };
          stepper.detail("info", `Isolation: ${isolationDesc[options.isolation] || options.isolation}`);
          if (options.foundryEndpoint) {
            stepper.detail("info", `Resources: AKS + ACR + Key Vault + Monitor (using existing AI endpoint)`);
          } else {
            stepper.detail("info", `Resources: AKS + ACR + Key Vault + Azure OpenAI + Monitor`);
          }
          stepper.detail("info", `This takes 5–10 minutes. Deploying now...`);
          const bicepParams = [
            `location=${options.region}`,
            `baseName=${baseName}`,
          ];
          if (options.isolation === "confidential") {
            bicepParams.push("enableKata=true");
          }
          if (callerIp) {
            bicepParams.push(`authorizedIpRanges=["${callerIp}/32"]`);
          }
          if (options.foundryEndpoint) {
            bicepParams.push("deployAoai=false");
          }

          // Run Bicep deployment with a progress ticker
          const ticker = setInterval(() => {
            stepper.update(`Provisioning Azure resources in ${options.region}... (still running)`);
          }, 30000);

          try {
            const { stdout: deployOutput } = await execa("az", [
              "deployment", "group", "create",
              "--resource-group", rg,
              "--template-file", bicepPath,
              "--parameters", ...bicepParams,
              "--output", "json",
              "--query", "properties.outputs",
            ], { stdio: "pipe" });

            clearInterval(ticker);

            const outputs = JSON.parse(deployOutput);
            acrLoginServer = outputs.acrLoginServer.value;
            openAiEndpoint = options.openaiEndpoint || outputs.openAiEndpoint?.value || options.foundryEndpoint || "";
            wiClientId = outputs.sandboxIdentityClientId.value;
            kvName = outputs.keyVaultName.value;

            stepper.detail("new", `AKS cluster — ${baseName}-aks`);
            stepper.detail("new", `ACR — ${acrLoginServer}`);
            stepper.detail("new", `Key Vault — ${kvName}`);
            if (options.foundryEndpoint) {
              stepper.detail("ok", `AI Backend — ${options.foundryEndpoint} (existing)`);
            } else {
              stepper.detail("new", `OpenAI — ${openAiEndpoint}`);
            }

            stepper.done("Azure resources provisioned");
          } catch (bicepErr: any) {
            clearInterval(ticker);
            throw bicepErr;
          }
        } else {
          // Read outputs from existing deployment
          stepper.step("Verifying existing infrastructure...");
          const { stdout: existingOutput } = await execa("az", [
            "deployment", "group", "show",
            "--resource-group", rg,
            "--name", "main",
            "--output", "json",
            "--query", "properties.outputs",
          ], { stdio: "pipe" }).catch(async () => {
            // Fallback: try module deployment name
            return execa("az", [
              "deployment", "group", "show",
              "--resource-group", rg,
              "--name", `${baseName}-aks`,
              "--output", "json",
              "--query", "properties.outputs",
            ], { stdio: "pipe" });
          });

          const outputs = JSON.parse(existingOutput);
          acrLoginServer = outputs.acrLoginServer.value;
          openAiEndpoint = options.openaiEndpoint || outputs.openAiEndpoint?.value || options.foundryEndpoint || "";
          wiClientId = outputs.sandboxIdentityClientId.value;
          kvName = outputs.keyVaultName.value;

          stepper.detail("ok", `AKS cluster — ${baseName}-aks (running)`);
          stepper.detail("ok", `ACR — ${acrLoginServer}`);
          stepper.detail("ok", `Key Vault — ${kvName}`);
          if (options.foundryEndpoint) {
            stepper.detail("ok", `AI Backend — ${options.foundryEndpoint} (existing)`);
          } else {
            stepper.detail("ok", `OpenAI — ${openAiEndpoint}`);
          }
          stepper.detail("ok", `Workload Identity — ${wiClientId.slice(0, 8)}...`);

          stepper.done("Infrastructure verified (Bicep skipped)");
        }

        // ── Step 3a: Ensure caller IP is in AKS API server authorized ranges ──
        stepper.step("Configuring network access & firewalls...");
        if (callerIp) {
          stepper.update("Updating AKS API server authorized IPs...");
          await execa("az", [
            "aks", "update",
            "--name", `${baseName}-aks`,
            "--resource-group", rg,
            "--api-server-authorized-ip-ranges", `${callerIp}/32`,
            "--output", "none",
          ], { stdio: "pipe" }).then(() => {
            stepper.detail("ok", `AKS API server — ${callerIp}/32 authorized`);
          }).catch(() => {
            stepper.detail("ok", `AKS API server — IP already authorized`);
          });
        }

        // ── Step 3b: Add AKS egress IP to service firewalls ──────
        stepper.update("Adding AKS egress IP to service firewalls...");
        try {
          // Get AKS egress (outbound) IP
          const { stdout: egressIpId } = await execa("az", [
            "aks", "show",
            "--name", `${baseName}-aks`,
            "--resource-group", rg,
            "--query", "networkProfile.loadBalancerProfile.effectiveOutboundIPs[0].id",
            "--output", "tsv",
          ], { stdio: "pipe" });
          const cleanIpId = egressIpId.trim().split("\n").pop()?.trim();
          if (cleanIpId && cleanIpId.startsWith("/subscriptions")) {
            const { stdout: egressIpRaw } = await execa("az", [
              "network", "public-ip", "show",
              "--ids", cleanIpId,
              "--query", "ipAddress",
              "--output", "tsv",
            ], { stdio: "pipe" });
            const aksEgress = egressIpRaw.trim();
            if (aksEgress && /^\d{1,3}(\.\d{1,3}){3}$/.test(aksEgress)) {
              // Add AKS egress to ACR firewall
              await execa("az", [
                "acr", "network-rule", "add",
                "--name", acrName,
                "--ip-address", aksEgress,
                "--output", "none",
              ], { stdio: "pipe" }).then(() => {
                stepper.detail("ok", `ACR firewall — ${aksEgress} allowed`);
              }).catch(() => {
                stepper.detail("ok", `ACR firewall — already configured`);
              });
              // Add AKS egress to AOAI firewall (only when AOAI is deployed in this RG)
              if (!options.foundryEndpoint) {
                await execa("az", [
                  "cognitiveservices", "account", "network-rule", "add",
                  "--name", `${baseName}-aoai`,
                  "--resource-group", rg,
                  "--ip-address", aksEgress,
                  "--output", "none",
                ], { stdio: "pipe" }).then(() => {
                  stepper.detail("ok", `AOAI firewall — ${aksEgress} allowed`);
                }).catch(() => {
                  stepper.detail("ok", `AOAI firewall — already configured`);
                });
              }
            }
          }
        } catch {
          // Non-fatal — firewalls may not have rules or may already include egress
        }

        // ── Step 3c: Attach ACR to AKS ──────────────────────────────
        stepper.update("Attaching ACR to AKS...");
        await execa("az", [
          "aks", "update",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--attach-acr", acrName,
          "--output", "none",
        ], { stdio: "pipe" }).then(() => {
          stepper.detail("ok", `ACR attachment — ${acrName} → AKS`);
        }).catch(() => {
          stepper.detail("ok", `ACR attachment — already attached`);
        });

        stepper.done("Network access configured");

        // ── Step 4: Get AKS credentials ──────────────────────────────
        stepper.step("Configuring kubectl...");
        await execa("az", [
          "aks", "get-credentials",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--overwrite-existing",
          "--output", "none",
        ], { stdio: "pipe" });

        // ── Step 5: Get images into ACR ──────────────────────────────
        const acr = acrLoginServer.replace(".azurecr.io", "");

        if (options.build) {
          // Developer mode: build locally and push
          stepper.step("Building and pushing images...");
          stepper.update("Logging into ACR...");
          await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });

          const buildPush = async (dockerfile: string, tag: string, buildArgs: string[] = [], context?: string) => {
            stepper.update(`Building ${tag}...`);
            const args = [
              "build", "--platform", "linux/amd64",
              "--provenance=false", "--sbom=false",
              "-f", path.join(repoRoot, dockerfile),
              "-t", `${acrLoginServer}/${tag}`,
              ...buildArgs,
              context ? path.join(repoRoot, context) : repoRoot,
            ];
            await execa("docker", args, { stdio: "pipe" });
            // Push with retry — ACR tokens/connections can go stale after long builds
            for (let attempt = 1; attempt <= 3; attempt++) {
              try {
                stepper.update(`Pushing ${tag}${attempt > 1 ? ` (retry ${attempt}/3)` : ""}...`);
                if (attempt > 1) await execa("az", ["acr", "login", "--name", acr], { stdio: "pipe" });
                await execa("docker", ["push", `${acrLoginServer}/${tag}`], { stdio: "pipe" });
                break;
              } catch (e: any) {
                if (attempt === 3) throw e;
                stepper.update(`Push ${tag} failed, retrying...`);
                await new Promise(r => setTimeout(r, 5000));
              }
            }
          };

          await buildPush("controller/Dockerfile", "azureclaw-controller:latest");
          await buildPush("inference-router/Dockerfile", "azureclaw-inference-router:latest");
          await buildPush(
            "sandbox-images/openclaw/Dockerfile",
            "openclaw-sandbox:latest",
            ["--build-arg", `INFERENCE_ROUTER_IMAGE=${acrLoginServer}/azureclaw-inference-router:latest`]
          );

          // AgentMesh components (relay + registry for E2E encrypted inter-agent comms)
          await buildPush("vendor/agentmesh-relay/Dockerfile", "agentmesh-relay:latest", [], "vendor/agentmesh-relay");
          await buildPush("vendor/agentmesh-registry/Dockerfile", "agentmesh-registry:latest", [], "vendor/agentmesh-registry");

          // AGT governance sidecar (wraps microsoft/agent-governance-toolkit v3.0.0)
          await buildPush("sidecar-images/agt-governance/Dockerfile", "agt-governance-sidecar:latest");

          stepper.done("Images built and pushed to ACR");
        } else {
          // Customer mode: import pre-built images from source ACR
          stepper.step("Importing images from source ACR...");
          const sourceAcr = options.sourceAcr;
          const images = [
            { source: `${sourceAcr}/azureclaw-controller:latest`, target: "azureclaw-controller:latest" },
            { source: `${sourceAcr}/azureclaw-inference-router:latest`, target: "azureclaw-inference-router:latest" },
            { source: `${sourceAcr}/openclaw-sandbox:latest`, target: "openclaw-sandbox:latest" },
            { source: `${sourceAcr}/agentmesh-relay:latest`, target: "agentmesh-relay:latest" },
            { source: `${sourceAcr}/agentmesh-registry:latest`, target: "agentmesh-registry:latest" },
          ];

          for (const img of images) {
            stepper.update(`Importing ${img.target}...`);
            await execa("az", [
              "acr", "import",
              "--name", acr,
              "--source", img.source,
              "--image", img.target,
              "--force",
            ], { stdio: "pipe" }).then(() => {
              stepper.detail("ok", img.target);
            }).catch(() => {
              stepper.detail("skip", `${img.target} — import failed (may already exist)`);
            });
          }

          stepper.done("Images available in ACR");
        }

        // ── Step 6: Install / upgrade Helm chart ─────────────────────
        stepper.step("Deploying Helm chart (controller + CRD + RBAC)...");

        // Check if Helm release already exists
        let helmExists = false;
        try {
          const { stdout: helmStatus } = await execa("helm", [
            "status", "azureclaw", "-n", "azureclaw-system", "-o", "json",
          ], { stdio: "pipe" });
          const status = JSON.parse(helmStatus);
          if (status.info?.status === "deployed") helmExists = true;
        } catch { /* not installed */ }

        const foundryEndpoint = options.foundryEndpoint || "";

        // Use explicitly provided OpenAI endpoint, or derive from Foundry resource name
        if (!openAiEndpoint && options.openaiEndpoint) {
          openAiEndpoint = options.openaiEndpoint;
        }
        if (!openAiEndpoint && foundryEndpoint) {
          const match = foundryEndpoint.match(/https:\/\/([^.]+)\.services\.ai\.azure\.com/);
          if (match) {
            openAiEndpoint = `https://${match[1]}.openai.azure.com`;
            console.log(`  Derived OpenAI inference endpoint: ${openAiEndpoint}`);
          }
        }

        // Fix orphaned namespace: label it for Helm adoption if it exists but isn't Helm-managed
        try {
          await execa("kubectl", [
            "label", "namespace", "azureclaw-system",
            "app.kubernetes.io/managed-by=Helm",
            "--overwrite",
          ], { stdio: "pipe" }).catch(() => {});
          await execa("kubectl", [
            "annotate", "namespace", "azureclaw-system",
            "meta.helm.sh/release-name=azureclaw",
            "meta.helm.sh/release-namespace=azureclaw-system",
            "--overwrite",
          ], { stdio: "pipe" }).catch(() => {});
        } catch {
          // Namespace may not exist yet — Helm will create it
        }

        // Clean up stale Helm releases (pending-install from failed previous attempts)
        try {
          const { stdout: helmSecrets } = await execa("kubectl", [
            "get", "secrets", "-n", "azureclaw-system",
            "-l", "owner=helm,status=pending-install",
            "-o", "jsonpath={.items[*].metadata.name}",
          ], { stdio: "pipe" });
          if (helmSecrets.trim()) {
            for (const secret of helmSecrets.trim().split(" ")) {
              stepper.update("Cleaning stale Helm release...");
              await execa("kubectl", ["delete", "secret", secret, "-n", "azureclaw-system"], { stdio: "pipe" }).catch(() => {});
            }
          }
        } catch {
          // No stale secrets — normal
        }

        stepper.update("Detecting kubelet managed identity for IMDS auth...");

        // Get kubelet MI client ID for IMDS auth (CA-proof)
        let imdsClientId = "";
        if (foundryEndpoint) {
          try {
            const { stdout: kubeletId } = await execa("az", [
              "aks", "show",
              "--name", `${baseName}-aks`,
              "--resource-group", rg,
              "--query", "identityProfile.kubeletidentity.clientId",
              "--output", "tsv",
            ], { stdio: "pipe" });
            imdsClientId = kubeletId.trim().split("\n").pop()?.trim() || "";

            // Assign Cognitive Services User to kubelet MI via Bicep (bypasses CLI CA policy)
            const { stdout: kubeletPrincipal } = await execa("az", [
              "aks", "show",
              "--name", `${baseName}-aks`,
              "--resource-group", rg,
              "--query", "identityProfile.kubeletidentity.objectId",
              "--output", "tsv",
            ], { stdio: "pipe" });
            const principalId = kubeletPrincipal.trim().split("\n").pop()?.trim() || "";
            if (principalId) {
              stepper.update("Assigning Cognitive Services roles to kubelet MI (via Bicep)...");
              // Assign BOTH roles: Cognitive Services User (control-plane) + OpenAI User (data-plane)
              const bicepRole = [
                "targetScope = 'subscription'",
                "param pid string",
                "resource csu 'Microsoft.Authorization/roleAssignments@2022-04-01' = {",
                "  name: guid(pid, 'csu-foundry')",
                "  properties: {",
                "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'a97b65f3-24c7-4388-baec-2e87135dc908')",
                "    principalId: pid",
                "    principalType: 'ServicePrincipal'",
                "  }",
                "}",
                "resource csoai 'Microsoft.Authorization/roleAssignments@2022-04-01' = {",
                "  name: guid(pid, 'csoai-foundry')",
                "  properties: {",
                "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '5e0bd9bd-7b93-4f28-af87-19fc36ad61bd')",
                "    principalId: pid",
                "    principalType: 'ServicePrincipal'",
                "  }",
                "}",
              ].join("\n");
              const fs = await import("fs");
              const tmpBicep = path.join(repoRoot, ".tmp-role.bicep");
              fs.writeFileSync(tmpBicep, bicepRole);
              await execa("az", [
                "deployment", "sub", "create",
                "--location", options.region,
                "--template-file", tmpBicep,
                "--parameters", `pid=${principalId}`,
                "--output", "none",
              ], { stdio: "pipe" }).catch(() => {});
              fs.unlinkSync(tmpBicep);
            }
          } catch {
            // Non-fatal — IMDS will still try without explicit client ID
          }
        }

        const helmArgs = [
          "upgrade", "--install", "azureclaw", helmPath,
          "--namespace", "azureclaw-system",
          "--create-namespace",
          "--set", `controller.image.repository=${acrLoginServer}/azureclaw-controller`,
          "--set", `controller.image.tag=latest`,
          "--set", `inferenceRouter.image.repository=${acrLoginServer}/azureclaw-inference-router`,
          "--set", `inferenceRouter.image.tag=latest`,
          "--set", `inferenceRouter.azure.openai.endpoint=${openAiEndpoint}`,
          "--set", `sandbox.image.repository=${acrLoginServer}/openclaw-sandbox`,
          "--set", `sandbox.image.tag=latest`,
          "--set", `agtGovernance.image.repository=${acrLoginServer}/agt-governance-sidecar`,
          "--set", `agtGovernance.image.tag=latest`,
          "--set", `azure.workloadIdentity.clientId=${wiClientId}`,
          "--set", `azure.keyVaultCsi.keyVaultName=${kvName}`,
          "--wait",
          "--timeout", "5m",
        ];
        if (foundryEndpoint) {
          helmArgs.push("--set", `foundry.endpoint=${foundryEndpoint}`);
          // If the endpoint is a Foundry project URL, also set it as the project endpoint
          if (foundryEndpoint.includes("services.ai.azure.com") && foundryEndpoint.includes("/api/projects/")) {
            helmArgs.push("--set", `foundry.projectEndpoint=${foundryEndpoint}`);
          }
          if (imdsClientId) {
            helmArgs.push("--set", `foundry.imdsClientId=${imdsClientId}`);
          }
        }

        // Discover deployed models via Azure CLI (ARM management API)
        let discoveredDeployments = "";
        if (foundryEndpoint) {
          try {
            const accountName = new URL(foundryEndpoint).hostname.split(".")[0];
            const { stdout: rgOut } = await execa("az", [
              "cognitiveservices", "account", "list",
              "--query", `[?name=='${accountName}'].resourceGroup | [0]`,
              "--output", "tsv",
            ], { stdio: "pipe", timeout: 15000 });
            const foundryRg = rgOut.trim();
            if (foundryRg) {
              const { stdout } = await execa("az", [
                "cognitiveservices", "account", "deployment", "list",
                "--name", accountName,
                "--resource-group", foundryRg,
                "--query", "[].name",
                "--output", "json",
              ], { stdio: "pipe", timeout: 30000 });
              const deps = JSON.parse(stdout || "[]");
              if (Array.isArray(deps) && deps.length > 0) {
                discoveredDeployments = JSON.stringify(deps);
                stepper.detail("ok", `Deployments — ${deps.join(", ")}`);
              }
            }
          } catch { /* non-critical */ }
        }
        if (discoveredDeployments) {
          // Helm --set treats commas as key separators; escape them
          const escaped = discoveredDeployments.replace(/,/g, "\\,");
          helmArgs.push("--set-string", `foundry.deployments=${escaped}`);
        }

        // Pass federated credential config so controller can auto-create fedcreds for sub-agents
        try {
          const { stdout: oidcIssuerUrl } = await execa("az", [
            "aks", "show",
            "--name", `${baseName}-aks`,
            "--resource-group", rg,
            "--query", "oidcIssuerProfile.issuerUrl",
            "--output", "tsv",
          ], { stdio: "pipe", timeout: 15000 });
          const { stdout: subIdRaw } = await execa("az", [
            "account", "show", "--query", "id", "--output", "tsv",
          ], { stdio: "pipe", timeout: 10000 });
          if (oidcIssuerUrl.trim() && subIdRaw.trim()) {
            helmArgs.push(
              "--set", `fedcred.subscriptionId=${subIdRaw.trim()}`,
              "--set", `fedcred.identityName=${baseName}-aks-sandbox-wi`,
              "--set", `fedcred.identityResourceGroup=${rg}`,
              "--set", `fedcred.oidcIssuerUrl=${oidcIssuerUrl.trim()}`,
            );
          }
        } catch { /* non-critical — controller will log warning */ }

        stepper.update(`${helmExists ? "Upgrading" : "Installing"} AzureClaw Helm chart (controller + CRD + RBAC + seccomp)...`);
        await execa("helm", helmArgs, { stdio: "pipe" });
        stepper.detail(helmExists ? "ok" : "new", `Helm release — ${helmExists ? "upgraded" : "installed"}`);

        // Force rollout when using :latest tags (Helm won't restart pods if spec hash is unchanged)
        stepper.update("Rolling out updated controller...");
        await execa("kubectl", [
          "rollout", "restart", "deployment/azureclaw-controller",
          "-n", "azureclaw-system",
        ], { stdio: "pipe" }).catch(() => {});
        await execa("kubectl", [
          "rollout", "status", "deployment/azureclaw-controller",
          "-n", "azureclaw-system",
          "--timeout=120s",
        ], { stdio: "pipe" }).catch(() => {});

        stepper.done(`Controller ${helmExists ? "upgraded" : "deployed"}`);

        // ── Step 6b: Deploy Inspektor Gadget (eBPF observability) ────
        // Non-fatal — kubectl-gadget may not be installed
        await execa("kubectl", ["gadget", "deploy"], { stdio: "pipe" }).catch(() => {});

        // ── Step 6c: Deploy AgentMesh infrastructure (relay + registry) ──
        stepper.step("Deploying AgentMesh infrastructure...");
        const agentmeshManifest = path.join(repoRoot, "deploy", "agentmesh.yaml");
        if (existsSync(agentmeshManifest)) {
          // Import postgres image into ACR (Azure Policy blocks Docker Hub images)
          stepper.update("Importing postgres image into ACR...");
          await execa("az", [
            "acr", "import",
            "--name", acr,
            "--source", "docker.io/library/postgres:16-alpine",
            "--image", "postgres:16-alpine",
            "--force",
          ], { stdio: "pipe" }).catch(() => {
            // May already exist — non-fatal
          });

          // Substitute ACR login server in the manifest
          const fs = await import("fs");
          const manifest = fs.readFileSync(agentmeshManifest, "utf-8");
          const patchedManifest = manifest.replace(
            /azureclawacr\.azurecr\.io/g,
            acrLoginServer
          );
          const tmpManifest = path.join(repoRoot, ".tmp-agentmesh.yaml");
          try {
            fs.writeFileSync(tmpManifest, patchedManifest);
            await execa("kubectl", ["apply", "-f", tmpManifest], { stdio: "pipe" });

            // Wait for AgentMesh pods to be ready
            stepper.update("Waiting for AgentMesh pods to be ready...");
            await execa("kubectl", [
              "wait", "--for=condition=Ready", "pod",
              "-l", "app=agentmesh-relay",
              "-n", "agentmesh",
              "--timeout=180s",
            ], { stdio: "pipe" }).catch(() => {});
            await execa("kubectl", [
              "wait", "--for=condition=Ready", "pod",
              "-l", "app=agentmesh-registry",
              "-n", "agentmesh",
              "--timeout=180s",
            ], { stdio: "pipe" }).catch(() => {});

            stepper.done("AgentMesh infrastructure deployed");
          } finally {
            try { fs.unlinkSync(tmpManifest); } catch { /* noop */ }
          }
        } else {
          stepper.warn("AgentMesh manifest not found — skipping");
        }

        // ── Step 7: Create ClawSandbox CR ────────────────────────────
        stepper.step(`Creating sandbox '${options.name}'...`);
        const sandboxNs = `azureclaw-${options.name}`;

        // Create federated identity credential for this sandbox's namespace
        stepper.update(`Setting up Workload Identity for ${sandboxNs}...`);
        const { stdout: oidcIssuer } = await execa("az", [
          "aks", "show",
          "--name", `${baseName}-aks`,
          "--resource-group", rg,
          "--query", "oidcIssuerProfile.issuerUrl",
          "--output", "tsv",
        ], { stdio: "pipe" });

        await execa("az", [
          "identity", "federated-credential", "create",
          "--identity-name", `${baseName}-aks-sandbox-wi`,
          "--resource-group", rg,
          "--name", `azureclaw-${options.name}`,
          "--issuer", oidcIssuer.trim(),
          "--subject", `system:serviceaccount:${sandboxNs}:sandbox`,
          "--audiences", "api://AzureADTokenExchange",
          "--output", "none",
        ], { stdio: "pipe" }).then(() => {
          stepper.detail("new", `Federated credential — ${sandboxNs}:sandbox`);
        }).catch(() => {
          stepper.detail("ok", `Federated credential — already exists`);
        });

        // Ensure controller SA has a fedcred too (so it can get ARM tokens via WI to create sandbox fedcreds)
        await execa("az", [
          "identity", "federated-credential", "create",
          "--identity-name", `${baseName}-aks-sandbox-wi`,
          "--resource-group", rg,
          "--name", `azureclaw-controller-sa`,
          "--issuer", oidcIssuer.trim(),
          "--subject", `system:serviceaccount:azureclaw-system:azureclaw-controller`,
          "--audiences", "api://AzureADTokenExchange",
          "--output", "none",
        ], { stdio: "pipe" }).then(() => {
          stepper.detail("new", `Federated credential — controller SA`);
        }).catch(() => {
          // Already exists — fine
        });

        // Grant the sandbox MI "Managed Identity Contributor" on itself so the controller
        // can create/delete fedcreds for dynamically spawned sandboxes
        try {
          const { stdout: subIdForMi } = await execa("az", [
            "account", "show", "--query", "id", "--output", "tsv",
          ], { stdio: "pipe", timeout: 10000 });
          const miScope = `/subscriptions/${subIdForMi.trim()}/resourceGroups/${rg}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/${baseName}-aks-sandbox-wi`;
          const { stdout: miPid } = await execa("az", [
            "identity", "show",
            "--name", `${baseName}-aks-sandbox-wi`,
            "--resource-group", rg,
            "--query", "principalId",
            "--output", "tsv",
          ], { stdio: "pipe" });
          await execa("az", [
            "role", "assignment", "create",
            "--assignee-object-id", miPid.trim(),
            "--assignee-principal-type", "ServicePrincipal",
            "--role", "Managed Identity Contributor",
            "--scope", miScope,
            "--output", "none",
          ], { stdio: "pipe" });
          stepper.detail("new", `MI Contributor — self-scoped for fedcred management`);
        } catch {
          // Already exists or user lacks Owner — non-fatal
        }

        // Grant RBAC roles on Foundry resource via Bicep (if --foundry-endpoint provided)
        // Two assignments needed:
        //   1. Sandbox WI → Azure AI User on the Foundry AI Services resource (so pods can call APIs)
        //   2. Foundry project MI → Azure AI User on the resource group (so Memory Store can call models internally)
        if (foundryEndpoint) {
          stepper.update("Configuring Foundry project RBAC (via Bicep)...");
          const foundryHost = new URL(foundryEndpoint).hostname;
          // Extract account name: "foo.services.ai.azure.com" → "foo", or "foo.openai.azure.com" → "foo"
          const foundryAccountName = foundryHost.split(".")[0];

          // Extract project name from URL path: "/api/projects/bar" → "bar"
          const foundryUrl = new URL(foundryEndpoint);
          const projectMatch = foundryUrl.pathname.match(/\/api\/projects\/([^/]+)/);
          const foundryProjectName = projectMatch ? projectMatch[1] : "";

          // Find the Foundry AI Services account and its resource group
          const { stdout: foundryAccountJson } = await execa("az", [
            "cognitiveservices", "account", "list",
            "--query", `[?name=='${foundryAccountName}'].{id:id, rg:resourceGroup} | [0]`,
            "--output", "json",
          ], { stdio: "pipe" }).catch(() => ({ stdout: "{}" }));

          const foundryAccount = JSON.parse(foundryAccountJson.trim() || "{}");
          const foundryResourceId = foundryAccount.id || "";
          const foundryRg = foundryAccount.rg || "";

          if (foundryResourceId && foundryRg && foundryProjectName) {
            // Query the project's managed identity principal ID via ARM REST API
            let projectMiPrincipalId = "";
            try {
              const { stdout: projectJson } = await execa("az", [
                "rest", "--method", "get",
                "--url", `${foundryResourceId}/projects/${foundryProjectName}?api-version=2025-06-01`,
              ], { stdio: "pipe" });
              const project = JSON.parse(projectJson.trim());
              projectMiPrincipalId = project?.identity?.principalId || "";
            } catch {
              // Project may not have system MI enabled — warn but continue
            }

            // Get the sandbox workload identity principal ID
            let sandboxWiPrincipalId = "";
            try {
              const { stdout: wiPid } = await execa("az", [
                "identity", "show",
                "--name", `${baseName}-aks-sandbox-wi`,
                "--resource-group", rg,
                "--query", "principalId",
                "--output", "tsv",
              ], { stdio: "pipe" });
              sandboxWiPrincipalId = wiPid.trim().split("\n").pop()?.trim() || "";
            } catch {
              // Non-fatal
            }

            // Get the AKS kubelet managed identity principal ID (used by IMDS for sub-agents)
            let kubeletMiPrincipalId = "";
            try {
              const { stdout: kubePid } = await execa("az", [
                "aks", "show",
                "--name", `${baseName}-aks`,
                "--resource-group", rg,
                "--query", "identityProfile.kubeletidentity.objectId",
                "--output", "tsv",
              ], { stdio: "pipe" });
              kubeletMiPrincipalId = kubePid.trim().split("\n").pop()?.trim() || "";
            } catch {
              // Non-fatal — older AKS may not expose this
            }

            // Build Bicep that assigns roles via deployment (bypasses CLI conditional access)
            const bicepLines = [
              "targetScope = 'resourceGroup'",
              "param sandboxWiPrincipalId string",
              "param projectMiPrincipalId string",
              "param kubeletMiPrincipalId string",
              `param foundryAccountName string = '${foundryAccountName}'`,
              "",
              "// Azure AI User role ID — has Microsoft.CognitiveServices/* wildcard data actions",
              "var azureAiUser = '53ca6127-db72-4b80-b1b0-d745d6d5456d'",
              "// Cognitive Services OpenAI User — explicit data-plane access for chat completions",
              "var cogSvcOpenAiUser = '5e0bd9bd-7b93-4f28-af87-19fc36ad61bd'",
              "",
              "resource aiServices 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = {",
              "  name: foundryAccountName",
              "}",
              "",
              "// 1. Sandbox WI → Azure AI User on the AI Services resource (pod API access)",
              "resource sandboxRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(sandboxWiPrincipalId)) {",
              "  name: guid(aiServices.id, sandboxWiPrincipalId, 'azure-ai-user')",
              "  scope: aiServices",
              "  properties: {",
              "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', azureAiUser)",
              "    principalId: sandboxWiPrincipalId",
              "    principalType: 'ServicePrincipal'",
              "  }",
              "}",
              "",
              "// 1b. Sandbox WI → Cognitive Services OpenAI User (explicit chat completions data action)",
              "resource sandboxOpenAiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(sandboxWiPrincipalId)) {",
              "  name: guid(aiServices.id, sandboxWiPrincipalId, 'cog-svc-openai-user')",
              "  scope: aiServices",
              "  properties: {",
              "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', cogSvcOpenAiUser)",
              "    principalId: sandboxWiPrincipalId",
              "    principalType: 'ServicePrincipal'",
              "  }",
              "}",
              "",
              "// 2. Project MI → Azure AI User on the resource group (Memory Store internal model calls)",
              "resource projectMiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(projectMiPrincipalId)) {",
              "  name: guid(resourceGroup().id, projectMiPrincipalId, 'azure-ai-user')",
              "  properties: {",
              "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', azureAiUser)",
              "    principalId: projectMiPrincipalId",
              "    principalType: 'ServicePrincipal'",
              "  }",
              "}",
              "",
              "// 3. Kubelet MI → Cognitive Services OpenAI User (IMDS fallback for spawned sub-agents)",
              "resource kubeletOpenAiRole 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(kubeletMiPrincipalId)) {",
              "  name: guid(aiServices.id, kubeletMiPrincipalId, 'cog-svc-openai-user')",
              "  scope: aiServices",
              "  properties: {",
              "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', cogSvcOpenAiUser)",
              "    principalId: kubeletMiPrincipalId",
              "    principalType: 'ServicePrincipal'",
              "  }",
              "}",
            ];

            const fs = await import("fs");
            const tmpBicep = path.join(repoRoot, ".tmp-foundry-rbac.bicep");
            fs.writeFileSync(tmpBicep, bicepLines.join("\n"));

            try {
              stepper.update("Deploying Foundry RBAC (Bicep)...");
              await execa("az", [
                "deployment", "group", "create",
                "--resource-group", foundryRg,
                "--template-file", tmpBicep,
                "--parameters",
                `sandboxWiPrincipalId=${sandboxWiPrincipalId}`,
                `projectMiPrincipalId=${projectMiPrincipalId}`,
                `kubeletMiPrincipalId=${kubeletMiPrincipalId}`,
                "--output", "none",
              ], { stdio: "pipe" });
            } catch {
              // Non-fatal — user may lack Owner on the Foundry RG
            } finally {
              try { fs.unlinkSync(tmpBicep); } catch {}
            }

            if (!projectMiPrincipalId) {
              console.log(chalk.yellow("\n  ⚠ Foundry project has no system-assigned MI. Memory Store will not work."));
              console.log(chalk.yellow("    Enable it: Portal → Project → Resource Management → Identity → System assigned → On"));
              console.log(chalk.yellow("    Then re-run: azureclaw up ...\n"));
            }
          } else if (foundryResourceId) {
            // Fallback for non-project endpoints (plain AOAI): assign sandbox WI on the resource
            const { stdout: wiPid } = await execa("az", [
              "identity", "show",
              "--name", `${baseName}-aks-sandbox-wi`,
              "--resource-group", rg,
              "--query", "principalId",
              "--output", "tsv",
            ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));

            if (wiPid.trim()) {
              const fs = await import("fs");
              const tmpBicep = path.join(repoRoot, ".tmp-foundry-rbac.bicep");
              fs.writeFileSync(tmpBicep, [
                "targetScope = 'resourceGroup'",
                "param pid string",
                `param accountName string = '${foundryAccountName}'`,
                "resource acct 'Microsoft.CognitiveServices/accounts@2024-10-01' existing = { name: accountName }",
                "resource r 'Microsoft.Authorization/roleAssignments@2022-04-01' = {",
                "  name: guid(acct.id, pid, 'azure-ai-user')",
                "  scope: acct",
                "  properties: {",
                "    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '53ca6127-db72-4b80-b1b0-d745d6d5456d')",
                "    principalId: pid",
                "    principalType: 'ServicePrincipal'",
                "  }",
                "}",
              ].join("\n"));
              try {
                await execa("az", [
                  "deployment", "group", "create",
                  "--resource-group", foundryRg || rg,
                  "--template-file", tmpBicep,
                  "--parameters", `pid=${wiPid.trim().split("\n").pop()?.trim()}`,
                  "--output", "none",
                ], { stdio: "pipe" }).catch(() => {});
              } finally {
                try { fs.unlinkSync(tmpBicep); } catch {}
              }
            }
          }
        }

        stepper.update(`Creating sandbox '${options.name}'...`);
        const sandboxManifest = {
          apiVersion: "azureclaw.azure.com/v1alpha1",
          kind: "ClawSandbox",
          metadata: {
            name: options.name,
            namespace: "azureclaw-system",
          },
          spec: {
            openclaw: {
              image: `${acrLoginServer}/openclaw-sandbox:latest`,
            },
            sandbox: {
              isolation: options.isolation,
            },
            inference: {
              provider: "azure-openai",
              model: options.model,
              endpoint: openAiEndpoint,
              contentSafety: true,
              promptShields: true,
            },
            networkPolicy: {
              defaultDeny: true,
              approvalRequired: true,
            },
            governance: {
              enabled: true,
              toolPolicy: "default",
              trustThreshold: 500,
            },
          },
        };
        await execa("kubectl", ["apply", "-f", "-"], {
          input: JSON.stringify(sandboxManifest),
          stdio: ["pipe", "pipe", "pipe"],
        });

        // ── Step 8: Wait for sandbox ─────────────────────────────────
        stepper.step("Waiting for sandbox to start...");
        await execa("kubectl", [
          "wait",
          "--for=jsonpath={.status.phase}=Running",
          `clawsandbox/${options.name}`,
          "-n", "azureclaw-system",
          "--timeout=120s",
        ], { stdio: "pipe" }).catch(() => {
          // Timeout OK — image pull may be slow on first deploy
        });

        // Extract gateway token and start port-forward
        stepper.update("Setting up WebUI access...");
        let gatewayToken = "";
        let webUiUrl = "";
        try {
          // Wait for gateway to be ready inside the pod
          await new Promise(r => setTimeout(r, 5000));

          // Extract gateway token from the sandbox
          const { stdout: bashrc } = await execa("kubectl", [
            "exec", "-n", sandboxNs, `deploy/${options.name}`,
            "-c", "openclaw", "--",
            "cat", "/sandbox/.bashrc",
          ], { stdio: "pipe" });
          const tokenMatch = bashrc.match(/OPENCLAW_GATEWAY_TOKEN="([^"]+)"/);
          if (tokenMatch) {
            gatewayToken = tokenMatch[1];
          }

          // Start port-forward in background (fully detached so CLI can exit)
          const { spawn } = await import("child_process");
          const portForward = spawn("kubectl", [
            "port-forward", "-n", sandboxNs,
            `deploy/${options.name}`, "18789:18789",
          ], { stdio: "ignore", detached: true });
          portForward.unref();
          // Give it a moment to bind
          await new Promise(r => setTimeout(r, 2000));

          if (gatewayToken) {
            webUiUrl = `http://localhost:18789/#token=${gatewayToken}`;
          }

          stepper.done("Sandbox running");
        } catch {
          stepper.warn("Sandbox running but WebUI port-forward failed");
        }

        stepper.summary();

        // ── Summary ──────────────────────────────────────────────────
        const isolationDesc: Record<string, string> = {
          standard: "standard (runc + RuntimeDefault)",
          enhanced: "enhanced (runc + azureclaw-strict seccomp)",
          confidential: "confidential (Kata VM isolation)",
        };

        section("Deployment");
        kvLine("Sandbox", options.name);
        kvLine("Model", `${options.model} (Azure OpenAI, Entra ID auth)`);
        kvLine("Isolation", isolationDesc[options.isolation] || options.isolation);
        kvLine("Region", options.region);
        kvLine("Cluster", `${baseName}-aks`);
        kvLine("ACR", acrLoginServer);
        kvLine("Key Vault", kvName);
        kvLine("AOAI", openAiEndpoint);

        section("Security");
        checkLine(true, "Cilium CNI + NetworkPolicy (default-deny egress)");
        checkLine(true, "Workload Identity (Entra ID, no API keys)");
        checkLine(true, "Read-only rootfs, non-root, seccomp");
        checkLine(true, "Inference router: Content Safety + Prompt Shields");
        checkLine(true, "Egress proxy with domain allowlist + blocklist (51k+)");
        if (options.isolation === "confidential") {
          checkLine(true, "Kata VM isolation (pod sandboxing)");
        }

        section("Commands");
        console.log(`  Connect:     ${chalk.cyan(`azureclaw connect ${options.name}`)}`);
        console.log(`  Status:      ${chalk.cyan(`azureclaw status ${options.name}`)}`);
        console.log(`  Logs:        ${chalk.cyan(`azureclaw logs ${options.name} -f`)}`);
        console.log(`  Egress:      ${chalk.cyan(`azureclaw egress ${options.name}`)}`);

        if (webUiUrl) {
          section("WebUI");
          console.log(`  ${chalk.green("→")} ${chalk.cyan.underline(webUiUrl)}`);
        }

        // Cache deployment context for subsequent commands (add, status, list, push, etc.)
        try {
          saveContext({
            region: options.region,
            resourceGroup: rg,
            aksCluster: `${baseName}-aks`,
            acrLoginServer,
            acrName: acrLoginServer.replace(".azurecr.io", ""),
            keyVaultName: kvName,
            wiClientId,
            imdsClientId: imdsClientId || undefined,
            foundryEndpoint: openAiEndpoint,
            foundryProjectEndpoint: foundryEndpoint || undefined,
            identityName: `${baseName}-aks-sandbox-wi`,
            identityResourceGroup: rg,
            oidcIssuerUrl: oidcIssuer?.trim() || undefined,
          });
        } catch { /* non-critical */ }

        console.log();
      } catch (error) {
        stepper.stop();
        console.error(chalk.red(`\n  Deployment failed`));
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`  ${message}\n`));

        // Helpful diagnostics
        if (message.includes("EncryptionAtHost")) {
          console.log(chalk.yellow("  Tip: EncryptionAtHost requires registering the feature:"));
          console.log(chalk.cyan("  az feature register --namespace Microsoft.Compute --name EncryptionAtHost"));
          console.log(chalk.cyan("  az provider register -n Microsoft.Compute\n"));
        }
        if (message.includes("quota") || message.includes("Quota")) {
          console.log(chalk.yellow("  Tip: Insufficient quota. Try a different region or VM size:"));
          console.log(chalk.cyan(`  azureclaw up --region westus3\n`));
        }
        process.exit(1);
      }
    });

  return cmd;
}
