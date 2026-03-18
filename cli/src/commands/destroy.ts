import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function destroyCommand(): Command {
  const cmd = new Command("destroy");

  cmd
    .description("Teardown a sandbox with confirmation")
    .argument("<name>", "Sandbox name")
    .option("-y, --yes", "Skip confirmation prompt", false)
    .action(async (name: string, options) => {
      if (!options.yes) {
        console.log(
          chalk.yellow(
            `\n⚠️  This will permanently destroy sandbox '${name}' and all its data.`
          )
        );
        console.log(
          chalk.dim(`  Run with --yes to skip this confirmation.\n`)
        );
        // TODO: inquirer confirmation prompt
        return;
      }

      const spinner = ora(`Destroying sandbox '${name}'...`).start();

      try {
        // TODO: kubectl delete clawsandbox <name>
        spinner.succeed(`Sandbox '${name}' destroyed`);
      } catch (error) {
        spinner.fail(`Failed to destroy sandbox '${name}'`);
        const message =
          error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
