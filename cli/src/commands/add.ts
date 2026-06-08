// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import { loadContext, resolveSecret } from "../config.js";
import { assertRuntimeWired, buildRuntimeBlock, flagToKind } from "../runtime.js";
import {
  buildInferencePolicy,
  buildToolPolicy,
  inferenceRefName,
  toolPolicyRefName,
} from "../refs.js";

export function addCommand(): Command {
  const cmd = new Command("add");

  cmd
    .description("Add a new sandboxed agent to an existing kars cluster")
    .argument("<name>", "Name for the new sandbox agent")

    // ── Core (all runtimes) ────────────────────────────────────────────
    .option("--runtime <kind>", "Runtime kind: openclaw | openai-agents | microsoft-agent-framework | langgraph | anthropic | pydantic-ai | hermes | byo", "openclaw")
    .option("--model <model>", "AI model deployment name in Foundry", "gpt-4.1")
    .option("--isolation <level>", "Isolation level: standard | enhanced | confidential", "enhanced")
    .option("--image <image>", "Custom sandbox image (default: from Helm values; OpenClaw runtime only)")

    // ── Inference budget (all runtimes) ────────────────────────────────
    .option("--token-budget-daily <tokens>", "Daily token budget (0 = unlimited)", "0")
    .option("--token-budget-per-request <tokens>", "Per-request token limit (0 = unlimited)", "0")

    // ── Governance / network (all runtimes) ────────────────────────────
    // Note: router-side content safety, audit, and rate limits are ALWAYS
    // on. `--governance` only controls whether per-sandbox AGT
    // ToolPolicy / TrustGraph CRs are generated.
    .option("--governance", "Generate per-sandbox AGT ToolPolicy + TrustGraph CRs (router guardrails are always on regardless)", true)
    .option("--no-governance", "Skip generating per-sandbox ToolPolicy / TrustGraph CRs (router guardrails still enforced)")
    .option("--trust-threshold <score>", "AGT trust threshold (0-1000)", "500")
    .option("--policy-profile <profile>", "AGT policy profile name", "default")
    .option("--learn-egress", "Egress learn mode: observe outbound domains (blocklist still enforced); review with 'kars policy learn'", false)

    // ── Foundry agent (all runtimes; optional) ─────────────────────────
    .option("--agent-instructions <instructions>", "System prompt for the Foundry agent")
    .option("--agent-tools <tools>", "Foundry tools: file_search,web_search,code_interpreter (comma-separated)")

    // ── Runtime-specific: OpenClaw + Hermes (channel-capable runtimes) ─
    .option("--channels <channels>", "[OpenClaw + Hermes] Channels to enable: telegram,slack,discord,whatsapp (comma-separated)")
    .option("--telegram-token <token>", "[OpenClaw + Hermes] Telegram bot token (from BotFather)")
    .option("--telegram-allow-from <ids>", "[OpenClaw + Hermes] Telegram user IDs allowed to DM (comma-separated)")
    .option("--slack-token <token>", "[OpenClaw + Hermes] Slack bot OAuth token")
    .option("--discord-token <token>", "[OpenClaw + Hermes] Discord bot token")
    // ── Runtime-specific: OpenClaw only (skills + plugin API keys) ─────
    .option("--skills <skills>", "[OpenClaw only] Skills to activate: browser,github,summarize,weather (comma-separated)")
    .option("--brave-api-key <key>", "[OpenClaw only] Brave Search API key")
    .option("--tavily-api-key <key>", "[OpenClaw only] Tavily search API key")
    .option("--exa-api-key <key>", "[OpenClaw only] Exa search API key")
    .option("--firecrawl-api-key <key>", "[OpenClaw only] Firecrawl web scraping API key")
    .option("--perplexity-api-key <key>", "[OpenClaw only] Perplexity API key")
    .option("--openai-api-key <key>", "[OpenClaw only] OpenAI API key (for dual-provider setups)")

    // ── Runtime-specific: BYO ──────────────────────────────────────────
    .option("--byo-image <image>", "[BYO only] Container image (must declare org.kars.runtime.contract=v1)")
    .option("--byo-contract-version <version>", "[BYO only] BYO contract version", "v1")

    // ── Runtime-specific: Microsoft Agent Framework ────────────────────
    .option("--maf-language <lang>", "[MAF only] Microsoft Agent Framework language: python (dotnet not yet wired)", "python")

    // ── Output control ─────────────────────────────────────────────────
    .option("--dry-run", "Print the KarsSandbox YAML without applying", false)
    .addHelpText("after", `
Flag groups (see --help for details):
  Core:                --runtime, --model, --isolation, --image
  Inference budget:    --token-budget-*
  Governance / net:    --governance, --trust-threshold, --policy-profile, --learn-egress
  Foundry agent:       --agent-instructions, --agent-tools
  Channels (OpenClaw + Hermes):  --channels, --telegram-*, --slack-*, --discord-*
  OpenClaw only:       --skills, --*-api-key
  BYO only:            --byo-image, --byo-contract-version
  MAF only:            --maf-language

Note: Router-side guardrails (content safety, rate limits, audit log,
egress allowlist) are ALWAYS enforced. --no-governance only skips
generating per-sandbox AGT ToolPolicy / TrustGraph CRs.
`)
    .action(async (name: string, options) => {
      const { execa } = await import("execa");

      const runtimeKind = flagToKind(options.runtime);
      assertRuntimeWired(runtimeKind);

      // Validate runtime-specific flag combinations before doing any work.
      // Reject incompatible flags up-front with a clear, actionable error
      // — better than silently ignoring a user's intent.
      const openClawOnlyFlags: Array<[string, unknown]> = [
        ["--channels", options.channels],
        ["--telegram-token", options.telegramToken],
        ["--telegram-allow-from", options.telegramAllowFrom],
        ["--slack-token", options.slackToken],
        ["--discord-token", options.discordToken],
        ["--skills", options.skills],
        ["--brave-api-key", options.braveApiKey],
        ["--tavily-api-key", options.tavilyApiKey],
        ["--exa-api-key", options.exaApiKey],
        ["--firecrawl-api-key", options.firecrawlApiKey],
        ["--perplexity-api-key", options.perplexityApiKey],
        ["--openai-api-key", options.openaiApiKey],
        ["--image", options.image],
      ];
      if (runtimeKind !== "OpenClaw") {
        const used = openClawOnlyFlags.filter(([, v]) => v !== undefined && v !== "" && v !== false).map(([f]) => f);
        if (used.length > 0) {
          console.error(chalk.red(`\n  Error: ${used.join(", ")} ${used.length === 1 ? "is" : "are"} only valid with --runtime openclaw.`));
          console.error(chalk.dim(`  Channels, skills, and plugin API keys are OpenClaw-specific entrypoint features.`));
          console.error(chalk.dim(`  For ${options.runtime}, configure equivalents inside the agent's own code.\n`));
          process.exit(1);
        }
      }
      if (runtimeKind !== "BYO" && (options.byoImage || (options.byoContractVersion && options.byoContractVersion !== "v1"))) {
        console.error(chalk.red(`\n  Error: --byo-image / --byo-contract-version are only valid with --runtime byo.\n`));
        process.exit(1);
      }
      if (runtimeKind === "BYO" && !options.byoImage) {
        console.error(chalk.red(`\n  Error: --runtime byo requires --byo-image <registry/image:tag>.\n`));
        process.exit(1);
      }
      if (runtimeKind !== "MicrosoftAgentFramework" && options.mafLanguage && options.mafLanguage !== "python") {
        console.error(chalk.red(`\n  Error: --maf-language is only valid with --runtime microsoft-agent-framework.\n`));
        process.exit(1);
      }

      const runtimeBlock = buildRuntimeBlock({
        kind: runtimeKind,
        openclawVersion: "2026.3.13",
        model: options.model,
        image: options.image,
        byoImage: options.byoImage,
        byoContractVersion: options.byoContractVersion,
        mafLanguage: options.mafLanguage as "python" | "dotnet",
      });

      const sandbox: Record<string, unknown> = {
        apiVersion: "kars.azure.com/v1alpha1",
        kind: "KarsSandbox",
        metadata: {
          name,
          namespace: "kars-system",
        },
        spec: {
          runtime: runtimeBlock,
          sandbox: {
            isolation: options.isolation,
            seccompProfile: options.isolation === "standard" ? "RuntimeDefault" : "kars-strict",
            readOnlyRootFilesystem: true,
            runAsNonRoot: true,
            allowPrivilegeEscalation: false,
            writablePaths: ["/sandbox", "/tmp"],
          },
          inferenceRef: {
            name: inferenceRefName(name),
          },
          networkPolicy: {
            defaultDeny: true,
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
          toolPolicyRef: { name: toolPolicyRefName(name) },
          trustThreshold: parseInt(options.trustThreshold) || 500,
        };
      }

      // Egress mode (Slice 5b): the `--learn-egress` flag now writes
      // `egressMode: Learn`. Default is also Learn (controller-side), so
      // omitting the flag is equivalent.
      if (options.learnEgress) {
        const np = (sandbox.spec as Record<string, unknown>).networkPolicy as Record<string, unknown>;
        np.egressMode = "Learn";
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
            console.error(chalk.yellow(`  ⚠ Channel '${channel}' enabled but no token found (use --${channel}-token or 'kars credentials set ${channel}-token <token>')`));
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

      // S13: build companion same-namespace policy CRs (sibling to KarsSandbox).
      const inferencePolicy = buildInferencePolicy({
        sandboxName: name,
        namespace: "kars-system",
        model: options.model,
        provider: "azure-ai-foundry",
        contentSafety: true,
        promptShields: true,
        tokenBudgetDaily: parseInt(options.tokenBudgetDaily) || 0,
        tokenBudgetPerRequest: parseInt(options.tokenBudgetPerRequest) || 0,
      });
      const toolPolicy = options.governance
        ? buildToolPolicy({
            sandboxName: name,
            namespace: "kars-system",
            profile: options.policyProfile || "default",
          })
        : undefined;

      const bundle: Record<string, unknown>[] = [inferencePolicy];
      if (toolPolicy) bundle.push(toolPolicy);
      bundle.push(sandbox);
      const yaml = JSON.stringify(bundle, null, 2);

      if (options.dryRun) {
        console.log(chalk.bold("\nKarsSandbox manifest (dry-run):\n"));
        console.log(yaml);
        console.log(chalk.dim("\nApply with: kubectl apply -f <file>"));
        return;
      }

      // Pre-flight: check for Kata nodepool when confidential isolation is requested
      if (options.isolation === "confidential") {
        let kataReady = false;
        try {
          const { stdout } = await execa("kubectl", [
            "get", "nodes", "-l", "kars.azure.com/pool=sandbox-kata",
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
                "--labels", "kars.azure.com/pool=sandbox-kata",
                "--node-taints", "kars.azure.com/sandbox=true:NoSchedule",
              ], { stdio: "pipe", timeout: 600_000 });
              kataSpinner.succeed("Kata nodepool provisioned");
            } catch (e: any) {
              kataSpinner.fail("Failed to provision Kata nodepool");
              console.error(chalk.red(e.stderr?.substring(0, 200) || e.message));
              process.exit(1);
            }
          } else {
            console.error(chalk.red("\n✗ No Kata nodepool found and no cached cluster context."));
            console.error(chalk.dim("  Run 'kars up --isolation confidential' first, or provision a katapool manually."));
            process.exit(1);
          }
        }
      }

      const spinner = ora(`Creating sandbox '${name}' (${options.isolation}, ${options.model})...`).start();

      try {
        // Verify cluster is reachable
        await execa("kubectl", ["get", "crd", "karssandboxes.kars.azure.com"], { stdio: "pipe" });

        // Create federated credential FIRST (before CRD) so it propagates while pod starts
        const namespace = `kars-${name}`;
        try {
          spinner.text = "Creating federated credential...";
          const ctx = loadContext();

          let identityName = ctx?.identityName;
          let identityRg = ctx?.identityResourceGroup || ctx?.resourceGroup;
          let issuerUrl = ctx?.oidcIssuerUrl;

          if (!identityName || !identityRg || !issuerUrl) {
            const { stdout: wiClientId } = await execa("kubectl", [
              "get", "sa", "-n", "kars-system", "kars-controller",
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
              "--name", `kars-${name}`,
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
        // Apply InferencePolicy + (optional) ToolPolicy + KarsSandbox as a
        // single multi-doc bundle. The controller resolves refs at reconcile
        // time; if the policy CRs are missing the sandbox goes Degraded.
        const bundleManifest = {
          apiVersion: "v1",
          kind: "List",
          items: bundle,
        };
        await execa("kubectl", ["apply", "-f", "-"], {
          input: JSON.stringify(bundleManifest),
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
          // Wait loop timed out — distinguish "still pulling/scheduling"
          // (informational, exit 0) from "stuck container" (real failure,
          // exit 1 so the caller — including the operator TUI's spawn
          // dialog — surfaces the actual reason). Without this branch,
          // a pod stuck in ImagePullBackOff / ErrImageNeverPull /
          // CrashLoopBackOff would silently succeed with the misleading
          // message "(may still be starting)" — the operator's activity
          // log would then say "✓ Spawned" while the agent never came
          // up.
          let stuckReason: string | null = null;
          let stuckContainer: string | null = null;
          try {
            const { stdout: stuckJson } = await execa("kubectl", [
              "get", "pods", "-n", namespace,
              "-o", "jsonpath=" +
                "{range .items[*].status.containerStatuses[*]}" +
                "{.name}={.state.waiting.reason}|{.lastState.terminated.reason}|{.restartCount}\\n" +
                "{end}",
            ], { stdio: "pipe", timeout: 5000 });
            for (const line of stuckJson.split("\n").filter(Boolean)) {
              const [cName, rest] = line.split("=");
              const [waiting, terminated, restartStr] = (rest || "").split("|");
              const restarts = Number(restartStr) || 0;
              // ImagePullBackOff / ErrImageNeverPull / ErrImagePull /
              // InvalidImageName / CreateContainerConfigError →
              // unambiguous failure modes. CrashLoopBackOff +
              // restarts>=2 is also fatal-enough to surface.
              if (
                waiting === "ImagePullBackOff" ||
                waiting === "ErrImageNeverPull" ||
                waiting === "ErrImagePull" ||
                waiting === "InvalidImageName" ||
                waiting === "CreateContainerConfigError" ||
                (waiting === "CrashLoopBackOff" && restarts >= 2) ||
                terminated === "OOMKilled" ||
                terminated === "Error"
              ) {
                stuckReason = waiting || terminated;
                stuckContainer = cName;
                break;
              }
            }
          } catch { /* best-effort diagnostic; fall through */ }

          if (stuckReason) {
            spinner.fail(
              `Sandbox '${name}' failed to start: ${stuckContainer} → ${stuckReason}`,
            );
            console.error(chalk.red(
              `\n  Container '${stuckContainer}' is stuck in '${stuckReason}'.\n` +
              `  Inspect:  kubectl describe pod -n ${namespace} -l app.kubernetes.io/instance=${name}\n` +
              `  Logs:     kubectl logs -n ${namespace} deploy/${name} -c ${stuckContainer}\n`,
            ));
            if (stuckReason === "ImagePullBackOff" ||
                stuckReason === "ErrImageNeverPull" ||
                stuckReason === "ErrImagePull") {
              console.error(chalk.yellow(
                `  Common cause on local-k8s: the runtime image isn't loaded into kind.\n` +
                `  Build + load:\n` +
                `    docker build -t karsacr.azurecr.io/kars-runtime-<rt>:latest \\\n` +
                `                 -f sandbox-images/<rt>/Dockerfile .\n` +
                `    kind load docker-image karsacr.azurecr.io/kars-runtime-<rt>:latest --name kars-dev\n`,
              ));
            }
            process.exit(1);
          }

          // Genuinely still starting (e.g. image pull in progress but
          // not yet failed). Keep the original informational success
          // so existing scripts don't break.
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
        console.log(chalk.dim(`  Status:     kubectl get karssandbox ${name} -n kars-system`));
        console.log(chalk.dim(`  Connect:    kars connect ${name}`));
        console.log(chalk.dim(`  Remove:     kars destroy ${name}\n`));

      } catch (error) {
        spinner.fail("Failed to create sandbox");
        const message = error instanceof Error ? error.message : String(error);
        if (message.includes("karssandboxes.kars.azure.com")) { // lgtm[js/incomplete-url-substring-sanitization] — error message check, not URL validation
          console.error(chalk.red("\n  kars is not installed on this cluster."));
          console.error(chalk.red("  Run 'kars up' first to deploy the infrastructure.\n"));
        } else if (
          // Detect stale-CRD validation errors: when the user has built a
          // newer CLI (with new runtime kinds like Hermes) but the cluster
          // is still on the older CRD schema, kubectl apply rejects the
          // bundle with "unknown field" or "Unsupported value". The fix is
          // a one-shot chart re-apply — surface that command inline so the
          // user doesn't have to dig through docs.
          message.includes("unknown field") ||
          message.includes("Unsupported value") ||
          message.includes("ValidationError")
        ) {
          console.error(chalk.red(`\n  Error: ${message}\n`));
          console.error(chalk.yellow(
            "  This looks like a CRD schema mismatch — the cluster's KarsSandbox CRD\n" +
            "  is older than your local CLI/sources. The ONLY safe fix on local-k8s:\n",
          ));
          console.error(chalk.cyan(
            "    kars dev --target local-k8s\n",
          ));
          console.error(chalk.dim(
            "  Re-runs the chart-install step which refreshes CRDs AND rebuilds the\n" +
            "  per-run dynamic overlay (KARS_DEV_PROFILE, inference creds, image\n" +
            "  pull policies). DO NOT apply the chart by hand with a naked\n" +
            "  `helm template | kubectl apply` — that nukes the dynamic overlay's\n" +
            "  inference creds and leaves every subsequent reconcile failing with\n" +
            "  \"No inference endpoint configured\".\n",
          ));
        } else {
          console.error(chalk.red(`\n  Error: ${message}\n`));
        }
        // Exit non-zero so callers (operator TUI's spawn dialog, scripts,
        // CI) see the failure. Previously this catch logged-then-returned,
        // leaving Node to exit 0 — which surfaced as a misleading
        // "✓ Spawned" in the operator activity log even though the CR
        // was never created (kubectl apply failed, fedcred lookup raced,
        // controller CRD not installed, etc.).
        process.exit(1);
      }
    });

  return cmd;
}
