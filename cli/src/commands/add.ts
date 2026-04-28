import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import { loadContext, resolveSecret } from "../config.js";

export function addCommand(): Command {
  const cmd = new Command("add");

  cmd
    .description("Add a new sandboxed agent to an existing AzureClaw cluster")
    .argument("<name>", "Name for the new sandbox agent")
    .option("--model <model>", "AI model", "gpt-4.1")
    .option("--isolation <level>", "Isolation level: standard | enhanced | confidential", "enhanced")
    .option("--token-budget-daily <tokens>", "Daily token budget (0 = unlimited)", "0")
    .option("--token-budget-per-request <tokens>", "Per-request token limit (0 = unlimited)", "0")
    .option("--agent-instructions <instructions>", "System prompt for Foundry agent")
    .option("--agent-tools <tools>", "Foundry tools: file_search,web_search,code_interpreter (comma-separated)")
    .option("--image <image>", "Custom sandbox image (default: from Helm values)")
    .option("--governance", "Enable AGT governance (tool policy, trust, audit)", true)
    .option("--no-governance", "Disable AGT governance")
    .option("--trust-threshold <score>", "AGT trust threshold (0-1000, default: 500)", "500")
    .option("--policy-profile <profile>", "AGT policy profile name", "default")
    .option("--channels <channels>", "Channels to enable: telegram,slack,discord,whatsapp (comma-separated)")
    .option("--telegram-token <token>", "Telegram bot token (from BotFather)")
    .option("--telegram-allow-from <ids>", "Telegram user IDs allowed to DM (comma-separated numeric IDs)")
    .option("--slack-token <token>", "Slack bot OAuth token")
    .option("--discord-token <token>", "Discord bot token")
    .option("--skills <skills>", "Skills to activate: browser,github,summarize,weather (comma-separated)")
    // Third-party plugin API keys (stored as K8s secrets on AKS)
    .option("--brave-api-key <key>", "Brave Search API key")
    .option("--tavily-api-key <key>", "Tavily search API key")
    .option("--exa-api-key <key>", "Exa search API key")
    .option("--firecrawl-api-key <key>", "Firecrawl web scraping API key")
    .option("--perplexity-api-key <key>", "Perplexity API key")
    .option("--openai-api-key <key>", "OpenAI API key (for dual-provider setups)")
    .option("--learn-egress", "Enable egress learn mode: observe all domains (blocklist still enforced), then review with 'azureclaw policy learn'", false)
    .option("--dry-run", "Print the ClawSandbox YAML without applying", false)
    .action(async (name: string, options) => {
      const { execa } = await import("execa");

      const sandbox: Record<string, unknown> = {
        apiVersion: "azureclaw.azure.com/v1alpha1",
        kind: "ClawSandbox",
        metadata: {
          name,
          namespace: "azureclaw-system",
        },
        spec: {
          runtime: {
            kind: "OpenClaw",
            openclaw: {
              version: "2026.3.13",
              ...(options.image ? { image: options.image } : {}),
              config: {
                agent: {
                  model: `azure/${options.model}`,
                },
              },
            },
          },
          sandbox: {
            isolation: options.isolation,
            seccompProfile: options.isolation === "standard" ? "RuntimeDefault" : "azureclaw-strict",
            readOnlyRootFilesystem: true,
            runAsNonRoot: true,
            allowPrivilegeEscalation: false,
            writablePaths: ["/sandbox", "/tmp"],
          },
          inference: {
            provider: "azure-ai-foundry",
            model: options.model,
            contentSafety: true,
            promptShields: true,
            tokenBudget: {
              daily: parseInt(options.tokenBudgetDaily) || 0,
              perRequest: parseInt(options.tokenBudgetPerRequest) || 0,
            },
          },
          networkPolicy: {
            defaultDeny: true,
            approvalRequired: true,
            allowedEndpoints: [
              { host: "github.com", port: 443 },
              { host: "api.github.com", port: 443 },
            ],
          },
          resources: {
            requests: { cpu: "500m", memory: "1Gi" },
            limits: { cpu: "2", memory: "4Gi" },
          },
        },
      };

      // Add Foundry agent config if provided
      if (options.agentInstructions || options.agentTools) {
        const agentSpec: Record<string, unknown> = {};
        if (options.agentInstructions) {
          agentSpec.instructions = options.agentInstructions;
        }
        if (options.agentTools) {
          agentSpec.tools = options.agentTools.split(",").map((t: string) => t.trim());
        }
        (sandbox.spec as Record<string, unknown>).agent = agentSpec;
      }

      // Add AGT governance config if enabled
      if (options.governance) {
        (sandbox.spec as Record<string, unknown>).governance = {
          enabled: true,
          toolPolicy: options.policyProfile || "default",
          trustThreshold: parseInt(options.trustThreshold) || 500,
        };
      }

      // Egress learn mode
      if (options.learnEgress) {
        const np = (sandbox.spec as Record<string, unknown>).networkPolicy as Record<string, unknown>;
        np.learnEgress = true;
      }

      // Channel and plugin credentials — stored in K8s secret, NOT in CRD spec.
      // The entrypoint reads env vars and auto-configures channels/plugins.
      let channelEnvSecrets: Record<string, string> = {};

      // Channel configuration — map channels to env vars and secrets.
      // Channel domains are NOT auto-added to the egress allowlist — they go through
      // the learn→approve flow so operators can see and approve each domain explicitly.
      const channelTokenFlags: Record<string, string> = {
        telegram: "telegramToken",
        slack: "slackToken",
        discord: "discordToken",
      };
      const channelEnvVars: Record<string, string> = {
        telegram: "TELEGRAM_BOT_TOKEN",
        slack: "SLACK_BOT_TOKEN",
        discord: "DISCORD_BOT_TOKEN",
        whatsapp: "WHATSAPP_ENABLED",
      };
      const knownChannels = new Set(["telegram", "slack", "discord", "whatsapp"]);

      if (options.channels) {
        const channels = options.channels.split(",").map((c: string) => c.trim().toLowerCase());
        const envSecrets: Record<string, string> = {};

        // Map channel names to secret keys for resolveSecret lookup
        const channelSecretKeys: Record<string, string> = {
          telegram: "telegram-token",
          slack: "slack-token",
          discord: "discord-token",
        };

        for (const channel of channels) {
          if (!knownChannels.has(channel)) {
            console.error(chalk.yellow(`  ⚠ Unknown channel '${channel}' — skipping`));
            continue;
          }
          const tokenFlag = channelTokenFlags[channel];
          const secretKey = channelSecretKeys[channel];
          // Resolve: CLI flag > secrets.json > env var
          const resolved = secretKey
            ? resolveSecret(tokenFlag ? options[tokenFlag] : undefined, secretKey)
            : undefined;
          if (resolved) {
            envSecrets[channelEnvVars[channel]] = resolved;
          } else if (channel === "whatsapp") {
            envSecrets[channelEnvVars[channel]] = "true";
          } else if (tokenFlag && !resolved) {
            console.error(chalk.yellow(`  ⚠ Channel '${channel}' enabled but no token found (use --${channel}-token or 'azureclaw credentials set ${channel}-token <token>')`));
          }
        }

        // Store for secret creation (NOT in CRD spec — entrypoint reads env vars)
        channelEnvSecrets = envSecrets;

        // Telegram allow-from (which user IDs can DM the bot)
        if (channels.includes("telegram")) {
          const allowFrom = resolveSecret(options.telegramAllowFrom, "telegram-allow-from");
          if (allowFrom) channelEnvSecrets["TELEGRAM_ALLOW_FROM"] = allowFrom;
        }
      }

      // Third-party plugin API keys — stored in the same K8s secret as channel tokens.
      // The entrypoint auto-enables plugins when their env var is present.
      // Resolve: CLI flag > secrets.json > env var
      const pluginKeyFlags: Record<string, { flag: string; env: string; secretKey: string }> = {
        brave:      { flag: "braveApiKey",      env: "BRAVE_API_KEY",       secretKey: "brave-api-key" },
        tavily:     { flag: "tavilyApiKey",     env: "TAVILY_API_KEY",      secretKey: "tavily-api-key" },
        exa:        { flag: "exaApiKey",        env: "EXA_API_KEY",         secretKey: "exa-api-key" },
        firecrawl:  { flag: "firecrawlApiKey",  env: "FIRECRAWL_API_KEY",   secretKey: "firecrawl-api-key" },
        perplexity: { flag: "perplexityApiKey", env: "PERPLEXITY_API_KEY",  secretKey: "perplexity-api-key" },
        openai:     { flag: "openaiApiKey",     env: "OPENAI_API_KEY",      secretKey: "openai-api-key" },
      };
      const pluginSecrets: Record<string, string> = {};
      for (const [, { flag, env, secretKey }] of Object.entries(pluginKeyFlags)) {
        const resolved = resolveSecret(options[flag], secretKey);
        if (resolved) {
          pluginSecrets[env] = resolved;
        }
      }

      // Skills — just log for now (entrypoint activates pre-installed skills)
      if (options.skills) {
        console.log(chalk.dim(`  Skills: ${options.skills}`));
      }

      const yaml = JSON.stringify(sandbox, null, 2);

      if (options.dryRun) {
        console.log(chalk.bold("\nClawSandbox manifest (dry-run):\n"));
        console.log(yaml);
        console.log(chalk.dim("\nApply with: kubectl apply -f <file>"));
        return;
      }

      // Pre-flight: check for Kata nodepool when confidential isolation is requested
      if (options.isolation === "confidential") {
        let kataReady = false;
        try {
          const { stdout } = await execa("kubectl", [
            "get", "nodes", "-l", "azureclaw.azure.com/pool=sandbox-kata",
            "--no-headers",
          ], { stdio: "pipe" });
          kataReady = stdout.trim().split("\n").filter(Boolean).length > 0;
        } catch { /* no nodes */ }

        if (!kataReady) {
          const ctx = loadContext();
          if (ctx?.aksCluster && ctx?.resourceGroup) {
            console.log(chalk.yellow("\n⚠  No Kata nodepool found."));
            console.log(chalk.dim("  Confidential isolation requires a nodepool with Kata VM runtime.\n"));

            const { default: inquirer } = await import("inquirer");
            const { provision } = await inquirer.prompt([{
              type: "confirm",
              name: "provision",
              message: `Provision a Kata nodepool on ${ctx.aksCluster}? (takes ~3-5 min)`,
              default: true,
            }]);

            if (!provision) {
              console.log(chalk.dim("Aborted."));
              process.exit(0);
            }

            // DC-series for confidential Kata — AMD SEV-SNP (matches up.ts --isolation confidential)
            const kataSpinner = ora("Provisioning Kata nodepool (Standard_DC4as_v5, AzureLinux)...").start();
            try {
              await execa("az", [
                "aks", "nodepool", "add",
                "--resource-group", ctx.resourceGroup,
                "--cluster-name", ctx.aksCluster,
                "--name", "katapool",
                "--node-count", "1",
                "--node-vm-size", "Standard_DC4as_v5",
                "--os-sku", "AzureLinux",
                "--workload-runtime", "KataVmIsolation",
                "--labels", "azureclaw.azure.com/pool=sandbox-kata",
                "--node-taints", "azureclaw.azure.com/sandbox=true:NoSchedule",
              ], { stdio: "pipe", timeout: 600_000 });
              kataSpinner.succeed("Kata nodepool provisioned");
            } catch (e: any) {
              kataSpinner.fail("Failed to provision Kata nodepool");
              console.error(chalk.red(e.stderr?.substring(0, 200) || e.message));
              process.exit(1);
            }
          } else {
            console.error(chalk.red("\n✗ No Kata nodepool found and no cached cluster context."));
            console.error(chalk.dim("  Run 'azureclaw up --isolation confidential' first, or provision a katapool manually."));
            process.exit(1);
          }
        }
      }

      const spinner = ora(`Creating sandbox '${name}' (${options.isolation}, ${options.model})...`).start();

      try {
        // Verify cluster is reachable
        await execa("kubectl", ["get", "crd", "clawsandboxes.azureclaw.azure.com"], { stdio: "pipe" });

        // Create federated credential FIRST (before CRD) so it propagates while pod starts
        const namespace = `azureclaw-${name}`;
        try {
          spinner.text = "Creating federated credential...";
          const ctx = loadContext();

          let identityName = ctx?.identityName;
          let identityRg = ctx?.identityResourceGroup || ctx?.resourceGroup;
          let issuerUrl = ctx?.oidcIssuerUrl;

          if (!identityName || !identityRg || !issuerUrl) {
            const { stdout: wiClientId } = await execa("kubectl", [
              "get", "sa", "-n", "azureclaw-system", "azureclaw-controller",
              "-o", "jsonpath={.metadata.annotations.azure\\.workload\\.identity/client-id}",
            ], { stdio: "pipe" });

            if (wiClientId) {
              const { stdout: identityJson } = await execa("az", [
                "identity", "list",
                "--query", `[?clientId=='${wiClientId}'].{name:name, rg:resourceGroup}`,
                "--output", "json",
              ], { stdio: "pipe" });
              const identities = JSON.parse(identityJson || "[]");
              if (identities[0]) {
                identityName = identities[0].name;
                identityRg = identities[0].rg;
              }

              if (identityRg && !issuerUrl) {
                const { stdout: aksJson } = await execa("az", [
                  "aks", "list", "--resource-group", identityRg,
                  "--query", "[0].oidcIssuerProfile.issuerUrl", "--output", "tsv",
                ], { stdio: "pipe" });
                issuerUrl = aksJson.trim();
              }
            }
          }

          if (identityName && identityRg && issuerUrl) {
            await execa("az", [
              "identity", "federated-credential", "create",
              "--name", `azureclaw-${name}`,
              "--identity-name", identityName,
              "--resource-group", identityRg,
              "--issuer", issuerUrl,
              "--subject", `system:serviceaccount:${namespace}:sandbox`,
              "--audience", "api://AzureADTokenExchange",
            ], { stdio: "pipe" }).catch(() => { /* may already exist */ });
          }
        } catch {
          // Non-fatal
        }

        // Now apply the CRD (controller will create pod — fedcred already propagating)
        spinner.text = `Creating sandbox '${name}'...`;

        // Create K8s secret for channel tokens and plugin API keys
        const allSecrets = {
          ...channelEnvSecrets,
          ...pluginSecrets,
        };
        if (Object.keys(allSecrets).length > 0) {
          spinner.text = "Creating credential secret...";
          try {
            // Ensure namespace exists
            await execa("kubectl", ["create", "namespace", namespace], { stdio: "pipe" }).catch(() => {});
            const secretArgs = ["create", "secret", "generic", `${name}-credentials`, "-n", namespace];
            for (const [envVar, value] of Object.entries(allSecrets)) {
              secretArgs.push(`--from-literal=${envVar}=${value}`);
            }
            await execa("kubectl", secretArgs, { stdio: "pipe" }).catch(async () => {
              // Already exists — delete and recreate with updated values
              await execa("kubectl", ["delete", "secret", `${name}-credentials`, "-n", namespace], { stdio: "pipe" }).catch(() => {});
              return execa("kubectl", secretArgs, { stdio: "pipe" });
            });
          } catch {
            // Non-fatal — controller can still create pod without credential secret
          }
        }
        spinner.text = `Creating sandbox '${name}'...`;
        await execa("kubectl", ["apply", "-f", "-"], {
          input: JSON.stringify(sandbox),
          stdio: ["pipe", "pipe", "pipe"],
        });

        // The controller auto-mounts <name>-credentials secret via envFrom (optional: true).
        // If the secret exists, env vars are injected into the sandbox container at startup.
        // No deployment patching needed — the controller handles it natively.

        // Wait for pod to be ready and WI token to propagate
        spinner.text = `Waiting for '${name}' to be ready...`;
        const maxWait = 120; // seconds
        const start = Date.now();
        let ready = false;
        for (let i = 0; i < maxWait / 3; i++) {
          await new Promise(r => setTimeout(r, 3000));
          const elapsed = Math.round((Date.now() - start) / 1000);
          spinner.text = `Waiting for '${name}' to be ready... (${elapsed}s)`;
          try {
            // Check if pod is running with both containers ready
            const { stdout: phase } = await execa("kubectl", [
              "get", "pods", "-n", namespace,
              "-o", "jsonpath={.items[0].status.containerStatuses[*].ready}",
            ], { stdio: "pipe", timeout: 5000 });
            if (!phase.includes("true")) continue;

            // Verify LLM access works (router health + token exchange)
            const { stdout: healthz } = await execa("kubectl", [
              "exec", "-n", namespace,
              "deploy/" + name, "-c", "inference-router",
              "--", "wget", "-qO-", "--timeout=5", "http://127.0.0.1:8443/healthz",
            ], { stdio: "pipe", timeout: 10000 }).catch(() => ({ stdout: "" }));
            if (healthz.includes("ok") || healthz.includes("healthy")) {
              ready = true;
              break;
            }
          } catch { /* pod not ready yet */ }
        }

        if (ready) {
          spinner.succeed(`Sandbox '${name}' ready`);
        } else {
          spinner.succeed(`Sandbox '${name}' created (may still be starting)`);
        }
        console.log(chalk.dim(`  Namespace:  ${namespace}`));
        console.log(chalk.dim(`  Model:      ${options.model}`));
        console.log(chalk.dim(`  Isolation:  ${options.isolation}`));
        if (options.channels) {
          console.log(chalk.dim(`  Channels:   ${options.channels}`));
        }
        if (options.skills) {
          console.log(chalk.dim(`  Skills:     ${options.skills}`));
        }
        console.log(chalk.dim(`  Status:     kubectl get clawsandbox ${name} -n azureclaw-system`));
        console.log(chalk.dim(`  Connect:    azureclaw connect ${name}`));
        console.log(chalk.dim(`  Remove:     azureclaw destroy ${name}\n`));

      } catch (error) {
        spinner.fail("Failed to create sandbox");
        const message = error instanceof Error ? error.message : String(error);
        if (message.includes("clawsandboxes.azureclaw.azure.com")) { // lgtm[js/incomplete-url-substring-sanitization] — error message check, not URL validation
          console.error(chalk.red("\n  AzureClaw is not installed on this cluster."));
          console.error(chalk.red("  Run 'azureclaw up' first to deploy the infrastructure.\n"));
        } else {
          console.error(chalk.red(`\n  Error: ${message}\n`));
        }
      }
    });

  return cmd;
}
