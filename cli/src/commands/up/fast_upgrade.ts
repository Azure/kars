// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15.d.1: `kars up --upgrade` fast-path extracted
// from up.ts. Skips all prompts and infra; just re-runs Helm with
// cached context. Caller invokes when `options.upgrade` is set and
// returns immediately afterwards.

import * as path from "node:path";
import * as fs from "node:fs";
import chalk from "chalk";
import ora from "ora";
import { execa } from "execa";
import { loadContext } from "../../config.js";

export interface UpOptionsForUpgrade {
  upgrade?: boolean;
  registrationMode?: string;
  acrName?: string;
  [key: string]: unknown;
}

const blue = chalk.hex("#0078D4");

export async function runFastUpgrade(options: UpOptionsForUpgrade): Promise<void> {
        const ctx = loadContext();
        if (!ctx?.acrLoginServer || !ctx?.aksCluster || !ctx?.resourceGroup) {
          console.error(chalk.red("\n  No cached deployment context. Run 'kars up' first (without --upgrade).\n"));
          process.exit(1);
        }

        console.log(blue("\n  Kars · Fast Upgrade\n"));

        // Connect to AKS
        let spin = ora("Connecting to AKS...").start();
        await execa("az", ["aks", "get-credentials", "--name", ctx.aksCluster, "--resource-group", ctx.resourceGroup, "--overwrite-existing"], { stdio: "pipe" });
        spin.succeed("AKS connected");

        // Find Helm chart — try cwd, then walk up, then try relative to CLI source
        let repoRoot = process.cwd();
        for (let i = 0; i < 5; i++) {
          if (fs.existsSync(path.join(repoRoot, "deploy", "helm"))) break;
          repoRoot = path.dirname(repoRoot);
        }
        if (!fs.existsSync(path.join(repoRoot, "deploy", "helm"))) {
          // Try relative to the CLI package itself
          const cliDir = new URL("../../..", import.meta.url).pathname;
          repoRoot = cliDir;
          for (let i = 0; i < 3; i++) {
            if (fs.existsSync(path.join(repoRoot, "deploy", "helm"))) break;
            repoRoot = path.dirname(repoRoot);
          }
        }
        if (!fs.existsSync(path.join(repoRoot, "deploy", "helm"))) {
          console.error(chalk.red("\n  Helm chart not found. Run from the Kars repo directory.\n"));
          process.exit(1);
        }
        const helmPath = path.join(repoRoot, "deploy", "helm", "kars");

        // Build Helm args from cached context
        const openAiEndpoint = ctx.foundryEndpoint || "";
        const helmArgs = [
          "upgrade", "--install", "kars", helmPath,
          "--namespace", "kars-system",
          "--create-namespace",
          "--set", `controller.image.repository=${ctx.acrLoginServer}/kars-controller`,
          "--set", `controller.image.tag=latest`,
          "--set", `inferenceRouter.image.repository=${ctx.acrLoginServer}/kars-inference-router`,
          "--set", `inferenceRouter.image.tag=latest`,
          "--set", `inferenceRouter.azure.openai.endpoint=${openAiEndpoint}`,
          "--set", `sandbox.image.repository=${ctx.acrLoginServer}/openclaw-sandbox`,
          "--set", `sandbox.image.tag=latest`,
          "--set", `azure.workloadIdentity.clientId=${ctx.wiClientId || ""}`,
          "--set", `azure.keyVaultCsi.keyVaultName=${ctx.keyVaultName || ""}`,
          "--wait",
          "--timeout", "5m",
        ];
        if (ctx.foundryEndpoint) {
          helmArgs.push("--set", `foundry.endpoint=${ctx.foundryEndpoint}`);
        }
        if (ctx.foundryProjectEndpoint) {
          helmArgs.push("--set", `foundry.projectEndpoint=${ctx.foundryProjectEndpoint}`);
        }
        if (ctx.imdsClientId) {
          helmArgs.push("--set", `foundry.imdsClientId=${ctx.imdsClientId}`);
        }
        // meshPeer defaults to ON in values.yaml. Only pass a --set flag
        // when the user explicitly opts out via --no-mesh-peer (commander
        // sets options.meshPeer === false). options.meshPeer === true
        // (explicit --mesh-peer) is already the default, no action needed.
        if (options.meshPeer === false) {
          helmArgs.push("--set", "meshPeer.enabled=false");
        }
        // Fedcred config for controller auto-creation
        if (ctx.oidcIssuerUrl) {
          try {
            const { stdout: subId } = await execa("az", ["account", "show", "--query", "id", "--output", "tsv"], { stdio: "pipe", timeout: 10000 });
            helmArgs.push(
              "--set", `fedcred.subscriptionId=${subId.trim()}`,
              "--set", `fedcred.identityName=${ctx.identityName || ""}`,
              "--set", `fedcred.identityResourceGroup=${ctx.identityResourceGroup || ctx.resourceGroup}`,
              "--set", `fedcred.oidcIssuerUrl=${ctx.oidcIssuerUrl}`,
            );
          } catch { /* non-critical */ }
        }
        // Discover deployments
        try {
          const accountName = ctx.foundryEndpoint ? new URL(ctx.foundryEndpoint).hostname.split(".")[0] : "";
          if (accountName) {
            const { stdout: rgOut } = await execa("az", [
              "cognitiveservices", "account", "list",
              "--query", `[?name=='${accountName}'].resourceGroup | [0]`,
              "--output", "tsv",
            ], { stdio: "pipe", timeout: 15000 });
            const foundryRg = rgOut.trim();
            if (foundryRg) {
              const { stdout } = await execa("az", [
                "cognitiveservices", "account", "deployment", "list",
                "--name", accountName, "--resource-group", foundryRg,
                "--query", "[].name", "--output", "json",
              ], { stdio: "pipe", timeout: 30000 });
              const deps = JSON.parse(stdout || "[]");
              if (Array.isArray(deps) && deps.length > 0) {
                const escaped = JSON.stringify(deps).replace(/,/g, "\\,");
                helmArgs.push("--set-string", `foundry.deployments=${escaped}`);
              }
            }
          }
        } catch { /* non-critical */ }

        spin = ora("Upgrading Helm release...").start();
        await execa("helm", helmArgs, { stdio: "pipe" });
        spin.succeed("Helm upgraded");

        // Rollout restart
        spin = ora("Restarting controller...").start();
        await execa("kubectl", ["rollout", "restart", "deployment/kars-controller", "-n", "kars-system"], { stdio: "pipe" }).catch(() => {});
        await execa("kubectl", ["rollout", "status", "deployment/kars-controller", "-n", "kars-system", "--timeout=120s"], { stdio: "pipe" }).catch(() => {});
        spin.succeed("Controller restarted");

        // Ensure controller SA has a fedcred (so it can get ARM tokens via WI to create sandbox fedcreds)
        if (ctx.oidcIssuerUrl && ctx.identityName) {
          spin = ora("Ensuring controller SA fedcred + MI Contributor...").start();
          const idRg = ctx.identityResourceGroup || ctx.resourceGroup;

          // Controller SA fedcred
          await execa("az", [
            "identity", "federated-credential", "create",
            "--identity-name", ctx.identityName,
            "--resource-group", idRg,
            "--name", "kars-controller-sa",
            "--issuer", ctx.oidcIssuerUrl,
            "--subject", "system:serviceaccount:kars-system:kars-controller",
            "--audiences", "api://AzureADTokenExchange",
            "--output", "none",
          ], { stdio: "pipe", timeout: 30000 }).catch(() => {});

          // MI Contributor self-scoped (so controller can create/delete fedcreds)
          try {
            const { stdout: subId } = await execa("az", [
              "account", "show", "--query", "id", "--output", "tsv",
            ], { stdio: "pipe", timeout: 10000 });
            const miScope = `/subscriptions/${subId.trim()}/resourceGroups/${idRg}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/${ctx.identityName}`;
            const { stdout: miPid } = await execa("az", [
              "identity", "show",
              "--name", ctx.identityName,
              "--resource-group", idRg,
              "--query", "principalId",
              "--output", "tsv",
            ], { stdio: "pipe" });
            await execa("az", [
              "role", "assignment", "create",
              "--assignee-object-id", miPid.trim(),
              "--assignee-principal-type", "ServicePrincipal",
              "--role", "Managed Identity Contributor",
              "--scope", miScope,
              "--output", "none",
            ], { stdio: "pipe" });
          } catch { /* already exists or lacks Owner — non-fatal */ }

          spin.succeed("Controller SA fedcred + MI Contributor ready");
        }

        // Ensure federated credentials exist for all sandboxes
        if (ctx.oidcIssuerUrl && ctx.identityName) {
          spin = ora("Syncing federated credentials for sandboxes...").start();
          try {
            const { stdout: sandboxJson } = await execa("kubectl", [
              "get", "karssandbox", "-A", "-o", "json",
            ], { stdio: "pipe", timeout: 15000 });
            const sandboxes = JSON.parse(sandboxJson).items || [];
            let created = 0;
            for (const sb of sandboxes) {
              const sbName = sb.metadata?.name;
              if (!sbName) continue;
              const sbNs = `kars-${sbName}`;
              await execa("az", [
                "identity", "federated-credential", "create",
                "--identity-name", ctx.identityName,
                "--resource-group", ctx.identityResourceGroup || ctx.resourceGroup,
                "--name", `kars-${sbName}`,
                "--issuer", ctx.oidcIssuerUrl,
                "--subject", `system:serviceaccount:${sbNs}:sandbox`,
                "--audiences", "api://AzureADTokenExchange",
                "--output", "none",
              ], { stdio: "pipe", timeout: 30000 }).then(() => { created++; }).catch(() => {});
            }
            spin.succeed(`Federated credentials synced (${created} created, ${sandboxes.length} total)`);
          } catch {
            spin.warn("Federated credential sync skipped");
          }
        }

        console.log(chalk.green("\n  ✓ Fast upgrade complete\n"));
}
