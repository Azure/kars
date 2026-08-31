// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import { randomBytes } from "node:crypto";
import { banner, section } from "../stepper.js";
import {
  promptAndSaveCredentials, SECRETS_FILE,
  KNOWN_SECRETS, loadSecrets, setSecret, getSecret, deleteSecret, listSecretVariants,
} from "../config.js";
import { buildCredentialSecretManifest } from "./add.js";
import {
  classifyFeishuSecretReference,
  shouldCleanupStagedFeishuSecret,
  type FeishuSecretReferenceState,
} from "../lib/feishu-secret-reference.js";

export const CREDENTIAL_FLAG_TO_ENV: Record<string, string> = {
  telegramToken: "TELEGRAM_BOT_TOKEN",
  telegramAllowFrom: "TELEGRAM_ALLOW_FROM",
  slackToken: "SLACK_BOT_TOKEN",
  discordToken: "DISCORD_BOT_TOKEN",
  feishuAppId: "FEISHU_APP_ID",
  feishuAppSecret: "FEISHU_APP_SECRET",
  braveApiKey: "BRAVE_API_KEY",
  tavilyApiKey: "TAVILY_API_KEY",
  exaApiKey: "EXA_API_KEY",
  firecrawlApiKey: "FIRECRAWL_API_KEY",
  perplexityApiKey: "PERPLEXITY_API_KEY",
  openaiApiKey: "OPENAI_API_KEY",
};

const FEISHU_CREDENTIAL_KEYS = new Set(["FEISHU_APP_ID", "FEISHU_APP_SECRET"]);

type SandboxCredentialView = {
  metadata?: { name?: string; namespace?: string; resourceVersion?: string };
  status?: { namespace?: string };
  spec?: {
    channels?: Array<{
      type?: string;
      credentialSecretRef?: { name?: string };
      [key: string]: unknown;
    }>;
  };
};

export function selectSandboxForCredentialUpdate(
  sandboxName: string,
  list: { items?: SandboxCredentialView[] },
): SandboxCredentialView {
  const matches = (list.items ?? []).filter((item) => item.metadata?.name === sandboxName);
  if (matches.length === 0) {
    throw new Error(`KarsSandbox '${sandboxName}' was not found`);
  }
  if (matches.length > 1) {
    throw new Error(
      `multiple KarsSandboxes named '${sandboxName}' exist; update credentials with kubectl in the intended namespace`,
    );
  }
  return matches[0];
}

export type CredentialSecretUpdatePlan = {
  kind: "conventional" | "feishu";
  secretName: string;
  updates: Record<string, string>;
};

export function validateCredentialUpdates(updates: Record<string, string>): void {
  const hasAppId = Boolean(updates.FEISHU_APP_ID);
  const hasAppSecret = Boolean(updates.FEISHU_APP_SECRET);
  if (hasAppId !== hasAppSecret) {
    throw new Error("Feishu App ID and App Secret must be updated together");
  }
  if (hasAppId && Object.keys(updates).some((key) => !FEISHU_CREDENTIAL_KEYS.has(key))) {
    throw new Error("Feishu credentials must be rotated separately from other credentials");
  }
}

export function planCredentialSecretUpdates(
  sandboxName: string,
  sandbox: SandboxCredentialView,
  updates: Record<string, string>,
): CredentialSecretUpdatePlan[] {
  validateCredentialUpdates(updates);
  const conventionalSecret = `${sandboxName}-credentials`;
  const feishuChannel = sandbox.spec?.channels?.find((channel) => channel.type === "Feishu");
  if (updates.FEISHU_APP_ID && !feishuChannel) {
    throw new Error(`KarsSandbox '${sandboxName}' does not declare a Feishu channel`);
  }
  const feishuSecret = feishuChannel?.credentialSecretRef?.name || conventionalSecret;
  const conventionalUpdates: Record<string, string> = {};
  const feishuUpdates: Record<string, string> = {};
  for (const [key, value] of Object.entries(updates)) {
    if (FEISHU_CREDENTIAL_KEYS.has(key)) {
      feishuUpdates[key] = value;
    } else {
      conventionalUpdates[key] = value;
    }
  }
  const plans: CredentialSecretUpdatePlan[] = [];
  if (Object.keys(conventionalUpdates).length > 0) {
    plans.push({ kind: "conventional", secretName: conventionalSecret, updates: conventionalUpdates });
  }
  if (Object.keys(feishuUpdates).length > 0) {
    plans.push({ kind: "feishu", secretName: feishuSecret, updates: feishuUpdates });
  }
  return plans;
}

