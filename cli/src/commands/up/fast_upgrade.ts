// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15.d.1: `kars up --upgrade` fast-path extracted
// from up.ts. Skips all prompts and infra; just re-runs Helm with
// cached context. Caller invokes when `options.upgrade` is set and
// returns immediately afterwards.

import chalk from "chalk";
import ora from "ora";
import { execa } from "execa";
import { loadContext } from "../../config.js";
import { requireBundledAsset } from "../../lib/repo-assets.js";
import { cliReleaseTag } from "../../lib/version.js";
import { rolloutRestartAll } from "../upgrade.js";
import { createSubscriptionPinnedExeca } from "./orchestration.js";

export interface UpOptionsForUpgrade {
  upgrade?: boolean;
  registrationMode?: string;
  acrName?: string;
  [key: string]: unknown;
}

const blue = chalk.hex("#0078D4");

function parseJsonArray(value: string, operation: string): unknown[] {
  try {
    const parsed = JSON.parse(value || "[]") as unknown;
    if (Array.isArray(parsed)) return parsed;
  } catch {
    // Use the actionable error below.
  }
  throw new Error(`${operation} returned invalid JSON`);
}

async function discoverLegacyDeploymentSubscription(
  resourceGroup: string,
  aksCluster: string,
): Promise<string> {
  const { stdout } = await execa("az", [
    "account", "list",
    "--query", "[?state=='Enabled'].{id:id}",
    "--output", "json",
  ], { stdio: "pipe", timeout: 15000 });
  const subscriptionIds = new Set(
    parseJsonArray(
      String(stdout),
      "Listing enabled Azure subscriptions",
    ).map((candidate) => {
      if (
        typeof candidate !== "object" ||
        candidate === null ||
        typeof (candidate as { id?: unknown }).id !== "string" ||
        !(candidate as { id: string }).id.trim()
      ) {
        throw new Error("Azure returned an invalid enabled subscription list");
      }
      return (candidate as { id: string }).id.trim();
    }),
  );

  const matches = new Set<string>();
  for (const subscriptionId of subscriptionIds) {
    const { stdout: clusterOutput } = await execa("az", [
      "aks", "list",
      "--query", "[].{name:name,resourceGroup:resourceGroup}",
      "--output", "json",
      "--subscription", subscriptionId,
    ], { stdio: "pipe", timeout: 30000 });
    const clusters = parseJsonArray(
      String(clusterOutput),
      `Listing AKS clusters in subscription '${subscriptionId}'`,
    );
    if (clusters.some((candidate) => {
      if (typeof candidate !== "object" || candidate === null) return false;
      const cluster = candidate as {
        name?: unknown;
        resourceGroup?: unknown;
      };
      return (
        typeof cluster.name === "string" &&
        typeof cluster.resourceGroup === "string" &&
        cluster.name.toLowerCase() === aksCluster.toLowerCase() &&
        cluster.resourceGroup.toLowerCase() === resourceGroup.toLowerCase()
      );
    })) {
      matches.add(subscriptionId);
    }
  }

  if (matches.size === 1) return [...matches][0];
  if (matches.size === 0) {
    throw new Error(
      `No enabled Azure subscription contains cached AKS cluster '${aksCluster}' in resource group '${resourceGroup}'. ` +
      "Fast upgrade stopped before making changes. Run 'kars up' without --upgrade to refresh the deployment context.",
    );
  }
  throw new Error(
    `Cached AKS cluster '${aksCluster}' in resource group '${resourceGroup}' exists in multiple enabled Azure subscriptions ` +
    `(${[...matches].join(", ")}). Fast upgrade stopped before making changes. ` +
    "Run 'kars up' without --upgrade and select the intended subscription to refresh the deployment context.",
  );
}

