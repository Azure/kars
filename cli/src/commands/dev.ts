// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { execa } from "execa";
import { existsSync } from "fs";
import * as path from "node:path";
import * as os from "node:os";
import { Stepper, banner, section, kvLine, checkLine } from "../stepper.js";
import { loadConfig, promptAndSaveCredentials, resolveSecret, getSecret, loadSecrets, listSecretVariants, type KarsConfig } from "../config.js";
import { stageRustBinaries, archForDockerPlatform } from "../lib/stage-rust-bin.js";
import { stageMeshPlugin } from "../lib/stage-mesh-plugin.js";
import { ensureAgtRepo } from "../lib/agt-bootstrap.js";

/**
 * Pre-flight: verify every binary `kars dev` shells out to is on PATH.
 * Fails fast (before any prompts) with copy-pasteable install URLs.
 * `kars dev` takes ~5–10 min on a cold first run; bailing halfway
 * through with "helm: command not found" is a bad first impression.
 */
async function preflightTools(target: "docker" | "local-k8s", agtRepo: string): Promise<void> {
  // Per-target tool requirements:
  //   docker     → just docker (single container, no kind/helm/kubectl)
  //   local-k8s  → docker + kind + kubectl + helm
  const required: Array<{ bin: string; install: string }> =
    target === "local-k8s"
      ? [
          { bin: "docker",  install: "https://docs.docker.com/get-docker/" },
          { bin: "kind",    install: "https://kind.sigs.k8s.io/docs/user/quick-start/#installation" },
          { bin: "kubectl", install: "https://kubernetes.io/docs/tasks/tools/" },
          { bin: "helm",    install: "https://helm.sh/docs/intro/install/" },
          { bin: "cargo",   install: "https://rustup.rs/" },
        ]
      : [
          { bin: "docker", install: "https://docs.docker.com/get-docker/" },
          { bin: "cargo",  install: "https://rustup.rs/" },
        ];

  const missing: typeof required = [];
  for (const t of required) {
    try {
      await execa("which", [t.bin], { stdio: "pipe" });
    } catch {
      missing.push(t);
    }
  }

  // AGT toolkit checkout is required to build the mesh relay + registry
  // images. Surface the missing clone HERE (before any other work) with
  // a copy-pasteable command + env var hint, instead of failing 5
  // minutes later inside the sandbox build step.
  const agtDockerfile = path.join(agtRepo, "agent-governance-python/agent-mesh/docker/Dockerfile");
  const agtMissing = !existsSync(agtDockerfile);

  if (missing.length === 0 && !agtMissing) return;

  console.error("");
  if (missing.length > 0) {
    console.error(chalk.red(`  ✗ Missing required tool${missing.length > 1 ? "s" : ""} for \`kars dev --target ${target}\`:`));
    for (const t of missing) {
      console.error(chalk.red(`    • ${chalk.bold(t.bin)}  — install: ${chalk.cyan(t.install)}`));
    }
    if (target === "local-k8s") {
      console.error(chalk.dim(`\n  Tip: macOS one-liner — \`brew install kind kubectl helm\` + Docker Desktop.`));
      console.error(chalk.dim(`       (Or fall back to \`--target docker\` which only needs Docker.)`));
    }
  }
  if (agtMissing) {
    if (missing.length > 0) console.error("");
    console.error(chalk.red(`  ✗ Microsoft Agent Governance Toolkit checkout not found at:`));
    console.error(chalk.red(`      ${agtRepo}`));
    console.error("");
    console.error(chalk.yellow(`  kars dev builds the AGT mesh relay + registry images from source.`));
    console.error(chalk.yellow(`  Clone the toolkit and re-run:`));
    console.error(chalk.cyan(`      git clone https://github.com/microsoft/agent-governance-toolkit ${agtRepo}`));
    console.error(chalk.cyan(`      kars dev`));
    console.error("");
    console.error(chalk.dim(`  Or, if you already have it elsewhere, point kars at it:`));
    console.error(chalk.cyan(`      export KARS_AGT_REPO=/path/to/agent-governance-toolkit`));
    console.error(chalk.dim(`  (or pass --agt-repo <path>).`));
  }
  console.error("");
  process.exit(1);
}

const DEFAULT_SANDBOX_IMAGE =
  "kars-sandbox:dev";
const SANDBOX_BASE_IMAGE =
  "kars-sandbox-base:dev";
const AZURELINUX_BASE =
  "mcr.microsoft.com/azurelinux/base/core:3.0";

const AGT_NETWORK = "kars-dev";
const AGT_POSTGRES = "kars-agt-postgres";
const AGT_RELAY = "kars-agt-relay";
const AGT_REGISTRY = "kars-agt-registry";

