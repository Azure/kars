// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Sub-slice S15.d.3 of S15.d phase2-hotspot-up-cli.
//
// AgentMesh infrastructure deploy phase extracted verbatim from
// cli/src/commands/up.ts. Covers:
//   - relay + registry deployment in-cluster (local mode)
//   - external-registry shortcut (--global-registry)
//   - optional AGIC Ingress for public endpoints (--expose-registry)
//
// Returns the registry-mode triple consumed by the KarsSandbox CR
// creation step (d.4) and the saveContext() call at end-of-deploy.
import path from "node:path";
import { existsSync, readFileSync, writeFileSync, unlinkSync } from "node:fs";
import type { Stepper } from "../../stepper.js";
import { kvLine } from "../../stepper.js";

export interface AgentMeshDeployContext {
  repoRoot: string;
  /** Bare ACR name (e.g. "karsacr"). */
  acr: string;
  /** ACR login server (e.g. "karsacr.azurecr.io"). */
  acrLoginServer: string;
  /** Cluster base name (cluster name with -aks suffix stripped). */
  baseName: string;
  /** Resolved resource group name. */
  rg: string;
  stepper: Stepper;
}

export interface AgentMeshDeployOptions {
  globalRegistry?: string;
  exposeRegistry?: boolean;
  /**
   * Mesh stack to deploy. Only 'agt' is supported after Phase 5.2; the
   * vendored Rust relay + Postgres registry have been removed. Kept as
   * a flag for backward-compatible callers.
   */
  meshProvider?: "agt";
}

export interface AgentMeshDeployResult {
  registryMode: "local" | "global";
  globalRegistryUrl?: string;
  globalRelayUrl?: string;
}

/**
 * Deploy the AgentMesh relay + registry infrastructure (or wire to an
 * external one), and optionally publish AGIC ingress for cross-cluster
 * federation.
 */
export async function deployAgentMesh(
  ctx: AgentMeshDeployContext,
  options: AgentMeshDeployOptions,
): Promise<AgentMeshDeployResult> {
  const { execa } = await import("execa");
  const { repoRoot, acr: _acr, acrLoginServer, baseName, rg, stepper } = ctx;

  // Inspektor Gadget (eBPF observability) — non-fatal
  await execa("kubectl", ["gadget", "deploy"], { stdio: "pipe" }).catch(() => {});

  stepper.step("Deploying AgentMesh infrastructure...");

  let registryMode: "local" | "global" = "local";
  let globalRegistryUrl: string | undefined;
  let globalRelayUrl: string | undefined;

  if (options.globalRegistry) {
    // External registry mode — skip local deployment, set env vars
    stepper.update(`Using external registry: ${options.globalRegistry}`);
    kvLine("Registry mode", "global");
    kvLine("Registry URL", options.globalRegistry);

    registryMode = "global";
    globalRegistryUrl = options.globalRegistry;

    stepper.done("AgentMesh: using external registry (skipped local deploy)");
  } else {
    // Local registry mode — deploy AGT relay + registry in-cluster.
    const manifestName = "agentmesh-agt.yaml";
    const agentmeshManifest = path.join(repoRoot, "deploy", manifestName);
    if (existsSync(agentmeshManifest)) {
      // Ensure the agentmesh namespace exists
      await execa("kubectl", ["create", "namespace", "agentmesh"], { stdio: "pipe" }).catch(() => {});

      // Substitute ACR login server in the manifest
      const manifest = readFileSync(agentmeshManifest, "utf-8");
      const patchedManifest = manifest.replaceAll(
        "karsacr.azurecr.io",
        acrLoginServer,
      );
      const tmpManifest = path.join(repoRoot, `.tmp-${manifestName}`);
      try {
        writeFileSync(tmpManifest, patchedManifest);
        await execa("kubectl", ["apply", "-f", tmpManifest], { stdio: "pipe" });

        // Wait for AgentMesh pods to be ready
        stepper.update("Waiting for AgentMesh pods to be ready...");
        await execa("kubectl", [
          "wait", "--for=condition=Ready", "pod",
          "-l", "app=agentmesh-relay",
          "-n", "agentmesh",
          "--timeout=180s",
        ], { stdio: "pipe" }).catch(() => {});
        await execa("kubectl", [
          "wait", "--for=condition=Ready", "pod",
          "-l", "app=agentmesh-registry",
          "-n", "agentmesh",
          "--timeout=180s",
        ], { stdio: "pipe" }).catch(() => {});

        stepper.done(`AgentMesh infrastructure deployed (agt)`);
      } finally {
        try { unlinkSync(tmpManifest); } catch { /* noop */ }
      }
    } else {
      stepper.warn(`AgentMesh manifest not found (${manifestName}) — skipping`);
    }

    // Deploy AGIC Ingress if --expose-registry is set
    if (options.exposeRegistry) {
      stepper.step("Deploying AgentMesh Ingress (public endpoints)...");
      const ingressManifest = path.join(repoRoot, "deploy", "agentmesh-ingress.yaml");
      if (existsSync(ingressManifest)) {
        const ingressYaml = readFileSync(ingressManifest, "utf-8");
        const domain = `${baseName}.kars.dev`;
        const { stdout: currentSubId } = await execa("az", [
          "account", "show", "--query", "id", "--output", "tsv",
        ], { stdio: "pipe", timeout: 10000 }).catch(() => ({ stdout: "" }));
        const patchedIngress = ingressYaml
          .replace(/DOMAIN_PLACEHOLDER/g, domain)
          .replace(/SUBSCRIPTION_ID/g, currentSubId.trim())
          .replace(/RESOURCE_GROUP/g, rg)
          .replace(/karsacr\.azurecr\.io/g, acrLoginServer);
        const tmpIngress = path.join(repoRoot, ".tmp-agentmesh-ingress.yaml");
        try {
          writeFileSync(tmpIngress, patchedIngress);
          await execa("kubectl", ["apply", "-f", tmpIngress], { stdio: "pipe" });
          stepper.done(`AgentMesh Ingress deployed (registry.${domain}, relay.${domain})`);

          registryMode = "global";
          globalRegistryUrl = `https://registry.${domain}`;
          globalRelayUrl = `wss://relay.${domain}`;
        } finally {
          try { unlinkSync(tmpIngress); } catch { /* noop */ }
        }
      } else {
        stepper.warn("Ingress manifest not found — skipping");
      }
    }
  }

  return { registryMode, globalRegistryUrl, globalRelayUrl };
}
