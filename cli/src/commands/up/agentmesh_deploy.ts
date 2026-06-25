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
import * as os from "node:os";
import { readFileSync, writeFileSync, unlinkSync } from "node:fs";
import { resolveBundledAsset } from "../../lib/repo-assets.js";
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
  /**
   * Phase 6.c — when set, the relay + registry are configured at
   * deploy time to verify inbound Entra-signed JWTs from sandbox
   * mesh peers. Both env vars are required; both empty/undefined
   * keeps the relay+registry on the legacy anonymous-tier path
   * for backward compatibility.
   */
  entraVerify?: {
    audience: string;
    tenantId: string;
  };
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
  const { acr: _acr, acrLoginServer, baseName, rg, stepper, entraVerify } = ctx;
  void ctx.repoRoot;

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
    // Resolve from a repo checkout OR the bundled package copy (so
    // `kars up --release` works without a source tree).
    const agentmeshManifest = resolveBundledAsset(`deploy/${manifestName}`);
    if (agentmeshManifest) {
      // Ensure the agentmesh namespace exists
      await execa("kubectl", ["create", "namespace", "agentmesh"], { stdio: "pipe" }).catch(() => {});

      // Substitute ACR login server in the manifest
      const manifest = readFileSync(agentmeshManifest, "utf-8");
      const patchedManifest = manifest.replaceAll(
        "karsacr.azurecr.io",
        acrLoginServer,
      );
      const tmpManifest = path.join(os.tmpdir(), `kars-${manifestName}-${Date.now()}`);
      try {
        writeFileSync(tmpManifest, patchedManifest);
        await execa("kubectl", ["apply", "-f", tmpManifest], { stdio: "pipe" });

        // Wait for AgentMesh pods to be ready
        stepper.update("Waiting for AgentMesh pods to be ready...");
        const relayReady = await execa("kubectl", [
          "wait", "--for=condition=Ready", "pod",
          "-l", "app=agentmesh-relay",
          "-n", "agentmesh",
          "--timeout=180s",
        ], { stdio: "pipe" }).then(() => true).catch(() => false);
        const registryReady = await execa("kubectl", [
          "wait", "--for=condition=Ready", "pod",
          "-l", "app=agentmesh-registry",
          "-n", "agentmesh",
          "--timeout=180s",
        ], { stdio: "pipe" }).then(() => true).catch(() => false);

        if (!relayReady || !registryReady) {
          // Don't claim a clean success when the mesh never came up — that's
          // the silent "broken mesh, green checkmark" trap. Sub-agent handoff
          // will fail until these are Running; point the user at the fix.
          const which = [!relayReady ? "relay" : "", !registryReady ? "registry" : ""]
            .filter(Boolean).join(" + ");
          stepper.warn(
            `AgentMesh ${which} not Ready after 180s — sub-agent handoff won't work yet. ` +
              `Check: kubectl get pods -n agentmesh (likely ImagePullBackOff or slow pull).`,
          );
        } else {
          stepper.done(`AgentMesh infrastructure deployed (agt)`);
        }

        // Phase 6.c — enable JWT verification on the relay + registry
        // when the operator has provisioned an Entra Agent Identity
        // blueprint. Both env vars must be set together (the verifier
        // refuses to enable on a half-configured deployment), so we
        // apply both via a single kubectl set env on each deploy.
        // Idempotent: running again with the same values is a no-op.
        if (entraVerify?.audience && entraVerify?.tenantId) {
          stepper.update("Enabling Phase 6.c Entra-signed JWT verification on relay + registry...");
          for (const deploy of ["registry", "relay"]) {
            try {
              await execa("kubectl", [
                "set", "env", "-n", "agentmesh", `deploy/${deploy}`,
                `AGENTMESH_ENTRA_AUDIENCE=${entraVerify.audience}`,
                `AGENTMESH_ENTRA_TENANT_ID=${entraVerify.tenantId}`,
              ], { stdio: "pipe" });
            } catch (e: unknown) {
              const msg = e instanceof Error ? e.message : String(e);
              stepper.detail("info", `set env on deploy/${deploy} failed: ${msg.split("\n")[0].slice(0, 100)}`);
            }
          }
          // Wait briefly for the rollout — set env triggers a fresh
          // pod, and the registry needs to be Ready before the first
          // sandbox boots and POSTs /v1/registry/verify.
          for (const deploy of ["registry", "relay"]) {
            await execa("kubectl", [
              "rollout", "status", "-n", "agentmesh", `deploy/${deploy}`,
              "--timeout=120s",
            ], { stdio: "pipe" }).catch(() => {});
          }
          kvLine("Entra verify", `enabled (aud=${entraVerify.audience})`);
        }
      } finally {
        try { unlinkSync(tmpManifest); } catch { /* noop */ }
      }
    } else {
      stepper.warn(`AgentMesh manifest not found (${manifestName}) — skipping`);
    }

    // Deploy AGIC Ingress if --expose-registry is set
    if (options.exposeRegistry) {
      stepper.step("Deploying AgentMesh Ingress (public endpoints)...");
      const ingressManifest = resolveBundledAsset("deploy/agentmesh-ingress.yaml");
      if (ingressManifest) {
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
        const tmpIngress = path.join(os.tmpdir(), `kars-agentmesh-ingress-${Date.now()}.yaml`);
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
