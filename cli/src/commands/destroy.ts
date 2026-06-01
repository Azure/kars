// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";

export function destroyCommand(): Command {
  const cmd = new Command("destroy");

  cmd
    .description("Teardown sandbox(es) or the entire kars deployment")
    .argument("[name]", "Sandbox name (omit to destroy all sandboxes)")
    .option("-y, --yes", "Skip confirmation prompt", false)
    .option("--local", "Destroy local Docker sandbox only (skip AKS)", false)
    .option("--cloud", "Destroy AKS cloud sandbox only (skip Docker)", false)
    .option("--all", "Destroy ALL resources (AKS, ACR, KV, AOAI — deletes the resource group)", false)
    .option("-g, --resource-group <name>", "Resource group name")
    .option("--region <region>", "Azure region (used to derive resource group)", "eastus2")
    .option("--context <name>", "Kubernetes context to use (defaults to current)")
    .action(async (name: string | undefined, options) => {
      const rg = options.resourceGroup || `kars-${options.region}`;
      // Propagate --context to every kubectl invocation in this command.
      const kctlCtx = options.context ? ["--context", options.context] : [];

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
          const baseName = "kars";

          // Delete the resource group (async)
          await execa("az", [
            "group", "delete", "--name", rg, "--yes", "--no-wait", "--output", "none",
          ], { stdio: "pipe" });

          // Purge soft-deleted resources so a fresh 'up' works without conflicts
          spinner.text = "Purging soft-deleted Azure OpenAI account...";
          await execa("az", [
            "cognitiveservices", "account", "purge",
            "--name", `${baseName}-aoai`,
            "--resource-group", rg,
            "--location", options.region,
            "--output", "none",
          ], { stdio: "pipe" }).catch(() => {});

          spinner.text = "Purging soft-deleted Key Vault...";
          await execa("az", [
            "keyvault", "purge", "--name", `${baseName}-kv`,
          ], { stdio: "pipe" }).catch(() => {});

          spinner.succeed(`Resource group '${rg}' deletion initiated + soft-deleted resources purged`);
        } catch (error) {
          spinner.fail("Failed to delete resource group");
          const message = error instanceof Error ? error.message : String(error);
          console.error(chalk.red(`\nError: ${message}\n`));
          process.exit(1);
        }
        return;
      }

      // ── Local Docker sandbox ───────────────────────────────────
      if (name) {
        const { execa } = await import("execa");
        const containerName = `kars-${name}`;

        // Detect where the agent exists
        let localExists = false;
        let aksExists = false;

        if (!options.cloud) {
          try {
            await execa("docker", ["inspect", containerName], { stdio: "pipe" });
            localExists = true;
          } catch { /* no local container */ }
        }

        if (!options.local) {
          try {
            await execa("kubectl", [
              ...kctlCtx,
              "get", "karssandbox", name, "-n", "kars-system", "--no-headers",
            ], { stdio: "pipe" });
            aksExists = true;
          } catch { /* no AKS sandbox */ }
        }

        // Ambiguity: both exist, no explicit flag
        if (localExists && aksExists && !options.local && !options.cloud) {
          console.log(chalk.yellow(`\n  ⚠️  '${name}' exists in both Docker and AKS.`));
          console.log();
          console.log(`  ${chalk.cyan(`kars destroy ${name} --local`)}   → destroy Docker container`);
          console.log(`  ${chalk.cyan(`kars destroy ${name} --cloud`)}   → destroy AKS sandbox`);
          console.log(`  ${chalk.cyan(`kars destroy ${name} --local --cloud`)}   → destroy both`);
          console.log();
          return;
        }

        // Destroy local if requested or if it's the only one
        if (localExists && (options.local || !aksExists)) {
          const spinner = ora(`Destroying local sandbox '${name}'...`).start();
          try {
            await execa("docker", ["rm", "-f", containerName], { stdio: "pipe" });
            // Clean up volume
            await execa("docker", ["volume", "rm", `${containerName}-data`], { stdio: "pipe" }).catch(() => {});

            // Check if any other kars sandbox containers are still running
            const { stdout: ps } = await execa("docker", [
              "ps", "--filter", "name=kars-", "--format", "{{.Names}}",
            ], { stdio: "pipe" });
            const remaining = ps.split("\n").filter(n =>
              n.startsWith("kars-") &&
              !n.startsWith("kars-agt-")
            );

            if (remaining.length === 0) {
              // Last sandbox — tear down AGT infrastructure
              spinner.text = "Stopping AGT infrastructure...";
              for (const c of ["kars-agt-registry", "kars-agt-relay", "kars-agt-postgres"]) {
                // -v removes anonymous volumes attached to the container (e.g. postgres data)
                await execa("docker", ["rm", "-fv", c], { stdio: "pipe" }).catch(() => {});
              }
              await execa("docker", ["network", "rm", "kars-dev"], { stdio: "pipe" }).catch(() => {});
              // Clean up any remaining sub-agent containers and their volumes
              const { stdout: allCs } = await execa("docker", [
                "ps", "-a", "--filter", "name=kars-", "--format", "{{.Names}}",
              ], { stdio: "pipe" }).catch(() => ({ stdout: "" }));
              for (const c of allCs.split("\n").filter(Boolean)) {
                await execa("docker", ["rm", "-fv", c], { stdio: "pipe" }).catch(() => {});
                await execa("docker", ["volume", "rm", `${c}-data`], { stdio: "pipe" }).catch(() => {});
              }
              // Prune dangling volumes left by previous postgres/sub-agent containers
              await execa("docker", ["volume", "ls", "-q", "--filter", "dangling=true"], { stdio: "pipe" })
                .then(async ({ stdout }) => {
                  for (const v of stdout.split("\n").filter(Boolean)) {
                    await execa("docker", ["volume", "rm", v], { stdio: "pipe" }).catch(() => {});
                  }
                }).catch(() => {});
            }

            spinner.succeed(`Local sandbox '${name}' destroyed`);
          } catch (error) {
            spinner.fail("Destroy failed");
            const message = error instanceof Error ? error.message : String(error);
            console.error(chalk.red(`\nError: ${message}\n`));
            process.exit(1);
          }
          // If also destroying cloud, continue; otherwise done
          if (!aksExists || (!options.cloud && !options.local)) return;
        }

        // Nothing found locally and no AKS either
        if (!localExists && !aksExists) {
          console.log(chalk.red(`\n  Sandbox '${name}' not found.\n`));
          return;
        }
      }

      // ── AKS sandbox (kubectl) ──────────────────────────────────
      const spinner = ora().start();
      try {
        const { execa } = await import("execa");

        // Resolve + announce which cluster we're targeting. With both
        // a kind cluster (local-k8s) and an AKS cluster (--cloud) in
        // ~/.kube/config it's very easy to delete from the wrong one.
        // Print the active context up-front so a `^C` is possible.
        let activeContext = options.context || "";
        if (!activeContext) {
          try {
            const { stdout } = await execa("kubectl", [
              "config", "current-context",
            ], { stdio: "pipe" });
            activeContext = stdout.trim();
          } catch {
            activeContext = "(none — kubectl will fail)";
          }
        }
        spinner.stop();
        console.log(chalk.cyan(`  → targeting cluster context: ${chalk.bold(activeContext)}`));
        if (!options.yes && !options.context) {
          console.log(chalk.dim("    pass --context <name> to override"));
        }
        spinner.start();

        if (name) {
          // Destroy a single sandbox
          if (!options.yes) {
            console.log(
              chalk.yellow(
                `\n⚠️  This will destroy sandbox '${name}' on cluster '${activeContext}' and its namespace.\n`
              )
            );
            console.log(chalk.dim(`  Run with --yes to confirm.\n`));
            return;
          }

          spinner.text = `Destroying sandbox '${name}' on '${activeContext}'...`;
          const sandboxNs = `kars-${name}`;

          // Delete the CR (controller will clean up the namespace)
          await execa("kubectl", [
            ...kctlCtx,
            "delete", "karssandbox", name,
            "-n", "kars-system",
            "--ignore-not-found",
          ], { stdio: "pipe" });

          // Delete the namespace directly (in case controller doesn't handle finalizers)
          await execa("kubectl", [
            ...kctlCtx,
            "delete", "ns", sandboxNs,
            "--ignore-not-found", "--wait=false",
          ], { stdio: "pipe" }).catch(() => {});

          // Remove the federated identity credential
          await execa("az", [
            "identity", "federated-credential", "delete",
            "--identity-name", "kars-aks-sandbox-wi",
            "--resource-group", rg,
            "--name", `kars-${name}`,
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
            ...kctlCtx,
            "delete", "karssandbox", "--all",
            "-n", "kars-system",
            "--ignore-not-found",
          ], { stdio: "pipe" });

          // Clean up sandbox namespaces and federated credentials
          const { stdout: nsList } = await execa("kubectl", [
            "get", "ns", "-o", "jsonpath={.items[*].metadata.name}",
          ], { stdio: "pipe" });
          for (const ns of nsList.split(" ")) {
            if (ns.startsWith("kars-") && ns !== "kars-system") {
              // Delete federated credential for this sandbox
              const sandboxName = ns.replace("kars-", "");
              await execa("az", [
                "identity", "federated-credential", "delete",
                "--identity-name", "kars-aks-sandbox-wi",
                "--resource-group", rg,
                "--name", `kars-${sandboxName}`,
                "--yes", "--output", "none",
              ], { stdio: "pipe" }).catch(() => {});

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
