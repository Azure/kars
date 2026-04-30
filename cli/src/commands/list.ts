import { Command } from "commander";
import chalk from "chalk";

export function listCommand(): Command {
  const cmd = new Command("list");

  cmd
    .description("List all AzureClaw sandboxes (Docker + AKS)")
    .option("--aks-only", "Only show AKS sandboxes")
    .option("--docker-only", "Only show local Docker sandboxes")
    .action(async (options) => {
      const { execa } = await import("execa");
      const blue = chalk.hex("#0078D4");
      let found = false;

      console.log(blue(`\n  AzureClaw · Sandbox Inventory\n`));

      // ── Local Docker sandboxes ──
      if (!options.aksOnly) {
        try {
          const { stdout } = await execa("docker", [
            "ps", "-a",
            "--filter", "name=azureclaw-",
            "--format", "{{.Names}}\t{{.Status}}\t{{.Image}}\t{{.Ports}}",
          ], { stdio: "pipe" });

          if (stdout.trim()) {
            console.log(chalk.bold("  Local (Docker)"));
            console.log(chalk.dim("  ─────────────────────────────────────────────────────────────────"));
            for (const line of stdout.trim().split("\n")) {
              const [name, status, image] = line.split("\t");
              const agentName = name.replace(/^azureclaw-/, "");
              const isUp = status.toLowerCase().startsWith("up");
              const icon = isUp ? chalk.green("●") : chalk.red("●");
              const shortStatus = status.replace(/ \(.*\)/, "");
              console.log(`  ${icon} ${chalk.bold(agentName.padEnd(24))} ${shortStatus.padEnd(20)} ${chalk.dim(image || "")}`);
              found = true;
            }
            console.log();
          }
        } catch { /* docker not available */ }
      }

      // ── AKS sandboxes (ClawSandbox CRDs) ──
      if (!options.dockerOnly) {
        try {
          const { stdout } = await execa("kubectl", [
            "get", "clawsandbox",
            "-n", "azureclaw-system",
            "-o", "json",
          ], { stdio: "pipe" });

          const list = JSON.parse(stdout);
          const items = list.items || [];

          if (items.length > 0) {
            console.log(chalk.bold("  AKS Cluster"));
            console.log(chalk.dim("  ─────────────────────────────────────────────────────────────────"));
            console.log(chalk.dim(`  ${"NAME".padEnd(22)} ${"STATUS".padEnd(12)} ${"RUNTIME".padEnd(22)} ${"MODEL".padEnd(14)} ${"ISO".padEnd(10)} NS`));

            for (const sb of items) {
              const name = sb.metadata?.name || "unknown";
              const phase = sb.status?.phase || "Unknown";
              const model = sb.spec?.inference?.model || "gpt-4.1";
              const isolation = sb.spec?.sandbox?.isolation || "enhanced";
              const runtimeKind = sb.spec?.runtime?.kind || "OpenClaw";
              const ns = `azureclaw-${name}`;

              let icon: string;
              if (phase === "Running") icon = chalk.green("●");
              else if (phase === "Pending" || phase === "Creating") icon = chalk.yellow("●");
              else icon = chalk.red("●");

              console.log(`  ${icon} ${chalk.bold(name.padEnd(22))} ${phase.padEnd(12)} ${runtimeKind.padEnd(22)} ${model.padEnd(14)} ${isolation.padEnd(10)} ${chalk.dim(ns)}`);
              found = true;
            }
            console.log();
          }
        } catch {
          if (!options.aksOnly) {
            // AKS not configured — only show if explicitly requested
          } else {
            console.log(chalk.dim("  No AKS cluster configured (kubectl not connected)\n"));
          }
        }
      }

      if (!found) {
        console.log(chalk.dim("  No sandboxes found.\n"));
        console.log(chalk.dim("  Create one with: azureclaw dev (local) or azureclaw add <name> (AKS)\n"));
      }
    });

  return cmd;
}
