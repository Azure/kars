// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import chalk from "chalk";
import ora from "ora";
import { loadContext } from "../config.js";
import {
  createAzureRunner,
  type AzureRunner,
} from "./up/orchestration.js";

interface AzureSubscription {
  id: string;
}

interface AzureAksCluster {
  name: string;
  resourceGroup: string;
}

function parseJsonArray(value: string, operation: string): unknown[] {
  try {
    const parsed = JSON.parse(value || "[]") as unknown;
    if (Array.isArray(parsed)) return parsed;
  } catch {
    // Use the actionable error below.
  }
  throw new Error(`${operation} returned invalid JSON`);
}

async function discoverDestroySubscription(
  execute: typeof import("execa").execa,
  resourceGroup: string,
  aksCluster: string,
  bestEffort = false,
): Promise<string> {
  const { stdout } = await execute("az", [
    "account", "list",
    "--query", "[?state=='Enabled'].{id:id}",
    "--output", "json",
  ], { stdio: "pipe", timeout: 15000 });
  const subscriptions = parseJsonArray(
    String(stdout),
    "Listing enabled Azure subscriptions",
  ).map((candidate): AzureSubscription => {
    if (
      typeof candidate !== "object" ||
      candidate === null ||
      typeof (candidate as { id?: unknown }).id !== "string" ||
      !(candidate as { id: string }).id.trim()
    ) {
      throw new Error("Azure returned an invalid enabled subscription list");
    }
    return { id: (candidate as { id: string }).id.trim() };
  });

  const matches = new Set<string>();
  for (const subscription of subscriptions) {
    const { stdout: clusterOutput } = await execute("az", [
      "aks", "list",
      "--query", "[].{name:name,resourceGroup:resourceGroup}",
      "--output", "json",
      "--subscription", subscription.id,
    ], { stdio: "pipe", timeout: 30000 });
    const clusters = parseJsonArray(
      String(clusterOutput),
      `Listing AKS clusters in subscription '${subscription.id}'`,
    );
    for (const candidate of clusters) {
      if (typeof candidate !== "object" || candidate === null) continue;
      const cluster = candidate as Partial<AzureAksCluster>;
      if (
        typeof cluster.name === "string" &&
        typeof cluster.resourceGroup === "string" &&
        cluster.name.toLowerCase() === aksCluster.toLowerCase() &&
        cluster.resourceGroup.toLowerCase() === resourceGroup.toLowerCase()
      ) {
        matches.add(subscription.id);
      }
    }
  }

  if (matches.size === 1) return [...matches][0];
  const unresolvedAction = bestEffort
    ? "Federated credential cleanup was skipped."
    : "Nothing was deleted.";
  if (matches.size === 0) {
    throw new Error(
      `No enabled Azure subscription contains AKS cluster '${aksCluster}' in resource group '${resourceGroup}'. ` +
      `${unresolvedAction} Verify the target or pass --subscription <id> explicitly.`,
    );
  }
  throw new Error(
    `AKS cluster '${aksCluster}' in resource group '${resourceGroup}' exists in multiple enabled Azure subscriptions ` +
    `(${[...matches].join(", ")}). ${unresolvedAction} Pass --subscription <id> explicitly.`,
  );
}

async function resolveFullDestroyAzureRunner(
  execute: typeof import("execa").execa,
  resourceGroup: string,
  explicitSubscription?: string,
): Promise<{ runAzure: AzureRunner; subscriptionId: string }> {
  const requestedSubscription = explicitSubscription?.trim();
  if (explicitSubscription !== undefined && !requestedSubscription) {
    throw new Error("--subscription requires a non-empty subscription ID");
  }

  const context = loadContext();
  const cachedSubscription = context?.subscription?.trim();
  let subscriptionId = requestedSubscription;
  if (
    !subscriptionId &&
    cachedSubscription &&
    context?.resourceGroup?.toLowerCase() === resourceGroup.toLowerCase()
  ) {
    subscriptionId = cachedSubscription;
  }
  if (!subscriptionId) {
    const cachedCluster = context?.resourceGroup?.toLowerCase() ===
        resourceGroup.toLowerCase()
      ? context.aksCluster?.trim()
      : undefined;
    subscriptionId = await discoverDestroySubscription(
      execute,
      resourceGroup,
      cachedCluster || "kars-aks",
    );
  }

  return {
    subscriptionId,
    runAzure: createAzureRunner(execute, subscriptionId),
  };
}

async function resolveBestEffortDestroyAzureRunner(
  execute: typeof import("execa").execa,
  resourceGroup: string,
  explicitSubscription?: string,
): Promise<AzureRunner | undefined> {
  try {
    const requestedSubscription = explicitSubscription?.trim();
    if (explicitSubscription !== undefined && !requestedSubscription) {
      throw new Error("--subscription requires a non-empty subscription ID");
    }

    const context = loadContext();
    const contextMatches = context?.resourceGroup?.toLowerCase() ===
      resourceGroup.toLowerCase();
    const cachedSubscription = contextMatches
      ? context.subscription?.trim()
      : undefined;
    const subscriptionId = requestedSubscription || cachedSubscription ||
      (contextMatches && context.aksCluster?.trim()
        ? await discoverDestroySubscription(
          execute,
          resourceGroup,
          context.aksCluster.trim(),
          true,
        )
        : undefined);
    if (!subscriptionId) {
      throw new Error(
        `No deployment subscription is cached for resource group '${resourceGroup}', ` +
        "and no matching cached AKS cluster is available for read-only discovery. " +
        "Pass --subscription <id> to enable federated credential cleanup.",
      );
    }
    return createAzureRunner(execute, subscriptionId);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.warn(chalk.yellow(
      "  ⚠ Unable to resolve a safe Azure subscription; skipping federated credential cleanup.",
    ));
    console.warn(chalk.dim(`    ${message}`));
    return undefined;
  }
}

