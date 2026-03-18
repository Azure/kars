import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function destroyCommand(): Command {
  const cmd = new Command("destroy");

  cmd
    .description("Teardown sandbox(es) or the entire AzureClaw deployment")
    .argument("[name]", "Sandbox name (omit to destroy all sandboxes)")
    .option("-y, --yes", "Skip confirmation prompt", false)
    .option("--all", "Destroy ALL resources (AKS, ACR, KV, AOAI — deletes the resource group)", false)
    .option("-g, --resource-group <name>", "Resource group name")
    .option("--region <region>", "Azure region (used to derive resource group)", "eastus2")
    .action(async (name: string | undefined, options) => {
      const rg = options.resourceGroup || `azureclaw-${options.region}`;

      if (options.all) {
        // Full teardown — delete the entire resource group
        if (!options.yes) {
          console.log(
            chalk.red(
              `\n⚠️  This will PERMANENTLY DELETE the resource group '${rg}' and ALL resources inside it:`
            )
          );
          console.log(chalk.dim(`     AKS cluster, ACR, Key Vault, Azure OpenAI, Monitor, all sandboxes\n`));
          console.log(chalk.yellow(`  Run with --yes to confirm.\n`));
          return;
        }

        const spinner = ora(`Deleting resource group '${rg}' and all resources...`).start();
        try {
          const { execa } = await import("execa");
          await execa("az", [
            "group", "delete", "--name", rg, "--yes", "--no-wait", "--output", "none",
          ], { stdio: "pipe" });
          spinner.succeed(`Resource group '${rg}' deletion initiated (async — takes a few minutes)`);
        } catch (error) {
          spinner.fail("Failed to delete resource group");
          const message = error instanceof Error ? error.message : String(error);
          console.error(chalk.red(`\nError: ${message}\n`));
          process.exit(1);
        }
        return;
      }

      // Single sandbox or all sandboxes
      const spinner = ora().start();
      try {
        const { execa } = await import("execa");

        if (name) {
          // Destroy a single sandbox
          if (!options.yes) {
            console.log(
              chalk.yellow(
                `\n⚠️  This will destroy sandbox '${name}' and its namespace.\n`
              )
            );
            console.log(chalk.dim(`  Run with --yes to confirm.\n`));
            return;
          }

          spinner.text = `Destroying sandbox '${name}'...`;
          const sandboxNs = `azureclaw-${name}`;

          // Delete the CR (controller will clean up the namespace)
          await execa("kubectl", [
            "delete", "clawsandbox", name,
            "-n", "azureclaw-system",
            "--ignore-not-found",
          ], { stdio: "pipe" });

          // Delete the namespace directly (in case controller doesn't handle finalizers)
          await execa("kubectl", [
            "delete", "ns", sandboxNs,
            "--ignore-not-found", "--wait=false",
          ], { stdio: "pipe" }).catch(() => {});

          // Remove the federated identity credential
          await execa("az", [
            "identity", "federated-credential", "delete",
            "--identity-name", "azureclaw-aks-sandbox-wi",
            "--resource-group", rg,
            "--name", `azureclaw-${name}`,
            "--yes",
            "--output", "none",
          ], { stdio: "pipe" }).catch(() => {});

          spinner.succeed(`Sandbox '${name}' destroyed`);
        } else {
          // Destroy all sandboxes
          if (!options.yes) {
            console.log(
              chalk.yellow(`\n⚠️  This will destroy ALL sandboxes in the cluster.\n`)
            );
            console.log(chalk.dim(`  Run with --yes to confirm.\n`));
            return;
          }

          spinner.text = "Destroying all sandboxes...";
          await execa("kubectl", [
            "delete", "clawsandbox", "--all",
            "-n", "azureclaw-system",
            "--ignore-not-found",
          ], { stdio: "pipe" });

          // Clean up sandbox namespaces
          const { stdout: nsList } = await execa("kubectl", [
            "get", "ns", "-o", "jsonpath={.items[*].metadata.name}",
          ], { stdio: "pipe" });
          for (const ns of nsList.split(" ")) {
            if (ns.startsWith("azureclaw-") && ns !== "azureclaw-system") {
              await execa("kubectl", [
                "delete", "ns", ns, "--ignore-not-found", "--wait=false",
              ], { stdio: "pipe" }).catch(() => {});
            }
          }

          spinner.succeed("All sandboxes destroyed");
        }
      } catch (error) {
        spinner.fail("Destroy failed");
        const message = error instanceof Error ? error.message : String(error);
        console.error(chalk.red(`\nError: ${message}\n`));
        process.exit(1);
      }
    });

  return cmd;
}