export async function runFastUpgrade(options: UpOptionsForUpgrade): Promise<void> {
        const ctx = loadContext();
        if (!ctx?.acrLoginServer || !ctx?.aksCluster || !ctx?.resourceGroup) {
          console.error(chalk.red("\n  No cached deployment context. Run 'kars up' first (without --upgrade).\n"));
          process.exit(1);
        }

        let subscriptionId = ctx.subscription?.trim();
        if (!subscriptionId) {
          subscriptionId = await discoverLegacyDeploymentSubscription(
            ctx.resourceGroup,
            ctx.aksCluster,
          );
        }
        const subscriptionPinnedExeca = createSubscriptionPinnedExeca(
          execa,
          subscriptionId,
        );

        console.log(blue("\n  kars · Fast Upgrade\n"));

        // Connect to AKS
        let spin = ora("Connecting to AKS...").start();
        await subscriptionPinnedExeca("az", ["aks", "get-credentials", "--name", ctx.aksCluster, "--resource-group", ctx.resourceGroup, "--overwrite-existing"], { stdio: "pipe" });
        spin.succeed("AKS connected");

        // Resolve the Helm chart from a repo checkout OR the bundled package
        // copy (so `kars up --upgrade` works with no source tree).
        const helmPath = requireBundledAsset("deploy/helm/kars");

        // Build Helm args from cached context
        const openAiEndpoint = ctx.foundryEndpoint || "";
        const helmArgs = [
          "upgrade", "--install", "kars", helmPath,
          "--namespace", "kars-system",
          "--create-namespace",
          // Preserve values set by the original `kars up` (runtime images,
          // fedcred, mesh, …) that this fast path doesn't re-specify; the
          // `--set` flags below still override the image repos/tags.
          "--reuse-values",
          "--set", `controller.image.repository=${ctx.acrLoginServer}/kars-controller`,
          "--set", `controller.image.tag=latest`,
          "--set", `inferenceRouter.image.repository=${ctx.acrLoginServer}/kars-inference-router`,
          "--set", `inferenceRouter.image.tag=latest`,
          "--set", `inferenceRouter.azure.openai.endpoint=${openAiEndpoint}`,
          "--set", `sandbox.image.repository=${ctx.acrLoginServer}/openclaw-sandbox`,
          "--set", `sandbox.image.tag=latest`,
          "--set", `azure.workloadIdentity.clientId=${ctx.wiClientId || ""}`,
          "--set", `azure.keyVaultCsi.keyVaultName=${ctx.keyVaultName || ""}`,
          // Stamp the CLI's version as the deployed release so `kars upgrade`
          // can read back an accurate current version (the chart appVersion is
          // a static sentinel value).
          "--set", `karsRelease=${cliReleaseTag()}`,
          "--atomic",
          "--wait",
          "--timeout", "8m",
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
          helmArgs.push(
            "--set", `fedcred.subscriptionId=${subscriptionId}`,
            "--set", `fedcred.identityName=${ctx.identityName || ""}`,
            "--set", `fedcred.identityResourceGroup=${ctx.identityResourceGroup || ctx.resourceGroup}`,
            "--set", `fedcred.oidcIssuerUrl=${ctx.oidcIssuerUrl}`,
          );
        }
        // Discover deployments
        try {
          const accountName = ctx.foundryEndpoint ? new URL(ctx.foundryEndpoint).hostname.split(".")[0] : "";
          if (accountName) {
            const { stdout: rgOut } = await subscriptionPinnedExeca("az", [
              "cognitiveservices", "account", "list",
              "--query", `[?name=='${accountName}'].resourceGroup | [0]`,
              "--output", "tsv",
            ], { stdio: "pipe", timeout: 15000 });
            const foundryRg = rgOut.trim();
            if (foundryRg) {
              const { stdout } = await subscriptionPinnedExeca("az", [
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

        // Rollout restart — refresh ALL kars workloads, not just the
        // controller. `kars up --upgrade` re-runs Helm against the `:latest`
        // images in ACR, so (like `kars upgrade`) a rolling restart of the
        // sandboxes + the standalone AgentMesh relay/registry is what actually
        // pulls the new bits; restarting only the controller left them stale.
        spin = ora("Restarting kars workloads (controller, sandboxes, mesh)...").start();
        await rolloutRestartAll(execa);
        spin.succeed("Workloads restarted");

        // Ensure controller SA has a fedcred (so it can get ARM tokens via WI to create sandbox fedcreds)
        if (ctx.oidcIssuerUrl && ctx.identityName) {
          spin = ora("Ensuring controller SA fedcred + MI Contributor...").start();
          const idRg = ctx.identityResourceGroup || ctx.resourceGroup;

          // Controller SA fedcred
          await subscriptionPinnedExeca("az", [
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
            const miScope = `/subscriptions/${subscriptionId}/resourceGroups/${idRg}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/${ctx.identityName}`;
            const { stdout: miPid } = await subscriptionPinnedExeca("az", [
              "identity", "show",
              "--name", ctx.identityName,
              "--resource-group", idRg,
              "--query", "principalId",
              "--output", "tsv",
            ], { stdio: "pipe" });
            await subscriptionPinnedExeca("az", [
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
              await subscriptionPinnedExeca("az", [
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
