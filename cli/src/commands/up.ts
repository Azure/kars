import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function upCommand(): Command {
  const cmd = new Command("up");

  cmd
    .description(
      "One command to go from zero to running agent. Provisions Azure resources, creates sandbox, connects you."
    )
    .option("--name <name>", "Sandbox name", "my-assistant")
    .option("--model <model>", "AI model", "gpt-4.1")
    .option(
      "--policy <preset>",
      "Policy preset: minimal, developer, web, azure",
      "developer"
    )
    .option("--region <region>", "Azure region", "eastus2")
    .option(
      "--confidential",
      "Enable Confidential Containers (SEV-SNP)",
      false
    )
    .option("-g, --resource-group <name>", "Resource group name")
    .action(async (options) => {
      console.log(chalk.blue("\n🦞 AzureClaw — starting up...\n"));

      const rg =
        options.resourceGroup || `azureclaw-${options.region}`;
      const spinner = ora("").start();

      try {
        // Step 1: Check Azure auth
        spinner.text = "Checking Azure credentials...";
        // TODO: az account show, prompt az login if needed

        // Step 2: Check if cluster already exists (idempotent)
        spinner.text = "Checking for existing AzureClaw cluster...";
        // TODO: az aks show

        // Step 3: Provision infrastructure (if needed)
        spinner.text = `Provisioning Azure resources in ${options.region}...`;
        // TODO: az deployment group create (Bicep)

        // Step 4: Install AzureClaw controller (if needed)
        spinner.text = "Installing AzureClaw controller...";
        // TODO: helm upgrade --install

        // Step 5: Create sandbox
        spinner.text = `Creating sandbox '${options.name}' with ${options.model}...`;
        // TODO: kubectl apply ClawSandbox CRD

        // Step 6: Wait for sandbox to be ready
        spinner.text = "Waiting for sandbox to start...";
        // TODO: kubectl wait --for=condition=ready

        spinner.succeed("Ready!");

        // Print connection info
        console.log(
          chalk.green(
            "\n──────────────────────────────────────────────────"
          )
        );
        console.log(
          `  Sandbox      ${chalk.bold(options.name)}`
        );
        console.log(
          `  Model        ${chalk.bold(options.model)} (Azure OpenAI, Managed Identity)`
        );
        console.log(
          `  Policy       ${chalk.bold(options.policy)} preset`
        );
        console.log(
          `  Region       ${chalk.bold(options.region)}`
        );
        console.log(
          chalk.green(
            "──────────────────────────────────────────────────"
          )
        );
        console.log(
          `\n  Connect:     ${chalk.cyan(`azureclaw ${options.name} connect`)}`
        );
        console.log(
          `  Status:      ${chalk.cyan(`azureclaw ${options.name} status`)}`
        );
        console.log(
          `  Logs:        ${chalk.cyan(`azureclaw ${options.name} logs -f`)}`
        );
        console.log(
          `  Costs:       ${chalk.cyan(`azureclaw ${options.name} costs`)}`
        );
        console.log();
      } catch (error) {
        spinner.fail("Setup failed");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
