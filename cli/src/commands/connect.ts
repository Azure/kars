import { Command } from "commander";
import { execa } from "execa";
import chalk from "chalk";

export function connectCommand(): Command {
  const cmd = new Command("connect");

  cmd
    .description("Connect to a sandbox — launches OpenClaw interactive agent")
    .argument("<name>", "Sandbox name")
    .option("--shell", "Drop to bash shell instead of OpenClaw", false)
    .action(async (name: string, options: { shell: boolean }) => {
      const containerName = `azureclaw-${name}`;

      // What to run: bash shell (same as NemoClaw — user runs openclaw tui themselves)
      const launchCmd = "exec /bin/bash --login";

      // Try local Docker first
      let found = false;
      try {
        const { stdout } = await execa("docker", [
          "inspect", "--format", "{{.State.Running}}", containerName,
        ], { stdio: "pipe" });

        if (stdout.trim() === "true") {
          found = true;
          console.log(chalk.hex("#0078D4")(`\n  Connected to ${chalk.bold(name)}. OpenClaw is ready.\n`));
          console.log(chalk.dim(`  Chat:    openclaw tui`));
          console.log(chalk.dim(`  Message: openclaw agent --agent main --local -m "hello" --session-id test`));
          console.log(chalk.dim(`  Exit:    type "exit"\n`));
          await execa("docker", [
            "exec", "-it", containerName, "/bin/bash", "--login",
          ], { stdio: "inherit" });
          return;
        }
      } catch {
        // No local container or exec failed
      }

      if (found) return;

      // AKS fallback
      const namespace = `azureclaw-${name}`;
      const podLabel = `azureclaw.azure.com/sandbox=${name}`;
      try {
        await execa("kubectl", [
          "exec", "-it", "-n", namespace,
          "-l", podLabel,
          "--", "/bin/bash", "-c", launchCmd,
        ], { stdio: "inherit" });
      } catch {
        console.log(chalk.red(`\n  Sandbox '${name}' not found. Run ${chalk.cyan("azureclaw dev")} first.\n`));
      }
    });

  return cmd;
}
