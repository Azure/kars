// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { Execute } from "./deployment-target.js";
import { assertMeshReleaseConsistency, inspectMeshInstallation } from "./mesh-release.js";
import { rolloutRestartAll, verifyHealth } from "../commands/upgrade.js";
import { applyPushedImages } from "../commands/push-apply.js";

function externalResources(owner: "none" | "controller" | "helm") {
  const ownership = owner === "helm" ? {
    labels: { "app.kubernetes.io/managed-by": "Helm" },
    annotations: { "meta.helm.sh/release-name": "platform-mesh", "meta.helm.sh/release-namespace": "platform-system" },
  } : owner === "controller" ? { ownerReferences: [{ kind: "PlatformMesh", name: "shared", controller: true }] } : {};
  return ["registry", "relay"].flatMap(component => [
    { kind: "Service", metadata: { name: `agentmesh-${component}`, ...ownership }, spec: { selector: { app: `platform-${component}` } } },
    { kind: "Deployment", metadata: { name: `platform-${component}`, ...ownership },
      spec: { template: { metadata: { labels: { app: `platform-${component}` } }, spec: { containers: [{ name: "platform", image: "external.example/mesh:custom" }] } } } },
  ]);
}

describe("external mesh is not part of a disabled-Helm-mesh core operation", () => {
  it.each(["none", "controller", "helm"] as const)("classifies standard Services with %s-owned custom deployments without probing their owner", async owner => {
    const resources = externalResources(owner);
    const calls: Array<[string, readonly string[]]> = [];
    const execute = (async (bin: string, args: readonly string[]) => {
      calls.push([bin, args]);
      if (bin === "helm") throw new Error("External Helm owner must not be queried");
      if (args[1] === "deployment,service") return { stdout: JSON.stringify({ items: resources }) };
      if (args[0] === "get" && args[1] === "deployments") return { stdout: '{"items":[]}' };
      if (args[0] === "get" && args[1] === "deployment" && args[2] === "kars-controller") return { stdout: "True" };
      return { stdout: "" };
    }) as unknown as Execute;
    const mesh = await inspectMeshInstallation(execute);
    expect(mesh.kind).toBe("external");
    expect(mesh.deployments).toEqual([]);
    expect(calls).toHaveLength(1);
    expect(() => assertMeshReleaseConsistency(mesh, { agentMesh: { enabled: false } })).not.toThrow();
    expect(() => assertMeshReleaseConsistency(mesh, { agentMesh: { enabled: true } })).toThrow();
    calls.length = 0;
    // The same restart/health path is used after a core upgrade and rollback.
    await rolloutRestartAll(execute, mesh);
    expect((await verifyHealth(execute, mesh)).healthy).toBe(true);
    expect(calls.some(([, args]) => args.includes("agentmesh") || args.some(arg => arg.includes("platform-")))).toBe(false);
    calls.length = 0;
    await expect(applyPushedImages(execute, [{ name: "relay", image: "mirror.azurecr.io/relay:latest" }], "chart", mesh))
      .rejects.toThrow("External");
    expect(calls).toHaveLength(0);
  });

  it("still blocks mixed ownership when some resources claim the Kars release", async () => {
    const resources = externalResources("helm");
    resources[0].metadata.annotations = {
      "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system",
    };
    const execute = vi.fn().mockResolvedValue({ stdout: JSON.stringify({ items: resources }) }) as unknown as Execute;
    await expect(inspectMeshInstallation(execute)).rejects.toThrow("mixed");
  });

  it("does not treat unmarked external workloads as managed merely because their names match", async () => {
    const resources = externalResources("none");
    for (const item of resources) {
      if (item.kind === "Deployment") item.metadata.name = item.metadata.name.replace("platform-", "");
    }
    const execute = vi.fn().mockResolvedValue({ stdout: JSON.stringify({ items: resources }) }) as unknown as Execute;
    const mesh = await inspectMeshInstallation(execute);
    expect(mesh.kind).toBe("external");
    expect(() => assertMeshReleaseConsistency(mesh, { agentMesh: { enabled: false } })).not.toThrow();
    await expect(applyPushedImages(execute, [{ name: "relay", image: "mirror.azurecr.io/relay:latest" }], "chart", mesh))
      .rejects.toThrow("External");
    expect(execute).toHaveBeenCalledTimes(1);
  });
});
