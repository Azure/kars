import { Command } from "commander";
import chalk from "chalk";
import { banner, section } from "../stepper.js";
import { promptAndSaveCredentials, CONFIG_FILE, CREDENTIALS_FILE } from "../config.js";

export function credentialsCommand(): Command {
  const cmd = new Command("credentials");

  cmd
    .description(
      "Set or update Azure OpenAI credentials (endpoint, API key, model)"
    )
    .action(async () => {
      banner("AzureClaw · Credentials", "Secure AI Agent Runtime on Azure");

      const creds = await promptAndSaveCredentials();

      section("Saved");
      console.log(`  Endpoint:  ${chalk.bold(creds.endpoint)}`);
      console.log(`  Model:     ${chalk.bold(creds.model)}`);
      console.log(`  Key:       ${chalk.dim("••••" + creds.apiKey.slice(-4))}`);
      console.log(`  Config:    ${chalk.dim(CONFIG_FILE)}`);
      console.log(`  Key file:  ${chalk.dim(CREDENTIALS_FILE)} ${chalk.dim("(600)")}`);

      section("Next Steps");
      console.log(`  Dev:       ${chalk.cyan("azureclaw dev")}`);
      console.log(`  Prod:      ${chalk.cyan("azureclaw up")}`);
      console.log(`  Re-run:    ${chalk.cyan("azureclaw credentials")}`);
      console.log();
    });

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