export interface ResourceGroupLock {
  id: string;
  name: string;
}

function isKarsLockName(name: string): boolean {
  return name === "kars-up-adopted" || (
    name.startsWith("kars-up-lease-") &&
    name.length > "kars-up-lease-".length
  );
}

function resourceGroupLockIdPattern(resourceGroup: string): RegExp {
  const escaped = resourceGroup.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(
    `^/subscriptions/[^/]+/resourceGroups/${escaped}/providers/Microsoft\\.Authorization/locks/[^/]+$`,
    "i",
  );
}

/**
 * Select only Kars-owned, resource-group-scoped deployment locks.
 * Sorting makes removal deterministic and keeps the adopted-environment lock
 * after any run leases.
 */
export function selectKarsResourceGroupLocks(
  value: unknown,
  resourceGroup: string,
): ResourceGroupLock[] {
  if (!Array.isArray(value)) {
    throw new Error("Azure returned an invalid resource-group lock list");
  }

  const resourceGroupLockId = resourceGroupLockIdPattern(resourceGroup);
  const locks: ResourceGroupLock[] = [];
  for (const candidate of value) {
    if (typeof candidate !== "object" || candidate === null) continue;
    const record = candidate as Record<string, unknown>;
    if (typeof record.name !== "string" || !isKarsLockName(record.name)) continue;
    if (typeof record.id !== "string") {
      throw new Error(
        `Kars lock '${record.name}' is not a removable resource-group lock`,
      );
    }
    if (!resourceGroupLockId.test(record.id)) continue;
    locks.push({ id: record.id, name: record.name });
  }

  return locks.sort((a, b) => {
    const aAdopted = a.name === "kars-up-adopted";
    const bAdopted = b.name === "kars-up-adopted";
    if (aAdopted !== bAdopted) return aAdopted ? 1 : -1;
    return a.name.localeCompare(b.name) || a.id.localeCompare(b.id);
  });
}

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
    .option("--subscription <id>", "Azure subscription containing the deployment")
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
          const { runAzure } = await resolveFullDestroyAzureRunner(
            execa,
            rg,
            options.subscription,
          );

          spinner.text = `Checking Kars deployment locks on '${rg}'...`;
          const { stdout: lockOutput } = await runAzure([
            "lock", "list",
            "--resource-group", rg,
            "--output", "json",
          ]);
          const karsLocks = selectKarsResourceGroupLocks(
            JSON.parse(lockOutput || "[]"),
            rg,
          );
          for (const lock of karsLocks) {
            spinner.text = `Removing Kars deployment lock '${lock.name}'...`;
            await runAzure([
              "lock", "delete",
              "--ids", lock.id,
              "--output", "none",
            ]);
          }

          // Delete the resource group (async)
          spinner.text = `Deleting resource group '${rg}' and all resources...`;
          await runAzure([
            "group", "delete", "--name", rg, "--yes", "--no-wait", "--output", "none",
          ]);

          // Purge soft-deleted resources so a fresh 'up' works without conflicts
          spinner.text = "Purging soft-deleted Azure OpenAI account...";
          await runAzure([
            "cognitiveservices", "account", "purge",
            "--name", `${baseName}-aoai`,
            "--resource-group", rg,
            "--location", options.region,
            "--output", "none",
          ]).catch(() => {});

          spinner.text = "Purging soft-deleted Key Vault...";
          await runAzure([
            "keyvault", "purge", "--name", `${baseName}-kv`,
          ]).catch(() => {});

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
          const runAzure = await resolveBestEffortDestroyAzureRunner(
            execa,
            rg,
            options.subscription,
          );
          if (runAzure) {
            await runAzure([
              "identity", "federated-credential", "delete",
              "--identity-name", "kars-aks-sandbox-wi",
              "--resource-group", rg,
              "--name", `kars-${name}`,
              "--yes",
              "--output", "none",
            ]).catch(() => {});
          }

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
          const sandboxNamespaces = nsList.split(" ").filter(
            (ns) => ns.startsWith("kars-") && ns !== "kars-system",
          );
          for (const ns of sandboxNamespaces) {
            await execa("kubectl", [
              "delete", "ns", ns, "--ignore-not-found", "--wait=false",
            ], { stdio: "pipe" }).catch(() => {});
          }

          const runAzure = sandboxNamespaces.length > 0
            ? await resolveBestEffortDestroyAzureRunner(
              execa,
              rg,
              options.subscription,
            )
            : undefined;
          if (runAzure) {
            for (const ns of sandboxNamespaces) {
              const sandboxName = ns.replace("kars-", "");
              await runAzure([
                "identity", "federated-credential", "delete",
                "--identity-name", "kars-aks-sandbox-wi",
                "--resource-group", rg,
                "--name", `kars-${sandboxName}`,
                "--yes", "--output", "none",
              ]).catch(() => {});
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