// Mesh provider port matrix.
//   agt: Microsoft AGT Python relay/registry (in-memory, no Postgres)
// Vendored Rust relay/registry were removed in Phase 5.2 (no longer
// shipped). The MeshProvider type is kept as a single-variant alias so
// the framework remains extensible for future providers.
const MESH_PORTS = {
  // AGT relay/registry expose `/health` (NOT `/healthz` — that route only
  // exists on the trust-engine/policy-server/audit-collector/api-gateway
  // components per agent-governance-python/agent-mesh/src/agentmesh/server/__init__.py).
  agt: { relay: 8083, registry: 8082, healthPath: "/health" },
} as const;
type MeshProvider = keyof typeof MESH_PORTS;
const DEFAULT_AGT_REPO = path.join(os.homedir(), "agent-governance-toolkit");

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
your choice (and credentials) will be saved to ~/.kars/. Subsequent
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
    // ── Target / runtime ──────────────────────────────────────────────
    .option(
      "--target <target>",
      "Where to run the sandbox: docker (default, fast) | local-k8s (kind+helm, mirrors AKS layout)",
      "docker"
    )
    .option(
      "--cluster-name <name>",
      "Kind cluster name (only used with --target local-k8s)",
      "kars-dev"
    )
    .option(
      "--ephemeral",
      "(local-k8s only) destroy the kind cluster on exit",
      false
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
      "--mesh-provider <provider>",
      "Mesh stack: only 'agt' is supported (vendored Rust relay/registry were removed in Phase 5.2). Kept as a flag for backward-compatible scripts.",
      "agt"
    )
    .option(
      "--agt-repo <path>",
      `Path to the agent-governance-toolkit checkout (used to build relay/registry images). Defaults to $KARS_AGT_REPO or ${DEFAULT_AGT_REPO}`
    )
    .option(
      "--agt-sdk-tarball <path>",
      "Path to a locally-packed @microsoft/agent-governance-sdk .tgz to install in the sandbox image (test patched AGT SDK end-to-end). Requires --build."
    )
    .option(
      "--no-mesh",
      "Skip mesh relay/registry deployment in --target local-k8s. The controller will start but cannot reach a relay; sandboxes lose KNOCK/E2E. Use only for pure controller smoke tests."
    )
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
  Mesh federation:    --mesh-provider, --agt-repo, --agt-sdk-tarball,
                      --global-registry
  Channels:           --channels, --telegram-*, --slack-token, --discord-token
  Skills + plugins:   --skills, --brave-api-key, --tavily-api-key,
                      --exa-api-key, --firecrawl-api-key,
                      --perplexity-api-key, --openai-api-key

