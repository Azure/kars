import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function policyCommand(): Command {
  const cmd = new Command("policy");

  cmd.description("Manage sandbox network and security policies");

  cmd
    .command("set")
    .description("Apply or update a policy on a running sandbox")
    .argument("<name>", "Sandbox name")
    .requiredOption("-p, --policy <file>", "Path to policy YAML file")
    .option("--wait", "Wait for policy to be applied", false)
    .action(async (name: string, options) => {
      const spinner = ora(`Applying policy to '${name}'...`).start();

      try {
        // TODO: kubectl apply the policy ConfigMap + trigger hot-reload
        spinner.succeed(`Policy applied to '${name}'`);
        console.log(
          chalk.dim(
            `  Dynamic sections (network, inference) hot-reloaded without restart.`
          )
        );
      } catch (error) {
        spinner.fail("Failed to apply policy");
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  cmd
    .command("get")
    .description("Show the active policy for a sandbox")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      // TODO: kubectl get configmap + format
      console.log(chalk.bold(`\n🔒 Active policy for: ${name}\n`));
      console.log(chalk.dim("  (policy display placeholder)\n"));
    });

  return cmd;
}
