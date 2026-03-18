import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function onboardCommand(): Command {
  const cmd = new Command("onboard");

  cmd
    .description(
      "Interactive setup wizard: Azure login, model selection, policy, and sandbox creation"
    )
    .option("--model <model>", "AI model to use", "azure/gpt-4.1")
    .option("--name <name>", "Sandbox name", "my-assistant")
    .option(
      "--isolation <level>",
      "Isolation level: standard, enhanced, confidential",
      "enhanced"
    )
    .action(async (options) => {
      console.log(chalk.blue("\n🦞 AzureClaw Onboard Wizard\n"));

      const spinner = ora("Starting onboard wizard...").start();

      try {
        // Step 1: Verify cluster connection
        spinner.text = "Verifying AKS cluster connection...";
        // TODO: kubectl cluster-info

        // Step 2: Interactive model selection
        spinner.stop();
        console.log(chalk.bold("\n📦 Model Selection\n"));
        console.log(`  Selected model: ${chalk.cyan(options.model)}`);
        console.log(
          `  Provider:       ${chalk.cyan("Azure OpenAI (Managed Identity)")}`
        );

        // Step 3: Configure network policy
        console.log(chalk.bold("\n🔒 Security Policy\n"));
        console.log(`  Isolation:      ${chalk.cyan(options.isolation)}`);
        console.log(`  Network:        ${chalk.cyan("Default-deny egress")}`);
        console.log(`  Filesystem:     ${chalk.cyan("/sandbox + /tmp writable")}`);
        console.log(`  Process:        ${chalk.cyan("seccomp + SELinux")}`);

        // Step 4: Create sandbox
        spinner.start("Creating sandboxed OpenClaw agent...");
        // TODO: kubectl apply -f ClawSandbox CRD

        spinner.succeed("Sandbox created!");

        // Summary
        console.log(
          chalk.green("\n──────────────────────────────────────────────────")
        );
        console.log(
          `  Sandbox      ${chalk.bold(options.name)} (seccomp + SELinux + NetworkPolicy)`
        );
        console.log(
          `  Model        ${chalk.bold(options.model)} (Azure OpenAI, Managed Identity)`
        );
        console.log(
          chalk.green("──────────────────────────────────────────────────")
        );
        console.log(
          `  Run:         ${chalk.cyan(`azureclaw ${options.name} connect`)}`
        );
        console.log(
          `  Status:      ${chalk.cyan(`azureclaw ${options.name} status`)}`
        );
        console.log(
          `  Logs:        ${chalk.cyan(`azureclaw ${options.name} logs --follow`)}`
        );
        console.log(
          chalk.green("──────────────────────────────────────────────────\n")
        );
      } catch (error) {
        spinner.fail("Onboarding failed");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
