// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "../lib/deployment-target.js";
import { restartController, restartSandboxes } from "../lib/deployment-rollout.js";
import { applyMeshImages, inspectMeshInstallation, type MeshImages, type MeshInstallation } from "../lib/mesh-release.js";

export async function applyPushedImages(
  execute: Execute,
  images: Array<{ name: string; image: string }>,
  chart: string,
  installation?: MeshInstallation,
): Promise<void> {
  const meshImages: MeshImages = {};
  for (const image of images) {
    if (image.name === "relay" || image.name === "registry") meshImages[image.name] = image.image;
  }
  if (Object.keys(meshImages).length) {
    const mesh = installation ?? await inspectMeshInstallation(execute);
    await applyMeshImages(execute, mesh, meshImages, chart);
  }
  // Preserve the existing controller refresh for non-mesh builds; a mesh-only
  // push does not need to interrupt the unrelated controller or sandboxes.
  if (images.some(image => image.name !== "relay" && image.name !== "registry")) await restartController(execute);
  if (images.some(image => image.name === "sandbox" || image.name === "router")) await restartSandboxes(execute);
}
