// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Core Helm owns local mesh deployment. This phase verifies its result before
// sandbox creation, or selects an explicitly configured external registry.
import type { Stepper } from "../../stepper.js";
import { kvLine } from "../../stepper.js";
import { inspectUpMeshTrust, validateMeshTrustOptions, verifyHelmMeshTrust } from "./mesh-trust.js";
import { verifyMeshHealth } from "../../lib/mesh-release.js";

export interface AgentMeshDeployContext {
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
  meshTrust?: "anonymous" | "entra";
}

export interface AgentMeshDeployResult {
  registryMode: "local" | "global";
  globalRegistryUrl?: string;
  globalRelayUrl?: string;
}

/** Verify managed/identified legacy infrastructure without rewriting its trust. */
export async function deployAgentMesh(
  ctx: AgentMeshDeployContext,
  options: AgentMeshDeployOptions,
): Promise<AgentMeshDeployResult> {
  const mode = validateMeshTrustOptions(options);
  const { execa } = await import("execa");
  const { stepper } = ctx;
  const trust = await inspectUpMeshTrust(execa, mode, options.globalRegistry);
  stepper.step("Verifying AgentMesh infrastructure...");

  if (options.globalRegistry) {
    // External registry mode — skip local deployment, set env vars
    stepper.update(`Using external registry: ${options.globalRegistry}`);
    kvLine("Registry mode", "global");
    kvLine("Registry URL", options.globalRegistry);

    stepper.done("AgentMesh: external registry selected; remote trust and availability not verified");
    return { registryMode: "global", globalRegistryUrl: options.globalRegistry };
  } else {
    if (trust.mesh.kind !== "helm" && trust.mesh.kind !== "legacy") {
      throw new Error("AgentMesh is not installed by Core Helm or an identified legacy installation; sandbox creation stopped.");
    }
    await verifyHelmMeshTrust(execa, trust.mesh, mode === "entra");
    await verifyMeshHealth(execa, trust.mesh);
    stepper.done(`AgentMesh healthy (${trust.mesh.kind}); installed trust configuration preserved`);
  }

  return { registryMode: "local" };
}
