import { Command } from "commander";
import chalk from "chalk";

export function approveCommand(): Command {
  const cmd = new Command("approve");

  cmd
    .description("Review and approve/deny pending network egress requests from sandboxes")
    .argument("[name]", "Sandbox name (omit to show all pending)")
    .option("--approve <id>", "Approve a specific pending request by ID")
    .option("--deny <id>", "Deny a specific pending request by ID")
    .option("--list", "List all pending requests", false)
    .action(async (name: string | undefined, options) => {
      const { execa } = await import("execa");

      if (options.approve) {
        // Approve a pending request — add to NetworkPolicy allowlist
        const id = options.approve;
        try {
          const { stdout } = await execa("kubectl", [
            "get", "configmap", `pending-egress-${id}`,
            "-n", "azureclaw-system",
            "-o", "jsonpath={.data}",
          ], { stdio: "pipe" });

          const data = JSON.parse(stdout || "{}");
          console.log(chalk.green(`\n  Approved: ${data.host || id}`));
          console.log(chalk.dim(`  Sandbox: ${data.sandbox || "unknown"}`));
          console.log(chalk.dim(`  The NetworkPolicy will be updated to allow egress to this endpoint.\n`));

          // Mark as approved
          await execa("kubectl", [
            "annotate", "configmap", `pending-egress-${id}`,
            "-n", "azureclaw-system",
            "azureclaw.azure.com/approval=approved",
            "--overwrite",
          ], { stdio: "pipe" });
        } catch {
          console.log(chalk.red(`\n  Request '${id}' not found.\n`));
        }
        return;
      }

      if (options.deny) {
        const id = options.deny;
        try {
          await execa("kubectl", [
            "delete", "configmap", `pending-egress-${id}`,
            "-n", "azureclaw-system",
          ], { stdio: "pipe" });
          console.log(chalk.yellow(`\n  Denied and removed: ${id}\n`));
        } catch {
          console.log(chalk.red(`\n  Request '${id}' not found.\n`));
        }
        return;
      }

      // List pending requests
      console.log(chalk.hex("#0078D4")(`\n  Pending Egress Requests\n`));

      try {
        const { stdout } = await execa("kubectl", [
          "get", "configmap",
          "-n", "azureclaw-system",
          "-l", "azureclaw.azure.com/type=pending-egress",
          "-o", "json",
        ], { stdio: "pipe" });

        const result = JSON.parse(stdout);
        const items = result.items || [];

        if (items.length === 0) {
          console.log(chalk.dim("  No pending requests.\n"));
          return;
        }

        for (const item of items) {
          const data = item.data || {};
          const approved = item.metadata?.annotations?.["azureclaw.azure.com/approval"];
          const status = approved === "approved"
            ? chalk.green("approved")
            : chalk.yellow("pending");

          const id = item.metadata.name.replace("pending-egress-", "");
          console.log(`  ${chalk.bold(id)}  ${status}`);
          console.log(chalk.dim(`    Sandbox: ${data.sandbox || "?"}`));
          console.log(chalk.dim(`    Host:    ${data.host || "?"}`));
          console.log(chalk.dim(`    Port:    ${data.port || "443"}`));
          console.log(chalk.dim(`    Time:    ${data.timestamp || "?"}`));
          console.log();
        }

        console.log(chalk.dim(`  Approve:  azureclaw approve --approve <id>`));
        console.log(chalk.dim(`  Deny:     azureclaw approve --deny <id>\n`));
      } catch {
        console.log(chalk.dim("  Could not list pending requests. Is the cluster accessible?\n"));
      }
    });

  return cmd;
}