Mesh provider selection:
  --mesh-provider=agt (default): builds and runs the Microsoft AGT
    Python relay + Python registry from the local agent-governance-toolkit
    checkout (--agt-repo). Registry is in-memory (no Postgres). Combine
    with --agt-sdk-tarball to test a locally-patched
    @microsoft/agent-governance-sdk inside the sandbox image.
  Vendored Rust relay/registry were removed in Phase 5.2.

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

      // Validate --mesh-provider value early (before any prompts), but
      // resolve final value AFTER first-run prompts (which may set it).
      if (options.meshProvider && options.meshProvider !== "agt") {
        console.error(chalk.red(`\n  Error: --mesh-provider must be 'agt' (got "${options.meshProvider}"). Vendored Rust relay/registry were removed in Phase 5.2.\n`));
        process.exit(1);
      }

      // ── First-run target prompt ──────────────────────────────────
      // Brand-new user with no saved creds AND no explicit --target
      // flag: ask docker vs local-k8s up front, before we get into
      // creds collection. Once they've run dev once, we trust the
      // explicit --target (or the default "docker"). Detect "no
      // --target was passed" by scanning argv directly — commander
      // applies the default before we see the option, so options.target
      // alone can't tell us.
      const targetWasExplicit = process.argv.some(
        (a) => a === "--target" || a.startsWith("--target="),
      );
      const credsForFirstRun = loadConfig();
      if (!targetWasExplicit && (!credsForFirstRun || !credsForFirstRun.firstRunCompleted)) {
        const { default: inquirer } = await import("inquirer");
        console.log(chalk.yellow("\n  👋 First time running `kars dev`. Where should the sandbox run?"));
        const { chosenTarget } = await inquirer.prompt([
          {
            type: "list",
            name: "chosenTarget",
            message: "Pick a runtime target:",
            default: "docker",
            choices: [
              {
                name: "Docker             (recommended; fast bringup, single container — perfect for prompt iteration)",
                value: "docker",
              },
              {
                name: "Local Kubernetes   (kind cluster + Helm chart + Headlamp dashboard — mirrors AKS exactly, slower bringup)",
                value: "local-k8s",
              },
            ],
          },
        ]);
        options.target = chosenTarget;
        if (chosenTarget === "local-k8s") {
          console.log(
            chalk.dim(
              "  Tip: pass `--target docker` next time to skip this prompt and use Docker again.",
            ),
          );
        }
      }

      // ── Pre-flight tool check ────────────────────────────────────
      // Fail fast if docker / kind / kubectl / helm aren't on PATH,
      // BEFORE we spend any time on creds/agent-name prompts. Bailing
      // 30s into setup with "helm: command not found" is a worse
      // first-run experience than getting the missing-tool list up front.
      await preflightTools(
        options.target as "docker" | "local-k8s",
        options.agtRepo ?? process.env.KARS_AGT_REPO ?? DEFAULT_AGT_REPO,
      );

      // ── First-run common prompts (apply to BOTH targets) ──────────
      // Creds + agent name + (docker-only) channels and rebuild are all
      // useful regardless of target. We collected just `target` above;
      // now collect creds and name so local-k8s users get the same
      // welcome experience as docker users. Channels and the rebuild
      // confirm stay docker-specific (local-k8s doesn't wire channel
      // env vars and doesn't have a single "the" sandbox image — it
      // ships three).
      const isFirstRun = !credsForFirstRun || !credsForFirstRun.firstRunCompleted;
      const ephemeralGhToken =
        typeof options.githubToken === "string" && options.githubToken.trim().length > 0;

      if (isFirstRun && !ephemeralGhToken) {
        const { default: inquirer } = await import("inquirer");

        // Always start with the provider picker — gives the user a
        // chance to test Copilot vs Foundry vs Models even when there
        // are existing creds for a different provider. After the pick:
        //   • if we have existing creds for that exact provider, offer
        //     a reuse confirm,
        //   • otherwise launch the same flow `kars credentials`
        //     uses (forced to the chosen provider), which prompts and
        //     persists to ~/.kars/.
        const { provider: chosenProvider } = await inquirer.prompt([
          {
            type: "list",
            name: "provider",
            message: "Which inference provider do you want to use?",
            default: credsForFirstRun?.provider ?? "github-copilot",
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

        const providerLabelFor = (p: KarsConfig["provider"]): string =>
          p === "github-models"
            ? "GitHub Models"
            : p === "github-copilot"
              ? "GitHub Copilot"
              : "Azure AI Foundry";

        let newCreds: KarsConfig;
        const haveMatchingCreds =
          credsForFirstRun && credsForFirstRun.provider === chosenProvider;

        if (haveMatchingCreds) {
          const detail =
            chosenProvider === "foundry"
              ? ` (${credsForFirstRun.endpoint})`
              : "";
          const { reuse } = await inquirer.prompt([
            {
              type: "confirm",
              name: "reuse",
              message: `Use existing ${providerLabelFor(chosenProvider)} credentials${detail}?`,
              default: true,
            },
          ]);
          if (reuse) {
            console.log(
              chalk.dim(
                "    Change with `kars credentials` at any time.\n",
              ),
            );
            const { markFirstRunCompleted } = await import("../config.js");
            markFirstRunCompleted();
            newCreds = credsForFirstRun;
          } else {
            console.log(
              chalk.yellow(
                `\n  Configuring fresh ${providerLabelFor(chosenProvider)} credentials:`,
              ),
            );
            newCreds = await promptAndSaveCredentials({ provider: chosenProvider });
          }
        } else {
          if (credsForFirstRun) {
            console.log(
              chalk.dim(
                `\n  No saved ${providerLabelFor(chosenProvider)} credentials (current: ${providerLabelFor(credsForFirstRun.provider)}). Let's configure them.\n`,
              ),
            );
          } else {
            console.log(
              chalk.yellow(
                `\n  👋 First time — let's set up ${providerLabelFor(chosenProvider)}.`,
              ),
            );
            console.log(
              chalk.dim(
                "  These will be saved to ~/.kars/, same as `kars credentials`.\n",
              ),
            );
          }
          newCreds = await promptAndSaveCredentials({ provider: chosenProvider });
        }

        // Agent name — only ask if user accepted the default.
        if (options.name === "dev-agent") {
          const { agentName } = await inquirer.prompt([
            {
              type: "input",
              name: "agentName",
              message: "Agent name:",
              default: "dev-agent",
              validate: (v: string) =>
                /^[a-z0-9][a-z0-9-]*[a-z0-9]?$/i.test(v.trim())
                  ? true
                  : "Use letters, numbers, and dashes only (e.g. dev-agent, alice-bot)",
            },
          ]);
          options.name = agentName.trim();
        }

        // Echo the chosen provider so the user can confirm at a glance.
        const providerLabel =
          newCreds.provider === "github-models"
            ? "GitHub Models"
            : newCreds.provider === "github-copilot"
              ? "GitHub Copilot"
              : "Azure AI Foundry";
        console.log(chalk.green(`  ✓ Credentials ready (${providerLabel})\n`));

        // ── Channels: prompt moved below the first-run block so it
        // fires on every run (a user who adds a Telegram token via
        // `kars credentials` after first-run should still get the
        // channel attached on the next `kars dev`).

        // Rebuild prompt — applies to BOTH targets. Cached images can
        // be stale (wrong arch after an `kars push` that always
        // builds linux/amd64; or out-of-date plugin/entrypoint code).
        // Defaults to no — first-time users want fast bringup. Power
        // users testing local changes can opt in here without
        // remembering the --build flag. Skipped if --build was passed
        // explicitly.
        if (!options.build) {
          const { rebuild } = await inquirer.prompt([{
            type: "confirm",
            name: "rebuild",
            message: "Rebuild sandbox image from local source? (slower, picks up plugin/entrypoint changes)",
            default: false,
          }]);
          if (rebuild) options.build = true;
        }

        // ── Mesh source prompt (local docker vs remote AKS) ─────────
        // Default to "local" — docker-compose'd relay/registry on the
        // user's laptop. "Remote" picks up an existing port-forward
        // (or auto-spawns one) against a previously-provisioned AKS
        // mesh, so the dev sandbox federates with whatever is already
        // running in the cluster. Only ask if the user did NOT pass
        // --global-registry explicitly (advanced flow).
        const globalRegistryExplicit = process.argv.some(
          a => a === "--global-registry" || a.startsWith("--global-registry="),
        );
        if (!globalRegistryExplicit) {
          const { loadContext } = await import("../config.js");
          const cachedCtx = loadContext();
          const cachedRegistryUrl = cachedCtx?.globalRegistryUrl;
          const aksAvailable = !!cachedCtx?.aksCluster;

          // Compose the remote-option label dynamically so the user
          // sees what would happen if they pick it: re-use cached
          // tunnel, or spawn one against the known AKS cluster.
          const remoteLabel = cachedRegistryUrl
            ? `Remote  (reuse port-forward to AKS: ${cachedRegistryUrl})`
            : aksAvailable
              ? `Remote  (auto port-forward to AKS cluster: ${cachedCtx!.aksCluster})`
              : "Remote  (port-forward to existing AKS mesh — requires `kars up` first)";

          const localLabel =
            options.target === "local-k8s"
              ? "Local   (recommended; relay + registry in the kind cluster alongside the sandbox)"
              : "Local   (recommended; spin up relay + registry in Docker on this laptop)";

          const { default: inquirer } = await import("inquirer");
          const { meshSource } = await inquirer.prompt([{
            type: "list",
            name: "meshSource",
            message: "Where should the mesh live?",
            default: "local",
            choices: [
              {
                name: localLabel,
                value: "local",
              },
              { name: remoteLabel, value: "remote" },
            ],
          }]);

          if (meshSource === "remote") {
            if (!cachedRegistryUrl && !aksAvailable) {
              console.log(chalk.yellow(
                "\n  ⚠ No cached AKS deployment context found. Falling back to local mesh.",
              ));
              console.log(chalk.dim(
                "    Run `kars up` first to provision an AKS cluster, then re-run `kars dev`.\n",
              ));
            } else {
              // Default port aligns with `kars mesh promote --port-forward`
              // (registry on 18080, relay on 18765). The downstream
              // global-registry block (around line 922) does the actual
              // health check + auto-spawn if the tunnels aren't already
              // up — so we just have to point it at the right URL.
              options.globalRegistry = cachedRegistryUrl ?? "http://localhost:18080";
              console.log(chalk.dim(
                `  → Will federate with remote mesh at ${options.globalRegistry}\n`,
              ));
            }
          }
        }

        // ── Mesh provider ───────────────────────────────────────────
        // Only AGT is supported after Phase 5.2 (vendored Rust relay+registry
        // removed). No prompt needed; the default and the flag both resolve
        // to "agt".
        options.meshProvider = "agt";
      }

      // ── Channels (works for both targets, on every run) ───────────
      // local-k8s ships a `<name>-credentials` Secret with the channel
      // tokens; docker mode passes them via `-e`. Either way we always
      // want to ask: a user who set their Telegram token via
      // `kars credentials` AFTER first-run completed would otherwise
      // never see the channel attached — `isFirstRun` is now false and
      // the channels prompt used to be gated on it. Skip only when the
      // user already passed `--channels` explicitly (CI / scripted use).
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
          if (variants.length === 0 && stored[baseKey]) {
            available.push({ name: displayName, value: channel });
          }
        };
        addChannel("telegram", "telegram-token", "Telegram");
        addChannel("slack",    "slack-token",    "Slack");
        addChannel("discord",  "discord-token",  "Discord");
        if (available.length > 0) {
          const { default: inquirer } = await import("inquirer");
          const { picked } = await inquirer.prompt([{
            type: "checkbox",
            name: "picked",
            message: "Enable any channels? (Space to toggle, Enter to confirm — leave empty to skip)",
            choices: available,
          }]);
          if (picked.length > 0) {
            options.channels = picked.join(",");
          }
        }
        // Note: if no channels are stored at all, we say nothing — that
        // hint message belongs to the first-run welcome block above.
      }

      // ── Target dispatch ───────────────────────────────────────────
      // local-k8s mode is a clean alternative to the docker stack: kind
      // cluster + helm-installed chart. It deliberately doesn't go
      // through the docker-compose path below — the two have different
      // bringup semantics, and conflating them muddies error reporting.
      const targets = ["docker", "local-k8s"];
      if (!targets.includes(options.target)) {
        console.error(
          chalk.red(
            `\n  Error: --target must be one of: ${targets.join(" | ")} (got "${options.target}").\n`,
          ),
        );
        process.exit(1);
      }
      if (options.target === "local-k8s") {
        const { runLocalK8s } = await import("./dev/local-k8s.js");
        try {
          await runLocalK8s({
            name: options.name,
            clusterName: options.clusterName,
            image: options.image,
            ephemeral: !!options.ephemeral,
            noBuild: false,
            forceRebuild: options.build === true,
            channels: typeof options.channels === "string" ? options.channels : undefined,
            meshProvider: "agt",
            agtRepo: options.agtRepo ?? process.env.KARS_AGT_REPO ?? DEFAULT_AGT_REPO,
            noMesh: options.noMesh === true,
            globalRegistry: typeof options.globalRegistry === "string" ? options.globalRegistry : undefined,
          });
          return;
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          console.error(chalk.red(`\n  local-k8s dev failed: ${msg}\n`));
          process.exit(1);
        }
      }

      banner("kars · Local Sandbox", "Secure AI Agent Runtime on Azure");

      const stepper = new Stepper({ totalSteps: 4 });

      // Find the kars repo root up-front. The auto-clone path below
      // reads vendor/agt/pin.json relative to this root, and various
      // downstream steps already expect to be able to resolve repo-
      // relative paths.
      const fsSync = await import("fs");
      const pathSync = await import("path");
      let repoRoot = process.cwd();
      while (repoRoot !== "/" && !fsSync.existsSync(pathSync.join(repoRoot, "Cargo.toml"))) {
        repoRoot = pathSync.dirname(repoRoot);
      }

      // Resolve mesh provider now that all interactive prompts have run.
      const meshProvider: MeshProvider = (options.meshProvider ?? "agt") as MeshProvider;
      const meshPorts = MESH_PORTS[meshProvider];
      // Auto-clone the pinned AGT fork (vendor/agt/pin.json) so fresh
      // machines can `kars dev --build` without first cloning AGT by
      // hand. Explicit --agt-repo / $KARS_AGT_REPO still win.
      let agtRepo: string;
      try {
        agtRepo = await ensureAgtRepo(options.agtRepo, repoRoot);
      } catch (e: unknown) {
        agtRepo = options.agtRepo ?? process.env.KARS_AGT_REPO ?? DEFAULT_AGT_REPO;
        if (meshProvider === "agt" && options.build) {
          console.error(chalk.red(`\n  Auto-cloning AGT failed:\n    ${(e as Error).message}\n`));
          process.exit(1);
        }
      }
      if (meshProvider === "agt" && options.build) {
        if (!existsSync(path.join(agtRepo, "agent-governance-python/agent-mesh/docker/Dockerfile"))) {
          console.error(chalk.red(`\n  Error: --mesh-provider=agt --build requires the agent-governance-toolkit checkout.`));
          console.error(chalk.red(`  Looked for: ${path.join(agtRepo, "agent-governance-python/agent-mesh/docker/Dockerfile")}\n`));
          console.error(chalk.yellow(`  Clone it:`));
          console.error(chalk.cyan(`      git clone https://github.com/microsoft/agent-governance-toolkit ${agtRepo}\n`));
          console.error(chalk.dim(`  Or pass --agt-repo <path> / set $KARS_AGT_REPO if you already have it elsewhere.\n`));
          process.exit(1);
        }
      }
      if (options.agtSdkTarball) {
        if (meshProvider !== "agt") {
          console.error(chalk.red(`\n  Error: --agt-sdk-tarball requires --mesh-provider=agt.\n`));
          process.exit(1);
        }
        if (!existsSync(options.agtSdkTarball)) {
          console.error(chalk.red(`\n  Error: --agt-sdk-tarball not found: ${options.agtSdkTarball}\n`));
          process.exit(1);
        }
      }

      try {
        let image = options.image;
        const { execa } = await import("execa");
        const path = await import("path");

        // repoRoot already computed at command entry; alias the local
        // path import for downstream code that uses path.join.

        // ── Credentials (first — prompt before potentially long build) ──
        stepper.step("Checking credentials...");
        const githubToken = typeof options.githubToken === "string" ? options.githubToken.trim() : undefined;
        let creds = loadConfig();
        // Always materialize a per-run secret tempfile from creds.apiKey.
        // Avoids depending on the legacy ~/.kars/credentials file
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
          // ── Docker-only first-run extras ────────────────────────────
          // Creds + agent name were already collected by the
          // common-prompt block before target dispatch. This branch only
          // runs in docker mode (local-k8s returns earlier). It handles
          // the channel + rebuild prompts that don't apply to local-k8s.
          stepper.stop();
          // Re-load — promptAndSaveCredentials wrote to disk.
          creds = loadConfig();
          if (!creds) {
            throw new Error(
              "Internal error: credentials missing after first-run prompt.",
            );
          }
          const { default: inquirer } = await import("inquirer");

          // Optional rebuild prompt is now hoisted to the common
          // first-run block — applies to both targets.

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
          const tmpDir = mkdtempSync(path.join(os.tmpdir(), "kars-secret-"));
          mountedSecretPath = path.join(tmpDir, "azure-openai-key");
          writeFileSync(mountedSecretPath, creds.apiKey, "utf-8"); // lgtm[js/http-to-file-access] — secret tempfile mounted into the dev container; 0o600 below
          chmodSync(mountedSecretPath, 0o600);
        }

        const isGithubModelsMode = creds.provider === "github-models";
        const isCopilotMode = creds.provider === "github-copilot";
        const isManagedTokenProvider = isGithubModelsMode || isCopilotMode;
        const model = isManagedTokenProvider
          ? creds.model
          : (options.model !== "gpt-4.1" ? options.model : creds.model);


        // ── Image resolution ─────────────────────────────────────────
        // Map Node.js `process.arch` → Docker platform arch token.
        // Docker uses linux/amd64 and linux/arm64; Node reports x64
        // and arm64. We force --platform on every dev build so the
        // image always matches the host (and won't trip Rosetta).
        const dockerArch = process.arch === "x64" ? "amd64" : process.arch === "arm64" ? "arm64" : process.arch;
        const dockerPlatform = `linux/${dockerArch}`;

        stepper.step("Resolving sandbox image...");
        let imageExists = false;
        if (!options.build) {
          stepper.update("Checking for sandbox image...");
          try {
            const { stdout: cachedArch } = await execa("docker", [
              "image", "inspect", image, "--format", "{{.Architecture}}",
            ], { stdio: "pipe" });
            if (cachedArch.trim() === dockerArch) {
              imageExists = true;
            } else {
              // Stale image from a prior `kars push` (which always
              // builds linux/amd64 for AKS) or a different host. Force
              // rebuild — running an amd64 sandbox under Rosetta on
              // Apple Silicon crashes with "rt_tgsigqueueinfo failed".
              console.log(chalk.dim(
                `  Cached ${image} is ${cachedArch.trim()}, host is ${dockerArch} — will rebuild.`,
              ));
            }
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
              await execa("docker", ["pull", "--platform", dockerPlatform, baseImage], { stdio: "pipe" });
            } catch {
              stepper.fail("Could not pull base image");
              console.log(chalk.yellow(`
  Failed to pull ${chalk.bold(baseImage)}.

  ${chalk.bold("1.")} Pull manually: ${chalk.cyan(`docker pull ${baseImage}`)}
  ${chalk.bold("2.")} Re-run:        ${chalk.cyan("kars dev")}

  Custom registry? ${chalk.cyan(`kars dev --base-image <your-registry>/azurelinux/base/core:3.0`)}
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
  Run from the kars repo root:
    ${chalk.cyan("git clone https://github.com/Azure/kars.git && cd kars")}
    ${chalk.cyan("kars dev")}
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
              "--platform", dockerPlatform,
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

          // Build inference router locally (sandbox Dockerfile copies
          // the binary in via FROM router-bin). Router Dockerfile is
          // COPY-only — stage the binary first.
          const routerImage = "kars-inference-router:dev";
          let routerExists = false;
          try {
            await execa("docker", ["image", "inspect", routerImage], { stdio: "pipe" });
            routerExists = true;
          } catch { /* not built yet */ }

          if (options.build || !routerExists) {
            const arch = archForDockerPlatform(dockerPlatform);
            const useMultistage = process.platform !== "linux";
            const dfPath = useMultistage
              ? path.join(repoRoot, "inference-router/Dockerfile.multistage")
              : routerDockerfile;
            if (!useMultistage) {
              stepper.update(`Staging inference-router binary (${arch})...`);
              stepper.stop();
              await stageRustBinaries(repoRoot, ["kars-inference-router"], arch, {
                forceRebuild: options.build,
              });
            }
            stepper.update(useMultistage
              ? "Building inference-router image (rust compile inside docker — ~3 min first run)..."
              : "Packaging inference-router image (distroless COPY — ~10s)...");
            stepper.stop();
            console.log(chalk.dim(`  docker build ${useMultistage ? "Dockerfile.multistage" : "Dockerfile"}...\n`));
            await execa("docker", [
              "build",
              "--platform", dockerPlatform,
              "-t", routerImage,
              "-f", dfPath,
              repoRoot,
            ], { stdio: "inherit" });
            console.log();
          }

          stepper.update("Building sandbox image (plugin + entrypoint overlay)...");
          stepper.stop();
          // Sandbox Dockerfile COPYs mesh-plugin/dist — stage it first.
          await stageMeshPlugin(repoRoot, { forceRebuild: options.build });
          console.log(chalk.dim("  Building sandbox image...\n"));

          // Stage the AGT SDK tarball into .agt-sdk/ (build context) when
          // --mesh-provider=agt --agt-sdk-tarball is set. The Dockerfile
          // ALWAYS copies .agt-sdk/ (.keep ensures it never fails); the
          // RUN step picks up $AGT_SDK_TARBALL only when actually staged.
          const sandboxBuildArgs = [
            "--build-arg", `SANDBOX_BASE_IMAGE=${SANDBOX_BASE_IMAGE}`,
            "--build-arg", `INFERENCE_ROUTER_IMAGE=${routerImage}`,
            "--build-arg", `MESH_PROVIDER=${meshProvider}`,
          ];
          const fsMod = await import("node:fs");
          const agtSdkStagingDir = path.join(repoRoot, ".agt-sdk");
          // Always clean previous staged tarballs to keep build context tight
          for (const f of fsMod.readdirSync(agtSdkStagingDir)) {
            if (f.endsWith(".tgz") || f.endsWith(".tar.gz")) {
              fsMod.unlinkSync(path.join(agtSdkStagingDir, f));
            }
          }
          if (meshProvider === "agt" && options.agtSdkTarball) {
            const tarballBasename = path.basename(options.agtSdkTarball);
            fsMod.copyFileSync(
              options.agtSdkTarball,
              path.join(agtSdkStagingDir, tarballBasename),
            );
            sandboxBuildArgs.push("--build-arg", `AGT_SDK_TARBALL=${tarballBasename}`);
            console.log(chalk.dim(`  Staged AGT SDK tarball: ${tarballBasename}\n`));
          } else if (meshProvider === "agt") {
            // Auto-discover OR pack-on-demand from the AGT repo. Stock
            // npm @^3.5.0 lacks registerSelf/autoRegister so the sandbox
            // can't register on the mesh — packing from source ships the
            // patched MeshClient that does.
            const tsDir = path.join(agtRepo, "agent-governance-typescript");
            const findTarball = (): string | undefined => {
              try {
                const hits = fsMod.readdirSync(tsDir).filter(
                  f => f.startsWith("microsoft-agent-governance-sdk-") && f.endsWith(".tgz"),
                ).sort();
                return hits.length > 0 ? hits[hits.length - 1] : undefined;
              } catch { return undefined; }
            };
            let tarballBasename = findTarball();
            if (!tarballBasename && existsSync(path.join(tsDir, "package.json"))) {
              console.log(chalk.dim(`  Packing AGT SDK from source (one-time, ~30s)...\n`));
              try {
                await execa("npm", ["install", "--prefer-offline", "--no-audit", "--no-fund"], { cwd: tsDir, stdio: "inherit" });
                await execa("npm", ["run", "build"], { cwd: tsDir, stdio: "inherit" });
                await execa("npm", ["pack"], { cwd: tsDir, stdio: "inherit" });
                tarballBasename = findTarball();
              } catch (e) {
                console.log(chalk.yellow(`  Could not pack AGT SDK: ${(e as Error).message}\n`));
              }
            }
            if (tarballBasename) {
              fsMod.copyFileSync(
                path.join(tsDir, tarballBasename),
                path.join(agtSdkStagingDir, tarballBasename),
              );
              sandboxBuildArgs.push("--build-arg", `AGT_SDK_TARBALL=${tarballBasename}`);
              console.log(chalk.dim(`  Staged AGT SDK tarball: ${tarballBasename}\n`));
            }
          }

          await execa("docker", [
            "build",
            "--platform", dockerPlatform,
            ...sandboxBuildArgs,
            "-t", "kars-sandbox:dev",
            "-f", dockerfilePath,
            repoRoot,
          ], { stdio: "inherit" });
          console.log();
          image = "kars-sandbox:dev";
          stepper.done("Sandbox image built");

          // Build mesh relay + registry images from the AGT toolkit.
          // (Vendored Rust relay/registry were removed in Phase 5.2.)
          {
            const agtDockerfile = path.join(agtRepo, "agent-governance-python/agent-mesh/docker/Dockerfile");

            // Check whether both images are already loaded locally.
            const haveImage = async (tag: string): Promise<boolean> => {
              try {
                await execa("docker", ["image", "inspect", tag], { stdio: "pipe" });
                return true;
              } catch { return false; }
            };
            const relayCached = await haveImage("agentmesh-relay:dev");
            const registryCached = await haveImage("agentmesh-registry:dev");

            if ((!relayCached || !registryCached) && !existsSync(agtDockerfile)) {
              console.error(chalk.red(`\n  Mesh images need to be built from the agent-governance-toolkit source,`));
              console.error(chalk.red(`  but no checkout was found at:`));
              console.error(chalk.red(`      ${agtRepo}\n`));
              console.error(chalk.yellow(`  Clone it next to your kars repo and re-run:`));
              console.error(chalk.cyan(`      git clone https://github.com/microsoft/agent-governance-toolkit ${agtRepo}`));
              console.error(chalk.cyan(`      kars dev\n`));
              console.error(chalk.dim(`  Or pass --agt-repo <path> / set $KARS_AGT_REPO if you already have it elsewhere.\n`));
              process.exit(1);
            }

            // agt: single Dockerfile, COMPONENT build-arg switches relay vs registry.
            if (!relayCached || options.build) {
              stepper.update("Building AGT relay image (Python)...");
              stepper.stop();
              console.log(chalk.dim("  Building agentmesh-relay (Python from local AGT)...\n"));
              await execa("docker", [
                "build", "--platform", dockerPlatform,
                "--build-arg", "COMPONENT=relay",
                "-t", "agentmesh-relay:dev",
                "-f", agtDockerfile,
                agtRepo,
              ], { stdio: "inherit" });
              console.log();
            }
            if (!registryCached || options.build) {
              stepper.update("Building AGT registry image (Python)...");
              stepper.stop();
              console.log(chalk.dim("  Building agentmesh-registry (Python from local AGT)...\n"));
              await execa("docker", [
                "build", "--platform", dockerPlatform,
                "--build-arg", "COMPONENT=registry",
                "-t", "agentmesh-registry:dev",
                "-f", agtDockerfile,
                agtRepo,
              ], { stdio: "inherit" });
              console.log();
            }
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
          // Local registry mode — deploy AGT relay/registry locally
          stepper.step("Starting mesh infrastructure (agt)...");

          // Helper: check if a container exists and is running
          async function isContainerRunning(name: string): Promise<boolean> {
            try {
              const { stdout } = await execa("docker", [
                "inspect", "-f", "{{.State.Running}}", name,
              ], { stdio: "pipe" });
              return stdout.trim() === "true";
            } catch { return false; }
          }

          // Tear down any stale postgres from a previous vendored run to
          // avoid name confusion in `docker ps`. The AGT Python registry
          // keeps state in-memory and does not need Postgres.
          try { await execa("docker", ["rm", "-fv", AGT_POSTGRES], { stdio: "pipe" }); } catch {}

          // Start AGT mesh relay (Python, binds 8083)
          if (!(await isContainerRunning(AGT_RELAY))) {
            stepper.update("Starting agt relay...");
            try { await execa("docker", ["rm", "-f", AGT_RELAY], { stdio: "pipe" }); } catch {}
            await execa("docker", [
              "run", "-d",
              "--name", AGT_RELAY,
              "--network", AGT_NETWORK,
              // Suppress upstream AGT Dockerfile HEALTHCHECK probing /healthz
              // (the relay/registry expose /health, not /healthz — the misnamed
              // upstream healthcheck spams 404s in the logs).
              "--no-healthcheck",
              "-e", "AGENTMESH_COMPONENT=relay",
              "-e", "HOST=0.0.0.0",
              "-e", `PORT=${meshPorts.relay}`,
              "-e", "LOG_LEVEL=info",
              "agentmesh-relay:dev",
            ], { stdio: "pipe" });
          }

          // Start AGT mesh registry (Python, in-memory)
          if (!(await isContainerRunning(AGT_REGISTRY))) {
            stepper.update("Starting agt registry...");
            try { await execa("docker", ["rm", "-f", AGT_REGISTRY], { stdio: "pipe" }); } catch {}
            await execa("docker", [
              "run", "-d",
              "--name", AGT_REGISTRY,
              "--network", AGT_NETWORK,
              "--no-healthcheck",
              "-e", "AGENTMESH_COMPONENT=registry",
              "-e", "HOST=0.0.0.0",
              "-e", `PORT=${meshPorts.registry}`,
              "-e", "LOG_LEVEL=info",
              "-e", "AGENTMESH_REGISTRY_ALLOW_UNAUTHED_DID=1",
              "agentmesh-registry:dev",
            ], { stdio: "pipe" });
          }

          // Health check — wait for registry to be ready
          stepper.update("Waiting for mesh services...");
          for (let i = 0; i < 30; i++) {
            try {
              await execa("docker", [
                "exec", AGT_REGISTRY, "curl", "-sf",
                `http://localhost:${meshPorts.registry}${meshPorts.healthPath}`,
              ], { stdio: "pipe" });
              agtReady = true;
              break;
            } catch { await new Promise(r => setTimeout(r, 1000)); }
          }

          const readyMsg = "mesh infrastructure ready (agt: relay + registry, in-memory)";
          const pendingMsg = "mesh infrastructure started (agt, health check pending)";
          stepper.done(agtReady ? readyMsg : pendingMsg);
        } else if (useGlobalRegistry) {
          // Global registry mode — skip local deployment, verify connectivity
          stepper.step("Connecting to global registry...");
          const registryUrl = options.globalRegistry as string;

          // Validate URL scheme — registryUrl may originate from a config file
          // or env var; reject anything that isn't http(s) before issuing
          // outbound requests (CodeQL js/file-access-to-http hardening).
          {
            let parsed: URL;
            try {
              parsed = new URL(registryUrl);
            } catch {
              throw new Error(`--global-registry must be a valid URL: ${registryUrl}`);
            }
            if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
              throw new Error(
                `--global-registry must use http(s); got ${parsed.protocol}`,
              );
            }
          }

          // Rewrite localhost URLs for Docker containers — localhost inside
          // the container refers to the container itself, not the host.
          const containerRegistryUrl = registryUrl.replace(
            /\/\/(localhost|127\.0\.0\.1)([:\/])/,
            "//host.docker.internal$2"
          );
          if (containerRegistryUrl !== registryUrl) {
            stepper.update(`Rewriting ${registryUrl} → ${containerRegistryUrl} for container access`);
          }

          // Health check from the host (validates port-forward / tunnel is up).
          // AGT registry exposes /health; vendored exposes both /health and
          // /v1/health — probe /health first, fall back for older clusters.
          stepper.update(`Checking ${registryUrl}...`);
          async function probeRegistry(url: string): Promise<Response | null> {
            for (const probe of ["/health", "/v1/health"]) {
              try {
                const r = await fetch(`${url.replace(/\/$/, "")}${probe}`, {
                  signal: AbortSignal.timeout(10000),
                });
                if (r.ok) return r;
              } catch { /* try next */ }
            }
            return null;
          }
          let initial: Response | null = null;
          try {
            initial = await probeRegistry(registryUrl);
          } catch { /* fall through to auto-promote */ }
          if (initial?.ok) {
            agtReady = true;
            stepper.done(`Global registry connected (${registryUrl}) — handoff enabled`);
          } else {
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
              const logDir = path.join(os.homedir(), ".kars", "logs");
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

              const pidFile = path.join(os.homedir(), ".kars", "port-forward-pids.json");
              writeFileSync(pidFile, JSON.stringify(pids, null, 2));

              // Kill any stale listeners that aren't our spawned PIDs
              const { killStaleListeners } = await import("./mesh.js");
              const portPidMap: Array<{ port: number; pid: number }> = [];
              if (pids.Registry) portPidMap.push({ port: regPort, pid: pids.Registry });
              if (pids.Relay) portPidMap.push({ port: relayPort, pid: pids.Relay });
              await killStaleListeners(portPidMap);

              // Re-check after promote (same /health → /v1/health fallback)
              const retry = await probeRegistry(registryUrl);
              agtReady = !!retry?.ok;
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
        const containerName = `kars-${options.name}`;

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
        const seccompPath = path.join(distDir, "profiles", "seccomp", "kars-strict.json");
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
            "-e", `KARS_MESH_PROVIDER=${meshProvider}`,
          );
        } else {
          // Local registry mode — router connects to colocated containers.
          // Ports differ between providers (see MESH_PORTS).
          agtEnvArgs.push(
            "-e", `AGT_RELAY_URL=ws://${AGT_RELAY}:${meshPorts.relay}`,
            "-e", `AGT_REGISTRY_URL=http://${AGT_REGISTRY}:${meshPorts.registry}`,
            "-e", "AGT_REGISTRY_MODE=local",
            "-e", "AGT_GOVERNANCE_ENABLED=true",
            "-e", `KARS_MESH_PROVIDER=${meshProvider}`,
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
          "-e", "KARS_DEV_MODE=true",
          "-e", "KARS_DEV_PROFILE=true",
          ...(isGithubModelsMode ? ["-e", "KARS_PROVIDER=github-models"] : []),
          ...(isCopilotMode ? ["-e", "KARS_PROVIDER=github-copilot"] : []),
          "-e", `DOCKER_NETWORK=${AGT_NETWORK}`,
          // Phase 2/F8 mitigations — env-gated suppression of false-positive
          // governance findings. Default-on in dev so research/citation
          // workloads aren't impeded; override with =0 to restore strict mode.
          "-e", "KARS_SUPPRESS_EXFIL_URL=1",
          "-e", "KARS_SUPPRESS_CONTENT_FLAGS=violence",
          "-e", "KARS_CONTENT_FLAG_MIN_SEVERITY=medium",
          ...(creds.foundryProjectEndpoint ? ["-e", `FOUNDRY_PROJECT_ENDPOINT=${creds.foundryProjectEndpoint}`] : []),
          ...(discoveredDeployments ? ["-e", `FOUNDRY_DEPLOYMENTS=${discoveredDeployments}`] : []),
          "-e", `PS1=kars@${options.name}:\\w\\$ `,
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
                "iptables -L KARS_EGRESS -n 2>/dev/null | grep -q REJECT",
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
        checkLine(hasSeccomp, `seccomp profile ${hasSeccomp ? "(kars-strict)" : "(not loaded)"}`);
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
        console.log(`  Connect:  ${chalk.cyan(`kars connect ${options.name}`)}`);
        console.log(`  Shell:    ${chalk.cyan(`kars connect ${options.name} --shell`)}`);
        console.log(`  Status:   ${chalk.cyan(`kars status ${options.name}`)}`);
        console.log(`  Stop:     ${chalk.cyan(`kars destroy ${options.name}`)}`);
        if (gatewayToken) {
          const url = `http://localhost:18789/#token=${gatewayToken}`;
          // Print URL without chalk formatting — terminals auto-detect http:// links.
          // Chalk ANSI codes break terminal URL detection in most emulators.
          console.log(`  Web UI:   ${url}`);
        }
        console.log(chalk.dim(`\n  Production: kars up (deploys to AKS)`));
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

  // `kars dev down [--target local-k8s] [--keep-cluster]`
  // Tears down the local-k8s dev environment created by `kars dev
  // --target local-k8s`. For Docker target, `kars destroy` already
  // does the right thing — `dev down` is local-k8s-specific.
  cmd
    .command("down")
    .description("Tear down a local-k8s dev environment (cluster + Headlamp port-forward)")
    .option(
      "--target <target>",
      "Which target to tear down (only 'local-k8s' is currently supported)",
      "local-k8s",
    )
    .option(
      "--cluster-name <name>",
      "Kind cluster name to delete",
      "kars-dev",
    )
    .option(
      "--keep-cluster",
      "Stop the port-forward and uninstall Headlamp, but keep the kind cluster running",
      false,
    )
    .action(async (options: { target: string; clusterName: string; keepCluster: boolean }) => {
      if (options.target !== "local-k8s") {
        console.error(
          chalk.red(`  --target ${options.target} is not supported by 'dev down'.`),
        );
        console.error(
          chalk.dim("  For Docker dev sandboxes, use 'kars destroy <name>' instead."),
        );
        process.exit(1);
      }
      const { execa } = await import("execa");
      console.log(chalk.bold("\nTearing down local-k8s dev environment…\n"));

      // Always: kill any lingering port-forward on :4466.
      try {
        const { stdout } = await execa("lsof", ["-ti", ":4466"]);
        const pids = stdout.trim().split(/\s+/).filter(Boolean);
        for (const pid of pids) {
          try {
            await execa("kill", [pid]);
            console.log(chalk.green(`  ✓ killed port-forward PID ${pid}`));
          } catch {
            /* already gone */
          }
        }
      } catch {
        console.log(chalk.dim("  • no port-forward listening on :4466"));
      }

      if (options.keepCluster) {
        console.log(
          chalk.green("\n  ✓ Done. Cluster '") +
            chalk.bold(options.clusterName) +
            chalk.green("' is still running. Use --no-keep-cluster to delete it.\n"),
        );
        return;
      }

      // Delete the kind cluster (idempotent — kind handles "doesn't exist").
      // Detect the runtime so we hand kind the right
      // KIND_EXPERIMENTAL_PROVIDER. If we don't, a cluster created under
      // podman/nerdctl is invisible to kind when we shell out without
      // the env var, and the delete silently no-ops while the user
      // thinks they reclaimed resources.
      try {
        const { detectRuntimeEnv } = await import("./dev/local-k8s.js");
        const env = await detectRuntimeEnv();
        await execa("kind", ["delete", "cluster", "--name", options.clusterName], {
          stdio: "inherit",
          env,
        });
        console.log(chalk.green(`\n  ✓ Cluster '${options.clusterName}' deleted.\n`));
      } catch (err) {
        console.error(
          chalk.red(`\n  Failed to delete cluster: ${(err as Error).message}\n`),
        );
        process.exit(1);
      }
    });

  return cmd;
}
