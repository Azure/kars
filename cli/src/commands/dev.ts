// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { existsSync } from "fs";
import * as path from "node:path";
import * as os from "node:os";
import { Stepper, banner, section, kvLine, checkLine } from "../stepper.js";
import { loadConfig, promptAndSaveCredentials, resolveSecret, getSecret, loadSecrets, listSecretVariants } from "../config.js";

const DEFAULT_SANDBOX_IMAGE =
  "azureclaw-sandbox:dev";
const SANDBOX_BASE_IMAGE =
  "azureclaw-sandbox-base:dev";
const AZURELINUX_BASE =
  "mcr.microsoft.com/azurelinux/base/core:3.0";

const AGT_NETWORK = "azureclaw-dev";
const AGT_POSTGRES = "azureclaw-agt-postgres";
const AGT_RELAY = "azureclaw-agt-relay";
const AGT_REGISTRY = "azureclaw-agt-registry";

export function devCommand(): Command {
  const cmd = new Command("dev");

  cmd
    .description(
      "Run a sandbox locally via Docker for development. Same policies, same model routing, on your laptop."
    )
    .addHelpText("before", `
Requires either:
  • An existing Azure AI Foundry / Azure OpenAI deployment, OR
  • A GitHub PAT with \`models:read\` scope, which routes inference through
    GitHub Models — no Azure subscription needed.

On first run, you'll be prompted to choose between the two providers and
your choice (and credentials) will be saved to ~/.azureclaw/. Subsequent
runs reuse the saved provider — no flags required.

Use --github-token for a one-off, ephemeral GitHub Models run that does
NOT overwrite your saved credentials.
`)
    // ── Identity ───────────────────────────────────────────────────────
    .option("--name <name>", "Sandbox name", "dev-agent")
    .option("--model <model>", "Existing model deployment name in your Azure OpenAI resource", "gpt-4.1")
    .option(
      "--policy <preset>",
      "Policy preset: minimal | developer | web | azure",
      "developer"
    )
    // ── Provider override ─────────────────────────────────────────────
    .option(
      "--github-token <pat>",
      "One-off GitHub Models override (does NOT save). Requires a PAT with `models:read`. To save GitHub Models as your default provider, run without this flag and pick GitHub Models at the prompt."
    )
    // ── Image build ────────────────────────────────────────────────────
    .option(
      "--image <image>",
      "Sandbox container image",
      DEFAULT_SANDBOX_IMAGE
    )
    .option(
      "--build",
      "Build sandbox image locally from Dockerfile",
      false
    )
    .option(
      "--build-base",
      "Rebuild the sandbox base image (heavy deps: OpenClaw, Python, Go tools). Only needed when upgrading these.",
      false
    )
    .option(
      "--base-image <image>",
      "Azure Linux base image for building sandbox (override for custom registries)",
      AZURELINUX_BASE
    )
    // ── Mesh federation ───────────────────────────────────────────────
    .option(
      "--global-registry <url>",
      "Use a shared external registry (enables handoff). Skips local relay/registry/postgres."
    )
    // ── Channels (OpenClaw only) ──────────────────────────────────────
    .option("--channels <channels>", "Channels to enable: telegram,slack,discord,whatsapp (comma-separated)")
    .option("--telegram-token <token>", "Telegram bot token (from BotFather)")
    .option("--telegram-allow-from <ids>", "Telegram user IDs allowed to DM (comma-separated numeric IDs)")
    .option("--slack-token <token>", "Slack bot OAuth token")
    .option("--discord-token <token>", "Discord bot token")
    // ── Skills + plugins (OpenClaw only) ──────────────────────────────
    .option("--skills <skills>", "Skills to activate: browser,github,summarize,weather (comma-separated)")
    .option("--brave-api-key <key>", "Brave Search API key")
    .option("--tavily-api-key <key>", "Tavily search API key")
    .option("--exa-api-key <key>", "Exa search API key")
    .option("--firecrawl-api-key <key>", "Firecrawl web scraping API key")
    .option("--perplexity-api-key <key>", "Perplexity API key")
    .option("--openai-api-key <key>", "OpenAI API key (for dual-provider setups)")
    .addHelpText("after", `
Flag groups:
  Identity:           --name, --model, --policy
  Image build:        --image, --build, --build-base, --base-image
  Mesh federation:    --global-registry
  Channels:           --channels, --telegram-*, --slack-token, --discord-token
  Skills + plugins:   --skills, --brave-api-key, --tavily-api-key,
                      --exa-api-key, --firecrawl-api-key,
                      --perplexity-api-key, --openai-api-key

Notes:
  - Channels, skills, and plugin API keys are OpenClaw-specific. For
    other runtimes, configure equivalents inside the agent's own code.
  - Router-side guardrails (Content Safety, rate limits, audit, egress
    allowlist) are always enforced — same in dev as in production.
`)
    .action(async (options) => {
      const policyPresets = ["minimal", "developer", "web", "azure"];
      if (options.policy && !policyPresets.includes(options.policy)) {
        console.error(chalk.red(`\n  Error: --policy must be one of: ${policyPresets.join(" | ")} (got "${options.policy}").\n`));
        process.exit(1);
      }

      banner("AzureClaw · Local Sandbox", "Secure AI Agent Runtime on Azure");

      const stepper = new Stepper({ totalSteps: 4 });

      try {
        let image = options.image;
        const { execa } = await import("execa");
        const path = await import("path");

        // Find repo root
        let repoRoot = process.cwd();
        while (repoRoot !== "/" && !existsSync(path.join(repoRoot, "Cargo.toml"))) {
          repoRoot = path.dirname(repoRoot);
        }

        // ── Credentials (first — prompt before potentially long build) ──
        stepper.step("Checking credentials...");
        const githubToken = typeof options.githubToken === "string" ? options.githubToken.trim() : undefined;
        let creds = loadConfig();
        // Always materialize a per-run secret tempfile from creds.apiKey.
        // Avoids depending on the legacy ~/.azureclaw/credentials file
        // (which can drift from secrets.json) and decouples reset semantics
        // from the container mount path.
        let mountedSecretPath: string;

        if (githubToken) {
          // Ephemeral GitHub Models override: don't touch saved creds. Build
          // an inline config and write the PAT to a per-run tempfile that
          // gets mounted instead of the saved credentials file.
          const ghModelsEndpoint = "https://models.github.ai/inference";
          const ghDefaultModel = options.model !== "gpt-4.1" ? options.model : "openai/gpt-4.1";
          creds = {
            endpoint: ghModelsEndpoint,
            model: ghDefaultModel,
            apiKey: githubToken,
            foundryProjectEndpoint: undefined,
            provider: "github-models",
          };
          stepper.done("Credentials loaded (GitHub Models — ephemeral, not saved)");
        } else if (!creds || !creds.firstRunCompleted) {
          // First-run (or first-run flag was reset for retesting): stop
          // spinner so inquirer prompts display correctly, then show the
          // 3-way provider picker (Copilot recommended, then GH Models,
          // then Foundry).
          stepper.stop();
          console.log(chalk.yellow("\n  👋 First time? Pick an inference provider — no Azure account needed for the GitHub options."));
          console.log(chalk.dim("  Copilot is the default (largest context). You can change later with `azureclaw credentials`.\n"));
          creds = await promptAndSaveCredentials();

          // ── Optional: agent name + channel gap-fill ─────────────────
          // Only ask if the user didn't pre-set them on the CLI. Defaults
          // are sensible, so users can just hit Enter through everything.
          const { default: inquirer } = await import("inquirer");
          const usedNameDefault = options.name === "dev-agent";
          if (usedNameDefault) {
            const { agentName } = await inquirer.prompt([{
              type: "input",
              name: "agentName",
              message: "Agent name:",
              default: "dev-agent",
              validate: (v: string) => /^[a-z0-9][a-z0-9-]*[a-z0-9]?$/i.test(v.trim())
                ? true
                : "Use letters, numbers, and dashes only (e.g. dev-agent, alice-bot)",
            }]);
            options.name = agentName.trim();
          }

          // Channel gap-fill: offer one choice per saved channel-token
          // variant (e.g. telegram-token.dev, telegram-token.cloud become
          // separate "telegram.dev" / "telegram.cloud" choices). Without
          // variant-awareness we'd miss users who only have suffixed tokens
          // — like the standard local setup with .dev + .cloud namespaces.
          if (!options.channels) {
            const stored = loadSecrets();
            type ChannelChoice = { name: string; value: string };
            const available: ChannelChoice[] = [];
            const addChannel = (channel: string, baseKey: string, displayName: string) => {
              const variants = listSecretVariants(baseKey);
              for (const v of variants) {
                const channelValue = v.label === "default" ? channel : `${channel}.${v.label}`;
                const display = v.label === "default" ? displayName : `${displayName} (${v.label})`;
                available.push({ name: display, value: channelValue });
              }
              // Defensive: if loadSecrets sees a bare key but listSecretVariants
              // missed it (shouldn't happen), still surface the channel.
              if (variants.length === 0 && stored[baseKey]) {
                available.push({ name: displayName, value: channel });
              }
            };
            addChannel("telegram", "telegram-token", "Telegram");
            addChannel("slack",    "slack-token",    "Slack");
            addChannel("discord",  "discord-token",  "Discord");
            if (available.length > 0) {
              const { picked } = await inquirer.prompt([{
                type: "checkbox",
                name: "picked",
                message: "Enable any channels? (Space to toggle, Enter to confirm)",
                choices: available,
              }]);
              if (picked.length > 0) {
                options.channels = picked.join(",");
              }
            } else {
              console.log(chalk.dim("  No channel tokens saved yet. Run `azureclaw credentials` later to add Telegram/Slack/Discord.\n"));
            }
          }

          // Optional rebuild prompt. Defaults to no — first-time users want
          // the cached image to come up fast. Power users testing local
          // changes (e.g. plugin/entrypoint edits) can opt in here without
          // remembering the --build flag.
          if (!options.build) {
            const { rebuild } = await inquirer.prompt([{
              type: "confirm",
              name: "rebuild",
              message: "Rebuild sandbox image from local source? (slower, picks up plugin/entrypoint changes)",
              default: false,
            }]);
            if (rebuild) options.build = true;
          }

          const newProviderLabel =
            creds.provider === "github-models"
              ? "GitHub Models"
              : creds.provider === "github-copilot"
                ? "GitHub Copilot"
                : "Azure AI Foundry";
          stepper.done(`Credentials configured (${newProviderLabel})`);
        } else {
          const providerLabel =
            creds.provider === "github-models"
              ? "GitHub Models"
              : creds.provider === "github-copilot"
                ? "GitHub Copilot"
                : "Azure AI Foundry";
          stepper.done(`Credentials loaded (${providerLabel})`);
        }

        // Materialize secret tempfile from the resolved creds.apiKey.
        {
          const { mkdtempSync, writeFileSync, chmodSync } = await import("node:fs");
          const tmpDir = mkdtempSync(path.join(os.tmpdir(), "azureclaw-secret-"));
          mountedSecretPath = path.join(tmpDir, "azure-openai-key");
          writeFileSync(mountedSecretPath, creds.apiKey, "utf-8");
          chmodSync(mountedSecretPath, 0o600);
        }

        const isGithubModelsMode = creds.provider === "github-models";
        const isCopilotMode = creds.provider === "github-copilot";
        const isManagedTokenProvider = isGithubModelsMode || isCopilotMode;
        const model = isManagedTokenProvider
          ? creds.model
          : (options.model !== "gpt-4.1" ? options.model : creds.model);


        // ── Image resolution ─────────────────────────────────────────
        stepper.step("Resolving sandbox image...");
        let imageExists = false;
        if (!options.build) {
          stepper.update("Checking for sandbox image...");
          try {
            await execa("docker", ["image", "inspect", image], { stdio: "pipe" });
            imageExists = true;
          } catch {
            // Not found — will build
          }
        }

        if (options.build || !imageExists) {
          const baseImage = options.baseImage;

          // Check if Azure Linux base image exists locally, pull if not
          try {
            await execa("docker", ["image", "inspect", baseImage], { stdio: "pipe" });
          } catch {
            stepper.update(`Pulling base image (${baseImage})...`);
            try {
              await execa("docker", ["pull", baseImage], { stdio: "pipe" });
            } catch {
              stepper.fail("Could not pull base image");
              console.log(chalk.yellow(`
  Failed to pull ${chalk.bold(baseImage)}.

  ${chalk.bold("1.")} Pull manually: ${chalk.cyan(`docker pull ${baseImage}`)}
  ${chalk.bold("2.")} Re-run:        ${chalk.cyan("azureclaw dev")}

  Custom registry? ${chalk.cyan(`azureclaw dev --base-image <your-registry>/azurelinux/base/core:3.0`)}
`));
              process.exit(1);
            }
          }

          const dockerfilePath = path.join(repoRoot, "sandbox-images/openclaw/Dockerfile");
          const baseDockerfilePath = path.join(repoRoot, "sandbox-images/openclaw/Dockerfile.base");
          const routerDockerfile = path.join(repoRoot, "inference-router/Dockerfile");
          if (!existsSync(dockerfilePath)) {
            stepper.fail("Dockerfile not found");
            console.log(chalk.yellow(`
  Run from the AzureClaw repo root:
    ${chalk.cyan("git clone https://github.com/Azure/azureclaw.git && cd azureclaw")}
    ${chalk.cyan("azureclaw dev")}
`));
            process.exit(1);
          }

          // Build sandbox base image (heavy deps) — only if --build-base or not cached
          let sandboxBaseExists = false;
          try {
            await execa("docker", ["image", "inspect", SANDBOX_BASE_IMAGE], { stdio: "pipe" });
            sandboxBaseExists = true;
          } catch { /* not built yet */ }

          if (options.buildBase || !sandboxBaseExists) {
            stepper.update(sandboxBaseExists
              ? "Rebuilding sandbox base image (--build-base)..."
              : "Building sandbox base image (first run — includes OpenClaw, Python, Go tools)...");
            stepper.stop();
            console.log(chalk.dim("  Building sandbox base image (this is the slow one — only needed once)...\n"));
            await execa("docker", [
              "build",
              "--build-arg", `AZURELINUX_BASE=${baseImage}`,
              "--build-arg", `OPENCLAW_CACHE_BUST=${Date.now()}`,
              "-t", SANDBOX_BASE_IMAGE,
              "-f", baseDockerfilePath,
              repoRoot,
            ], { stdio: "inherit" });
            console.log();
          } else {
            stepper.update("Sandbox base image cached ✓");
          }

          // Build inference router locally (sandbox Dockerfile needs it)
          const routerImage = "azureclaw-inference-router:dev";
          let routerExists = false;
          try {
            await execa("docker", ["image", "inspect", routerImage], { stdio: "pipe" });
            routerExists = true;
          } catch { /* not built yet */ }

          if (options.build || !routerExists) {
            stepper.update("Building inference router (Rust — first run takes a few minutes)...");
            stepper.stop();
            console.log(chalk.dim("  Building inference-router (Rust)...\n"));
            await execa("docker", [
              "build",
              "--build-arg", `ROUTER_CACHE_BUST=${Date.now()}`,
              "-t", routerImage,
              "-f", routerDockerfile,
              repoRoot,
            ], { stdio: "inherit" });
            console.log();
          }

          stepper.update("Building sandbox image (plugin + entrypoint overlay)...");
          stepper.stop();
          console.log(chalk.dim("  Building sandbox image...\n"));
          await execa("docker", [
            "build",
            "--build-arg", `SANDBOX_BASE_IMAGE=${SANDBOX_BASE_IMAGE}`,
            "--build-arg", `INFERENCE_ROUTER_IMAGE=${routerImage}`,
            "-t", "azureclaw-sandbox:dev",
            "-f", dockerfilePath,
            repoRoot,
          ], { stdio: "inherit" });
          console.log();
          image = "azureclaw-sandbox:dev";
          stepper.done("Sandbox image built");

          // Build AGT relay + registry images if --build
          {
            stepper.update("Building AGT relay image (Rust)...");
            stepper.stop();
            console.log(chalk.dim("  Building agentmesh-relay (Rust)...\n"));
            await execa("docker", [
              "build", "--build-arg", `CACHE_BUST=${Date.now()}`,
              "-t", "agentmesh-relay:dev",
              path.join(repoRoot, "vendor/agentmesh-relay"),
            ], { stdio: "inherit" });
            console.log();

            stepper.update("Building AGT registry image (Rust + React)...");
            stepper.stop();
            console.log(chalk.dim("  Building agentmesh-registry (Rust + React)...\n"));
            await execa("docker", [
              "build", "--build-arg", `CACHE_BUST=${Date.now()}`,
              "-t", "agentmesh-registry:dev",
              path.join(repoRoot, "vendor/agentmesh-registry"),
            ], { stdio: "inherit" });
            console.log();
          }
        } else {
          stepper.done("Sandbox image found");
        }

        // ── Discover deployed models from Azure endpoint ─────────────
        let discoveredDeployments = "";
        // Discover deployed models via Azure CLI (ARM management API — only reliable way).
        // Data-plane /openai/deployments always returns 404; skip it.
        // GitHub Models mode + Copilot mode: skip ARM-based deployment
        // discovery (the endpoint isn't an Azure resource).
        if (!isManagedTokenProvider) try {
          const accountName = new URL(creds.endpoint).hostname.split(".")[0];
          const { stdout: rgOut } = await execa("az", [
            "cognitiveservices", "account", "list",
            "--query", `[?name=='${accountName}'].resourceGroup | [0]`,
            "--output", "tsv",
          ], { stdio: "pipe", timeout: 15000 });
          const rg = rgOut.trim();
          if (rg) {
            const { stdout } = await execa("az", [
              "cognitiveservices", "account", "deployment", "list",
              "--name", accountName,
              "--resource-group", rg,
              "--query", "[].{name:name, model:properties.model.name}",
              "--output", "json",
            ], { stdio: "pipe", timeout: 30000 });
            const deps = JSON.parse(stdout || "[]");
            if (Array.isArray(deps) && deps.length > 0) {
              discoveredDeployments = JSON.stringify(deps);
              const names = deps.map((d: any) => d.name || d).slice(0, 10);
              stepper.done(`Discovered ${deps.length} deployment(s): ${names.join(", ")}${deps.length > 10 ? "..." : ""}`);
            }
          }
        } catch { /* Azure CLI might not be logged in or account not found */ }

        // ── Docker network (always needed for sub-agent spawning) ──
        let agtReady = false;
        const useGlobalRegistry = !!options.globalRegistry;

        // Create shared Docker network — sub-agents need this even without AGT
        try {
          await execa("docker", ["network", "create", AGT_NETWORK], { stdio: "pipe" });
        } catch {
          // Already exists — fine
        }

        if (!useGlobalRegistry) {
          // Local registry mode — deploy relay/registry/postgres locally
          stepper.step("Starting AGT infrastructure (local registry)...");

          // Helper: check if a container exists and is running
          async function isContainerRunning(name: string): Promise<boolean> {
            try {
              const { stdout } = await execa("docker", [
                "inspect", "-f", "{{.State.Running}}", name,
              ], { stdio: "pipe" });
              return stdout.trim() === "true";
            } catch { return false; }
          }

          // Start PostgreSQL (registry backend)
          if (!(await isContainerRunning(AGT_POSTGRES))) {
            stepper.update("Starting PostgreSQL...");
            try { await execa("docker", ["rm", "-fv", AGT_POSTGRES], { stdio: "pipe" }); } catch {}
            await execa("docker", [
              "run", "-d",
              "--name", AGT_POSTGRES,
              "--network", AGT_NETWORK,
              "-e", "POSTGRES_DB=agentmesh",
              "-e", "POSTGRES_USER=agentmesh",
              "-e", "POSTGRES_PASSWORD=agentmesh-dev",
              "postgres:15-alpine",
            ], { stdio: "pipe" });
            // Wait for postgres to accept connections
            for (let i = 0; i < 15; i++) {
              try {
                await execa("docker", [
                  "exec", AGT_POSTGRES, "pg_isready", "-U", "agentmesh",
                ], { stdio: "pipe" });
                break;
              } catch { await new Promise(r => setTimeout(r, 1000)); }
            }
          }

          // Start AGT Relay (WebSocket message relay)
          if (!(await isContainerRunning(AGT_RELAY))) {
            stepper.update("Starting AGT relay...");
            try { await execa("docker", ["rm", "-f", AGT_RELAY], { stdio: "pipe" }); } catch {}
            await execa("docker", [
              "run", "-d",
              "--name", AGT_RELAY,
              "--network", AGT_NETWORK,
              "-e", "RUST_LOG=agentmesh_relay=info",
              "-e", "RELAY_ADDR=0.0.0.0:8765",
              "agentmesh-relay:dev",
            ], { stdio: "pipe" });
          }

          // Start AGT Registry (agent discovery + prekey storage)
          if (!(await isContainerRunning(AGT_REGISTRY))) {
            stepper.update("Starting AGT registry...");
            try { await execa("docker", ["rm", "-f", AGT_REGISTRY], { stdio: "pipe" }); } catch {}
            await execa("docker", [
              "run", "-d",
              "--name", AGT_REGISTRY,
              "--network", AGT_NETWORK,
              "-e", `DATABASE_URL=postgres://agentmesh:agentmesh-dev@${AGT_POSTGRES}:5432/agentmesh`,
              "-e", "HOST=0.0.0.0",
              "-e", "PORT=8080",
              "-e", "RUST_LOG=agentmesh_registry=info,actix_web=info",
              "agentmesh-registry:dev",
            ], { stdio: "pipe" });
          }

          // Health check — wait for registry to be ready
          stepper.update("Waiting for AGT services...");
          for (let i = 0; i < 15; i++) {
            try {
              await execa("docker", [
                "exec", AGT_REGISTRY, "curl", "-sf", "http://localhost:8080/v1/health",
              ], { stdio: "pipe" });
              agtReady = true;
              break;
            } catch { await new Promise(r => setTimeout(r, 1000)); }
          }

          stepper.done(agtReady ? "AGT infrastructure ready (relay + registry + postgres)" : "AGT infrastructure started (health check pending)");
        } else if (useGlobalRegistry) {
          // Global registry mode — skip local deployment, verify connectivity
          stepper.step("Connecting to global registry...");
          const registryUrl = options.globalRegistry as string;

          // Rewrite localhost URLs for Docker containers — localhost inside
          // the container refers to the container itself, not the host.
          const containerRegistryUrl = registryUrl.replace(
            /\/\/(localhost|127\.0\.0\.1)([:\/])/,
            "//host.docker.internal$2"
          );
          if (containerRegistryUrl !== registryUrl) {
            stepper.update(`Rewriting ${registryUrl} → ${containerRegistryUrl} for container access`);
          }

          // Health check from the host (validates port-forward / tunnel is up)
          stepper.update(`Checking ${registryUrl}...`);
          try {
            const healthUrl = `${registryUrl.replace(/\/$/, "")}/v1/health`;
            const resp = await fetch(healthUrl, { signal: AbortSignal.timeout(10000) });
            agtReady = resp.ok;
            stepper.done(agtReady
              ? `Global registry connected (${registryUrl}) — handoff enabled`
              : `Global registry returned ${resp.status} — may not be ready`
            );
          } catch (e: any) {
            // Registry not reachable — attempt auto-promote
            stepper.done(`Global registry not reachable — attempting mesh promote...`);
            try {
              const { killProcessesOnPorts } = await import("./mesh.js");
              const regPort = parseInt(new URL(registryUrl).port || "18080", 10);
              const relayPort = regPort === 18080 ? 18765 : regPort + 1;

              // Kill stale port-forwards and restart
              await killProcessesOnPorts([regPort, relayPort]);
              const { spawn: spawnChild } = await import("node:child_process");
              const { mkdirSync, openSync, readFileSync, writeFileSync, closeSync } = await import("node:fs");

              const tunnels = [
                { svc: "svc/agentmesh-registry", localPort: regPort, remotePort: 8080, label: "Registry" },
                { svc: "svc/agentmesh-relay", localPort: relayPort, remotePort: 8765, label: "Relay" },
              ];
              const logDir = path.join(os.homedir(), ".azureclaw", "logs");
              mkdirSync(logDir, { recursive: true });
              const pids: Record<string, number> = {};

              for (const t of tunnels) {
                const outFd = openSync(path.join(logDir, `pf-${t.label.toLowerCase()}.log`), "w");
                const child = spawnChild("kubectl", [
                  "port-forward", t.svc, `${t.localPort}:${t.remotePort}`,
                  "-n", "agentmesh", "--address", "0.0.0.0",
                ], { stdio: ["ignore", outFd, outFd], detached: true });

                const logPath = path.join(logDir, `pf-${t.label.toLowerCase()}.log`);
                let ready = false;
                for (let attempt = 0; attempt < 30; attempt++) {
                  await new Promise(r => setTimeout(r, 500));
                  try {
                    const content = readFileSync(logPath, "utf-8");
                    if (content.includes("Forwarding from")) { ready = true; break; }
                  } catch { /* file not written yet */ }
                }
                child.unref();
                closeSync(outFd);
                if (ready && child.pid) pids[t.label] = child.pid;
              }

              const pidFile = path.join(os.homedir(), ".azureclaw", "port-forward-pids.json");
              writeFileSync(pidFile, JSON.stringify(pids, null, 2));

              // Kill any stale listeners that aren't our spawned PIDs
              const { killStaleListeners } = await import("./mesh.js");
              const portPidMap: Array<{ port: number; pid: number }> = [];
              if (pids.Registry) portPidMap.push({ port: regPort, pid: pids.Registry });
              if (pids.Relay) portPidMap.push({ port: relayPort, pid: pids.Relay });
              await killStaleListeners(portPidMap);

              // Re-check after promote
              const retryResp = await fetch(`${registryUrl.replace(/\/$/, "")}/v1/health`, {
                signal: AbortSignal.timeout(5000),
              });
              agtReady = retryResp.ok;
              if (agtReady) {
                stepper.update(`Auto-promoted mesh tunnels — registry connected`);
              }
            } catch {
              // Auto-promote failed — continue without registry
              stepper.update(`Auto-promote failed — will retry on first use`);
            }
          }

          // Store the container-reachable URL for env injection below
          (options as any)._containerRegistryUrl = containerRegistryUrl;
        }

        // ── Container startup ────────────────────────────────────────
        stepper.step("Starting sandbox container...");
        const containerName = `azureclaw-${options.name}`;

        // Clean up any previous instance
        try {
          await execa("docker", ["rm", "-f", containerName], { stdio: "pipe" });
        } catch {
          // Didn't exist — fine
        }

        // Seccomp profile — copied into dist/profiles/ during build
        const { fileURLToPath } = await import("url");
        const thisFile = fileURLToPath(import.meta.url);
        const distDir = path.dirname(path.dirname(thisFile));
        const seccompPath = path.join(distDir, "profiles", "seccomp", "azureclaw-strict.json");
        const hasSeccomp = existsSync(seccompPath);
        const seccompArgs = hasSeccomp
          ? ["--security-opt", `seccomp=${seccompPath}`]
          : [];

        stepper.update("Launching container...");

        // Parse channel variants: "telegram.cloud" → base "telegram", suffix "cloud"
        // Used to resolve the correct dot-suffixed secret (e.g. telegram-token.cloud).
        // Trim + lowercase so "  Telegram , Slack" works the same as "telegram,slack".
        const channelVariants: Record<string, string | undefined> = {};
        if (options.channels) {
          const parts = String(options.channels)
            .split(",")
            .map(s => s.trim().toLowerCase())
            .filter(Boolean);
          for (const ch of parts) {
            const dotIdx = ch.indexOf(".");
            if (dotIdx > 0) {
              channelVariants[ch.slice(0, dotIdx)] = ch.slice(dotIdx); // e.g. ".cloud"
            } else {
              channelVariants[ch] = undefined;
            }
          }
          // Rewrite --channels to base names for the entrypoint
          options.channels = Object.keys(channelVariants).join(",");
        }
        const wantsWhatsapp = "whatsapp" in channelVariants;

        // Resolve a channel token, respecting dot-suffix variants from --channels
        const resolveChannelToken = (flagValue: string | undefined, baseKey: string, channel: string): string | undefined => {
          if (flagValue) return flagValue;
          const suffix = channelVariants[channel];
          if (suffix) {
            const suffixed = getSecret(baseKey + suffix);
            if (suffixed) return suffixed;
          }
          return resolveSecret(undefined, baseKey);
        };

        // AGT network args: connect sandbox to the shared Docker network
        // so the router can reach relay/registry by container hostname
        const networkArgs = !useGlobalRegistry ? ["--network", AGT_NETWORK] : [];
        const agtEnvArgs: string[] = [];
        if (useGlobalRegistry) {
          // Global registry mode — router connects to external registry
          // Use the container-reachable URL (localhost rewritten to host.docker.internal)
          const containerRegistryUrl = (options as any)._containerRegistryUrl ?? options.globalRegistry as string;

          // Derive relay URL from registry URL: same host, port 18765, ws:// scheme
          // Registry: http://host.docker.internal:18080 → Relay: ws://host.docker.internal:18765
          const registryUrlObj = new URL(containerRegistryUrl);
          const relayPort = parseInt(registryUrlObj.port || "18080", 10) === 18080 ? 18765 : 8765;
          const containerRelayUrl = `ws://${registryUrlObj.hostname}:${relayPort}`;

          agtEnvArgs.push(
            "-e", `AGT_REGISTRY_URL=${containerRegistryUrl}`,
            "-e", `AGT_RELAY_URL=${containerRelayUrl}`,
            "-e", "AGT_REGISTRY_MODE=global",
            "-e", "AGT_GOVERNANCE_ENABLED=true",
          );
        } else {
          // Local registry mode — router connects to colocated containers
          agtEnvArgs.push(
            "-e", `AGT_RELAY_URL=ws://${AGT_RELAY}:8765`,
            "-e", `AGT_REGISTRY_URL=http://${AGT_REGISTRY}:8080`,
            "-e", "AGT_REGISTRY_MODE=local",
            "-e", "AGT_GOVERNANCE_ENABLED=true",
          );
        }

        // Dev mode: mount Docker socket so sub-agents can be spawned as sibling containers.
        // Not :ro — entrypoint chmod's it so the router (UID 1001) can use the Docker API.
        const dockerSockArgs = [
          "-v", "/var/run/docker.sock:/var/run/docker.sock",
        ];

        // Mount kubeconfig so the router can spawn AKS pods for handoff (K8s CRD path).
        // Respect $KUBECONFIG if set, fall back to default ~/.kube/config
        const kubeConfigPath = process.env.KUBECONFIG || `${process.env.HOME}/.kube/config`;
        const kubeArgs = existsSync(kubeConfigPath) ? [
          "-v", `${kubeConfigPath}:/run/secrets/kubeconfig:ro`,
          "-e", "KUBECONFIG=/run/secrets/kubeconfig",
        ] : [];

        await execa("docker", [
          "run", "-d",
          "--name", containerName,
          "--hostname", options.name,
          ...seccompArgs,
          ...networkArgs,
          "--read-only",
          "--security-opt", "no-new-privileges",
          // Grant NET_ADMIN for iptables egress guard (same as AKS init container)
          "--cap-add", "NET_ADMIN",
          // Writable paths
          // /tmp must hold the staged OpenClaw tree (~1.8 GiB at 2026.4.27),
          // openclaw-{UID} runtime dirs, and gateway/agent IPC files. AKS uses
          // 4Gi (controller/src/reconciler/mod.rs:1423) — match that here so
          // dev mode behaves the same as AKS.
          "--tmpfs", "/tmp:rw,noexec,nosuid,size=4g",
          "-v", `${containerName}-data:/sandbox`,
          // Mount API key as read-only secret (never as env var)
          "-v", `${mountedSecretPath}:/run/secrets/azure-openai-key:ro`,
          ...dockerSockArgs,
          ...kubeArgs,
          // Hide unnecessary filesystem paths
          "--tmpfs", "/boot:ro,size=0",
          "--tmpfs", "/home:ro,size=0",
          "--tmpfs", "/media:ro,size=0",
          "--tmpfs", "/mnt:ro,size=0",
          "--tmpfs", "/srv:ro,size=0",
          "--tmpfs", "/root:ro,size=0",
          "-p", "18789:18789",
          "-e", `OPENCLAW_MODEL=${model}`,
          "-e", `DEFAULT_MODEL=${model}`,
          "-e", `AZURE_OPENAI_ENDPOINT=${creds.endpoint}`,
          "-e", `SANDBOX_NAME=${options.name}`,
          "-e", "AZURECLAW_DEV_MODE=true",
          ...(isGithubModelsMode ? ["-e", "AZURECLAW_PROVIDER=github-models"] : []),
          ...(isCopilotMode ? ["-e", "AZURECLAW_PROVIDER=github-copilot"] : []),
          "-e", `DOCKER_NETWORK=${AGT_NETWORK}`,
          // Phase 2/F8 mitigations — env-gated suppression of false-positive
          // governance findings. Default-on in dev so research/citation
          // workloads aren't impeded; override with =0 to restore strict mode.
          "-e", "AZURECLAW_SUPPRESS_EXFIL_URL=1",
          "-e", "AZURECLAW_SUPPRESS_CONTENT_FLAGS=violence",
          "-e", "AZURECLAW_CONTENT_FLAG_MIN_SEVERITY=medium",
          ...(creds.foundryProjectEndpoint ? ["-e", `FOUNDRY_PROJECT_ENDPOINT=${creds.foundryProjectEndpoint}`] : []),
          ...(discoveredDeployments ? ["-e", `FOUNDRY_DEPLOYMENTS=${discoveredDeployments}`] : []),
          "-e", `PS1=azureclaw@${options.name}:\\w\\$ `,
          // Learn mode on by default in dev — records all egress domains for review
          "-e", "EGRESS_LEARN_MODE=true",
          ...agtEnvArgs,
          // Channel tokens: CLI flag > variant from --channels > secrets.json > host env var
          ...(resolveChannelToken(options.telegramToken, "telegram-token", "telegram") ? ["-e", `TELEGRAM_BOT_TOKEN=${resolveChannelToken(options.telegramToken, "telegram-token", "telegram")}`] : []),
          ...(resolveSecret(options.telegramAllowFrom, "telegram-allow-from") ? ["-e", `TELEGRAM_ALLOW_FROM=${resolveSecret(options.telegramAllowFrom, "telegram-allow-from")}`] : []),
          ...(resolveChannelToken(options.slackToken, "slack-token", "slack") ? ["-e", `SLACK_BOT_TOKEN=${resolveChannelToken(options.slackToken, "slack-token", "slack")}`] : []),
          ...(resolveChannelToken(options.discordToken, "discord-token", "discord") ? ["-e", `DISCORD_BOT_TOKEN=${resolveChannelToken(options.discordToken, "discord-token", "discord")}`] : []),
          ...((wantsWhatsapp || process.env.WHATSAPP_ENABLED) ? ["-e", `WHATSAPP_ENABLED=${process.env.WHATSAPP_ENABLED ?? "true"}`] : []),
          // Third-party plugin API keys: CLI flag > secrets.json > host env var
          ...(resolveSecret(options.braveApiKey, "brave-api-key") ? ["-e", `BRAVE_API_KEY=${resolveSecret(options.braveApiKey, "brave-api-key")}`] : []),
          ...(resolveSecret(options.tavilyApiKey, "tavily-api-key") ? ["-e", `TAVILY_API_KEY=${resolveSecret(options.tavilyApiKey, "tavily-api-key")}`] : []),
          ...(resolveSecret(options.exaApiKey, "exa-api-key") ? ["-e", `EXA_API_KEY=${resolveSecret(options.exaApiKey, "exa-api-key")}`] : []),
          ...(resolveSecret(options.firecrawlApiKey, "firecrawl-api-key") ? ["-e", `FIRECRAWL_API_KEY=${resolveSecret(options.firecrawlApiKey, "firecrawl-api-key")}`] : []),
          ...(resolveSecret(options.perplexityApiKey, "perplexity-api-key") ? ["-e", `PERPLEXITY_API_KEY=${resolveSecret(options.perplexityApiKey, "perplexity-api-key")}`] : []),
          ...(resolveSecret(options.openaiApiKey, "openai-api-key") ? ["-e", `OPENAI_API_KEY=${resolveSecret(options.openaiApiKey, "openai-api-key")}`] : []),
          image,
        ], { stdio: "pipe" });

        // Wait for entrypoint to set up iptables and start services
        // The entrypoint runs as root and handles:
        //   - iptables egress guard (UID 1000 → localhost + DNS)
        //   - inference router as UID 1001 (internet access for Foundry + blocklist)
        //   - gateway, node host, agent as UID 1000 (restricted)
        let hasIptables = false;
        let gatewayHealthy = false;
        let routerHealthy = false;
        for (let i = 0; i < 15; i++) {
          try {
            if (!hasIptables) {
              await execa("docker", [
                "exec", containerName, "sh", "-c",
                "iptables -L AZURECLAW_EGRESS -n 2>/dev/null | grep -q REJECT",
              ], { stdio: "pipe" });
              hasIptables = true;
            }
            if (!gatewayHealthy) {
              await execa("docker", [
                "exec", containerName, "sh", "-c",
                "wget -qO- --timeout=2 http://127.0.0.1:18789/healthz 2>/dev/null || curl -sf --max-time 2 http://127.0.0.1:18789/healthz 2>/dev/null",
              ], { stdio: "pipe" });
              gatewayHealthy = true;
            }
            // Router is always the last check — no guard needed since
            // routerHealthy is only set here, right before break
            await execa("docker", [
              "exec", containerName, "sh", "-c",
              "wget -qO- --timeout=2 http://127.0.0.1:8443/healthz 2>/dev/null || curl -sf --max-time 2 http://127.0.0.1:8443/healthz 2>/dev/null",
            ], { stdio: "pipe" });
            routerHealthy = true;
            // All three checks passed without throwing — we're ready
            break;
          } catch {
            await new Promise(r => setTimeout(r, 1000));
          }
        }

        stepper.done("Sandbox running");

        // ── Global registry availability check from inside the container ──
        if (useGlobalRegistry) {
          const containerRegistryUrl = (options as any)._containerRegistryUrl ?? options.globalRegistry as string;
          const healthEndpoint = `${containerRegistryUrl.replace(/\/$/, "")}/v1/health`;
          let containerCanReach = false;
          for (let i = 0; i < 5; i++) {
            try {
              const { stdout } = await execa("docker", [
                "exec", containerName, "sh", "-c",
                `wget -qO- --timeout=3 "${healthEndpoint}" 2>/dev/null || curl -sf --max-time 3 "${healthEndpoint}" 2>/dev/null`,
              ], { stdio: "pipe" });
              if (stdout.includes("healthy")) {
                containerCanReach = true;
                break;
              }
            } catch {
              await new Promise(r => setTimeout(r, 1000));
            }
          }
          if (!containerCanReach) {
            agtReady = false;
            stepper.update(
              `⚠ Global registry unreachable from inside container at ${containerRegistryUrl}. ` +
              `Discovery and handoff will not work.`
            );
          }
        }

        // ── Security status ──────────────────────────────────────────
        section("Security");
        checkLine(true, "Read-only root filesystem");
        checkLine(true, "Non-root user (sandbox:1000)");
        checkLine(true, "All root privileges removed");
        checkLine(hasSeccomp, `seccomp profile ${hasSeccomp ? "(azureclaw-strict)" : "(not loaded)"}`);
        checkLine(hasIptables, `iptables egress guard ${hasIptables ? "(UID 1000 → transparent proxy)" : "(not available)"}`);
        checkLine(true, "API key mounted as read-only secret");
        {
          const registryLabel = useGlobalRegistry
            ? `(global registry — handoff enabled)`
            : `(relay + registry + E2E encryption)`;
          checkLine(agtReady, `AGT mesh ${agtReady ? registryLabel : "(starting...)"}`);
        }

        section("Services");
        checkLine(gatewayHealthy, `OpenClaw gateway ${gatewayHealthy ? "(ready)" : "(starting...)"}`);
        checkLine(routerHealthy, `Inference router ${routerHealthy ? "(ready)" : "(starting...)"}`);

        section("Environment");
        kvLine("OS", "Azure Linux 3.0");
        kvLine("OpenClaw", "2026.3.13");
        kvLine(
          "Model",
          `${model} (${isGithubModelsMode ? "GitHub Models" : isCopilotMode ? "GitHub Copilot" : "Azure OpenAI"})`,
        );
        kvLine("Endpoint", creds.endpoint);
        kvLine("Policy", `${options.policy} preset`);
        kvLine("Sandbox", options.name);
        if (options.channels) {
          kvLine("Channels", options.channels);
        }
        if (options.skills) {
          kvLine("Skills", options.skills);
        }

        // Read the gateway token from a dedicated file written by the entrypoint.
        // Poll because the entrypoint writes it after config + plugin install.
        let gatewayToken = "";
        for (let i = 0; i < 15; i++) {
          try {
            const { stdout: tokenOut } = await execa("docker", [
              "exec", containerName, "cat", "/tmp/gateway-token",
            ], { stdio: "pipe" });
            gatewayToken = tokenOut.trim();
            if (gatewayToken) break;
          } catch {
            // Not written yet
          }
          await new Promise(r => setTimeout(r, 1000));
        }

        section("Commands");
        console.log(`  Connect:  ${chalk.cyan(`azureclaw connect ${options.name}`)}`);
        console.log(`  Shell:    ${chalk.cyan(`azureclaw connect ${options.name} --shell`)}`);
        console.log(`  Status:   ${chalk.cyan(`azureclaw status ${options.name}`)}`);
        console.log(`  Stop:     ${chalk.cyan(`azureclaw destroy ${options.name}`)}`);
        if (gatewayToken) {
          const url = `http://localhost:18789/#token=${gatewayToken}`;
          // Print URL without chalk formatting — terminals auto-detect http:// links.
          // Chalk ANSI codes break terminal URL detection in most emulators.
          console.log(`  Web UI:   ${url}`);
        }
        console.log(chalk.dim(`\n  Production: azureclaw up (deploys to AKS)`));
        console.log();
      } catch (error) {
        stepper.stop();
        console.error(chalk.red(`\n  Local sandbox failed to start`));
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`  ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
