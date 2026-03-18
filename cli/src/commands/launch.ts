import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function launchCommand(): Command {
  const cmd = new Command("launch");

  cmd
    .description("Create a new sandboxed OpenClaw agent")
    .argument("<name>", "Name for the sandbox")
    .option("--model <model>", "AI model", "azure/gpt-4.1")
    .option(
      "--isolation <level>",
      "Isolation: standard, enhanced, confidential",
      "enhanced"
    )
    .option("--policy <file>", "Path to network policy YAML")
    .option("--image <image>", "Custom sandbox image")
    .action(async (name: string, options) => {
      const spinner = ora(`Creating sandbox '${name}'...`).start();

      try {
        // TODO: Apply ClawSandbox CRD via kubectl
        spinner.succeed(`Sandbox '${name}' created`);

        console.log(
          `\nConnect: ${chalk.cyan(`azureclaw ${name} connect`)}\n`
        );
      } catch (error) {
        spinner.fail(`Failed to create sandbox '${name}'`);
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
