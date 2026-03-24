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

  return cmd;
}
