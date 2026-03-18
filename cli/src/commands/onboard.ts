import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import inquirer from "inquirer";
import { mkdirSync, writeFileSync, chmodSync, existsSync, readFileSync } from "fs";
import { join } from "path";
import { homedir } from "os";

const CONFIG_DIR = join(homedir(), ".azureclaw");
const CONFIG_FILE = join(CONFIG_DIR, "config.json");
const CREDENTIALS_FILE = join(CONFIG_DIR, "credentials");

export function onboardCommand(): Command {
  const cmd = new Command("onboard");

  cmd
    .description(
      "Interactive setup wizard: configure Azure OpenAI endpoint, API key, and model"
    )
    .action(async () => {
      const blue = chalk.hex("#0078D4");

      console.log(blue(`
  ╔══════════════════════════════════════════════════╗
  ║           AzureClaw · Onboard Wizard             ║
  ║        Secure AI Agent Runtime on Azure          ║
  ╚══════════════════════════════════════════════════╝
`));

      // Load existing config if present
      let existingConfig: Record<string, string> = {};
      if (existsSync(CONFIG_FILE)) {
        try {
          existingConfig = JSON.parse(readFileSync(CONFIG_FILE, "utf-8"));
          console.log(chalk.dim("  Existing configuration found. Press Enter to keep current values.\n"));
        } catch {
          // Ignore corrupt config
        }
      }

      const answers = await inquirer.prompt([
        {
          type: "input",
          name: "endpoint",
          message: "Azure OpenAI endpoint (from Foundry portal):",
          default: existingConfig.endpoint || "https://your-resource.openai.azure.com",
          validate: (input: string) => {
            if (!input.startsWith("https://")) return "Endpoint must start with https://";
            return true;
          },
          filter: (input: string) => {
            // Normalize: strip trailing /openai/v1, /openai, trailing slashes
            return input
              .replace(/\/openai\/v1\/?$/, "")
              .replace(/\/openai\/?$/, "")
              .replace(/\/+$/, "");
          },
        },
        {
          type: "input",
          name: "model",
          message: "Model deployment name:",
          default: existingConfig.model || "gpt-4.1",
        },
        {
          type: "password",
          name: "apiKey",
          message: "API key:",
          mask: "•",
          validate: (input: string) => {
            if (!input || input.length < 10) return "API key is required";
            return true;
          },
        },
      ]);

      const spinner = ora({ color: "cyan" }).start("Verifying credentials...");

      // Verify the endpoint + key work
      try {
        const response = await fetch(
          `${answers.endpoint.replace(/\/$/, "")}/openai/deployments/${answers.model}/chat/completions?api-version=2024-12-01-preview`,
          {
            method: "POST",
            headers: {
              "Content-Type": "application/json",
              "api-key": answers.apiKey,
            },
            body: JSON.stringify({
              messages: [{ role: "user", content: "hello" }],
              max_tokens: 5,
            }),
          }
        );

        if (!response.ok) {
          const body = await response.text();
          spinner.fail("Credential verification failed");
          console.log(chalk.red(`\n  ${response.status}: ${body}\n`));
          console.log(chalk.yellow("  Check your endpoint, model deployment name, and API key.\n"));
          process.exit(1);
        }

        spinner.succeed("Credentials verified");
      } catch (error) {
        spinner.fail("Could not reach endpoint");
        const message = error instanceof Error ? error.message : String(error);
        console.log(chalk.red(`\n  ${message}\n`));
        process.exit(1);
      }

      // Save config
      spinner.start("Saving configuration...");

      mkdirSync(CONFIG_DIR, { recursive: true });

      // Config file (non-sensitive)
      const config = {
        endpoint: answers.endpoint,
        model: answers.model,
        version: "0.1.0-alpha.1",
      };
      writeFileSync(CONFIG_FILE, JSON.stringify(config, null, 2), "utf-8");
      chmodSync(CONFIG_FILE, 0o600);

      // Credentials file (sensitive — restrictive permissions)
      writeFileSync(CREDENTIALS_FILE, answers.apiKey, "utf-8");
      chmodSync(CREDENTIALS_FILE, 0o600);

      spinner.succeed("Configuration saved");

      console.log(blue(`\n  ── Configuration ─────────────────────────────────`));
      console.log(`  Endpoint:  ${chalk.bold(answers.endpoint)}`);
      console.log(`  Model:     ${chalk.bold(answers.model)}`);
      console.log(`  Key:       ${chalk.dim("••••" + answers.apiKey.slice(-4))}`);
      console.log(`  Config:    ${chalk.dim(CONFIG_FILE)}`);
      console.log(`  Key file:  ${chalk.dim(CREDENTIALS_FILE)} ${chalk.dim("(permissions: 600)")}`);

      console.log(blue(`\n  ── Next Steps ────────────────────────────────────`));
      console.log(`  Dev:       ${chalk.cyan("azureclaw dev")}`);
      console.log(`  Connect:   ${chalk.cyan("azureclaw connect dev-agent")}`);
      console.log(`  Re-run:    ${chalk.cyan("azureclaw onboard")} to change settings`);
      console.log();
    });

  return cmd;
}
