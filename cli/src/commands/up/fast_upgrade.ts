// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Phase 2 / S15.d.1: `kars up --upgrade` fast-path extracted
// from up.ts. Skips all prompts and infra; just re-runs Helm with
// cached context. Caller invokes when `options.upgrade` is set and
// returns immediately afterwards.

import chalk from "chalk";
import ora from "ora";
import { execa as execute } from "execa";
import { createSubscriptionPinnedExeca } from "../../lib/azure-subscription.js";
import { ensureWorkloadIdentity, sandboxNamesForFederation, validateWorkloadIdentityConfig } from "../../lib/workload-identity.js";
import { loadContext } from "../../config.js";
import { requireBundledAsset } from "../../lib/repo-assets.js";
import { cliReleaseTag } from "../../lib/version.js";
import { rolloutRestartAll, verifyHealth } from "../upgrade.js";
import { helmUsesEntra, inspectUpMeshTrust, validateMeshTrustOptions, verifyHelmMeshTrust } from "./mesh-trust.js";
import { recheckMeshOwnership } from "../../lib/mesh-release.js";
import { inspectNamespaceOwnership } from "../../lib/namespace-ownership.js";
import { assertSafeMutation } from "../../lib/sre-authority.js";
import { prepareCoreHelmSchemas } from "../../lib/core-helm-schemas.js";
import { assertControllerMutationAllowed } from "../../lib/private-root-upgrade-guard.js";

export interface UpOptionsForUpgrade {
  upgrade?: boolean;
  registrationMode?: string;
  acrName?: string;
  meshTrust?: unknown;
  meshProvider?: unknown;
  globalRegistry?: unknown;
  exposeRegistry?: unknown;
  serviceTree?: unknown;
  [key: string]: unknown;
}

const blue = chalk.hex("#0078D4");

export async function runFastUpgrade(options: UpOptionsForUpgrade): Promise<void> {
        const mode = validateMeshTrustOptions(options);
        const ctx = loadContext();
        if (!ctx?.acrLoginServer || !ctx?.aksCluster || !ctx?.resourceGroup) {
          console.error(chalk.red("\n  No cached deployment context. Run 'kars up' first (without --upgrade).\n"));
          process.exit(1);
        }
        if (options.globalRegistry != null && options.globalRegistry !== ctx.globalRegistryUrl) {
          throw new Error("Fast upgrade cannot change the external registry; use a reviewed normal up configuration.");
        }
        const globalRegistry = ctx.registryMode === "global" ? ctx.globalRegistryUrl : undefined;
        validateMeshTrustOptions({ meshTrust: mode, globalRegistry });

        console.log(blue("\n  kars · Fast Upgrade\n"));

        if (options.dryRun) {
          console.log("Would connect to the cached AKS target, verify ownership/trust, upgrade Core Helm and restart its workloads; no changes made or live checks performed.");
          return;
        }
        if (typeof ctx.subscription !== "string" || !ctx.subscription.trim()) {
          throw new Error("Cached deployment context has no subscription. Run 'kars up' without --upgrade to select and save the intended subscription; fast upgrade will not use the ambient account.");
        }
        const subscriptionId = ctx.subscription.trim();
        const execa = createSubscriptionPinnedExeca(execute, subscriptionId);
        const federation = ctx.oidcIssuerUrl != null || ctx.identityName != null || ctx.identityResourceGroup != null
          ? validateWorkloadIdentityConfig({ subscriptionId, identityName: ctx.identityName ?? "",
            resourceGroup: ctx.identityResourceGroup ?? ctx.resourceGroup, issuer: ctx.oidcIssuerUrl ?? "" })
          : undefined;
        // Connect only after the non-mutating preview has returned.
        let spin = ora("Connecting to AKS...").start();
        await execa("az", ["aks", "get-credentials", "--name", ctx.aksCluster, "--resource-group", ctx.resourceGroup, "--overwrite-existing"], { stdio: "pipe" });
        spin.succeed("AKS connected");
        await assertControllerMutationAllowed(execa);
        for (const result of await inspectNamespaceOwnership(execa)) console.log(result);
        await assertSafeMutation(execa);
        const trust = await inspectUpMeshTrust(execa, mode, globalRegistry);
        const requireEntra = mode === "entra" || (trust.mesh.kind === "helm" && helmUsesEntra(trust.mesh.values));
        await verifyHelmMeshTrust(execa, trust.mesh, requireEntra);

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
          "--set", `metricsServer.image.repository=${ctx.acrLoginServer}/kars-metrics-server`,
          "--set", "metricsServer.image.tag=latest",
          "--set", `headlamp.image.repository=${ctx.acrLoginServer}/kars-headlamp`,
          "--set", "headlamp.image.tag=latest",
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
        // Use the same validated target for the controller and the ARM verification below.
        if (federation) {
          helmArgs.push(
            "--set", `fedcred.subscriptionId=${federation.subscriptionId}`,
            "--set", `fedcred.identityName=${federation.identityName}`,
            "--set", `fedcred.identityResourceGroup=${federation.resourceGroup}`,
            "--set", `fedcred.oidcIssuerUrl=${federation.issuer}`,
          );
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
        await recheckMeshOwnership(execa, trust.mesh);
        await prepareCoreHelmSchemas(execa, helmArgs);
        await recheckMeshOwnership(execa, trust.mesh);
        await execa("helm", helmArgs, { stdio: "pipe" });
        spin.succeed("Helm upgraded");

        // Rollout restart — refresh ALL kars workloads, not just the
        // controller. `kars up --upgrade` re-runs Helm against the `:latest`
        // images in ACR, so (like `kars upgrade`) a rolling restart of the
        // sandboxes + the standalone AgentMesh relay/registry is what actually
        // pulls the new bits; restarting only the controller left them stale.
        spin = ora("Restarting kars workloads (controller, sandboxes, mesh)...").start();
        await recheckMeshOwnership(execa, trust.mesh);
        const installed = await inspectUpMeshTrust(execa, requireEntra ? "entra" : mode, globalRegistry);
        await rolloutRestartAll(execa, installed.mesh);
        await recheckMeshOwnership(execa, installed.mesh);
        await verifyHelmMeshTrust(execa, installed.mesh, requireEntra);
        const health = await verifyHealth(execa, installed.mesh);
        if (!health.healthy) throw new Error(`Fast upgrade health verification failed: ${health.reason}`);
        spin.succeed("Workloads restarted and healthy");

        if (federation) {
          spin = ora("Verifying controller and sandbox Workload Identity configuration...").start();
          try {
            const { stdout } = await execa("kubectl", ["get", "karssandbox", "-A", "-o", "json"], { stdio: "pipe", timeout: 15000 });
            const names = sandboxNamesForFederation(stdout);
            await ensureWorkloadIdentity(execa, federation, names);
            spin.succeed(`Workload Identity configuration verified (controller + ${names.length} sandboxes); token exchange not tested`);
          } catch (error) {
            spin.fail("Workload Identity configuration incomplete; fast upgrade did not complete");
            throw error;
          }
        }

        console.log(chalk.green("\n  ✓ Fast upgrade complete\n"));
}
