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
    .argument("<model>", "Model name (e.g. gpt-4.1, Phi-4, Meta-Llama-3.1-405B-Instruct)")
    .action(async (name: string, model: string) => {
      const { execa } = await import("execa");
      const spinner = ora(`Switching ${name} to ${model}...`).start();

      try {
        // Patch the ClawSandbox CR's inference.model field
        await execa("kubectl", [
          "patch", "clawsandbox", name,
          "-n", "azureclaw-system",
          "--type", "merge",
          "-p", JSON.stringify({ spec: { inference: { model } } }),
        ], { stdio: "pipe" });

        // Update the inference router env var on the deployment
        const namespace = `azureclaw-${name}`;
        await execa("kubectl", [
          "set", "env", `deploy/${name}`,
          "-n", namespace,
          "-c", "inference-router",
          `DEFAULT_MODEL=${model}`,
          `AZURE_OPENAI_DEPLOYMENT=${model}`,
        ], { stdio: "pipe" }).catch(() => {});

        spinner.succeed(`${name} now using ${chalk.bold(model)}`);
        console.log(chalk.dim("  Model switch takes effect on next request.\n"));
      } catch (error) {
        spinner.fail("Failed to switch model");
        const message = error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  cmd
    .command("get")
    .description("Show current model for a sandbox")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      const { execa } = await import("execa");
      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawsandbox", name,
          "-n", "azureclaw-system",
          "-o", "jsonpath={.spec.inference.model}",
        ], { stdio: "pipe" });
        console.log(`\n  ${name}: ${chalk.bold(stdout.trim() || "gpt-4.1")} (Foundry)\n`);
      } catch {
        console.log(chalk.red(`\n  Sandbox '${name}' not found.\n`));
      }
    });

  cmd
    .command("list")
    .description("List available models from Foundry")
    .action(async () => {
      console.log(chalk.bold("\n  Available models (Foundry):\n"));
      console.log("  OpenAI:");
      console.log("    gpt-4.1, gpt-4o, gpt-4.1-mini, o3-mini, o1");
      console.log("  Microsoft:");
      console.log("    Phi-4, Phi-4-mini-instruct, Phi-4-multimodal-instruct");
      console.log("  Meta:");
      console.log("    Meta-Llama-3.1-405B-Instruct, Llama-3.2-90B-Vision-Instruct");
      console.log("  Mistral:");
      console.log("    Mistral-small-2503, Codestral-2501");
      console.log("  Anthropic:");
      console.log("    claude-sonnet-4-5, claude-opus-4-5");
      console.log(chalk.dim("\n  1800+ models available. Deploy via Foundry portal.\n"));
    });

  return cmd;
}
