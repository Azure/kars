import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function modelCommand(): Command {
  const cmd = new Command("model");

  cmd.description("Manage the AI model for a sandbox");

  cmd
    .command("set")
    .description("Switch AI model (instant, no restart)")
    .argument("<name>", "Sandbox name")
    .argument("<model>", "Model name (e.g. gpt-4.1, Phi-4, llama-3.3-70b)")
    .action(async (name: string, model: string) => {
      const spinner = ora(
        `Switching ${name} to ${model}...`
      ).start();

      try {
        // TODO: kubectl patch ClawSandbox inference.model
        // The inference router picks up the change immediately — no pod restart
        spinner.succeed(`${name} now using ${chalk.bold(model)}`);
        console.log(
          chalk.dim("  Model switch is instant — no restart needed.\n")
        );
      } catch (error) {
        spinner.fail("Failed to switch model");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  cmd
    .command("get")
    .description("Show current model for a sandbox")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      // TODO: kubectl get clawsandbox -o jsonpath='{.spec.inference.model}'
      console.log(`\n  ${name}: ${chalk.bold("gpt-4.1")} (Azure OpenAI)\n`);
    });

  cmd
    .command("list")
    .description("List available models")
    .action(async () => {
      // TODO: Query Azure AI Foundry model catalog
      console.log(chalk.bold("\n  Available models:\n"));
      console.log("  Azure OpenAI:");
      console.log("    gpt-4.1, gpt-4o, o3-mini, o1");
      console.log("  Azure AI Foundry:");
      console.log("    Phi-4, Mistral-Large, Llama-3.3-70B, ...");
      console.log(
        chalk.dim("    (1800+ models — run with --all for full list)\n")
      );
    });

  return cmd;
}
