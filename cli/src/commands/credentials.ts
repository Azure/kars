import { Command } from "commander";
import chalk from "chalk";
import { banner, section } from "../stepper.js";
import {
  promptAndSaveCredentials, SECRETS_FILE,
  KNOWN_SECRETS, loadSecrets, setSecret, getSecret, deleteSecret, listSecretVariants,
} from "../config.js";

export function credentialsCommand(): Command {
  const cmd = new Command("credentials");

  cmd
    .description(
      "Manage AzureClaw credentials (Azure OpenAI, channel tokens, API keys)"
    )
    .action(async () => {
      const { default: inquirer } = await import("inquirer");

      banner("AzureClaw · Credentials", "Secure AI Agent Runtime on Azure");

      // Show current state
      const secrets = loadSecrets();
      if (Object.keys(secrets).length > 0) {
        console.log(chalk.dim("  Currently stored:"));
        for (const key of Object.keys(secrets).sort()) {
          const val = secrets[key];
          const masked = val.length > 8 ? "••••" + val.slice(-4) : "••••";
          const info = KNOWN_SECRETS[key.includes(".") ? key.slice(0, key.indexOf(".")) : key];
          const label = info ? chalk.dim(` (${info.label})`) : "";
          console.log(`    ${chalk.cyan(key)} = ${masked}${label}`);
        }
        console.log();
      }

      // Main menu
      const categories = [
        { name: "Azure OpenAI  — endpoint, model, API key", value: "aoai" },
        { name: "Telegram      — bot token, allowed users", value: "telegram" },
        { name: "Slack         — bot OAuth token", value: "slack" },
        { name: "Discord       — bot token", value: "discord" },
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

        if (category === "aoai") {
          await promptAndSaveCredentials({ heading: "Azure OpenAI" });
        } else {
          const promptMap: Record<string, Array<{ key: string; label: string; allowSuffix?: boolean }>> = {
            telegram: [
              { key: "telegram-token", label: "Telegram bot token", allowSuffix: true },
              { key: "telegram-allow-from", label: "Telegram allowed user IDs (comma-separated)" },
            ],
            slack: [
              { key: "slack-token", label: "Slack bot OAuth token", allowSuffix: true },
            ],
            discord: [
              { key: "discord-token", label: "Discord bot token", allowSuffix: true },
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
            const currentHint = currentVal ? chalk.dim(` (current: ••••${currentVal.slice(-4)})`) : "";

            const { value } = await inquirer.prompt([{
              type: "password",
              name: "value",
              message: `${p.label}${currentHint}:`,
              mask: "•",
            }]);

            if (value && value.trim()) {
              setSecret(finalKey, value.trim());
              const masked = value.length > 8 ? "••••" + value.slice(-4) : "••••";
              console.log(chalk.green(`  ✔ ${finalKey} = ${masked}`));
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
      console.log(`  Dev:    ${chalk.cyan("azureclaw dev")}               ${chalk.dim("— tokens auto-loaded")}`);
      console.log(`  Add:    ${chalk.cyan("azureclaw add <name>")}        ${chalk.dim("— tokens auto-loaded")}`);
      console.log(`  List:   ${chalk.cyan("azureclaw credentials list")}  ${chalk.dim("— show all (masked)")}`);
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

      setSecret(key, value!);
      const masked = value!.length > 8 ? "••••" + value!.slice(-4) : "••••";
      console.log(chalk.green(`  ✔ ${key} = ${masked}`));
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
        console.log(chalk.dim("  No secrets stored. Use: azureclaw credentials set <key> <value>"));
        return;
      }
      console.log(chalk.bold("\n  Stored secrets:\n"));
      for (const key of keys.sort()) {
        const val = secrets[key];
        const masked = val.length > 8 ? "••••" + val.slice(-4) : "••••";
        const info = KNOWN_SECRETS[key];
        const label = info ? chalk.dim(` (${info.label})`) : "";
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
    .description("Update credentials for a running AKS sandbox (updates secret + restarts pod)")
    .argument("<name>", "Sandbox name")
    .option("--telegram-token <token>", "New Telegram bot token")
    .option("--slack-token <token>", "New Slack bot token")
    .option("--discord-token <token>", "New Discord bot token")
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

      const flagToEnv: Record<string, string> = {
        telegramToken: "TELEGRAM_BOT_TOKEN",
        slackToken: "SLACK_BOT_TOKEN",
        discordToken: "DISCORD_BOT_TOKEN",
        braveApiKey: "BRAVE_API_KEY",
        tavilyApiKey: "TAVILY_API_KEY",
        exaApiKey: "EXA_API_KEY",
        firecrawlApiKey: "FIRECRAWL_API_KEY",
        perplexityApiKey: "PERPLEXITY_API_KEY",
        openaiApiKey: "OPENAI_API_KEY",
      };

      // Collect new values
      const updates: Record<string, string> = {};
      for (const [flag, env] of Object.entries(flagToEnv)) {
        if (options[flag]) updates[env] = options[flag];
      }

      if (Object.keys(updates).length === 0) {
        console.error(chalk.red("  No credentials specified. Use --telegram-token, --brave-api-key, etc."));
        process.exit(1);
      }

      const namespace = `azureclaw-${name}`;
      const secretName = `${name}-credentials`;
      const spinner = ora(`Updating credentials for '${name}'...`).start();

      try {
        // Read existing secret (if any) and merge with new values
        let existing: Record<string, string> = {};
        try {
          const { stdout } = await execa("kubectl", [
            "get", "secret", secretName, "-n", namespace,
            "-o", "jsonpath={.data}",
          ], { stdio: "pipe" });
          if (stdout && stdout !== "{}") {
            const data = JSON.parse(stdout);
            for (const [k, v] of Object.entries(data)) {
              existing[k] = Buffer.from(v as string, "base64").toString();
            }
          }
        } catch { /* secret doesn't exist yet */ }

        const merged = { ...existing, ...updates };

        // Create/replace the secret
        const secretArgs = ["create", "secret", "generic", secretName, "-n", namespace, "--dry-run=client", "-o", "yaml"];
        for (const [env, val] of Object.entries(merged)) {
          secretArgs.push(`--from-literal=${env}=${val}`);
        }
        const { stdout: yaml } = await execa("kubectl", secretArgs, { stdio: "pipe" });
        await execa("kubectl", ["apply", "-f", "-"], { input: yaml, stdio: ["pipe", "pipe", "pipe"] });

        spinner.succeed("Secret updated");

        // Show what changed
        for (const [env, val] of Object.entries(updates)) {
          console.log(chalk.dim(`  ${env} = ••••${val.slice(-4)}`));
        }

        // Restart pod unless --no-restart
        if (options.restart !== false) {
          const restartSpinner = ora("Restarting pod...").start();
          await execa("kubectl", [
            "rollout", "restart", `deploy/${name}`, "-n", namespace,
          ], { stdio: "pipe" });

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
        spinner.fail(`Failed: ${err.message}`);
        process.exit(1);
      }
    });

  cmd.addCommand(update);

  return cmd;
}
