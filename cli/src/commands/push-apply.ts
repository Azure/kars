// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "../lib/deployment-target.js";
import { restartController } from "../lib/deployment-rollout.js";
import {
  assertMeshReleaseConsistency, inspectMeshInstallation, meshImageValueArgs,
  recheckMeshOwnership, restartMesh, updateLegacyMeshImages, verifyMeshHealth,
  type MeshImages, type MeshInstallation,
} from "../lib/mesh-release.js";
import { coreImageValues, imageValueArgs, PUSH_COMPONENTS, resolvePushedArtifacts, type PushedImage } from "../lib/image-targets.js";
import { inspectCoreInstallation, recheckCoreOwnership, requireHealthyDeployment, updateLegacyCore, verifyCoreConfiguration } from "../lib/core-image-apply.js";
import { inspectSandboxPlans, refreshSandboxImages } from "../lib/sandbox-image-apply.js";
import { inspectManagedMcpPlans, refreshManagedMcpImages } from "../lib/managed-mcp-image-apply.js";
import { assertSafeMutation } from "../lib/sre-authority.js";
import { prepareCoreHelmSchemas } from "../lib/core-helm-schemas.js";

export interface PushApplyResult {
  applied: string[];
  buildOnly: string[];
  updatedSandboxes: number;
  preservedOverrides: number;
  updatedManagedMcp?: number;
}

/** Update owning defaults, prove their selected artifact references, then ask
 * the controller to reconcile eligible CRs without altering explicit pins. */
export async function applyPushedImages(
  execute: Execute,
  images: PushedImage[],
  chart: string,
  installation?: MeshInstallation,
): Promise<PushApplyResult> {
  if (images.some(item => !PUSH_COMPONENTS.includes(item.name))) throw new Error("Unknown pushed component; no apply plan exists");
  const buildOnly = images.filter(item => item.name === "sandbox-base").map(item => item.name);
  const deployable = images.filter(item => item.name !== "sandbox-base");
  if (!deployable.length) throw new Error("sandbox-base is build-only; no deployment was applied");
  const isMesh = (item: PushedImage) => item.name === "relay" || item.name === "registry";
  const selectedCore = deployable.filter(item => !isMesh(item));
  const selectedMesh = deployable.some(isMesh);
  const core = selectedCore.length ? await inspectCoreInstallation(execute) : undefined;
  const mesh = selectedMesh || core?.kind === "helm"
    ? installation ?? await inspectMeshInstallation(execute) : undefined;
  if (selectedMesh && (!mesh || mesh.kind === "external" || mesh.kind === "absent")) {
    throw new Error("External or absent AgentMesh cannot be updated; choose explicit core targets instead.");
  }
  if (core?.kind === "helm" && mesh) assertMeshReleaseConsistency(mesh, core.values);
  await assertSafeMutation(execute);
  const artifacts = await resolvePushedArtifacts(execute, deployable);
  const coreImages = artifacts.filter(item => !isMesh(item));
  const meshImages: MeshImages = {};
  for (const item of artifacts) if (item.name === "relay" || item.name === "registry") meshImages[item.name] = item.image;
  const plans = await inspectSandboxPlans(execute, coreImages);
  const managedPlans = await inspectManagedMcpPlans(execute, coreImages);
  if (core) await recheckCoreOwnership(execute, core);
  if (mesh && (selectedMesh || core?.kind === "helm")) await recheckMeshOwnership(execute, mesh);

  const helmPlans = new Map<string, { release: string; namespace: string; args: string[] }>();
  function addHelm(release: string, namespace: string, args: string[]) {
    const key = `${namespace}/${release}`;
    const plan = helmPlans.get(key) ?? { release, namespace, args: [] };
    plan.args.push(...args);
    helmPlans.set(key, plan);
  }
  if (core?.kind === "helm") addHelm(core.release, core.releaseNamespace, imageValueArgs(coreImageValues(coreImages)));
  if (selectedMesh && mesh?.kind === "helm") addHelm(mesh.release, mesh.releaseNamespace, meshImageValueArgs(meshImages));
  if (helmPlans.size > 1) throw new Error("Selected components have different Helm owners; update one explicit target at a time.");

  if (selectedMesh && mesh?.kind === "legacy") {
    await updateLegacyMeshImages(execute, mesh, meshImages);
    await restartMesh(execute, mesh, Object.keys(meshImages) as Array<"registry" | "relay">);
    await verifyMeshHealth(execute, mesh, meshImages);
  }
  for (const plan of helmPlans.values()) {
    const args = ["upgrade", plan.release, chart, "--namespace", plan.namespace,
      "--reuse-values", ...plan.args, "--atomic", "--wait", "--timeout", "8m"];
    await prepareCoreHelmSchemas(execute, args);
    await execute("helm", args, { stdio: "pipe" });
  }
  if (selectedMesh && mesh?.kind === "helm") {
    await restartMesh(execute, mesh, Object.keys(meshImages) as Array<"registry" | "relay">);
    await verifyMeshHealth(execute, mesh, meshImages);
  }
  if (core?.kind === "legacy") await updateLegacyCore(execute, core, coreImages);
  if (core) {
    await verifyCoreConfiguration(execute, core, coreImages);
    await restartController(execute);
    requireHealthyDeployment(await verifyCoreConfiguration(execute, core, coreImages));
  }
  const updatedSandboxes = await refreshSandboxImages(execute, plans);
  const updatedManagedMcp = await refreshManagedMcpImages(execute, managedPlans, core?.deployment.metadata.namespace ?? "kars-system");
  return { applied: artifacts.map(item => item.name), buildOnly, updatedSandboxes,
    ...(coreImages.some(image => image.name === "mcp-everything") ? { updatedManagedMcp } : {}),
    preservedOverrides: plans.filter(plan => plan.pinned).length };
}
