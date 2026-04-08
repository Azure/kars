import { Command } from "commander";
import chalk from "chalk";

export function traceCommand(): Command {
  const cmd = new Command("trace");

  cmd
    .description(
      "Live eBPF trace — see network calls, file access, and process execution in real time"
    )
    .argument("<name>", "Sandbox name")
    .option("--network", "Show network connections only", false)
    .option("--files", "Show file operations only", false)
    .option("--exec", "Show process executions only", false)
    .option("--dns", "Show DNS lookups only", false)
    .action(async (name: string, options) => {
      const { execa } = await import("execa");
      const namespace = `azureclaw-${name}`;

      // Check kubectl-gadget is installed
      try {
        await execa("kubectl", ["gadget", "version"], { stdio: "pipe" });
      } catch {
        console.log(chalk.red("\n  kubectl-gadget not found."));
        console.log(chalk.dim("  Install: kubectl krew install gadget"));
        console.log(chalk.dim("  Or: brew install inspektor-gadget\n"));
        return;
      }

      const gadget = options.network ? "trace_tcp" :
        options.files ? "trace_open" :
        options.dns ? "trace_dns" : "trace_exec";

      console.log(chalk.hex("#0078D4")(
        `\n  Tracing ${chalk.bold(gadget)} in sandbox ${chalk.bold(name)}...\n`
      ));
      console.log(chalk.dim(`  Namespace: ${namespace}`));
      console.log(chalk.dim(`  Press Ctrl+C to stop.\n`));

      try {
        await execa("kubectl", [
          "gadget", "run", `${gadget}:latest`,
          "-n", namespace,
        ], { stdio: "inherit" });
      } catch {
        console.log(chalk.dim("\n  Trace stopped.\n"));
      }
    });

  return cmd;
}
