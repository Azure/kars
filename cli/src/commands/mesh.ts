// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// AzureClaw mesh CLI: identity/auth and registry management.
//
// S15.b decomposition: identity/crypto + OAuth callback + port-health
// helpers + the `mesh promote` subcommand body now live in ./mesh/*
// modules. mesh.ts orchestrates the remaining subcommands and re-exports
// the public surface tests already depend on.

import { Command } from "commander";
import chalk from "chalk";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execa } from "execa";
import { banner, section, kvLine, checkLine } from "../stepper.js";
import { loadContext, saveContext } from "../config.js";
import {
  IDENTITY_FILE,
  generateKeypair,
  base58Encode,
  encryptPrivateKey,
  decryptPrivateKey,
  loadIdentity,
  saveIdentity,
  type MeshIdentity,
} from "./mesh/identity.js";
import {
  checkRegistryHealth,
  checkRelayHealth,
  killProcessesOnPorts,
  killStaleListeners,
} from "./mesh/health.js";
import { attachAuthSubcommand } from "./mesh/auth.js";
import { attachPromoteSubcommand } from "./mesh/promote.js";
import { attachSetupTrustSubcommand } from "./mesh/setup-trust.js";
import { attachProviderSubcommand } from "./mesh/provider.js";

export function meshCommand(): Command {
  const cmd = new Command("mesh");
  cmd.description(
    "Manage AgentMesh identity and authentication for cross-environment handoff"
  );

  // -----------------------------------------------------------------------
  // mesh auth (S15.b: extracted to ./mesh/auth.ts)
  // -----------------------------------------------------------------------
  attachAuthSubcommand(cmd);

  // -----------------------------------------------------------------------
  // mesh setup-trust — provision the api://agentmesh Entra app reg
  // -----------------------------------------------------------------------
  attachSetupTrustSubcommand(cmd);

  // -----------------------------------------------------------------------
  // mesh provider — switch a live cluster between vendored ↔ AGT
  // -----------------------------------------------------------------------
  attachProviderSubcommand(cmd);

  // -----------------------------------------------------------------------
  // mesh status
  // -----------------------------------------------------------------------
  cmd
    .command("status")
    .description("Show current mesh identity")
    .action(async () => {
      banner("AzureClaw · Mesh Identity", "AgentMesh Identity Status");

      const identity = loadIdentity();
      if (!identity) {
        console.log(chalk.dim("  No mesh identity found."));
        console.log(
          chalk.dim(
            `  Run ${chalk.cyan("azureclaw mesh auth --registry <url>")} to create one.`
          )
        );
        return;
      }

      kvLine("AMID", identity.amid);
      kvLine("Public Key", identity.publicKey.substring(0, 20) + "...");
      kvLine("Created", identity.createdAt);

      if (identity.provider) {
        kvLine("Provider", identity.provider);
        if (identity.email) kvLine("Email", identity.email);
        if (identity.username) kvLine("Username", identity.username);
        if (identity.verifiedAt) kvLine("Verified", identity.verifiedAt);
      } else {
        console.log(chalk.yellow("  ⚠ Not verified (anonymous)"));
      }

      if (identity.registryUrl) {
        kvLine("Registry", identity.registryUrl);
      }

      console.log(
        chalk.dim(`\n  Identity file: ${IDENTITY_FILE}`)
      );
    });

  // -----------------------------------------------------------------------
  // mesh list — show cluster pairings and offload sandboxes
  // -----------------------------------------------------------------------
  cmd
    .command("list")
    .description("List mesh pairings and offload sandboxes on the cluster")
    .action(async () => {
      banner("AzureClaw · Mesh List", "Pairings & Offload Sandboxes");

      const ns = "azureclaw-system";

      // Pairings
      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawpairings", "-n", ns, "-o", "json",
        ], { stdio: "pipe" });
        const list = JSON.parse(stdout);
        const items = list.items as Array<{
          metadata: { name: string; creationTimestamp: string };
          spec: { tokenBudget?: number };
          status?: { phase?: string; boundAmid?: string; slotsUsed?: number; lastOffloadAt?: string };
        }>;

        section("Pairings");
        if (items.length === 0) {
          console.log(chalk.dim("  No pairings found."));
        } else {
          for (const p of items) {
            const phase = p.status?.phase || "Unknown";
            const amid = p.status?.boundAmid || "-";
            const budget = p.spec.tokenBudget ?? 0;
            const slots = p.status?.slotsUsed ?? 0;
            const icon = phase === "Active" ? "🟢" : phase === "PendingPairing" ? "🟡" : "⚪";
            console.log(`  ${icon} ${chalk.bold(p.metadata.name)}`);
            kvLine("  Phase", phase);
            kvLine("  AMID", amid);
            kvLine("  Budget", budget.toLocaleString() + " tokens");
            kvLine("  Offloads", String(slots));
            if (p.status?.lastOffloadAt) kvLine("  Last offload", p.status.lastOffloadAt);
            kvLine("  Age", p.metadata.creationTimestamp);
            console.log();
          }
        }
      } catch {
        console.log(chalk.red("  Failed to list pairings (is kubectl connected to the cluster?)"));
      }

      // Offload sandboxes
      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawsandboxes", "-n", ns, "-o", "json",
        ], { stdio: "pipe" });
        const list = JSON.parse(stdout);
        const items = list.items as Array<{
          metadata: { name: string; creationTimestamp: string; labels?: Record<string, string>; annotations?: Record<string, string> };
          status?: { phase?: string };
        }>;
        const offloads = items.filter((s) => s.metadata.labels?.["azureclaw.azure.com/spawned-by"] === "offload");

        section("Offload Sandboxes");
        if (offloads.length === 0) {
          console.log(chalk.dim("  No offload sandboxes."));
        } else {
          for (const s of offloads) {
            const phase = s.status?.phase || "Unknown";
            const requester = s.metadata.labels?.["azureclaw.azure.com/offload-requester"] || "-";
            const task = s.metadata.annotations?.["azureclaw.azure.com/offload-task"] || "-";
            const icon = phase === "Running" ? "🟢" : phase === "Pending" ? "🟡" : "🔴";
            console.log(`  ${icon} ${chalk.bold(s.metadata.name)}`);
            kvLine("  Phase", phase);
            kvLine("  Requester", requester);
            kvLine("  Task", task.length > 80 ? task.substring(0, 77) + "..." : task);
            kvLine("  Created", s.metadata.creationTimestamp);
            console.log();
          }
        }
      } catch {
        console.log(chalk.red("  Failed to list sandboxes"));
      }

      // Leader info
      try {
        const { stdout } = await execa("kubectl", [
          "get", "lease", "azureclaw-mesh-peer-leader", "-n", ns,
          "-o", "jsonpath={.spec.holderIdentity}",
        ], { stdio: "pipe" });
        if (stdout) {
          section("Mesh Peer");
          kvLine("Leader", stdout);
        }
      } catch { /* lease may not exist */ }
    });

  // -----------------------------------------------------------------------
  // mesh reset
  // -----------------------------------------------------------------------
  cmd
    .command("reset")
    .description("Delete mesh identity (requires re-authentication)")
    .action(async () => {
      if (!fs.existsSync(IDENTITY_FILE)) {
        console.log(chalk.dim("  No mesh identity to reset."));
        return;
      }

      const { default: inquirer } = await import("inquirer");
      const { confirm } = await inquirer.prompt([
        {
          type: "confirm",
          name: "confirm",
          message:
            "This will delete your mesh identity. You will need to re-authenticate. Continue?",
          default: false,
        },
      ]);

      if (confirm) {
        fs.unlinkSync(IDENTITY_FILE);
        checkLine(true, `Identity deleted: ${IDENTITY_FILE}`);
      } else {
        console.log(chalk.dim("  Cancelled."));
      }
    });

  // -----------------------------------------------------------------------
  // mesh security — toggle REQUIRE_REGISTRATION on the relay deployment
  //
  // Context: when REQUIRE_REGISTRATION=true, only agents that have been
  // registered with the mesh registry can connect to the relay. This is
  // the secure default. In transient states (registry DB wipe, fresh
  // cluster bootstrap) the controller and NemoClaw instances may need a
  // window to re-register. Use `mesh security open` temporarily to unblock.
  // -----------------------------------------------------------------------
  cmd
    .command("security <mode>")
    .description(
      "Toggle relay REQUIRE_REGISTRATION (mode: open | strict | status)"
    )
    .option("-n, --namespace <ns>", "AgentMesh namespace", "agentmesh")
    .option("--deployment <name>", "Relay deployment name", "relay")
    .action(async (mode: string, opts: { namespace: string; deployment: string }) => {
      banner("AzureClaw · Mesh Security", "Relay registration enforcement");

      const normalized = mode.toLowerCase();
      if (!["open", "strict", "status"].includes(normalized)) {
        console.error(chalk.red(`  ✘ Unknown mode "${mode}". Use: open, strict, or status.`));
        process.exit(1);
      }

      // Read current value
      let current = "(unknown)";
      try {
        const { stdout } = await execa(
          "kubectl",
          [
            "get",
            "deployment",
            opts.deployment,
            "-n",
            opts.namespace,
            "-o",
            "jsonpath={.spec.template.spec.containers[?(@.name==\"relay\")].env[?(@.name==\"REQUIRE_REGISTRATION\")].value}",
          ],
          { stdio: "pipe" }
        );
        current = stdout.trim() || "(unset, defaults to true)";
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Could not read relay deployment ${opts.namespace}/${opts.deployment}: ${
              e instanceof Error ? e.message : String(e)
            }`
          )
        );
        process.exit(1);
      }

      kvLine("Namespace", opts.namespace);
      kvLine("Deployment", opts.deployment);
      kvLine("Current REQUIRE_REGISTRATION", current);

      if (normalized === "status") {
        const isStrict = current === "true" || current.startsWith("(unset");
        console.log();
        console.log(
          isStrict
            ? chalk.green("  🔒 strict — only registered agents may connect")
            : chalk.yellow("  🔓 open — any signed agent may connect (insecure; use for bootstrap only)")
        );
        return;
      }

      const target = normalized === "strict" ? "true" : "false";
      if (current === target) {
        console.log(chalk.dim(`  Already ${normalized}. No change.`));
        return;
      }

      if (normalized === "open") {
        const { default: inquirer } = await import("inquirer");
        const { confirm } = await inquirer.prompt([
          {
            type: "confirm",
            name: "confirm",
            message:
              "Switching to OPEN mode lets any signed agent connect. This should only be used temporarily during bootstrap or recovery. Continue?",
            default: false,
          },
        ]);
        if (!confirm) {
          console.log(chalk.dim("  Cancelled."));
          return;
        }
      }

      section("Applying change");
      try {
        await execa(
          "kubectl",
          [
            "set",
            "env",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            `REQUIRE_REGISTRATION=${target}`,
          ],
          { stdio: "inherit" }
        );
        checkLine(true, `REQUIRE_REGISTRATION=${target}`);

        // Wait for rollout to complete so the setting is active before we return
        await execa(
          "kubectl",
          [
            "rollout",
            "status",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            "--timeout=120s",
          ],
          { stdio: "inherit" }
        );

        console.log();
        console.log(
          normalized === "open"
            ? chalk.yellow(
                "  ⚠️  Mode: OPEN — revert with `azureclaw mesh security strict` once bootstrap is complete."
              )
            : chalk.green(
                "  🔒 Mode: STRICT — only registered agents may connect."
              )
        );
        console.log(
          chalk.dim(
            "  Note: `azureclaw up` re-applies deploy/agentmesh.yaml which resets this to the manifest default."
          )
        );
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Failed to apply change: ${e instanceof Error ? e.message : String(e)}`
          )
        );
        process.exit(1);
      }
    });

  // -----------------------------------------------------------------------
  // mesh peer — toggle controller federation (MESH_PEER_ENABLED)
  // -----------------------------------------------------------------------
  cmd
    .command("peer <mode>")
    .description(
      "Toggle controller mesh federation (mode: enable | disable | status). When disabled, external agents cannot pair."
    )
    .option("-n, --namespace <ns>", "Controller namespace", "azureclaw-system")
    .option("--deployment <name>", "Controller deployment name", "azureclaw-controller")
    .action(async (mode: string, opts: { namespace: string; deployment: string }) => {
      banner("AzureClaw · Mesh Peer", "Controller federation (pair_request handler)");

      const normalized = mode.toLowerCase();
      if (!["enable", "disable", "status", "on", "off"].includes(normalized)) {
        console.error(chalk.red(`  ✘ Unknown mode "${mode}". Use: enable, disable, or status.`));
        process.exit(1);
      }
      const want = normalized === "status"
        ? "status"
        : (normalized === "enable" || normalized === "on" ? "enable" : "disable");

      // Read current value
      let current = "(unknown)";
      try {
        const { stdout } = await execa(
          "kubectl",
          [
            "get",
            "deployment",
            opts.deployment,
            "-n",
            opts.namespace,
            "-o",
            "jsonpath={.spec.template.spec.containers[0].env[?(@.name==\"MESH_PEER_ENABLED\")].value}",
          ],
          { stdio: "pipe" }
        );
        current = stdout.trim() || "(unset, defaults to true)";
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Could not read controller deployment ${opts.namespace}/${opts.deployment}: ${
              e instanceof Error ? e.message : String(e)
            }`
          )
        );
        process.exit(1);
      }

      kvLine("Namespace", opts.namespace);
      kvLine("Deployment", opts.deployment);
      kvLine("Current MESH_PEER_ENABLED", current);

      if (want === "status") {
        const isEnabled = current === "true" || current.startsWith("(unset");
        console.log();
        console.log(
          isEnabled
            ? chalk.green("  🔗 enabled — controller joins the relay and answers pair_request messages")
            : chalk.yellow("  🚫 disabled — external agent pairing will NOT work")
        );
        return;
      }

      const target = want === "enable" ? "true" : "false";
      if (current === target) {
        console.log(chalk.dim(`  Already ${want}d. No change.`));
        return;
      }

      section("Applying change");
      try {
        await execa(
          "kubectl",
          [
            "set",
            "env",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            `MESH_PEER_ENABLED=${target}`,
          ],
          { stdio: "inherit" }
        );
        checkLine(true, `MESH_PEER_ENABLED=${target}`);

        // Wait for rollout so the setting is active before we return
        await execa(
          "kubectl",
          [
            "rollout",
            "status",
            `deployment/${opts.deployment}`,
            "-n",
            opts.namespace,
            "--timeout=120s",
          ],
          { stdio: "inherit" }
        );

        console.log();
        console.log(
          want === "enable"
            ? chalk.green(
                "  🔗 Mesh peer ENABLED — controller will join the relay shortly and pair_request messages will be answered."
              )
            : chalk.yellow(
                "  🚫 Mesh peer DISABLED — external agent pairing will not work. Re-enable with `azureclaw mesh peer enable`."
              )
        );
        console.log(
          chalk.dim(
            "  Note: `azureclaw up` re-applies Helm values (default: enabled). Pass --no-mesh-peer to keep it disabled."
          )
        );
      } catch (e: unknown) {
        console.error(
          chalk.red(
            `  ✘ Failed to apply change: ${e instanceof Error ? e.message : String(e)}`
          )
        );
        process.exit(1);
      }
    });

  // -----------------------------------------------------------------------
  // mesh unpair — delete cluster pairings
  // -----------------------------------------------------------------------
  cmd
    .command("unpair")
    .description("Delete mesh pairings from the AKS cluster")
    .option("--all", "Delete all pairings without prompting")
    .option("--name <name>", "Delete a specific pairing by name")
    .action(async (opts: { all?: boolean; name?: string }) => {
      banner("AzureClaw · Mesh Unpair", "Remove Pairings");

      const ns = "azureclaw-system";
      try {
        const { stdout } = await execa("kubectl", [
          "get", "clawpairings", "-n", ns,
          "-o", "json",
        ], { stdio: "pipe" });
        const list = JSON.parse(stdout);
        const items = list.items as Array<{ metadata: { name: string }; status?: { phase?: string; boundAmid?: string } }>;

        if (items.length === 0) {
          console.log(chalk.dim("  No pairings found."));
          return;
        }

        if (opts.name) {
          const match = items.find((p) => p.metadata.name === opts.name);
          if (!match) {
            console.log(chalk.red(`  Pairing "${opts.name}" not found.`));
            console.log(chalk.dim(`  Available: ${items.map((p) => p.metadata.name).join(", ")}`));
            return;
          }
          await execa("kubectl", ["delete", "clawpairing", opts.name, "-n", ns], { stdio: "pipe" });
          checkLine(true, `Deleted pairing: ${opts.name}`);
          return;
        }

        // Show pairings
        console.log();
        for (const p of items) {
          const phase = p.status?.phase || "Unknown";
          const amid = p.status?.boundAmid || "-";
          const icon = phase === "Active" ? "🟢" : phase === "PendingPairing" ? "🟡" : "⚪";
          console.log(`  ${icon} ${chalk.bold(p.metadata.name)}  ${chalk.dim(phase)}  ${chalk.dim(amid)}`);
        }
        console.log();

        if (opts.all) {
          await execa("kubectl", ["delete", "clawpairings", "--all", "-n", ns], { stdio: "pipe" });
          checkLine(true, `Deleted all ${items.length} pairing(s)`);
          return;
        }

        const { default: inquirer } = await import("inquirer");
        const { targets } = await inquirer.prompt([
          {
            type: "checkbox",
            name: "targets",
            message: "Select pairings to delete:",
            choices: items.map((p) => ({
              name: `${p.metadata.name} (${p.status?.phase || "Unknown"})`,
              value: p.metadata.name,
            })),
          },
        ]);

        if (targets.length === 0) {
          console.log(chalk.dim("  No pairings selected."));
          return;
        }

        for (const name of targets) {
          await execa("kubectl", ["delete", "clawpairing", name, "-n", ns], { stdio: "pipe" });
          checkLine(true, `Deleted: ${name}`);
        }
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        console.log(chalk.red(`  Failed to manage pairings: ${msg}`));
      }
    });

  // -----------------------------------------------------------------------
  // mesh promote — expose cluster registry as a global endpoint (S15.b)
  // -----------------------------------------------------------------------
  attachPromoteSubcommand(cmd);

  // -----------------------------------------------------------------------
  // mesh demote — revert to cluster-local registry
  // -----------------------------------------------------------------------
  cmd
    .command("demote")
    .description("Demote the registry back to cluster-local (remove public endpoints)")
    .action(async () => {
      banner("AzureClaw · Mesh Demote", "Demote Registry to Local");

      const ctx = loadContext();
      if (!ctx?.aksCluster || !ctx?.resourceGroup) {
        console.error(chalk.red("  ✘ No deployment context found."));
        process.exit(1);
      }

      if (ctx.registryMode !== "global") {
        console.log(chalk.yellow("  ⚠ Registry is already local."));
        return;
      }

      if (ctx.promoteMode === "port-forward") {
        // Kill port-forward processes
        section("Stopping Port-Forward Tunnels");
        const pidFile = path.join(os.homedir(), ".azureclaw", "port-forward-pids.json");
        if (fs.existsSync(pidFile)) {
          try {
            const pids = JSON.parse(fs.readFileSync(pidFile, "utf-8")) as Record<string, number>;
            for (const [label, pid] of Object.entries(pids)) {
              try {
                process.kill(pid, "SIGTERM");
                checkLine(true, `${label} tunnel stopped (PID ${pid})`);
              } catch {
                console.log(chalk.dim(`  · ${label} tunnel already stopped (PID ${pid})`));
              }
            }
            fs.unlinkSync(pidFile);
          } catch {
            console.log(chalk.yellow("  ⚠ Could not read PID file"));
          }
        } else {
          console.log(chalk.dim("  · No PID file found (tunnels may have exited)"));
        }
      } else {
        // LoadBalancer mode: revert services to ClusterIP
        section("Reverting Services to ClusterIP");
        const services = ["agentmesh-registry", "agentmesh-relay"];
        for (const svc of services) {
          try {
            await execa("kubectl", [
              "patch", "svc", svc, "-n", "agentmesh",
              "--type", "merge",
              "-p", JSON.stringify({ spec: { type: "ClusterIP", loadBalancerSourceRanges: null } }),
            ], { stdio: "pipe" });
            checkLine(true, `${svc} → ClusterIP`);
          } catch {
            console.log(chalk.yellow(`  ⚠ Could not revert ${svc}`));
          }
        }

        // Clean up any leftover Ingress resources from earlier attempts
        const ingressResources = [
          "ingress/agentmesh-registry-ingress",
          "ingress/agentmesh-relay-ingress",
        ];
        for (const resource of ingressResources) {
          try {
            await execa("kubectl", [
              "delete", resource, "-n", "agentmesh", "--ignore-not-found",
            ], { stdio: "pipe" });
          } catch { /* ignore */ }
        }
      }

      // Update deployment context
      ctx.registryMode = "local";
      ctx.globalRegistryUrl = undefined;
      ctx.globalRelayUrl = undefined;
      delete ctx.promoteMode;
      saveContext(ctx);

      section("Status");
      kvLine("Registry mode", "local (cluster-only)");

      console.log();
      console.log(chalk.green("  ✓ ") + chalk.bold("Registry demoted to local."));
      console.log(chalk.dim("    Public endpoints removed. Agents in this cluster still work."));
      console.log(chalk.dim("    Cross-environment handoff is no longer available."));
      console.log();
    });

  return cmd;
}

// Exported for testing — re-export the public surface from sub-modules
// so external consumers (tests, downstream code) can keep importing
// from ./mesh.js after the S15.b decomposition.
export { generateKeypair, base58Encode, encryptPrivateKey, decryptPrivateKey, checkRegistryHealth, checkRelayHealth, killProcessesOnPorts, killStaleListeners };
export type { MeshIdentity };