export function buildFeishuRotationSecretName(baseName: string, suffix: string): string {
  const trailer = `-rotation-${suffix}`;
  return `${baseName.slice(0, 253 - trailer.length).replace(/[.-]+$/, "")}${trailer}`;
}

export function buildFeishuChannelSecretPatch(
  resourceVersion: string,
  channels: NonNullable<SandboxCredentialView["spec"]>["channels"],
  secretName: string,
): Array<Record<string, unknown>> {
  if (!resourceVersion) {
    throw new Error("KarsSandbox resourceVersion is required for Feishu credential rotation");
  }
  const channelIndex = (channels ?? []).findIndex((channel) => channel.type === "Feishu");
  if (channelIndex < 0) {
    throw new Error("KarsSandbox does not declare a Feishu channel");
  }
  const channel = channels![channelIndex];
  const refPath = `/spec/channels/${channelIndex}/credentialSecretRef`;
  return [
    { op: "test", path: "/metadata/resourceVersion", value: resourceVersion },
    { op: "test", path: `/spec/channels/${channelIndex}/type`, value: "Feishu" },
    channel.credentialSecretRef
      ? { op: "replace", path: `${refPath}/name`, value: secretName }
      : { op: "add", path: refPath, value: { name: secretName } },
  ];
}

export function credentialsCommand(): Command {
  const cmd = new Command("credentials");

  cmd
    .description(
      "Manage kars credentials (inference provider, channel tokens, API keys)"
    )
    .action(async () => {
      const { default: inquirer } = await import("inquirer");

      banner("kars · Credentials", "Secure AI Agent Runtime on Azure");

      // Show current state
      const secrets = loadSecrets();
      if (Object.keys(secrets).length > 0) {
        console.log(chalk.dim("  Currently stored:"));
        for (const key of Object.keys(secrets).sort()) {
          const masked = "••••";
          const info = KNOWN_SECRETS[key.includes(".") ? key.slice(0, key.indexOf(".")) : key];
          const label = info ? chalk.dim(` (${info.label})`) : "";
          console.log(`    ${chalk.cyan(key)} = ${masked}${label}`);
        }
        console.log();
      }

      // Main menu
      const categories = [
        { name: "Inference     — Azure AI Foundry / Azure OpenAI or GitHub Models", value: "inference" },
        { name: "Telegram      — bot token, allowed users", value: "telegram" },
        { name: "Slack         — bot OAuth token", value: "slack" },
        { name: "Discord       — bot token", value: "discord" },
        { name: "Feishu        — App ID and App Secret", value: "feishu" },
        { name: "Search APIs   — Brave, Tavily, Exa, Perplexity", value: "search" },
        { name: "Other APIs    — Firecrawl, OpenAI", value: "other" },
        new inquirer.Separator(),
        { name: "Done", value: "done" },
      ];

      while (true) {
        const { category } = await inquirer.prompt([{
          type: "list",
          name: "category",
          message: "What would you like to configure?",
          choices: categories,
        }]);

        if (category === "done") break;

        if (category === "inference") {
          await promptAndSaveCredentials({ heading: "Inference provider" });
        } else {
          const promptMap: Record<string, Array<{ key: string; label: string; allowSuffix?: boolean }>> = {
            telegram: [
              { key: "telegram-token", label: "Telegram bot token", allowSuffix: true },
              { key: "telegram-allow-from", label: "Telegram allowed user IDs (comma-separated)", allowSuffix: true },
            ],
            slack: [
              { key: "slack-token", label: "Slack bot OAuth token", allowSuffix: true },
            ],
            discord: [
              { key: "discord-token", label: "Discord bot token", allowSuffix: true },
            ],
            feishu: [
              { key: "feishu-app-id", label: "Feishu App ID", allowSuffix: true },
              { key: "feishu-app-secret", label: "Feishu App Secret", allowSuffix: true },
            ],
            search: [
              { key: "brave-api-key", label: "Brave Search API key" },
              { key: "tavily-api-key", label: "Tavily search API key" },
              { key: "exa-api-key", label: "Exa search API key" },
              { key: "perplexity-api-key", label: "Perplexity API key" },
            ],
            other: [
              { key: "firecrawl-api-key", label: "Firecrawl API key" },
              { key: "openai-api-key", label: "OpenAI API key" },
            ],
          };
          const prompts = promptMap[category] || [];

          for (const p of prompts) {
            let finalKey = p.key;

            // For tokens that support dot-suffix variants, ask for label
            if (p.allowSuffix) {
              const existing = listSecretVariants(p.key);
              if (existing.length > 0) {
                console.log(chalk.dim(`  Existing: ${existing.map(v => v.key).join(", ")}`));
              }
              const { suffix } = await inquirer.prompt([{
                type: "input",
                name: "suffix",
                message: `Label for this ${p.label} (e.g. "cloud", "dev", or blank for default):`,
                filter: (v: string) => v.trim().toLowerCase().replace(/\s+/g, "-"),
              }]);
              if (suffix) finalKey = `${p.key}.${suffix}`;
            }

            const currentVal = getSecret(finalKey);
            const currentHint = currentVal ? chalk.dim(" (current: set)") : "";

            const { value } = await inquirer.prompt([{
              type: "password",
              name: "value",
              message: `${p.label}${currentHint}:`,
              mask: "•",
            }]);

            if (value && value.trim()) {
              setSecret(finalKey, value.trim());
              console.log(chalk.green(`  ✔ ${finalKey} = ••••`));
            } else if (currentVal) {
              console.log(chalk.dim(`  Kept existing value for ${finalKey}`));
            } else {
              console.log(chalk.dim(`  Skipped ${finalKey}`));
            }
          }
        }
        console.log();
      }

      section("Summary");
      const final = loadSecrets();
      const count = Object.keys(final).length;
      console.log(`  ${chalk.bold(String(count))} secret${count !== 1 ? "s" : ""} stored in ${chalk.dim(SECRETS_FILE)}`);
      console.log();
      section("Next Steps");
      console.log(`  Dev:    ${chalk.cyan("kars dev")}               ${chalk.dim("— tokens auto-loaded")}`);
      console.log(`  Add:    ${chalk.cyan("kars add <name>")}        ${chalk.dim("— tokens auto-loaded")}`);
      console.log(`  List:   ${chalk.cyan("kars credentials list")}  ${chalk.dim("— show all (masked)")}`);
      console.log();
    });

  // ─── set <key> [value] ───────────────────────────────────────────────────
  const set = new Command("set");
  set
    .description("Store a secret locally (e.g. telegram-token, brave-api-key)")
    .argument("<key>", `Secret key (${Object.keys(KNOWN_SECRETS).join(", ")})`)
    .argument("[value]", "Secret value (omit for masked prompt)")
    .action(async (key: string, value?: string) => {
      const info = KNOWN_SECRETS[key];
      // Strip dot-suffix for validation (telegram-token.cloud → telegram-token)
      const baseKey = key.includes(".") ? key.slice(0, key.indexOf(".")) : key;
      const baseInfo = KNOWN_SECRETS[baseKey];
      if (!info && !baseInfo) {
        console.log(chalk.yellow(`  Warning: '${key}' is not a known secret key.`));
        console.log(chalk.dim(`  Known keys: ${Object.keys(KNOWN_SECRETS).join(", ")}`));
      }

      // If no value provided, prompt with masked input
      if (!value) {
        const label = (info || baseInfo)?.label || key;
        const { default: inquirer } = await import("inquirer");
        const answer = await inquirer.prompt([{
          type: "password",
          name: "secret",
          message: `${label}:`,
          mask: "•",
          validate: (input: string) => input.length > 0 ? true : "Value cannot be empty",
        }]);
        value = answer.secret;
      }

      // Note: `setSecret` runs `normalizeSecretValue` so Telegram `bot`
      // prefix stripping happens uniformly across all write paths.
      setSecret(key, value!);
      console.log(chalk.green(`  ✔ ${key} = ••••`));
      console.log(chalk.dim(`  Saved to ${SECRETS_FILE}`));
      if (info || baseInfo) {
        console.log(chalk.dim(`  → env var: ${(info || baseInfo)!.env}`));
      }
    });
  cmd.addCommand(set);

  // ─── list ──────────────────────────────────────────────────────────────────
  const list = new Command("list");
  list
    .description("List all stored secrets (values masked)")
    .action(() => {
      const secrets = loadSecrets();
      const keys = Object.keys(secrets);
      if (keys.length === 0) {
        console.log(chalk.dim("  No secrets stored. Use: kars credentials set <key> <value>"));
        return;
      }
      console.log(chalk.bold("\n  Stored secrets:\n"));
      for (const key of keys.sort()) {
        const masked = "••••";
        const info = KNOWN_SECRETS[key];
        let label = "";
        if (info) {
          label = chalk.dim(` (${info.label})`);
        } else {
          // Check for dot-suffixed variant (e.g. telegram-token.cloud → "Telegram bot token")
          const dotIdx = key.indexOf(".");
          if (dotIdx > 0) {
            const baseKey = key.substring(0, dotIdx);
            const variant = key.substring(dotIdx + 1);
            const baseInfo = KNOWN_SECRETS[baseKey];
            if (baseInfo) label = chalk.dim(` (${baseInfo.label} · ${variant})`);
          }
        }
        console.log(`  ${chalk.cyan(key)} = ${masked}${label}`);
      }
      console.log(chalk.dim(`\n  File: ${SECRETS_FILE}\n`));
    });
  cmd.addCommand(list);

  // ─── remove <key> ──────────────────────────────────────────────────────────
  const remove = new Command("remove");
  remove
    .description("Remove a stored secret")
    .argument("<key>", "Secret key to remove")
    .action((key: string) => {
      if (deleteSecret(key)) {
        console.log(chalk.green(`  ✔ Removed '${key}'`));
      } else {
        console.log(chalk.yellow(`  '${key}' not found in secrets`));
      }
    });
  cmd.addCommand(remove);

  // Subcommand: update credentials for a running AKS sandbox
  const update = new Command("update");
  update
    .description("Update credentials for a running AKS sandbox (updates Secret + coordinates pod restart)")
    .argument("<name>", "Sandbox name")
    .option("--telegram-token <token>", "New Telegram bot token")
    .option("--telegram-allow-from <ids>", "Telegram allowed user IDs (comma-separated)")
    .option("--slack-token <token>", "New Slack bot token")
    .option("--discord-token <token>", "New Discord bot token")
    .option("--feishu-app-id <id>", "New Feishu App ID")
    .option("--feishu-app-secret <secret>", "New Feishu App Secret")
    .option("--brave-api-key <key>", "New Brave Search API key")
    .option("--tavily-api-key <key>", "New Tavily API key")
    .option("--exa-api-key <key>", "New Exa API key")
    .option("--firecrawl-api-key <key>", "New Firecrawl API key")
    .option("--perplexity-api-key <key>", "New Perplexity API key")
    .option("--openai-api-key <key>", "New OpenAI API key")
    .option("--no-restart", "Update secret without restarting the pod")
    .action(async (name: string, options) => {
      const { execa } = await import("execa");
      const ora = (await import("ora")).default;

      // Collect new values
      const updates: Record<string, string> = {};
      for (const [flag, env] of Object.entries(CREDENTIAL_FLAG_TO_ENV)) {
        if (options[flag]) updates[env] = options[flag];
      }

      if (Object.keys(updates).length === 0) {
        console.error(chalk.red("  No credentials specified. Use --telegram-token, --brave-api-key, etc."));
        process.exit(1);
      }
      const rotatesFeishu = Boolean(updates.FEISHU_APP_ID);
      if (rotatesFeishu && options.restart === false) {
        console.error(chalk.red("  Feishu credential rotation does not support --no-restart; the controller must claim the new App before rollout."));
        process.exit(1);
      }

      const spinner = ora(`Updating credentials for '${name}'...`).start();
      let stagedFeishuSecret: string | undefined;
      let stagedFeishuNamespace: string | undefined;
      let feishuControlNamespace: string | undefined;

      try {
        const { stdout: sandboxJson } = await execa("kubectl", [
          "get", "karssandboxes", "-A", "-o", "json",
        ], { stdio: "pipe" });
        const sandbox = selectSandboxForCredentialUpdate(
          name,
          JSON.parse(sandboxJson) as { items?: SandboxCredentialView[] },
        );
        const controlNamespace = sandbox.metadata?.namespace || "kars-system";
        feishuControlNamespace = controlNamespace;
        const namespace = sandbox.status?.namespace || `kars-${name}`;
        const plans = planCredentialSecretUpdates(name, sandbox, updates);

        for (const plan of plans) {
          let existing: Record<string, string> = {};
          try {
            const { stdout } = await execa("kubectl", [
              "get", "secret", plan.secretName, "-n", namespace,
              "-o", "jsonpath={.data}",
            ], { stdio: "pipe" });
            if (stdout && stdout !== "{}") {
              const data = JSON.parse(stdout);
              for (const [key, value] of Object.entries(data)) {
                existing[key] = Buffer.from(value as string, "base64").toString();
              }
            }
          } catch { /* secret doesn't exist yet */ }

          const targetSecretName = plan.kind === "feishu"
            ? buildFeishuRotationSecretName(plan.secretName, randomBytes(6).toString("hex"))
            : plan.secretName;
          const retained = plan.kind === "feishu"
            ? Object.fromEntries(
                Object.entries(existing).filter(([key]) => FEISHU_CREDENTIAL_KEYS.has(key)),
              )
            : existing;
          const manifest = buildCredentialSecretManifest(
            targetSecretName,
            namespace,
            { ...retained, ...plan.updates },
            plan.kind === "feishu"
              ? {
                  immutable: true,
                  labels: {
                    "channels.kars.azure.com/managed-rotation": "true",
                    "channels.kars.azure.com/revision-state": "staged",
                    "kars.azure.com/sandbox": name,
                  },
                }
              : {},
          );
          const secretCommand = plan.kind === "feishu"
            ? ["create", "-f", "-"]
            : ["apply", "--server-side", "--field-manager=kars-cli", "-f", "-"];
          await execa("kubectl", secretCommand, {
            input: JSON.stringify(manifest),
            stdio: ["pipe", "pipe", "pipe"],
          });
          if (plan.kind === "feishu") {
            stagedFeishuSecret = targetSecretName;
            stagedFeishuNamespace = namespace;
          }
        }

        if (stagedFeishuSecret) {
          await execa("kubectl", [
            "patch", "karssandbox", name, "-n", controlNamespace,
            "--type=json", "-p", JSON.stringify(
              buildFeishuChannelSecretPatch(
                sandbox.metadata?.resourceVersion ?? "",
                sandbox.spec?.channels,
                stagedFeishuSecret,
              ),
            ),
          ], { stdio: "pipe" });
        }

        spinner.succeed("Secret updated");

        // Show what changed
        for (const env of Object.keys(updates)) {
          console.log(chalk.dim(`  ${env} updated`));
        }

        if (options.restart !== false) {
          const restartSpinner = ora("Restarting pod...").start();
          if (!rotatesFeishu) {
            await execa("kubectl", [
              "rollout", "restart", `deploy/${name}`, "-n", namespace,
            ], { stdio: "pipe" });
          }

          // Wait for rollout
          try {
            await execa("kubectl", [
              "rollout", "status", `deploy/${name}`, "-n", namespace,
              "--timeout=90s",
            ], { stdio: "pipe" });
            restartSpinner.succeed("Pod restarted with new credentials");
          } catch {
            restartSpinner.warn("Rollout started — pod may still be starting");
          }
        } else {
          console.log(chalk.yellow("  Secret updated but pod NOT restarted (--no-restart)"));
          console.log(chalk.dim(`  Restart manually: kubectl rollout restart deploy/${name} -n ${namespace}`));
        }
      } catch (err: any) {
        let referenceState: FeishuSecretReferenceState = "unknown";
        if (stagedFeishuSecret && feishuControlNamespace) {
          try {
            const { stdout } = await execa("kubectl", [
              "get", "karssandbox", name, "-n", feishuControlNamespace,
              "--ignore-not-found=true", "-o", "json",
            ], { stdio: "pipe" });
            referenceState = classifyFeishuSecretReference(stdout, stagedFeishuSecret);
          } catch { /* preserve the Secret when the live reference is unknown */ }
        }
        if (
          shouldCleanupStagedFeishuSecret(stagedFeishuSecret, referenceState)
          && stagedFeishuNamespace
        ) {
          try {
            await execa("kubectl", [
              "delete", "secret", stagedFeishuSecret!, "-n", stagedFeishuNamespace,
              "--ignore-not-found=true",
            ], { stdio: "pipe" });
          } catch {
            console.error(chalk.yellow(
              `  Warning: failed to remove unreferenced Secret '${stagedFeishuSecret}'`,
            ));
          }
        }
        spinner.fail(`Failed: ${err.message}`);
        process.exit(1);
      }
    });

  cmd.addCommand(update);

  return cmd;
}
