import { Command } from "commander";
import chalk from "chalk";

export function statusCommand(): Command {
  const cmd = new Command("status");

  cmd
    .description("Show sandbox health, policy state, and inference configuration")
    .argument("<name>", "Sandbox name")
    .action(async (name: string) => {
      // TODO: kubectl get clawsandbox <name> -o json + parse

      console.log(chalk.bold(`\n📊 Sandbox: ${name}\n`));
      console.log(`  Status:        ${chalk.green("Running")}`);
      console.log(`  Model:         azure/gpt-4.1`);
      console.log(`  Isolation:     enhanced (seccomp + SELinux)`);
      console.log(`  Network:       default-deny + 6 allowed endpoints`);
      console.log(`  Uptime:        2h 15m`);
      console.log(`  Tokens used:   12,450 (input) / 8,320 (output)`);
      console.log(`  Pending:       0 egress approval requests`);
      console.log();
    });

  return cmd;
}
