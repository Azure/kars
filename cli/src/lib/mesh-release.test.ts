// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { Execute } from "./deployment-target.js";
import {
  applyMeshImages, assertMeshReleaseConsistency, helmMeshEnabled,
  inspectMeshInstallation, meshImageValueArgs, verifyMeshHealth,
} from "./mesh-release.js";
import { applyPushedImages } from "../commands/push-apply.js";
vi.mock("./core-helm-schemas.js", () => ({ prepareCoreHelmSchemas: vi.fn(async () => {}) }));

function fixture(owned: boolean) {
  const labels: Record<string, string> = owned
    ? { "app.kubernetes.io/managed-by": "Helm" }
    : { "kars.azure.com/mesh-provider": "agt" };
  const metadata = (name: string) => ({
    name, generation: 1, ownerReferences: [] as Array<{ controller: boolean }>,
    labels: { ...labels },
    annotations: owned ? { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" } : {},
  });
  return ["registry", "relay"].flatMap(component => [
    {
      kind: "Deployment", metadata: metadata(component),
      spec: { replicas: 1, selector: { matchLabels: { app: `agentmesh-${component}` } },
        template: { metadata: { labels: { app: `agentmesh-${component}` } }, spec: { containers: [{ name: component, image: `old.example/${component}:old` }] } } },
      status: { observedGeneration: 1, readyReplicas: 1, updatedReplicas: 1, conditions: [{ type: "Available", status: "True" }] },
    },
    { kind: "Service", metadata: metadata(`agentmesh-${component}`), spec: { selector: { app: `agentmesh-${component}` } } },
  ]);
}

function recorded(execute: Execute): Array<[string, string[]]> {
  return vi.mocked(execute).mock.calls as unknown as Array<[string, string[]]>;
}

function mockInstallation(owned: boolean, fail?: (bin: string, args: readonly string[]) => void) {
  const resources = fixture(owned);
  const execute = vi.fn(async (bin: string, args: readonly string[]) => {
    fail?.(bin, args);
    if (bin === "az") return { stdout: `sha256:${"a".repeat(64)}` };
    if (bin === "kubectl" && args[1] === "deployment,service") return { stdout: JSON.stringify({ items: resources }) };
    if (bin === "helm" && args[0] === "list") return { stdout: JSON.stringify([{ name: "kars", chart: "kars-0.1.0" }]) };
    if (bin === "helm" && args[0] === "get") return { stdout: JSON.stringify({ agentMesh: { enabled: true } }) };
    if (bin === "helm" && args[0] === "upgrade") {
      for (const component of ["registry", "relay"]) {
        const repository = args.find(arg => arg.startsWith(`agentMesh.${component}.image.repository=`))?.split("=")[1];
        const tag = args.find(arg => arg.startsWith(`agentMesh.${component}.image.tag=`))?.split("=")[1];
        if (repository && tag) {
          resources.find(item => item.kind === "Deployment" && item.metadata.name === component)!
            .spec.template!.spec.containers[0].image = `${repository}:${tag}`;
        }
      }
    }
    if (bin === "kubectl" && args[0] === "set") {
      const name = args[2].replace("deployment/", "");
      resources.find(item => item.kind === "Deployment" && item.metadata.name === name)!
        .spec.template!.spec.containers[0].image = args[3].slice(args[3].indexOf("=") + 1);
    }
    if (bin === "kubectl" && args[0] === "get" && args[1] === "deployment") {
      return { stdout: JSON.stringify(resources.find(item => item.kind === "Deployment" && item.metadata.name === args[2])) };
    }
    return { stdout: "" };
  }) as unknown as Execute;
  return { execute, resources };
}

describe("AgentMesh ownership and image application", () => {
  it("uses the owning Helm release for push --only relay --apply, never applies a legacy selector", async () => {
    const { execute, resources } = mockInstallation(true);
    const selectors = resources.map(item => JSON.stringify(item.spec.selector));
    await applyPushedImages(execute, [{ name: "relay", image: "mirror.azurecr.io/agentmesh-relay-agt:latest" }], "chart");
    const calls = recorded(execute);
    const upgrade = calls.find(([bin, args]) => bin === "helm" && args?.[0] === "upgrade");
    expect(upgrade?.[1]).toEqual(expect.arrayContaining([
      "kars", "chart", "--namespace", "kars-system", "--reuse-values",
      "agentMesh.relay.image.repository=mirror.azurecr.io/agentmesh-relay-agt",
      `agentMesh.relay.image.tag=latest@sha256:${"a".repeat(64)}`, "--atomic",
    ]));
    expect(calls.some(([bin, args]) => bin === "kubectl" && (args?.[0] === "apply" || args?.[0] === "set"))).toBe(false);
    expect(calls.some(([, args]) => args?.some(arg => ["--take-ownership", "--force", "--force-recreate", "--force-conflicts"].includes(arg)))).toBe(false);
    expect(calls.some(([, args]) => args?.includes("deployment/relay"))).toBe(true);
    expect(calls.some(([, args]) => args?.includes("deployment/agentmesh-relay"))).toBe(false);
    expect(calls.some(([, args]) => args?.includes("deployment/kars-controller"))).toBe(false);
    expect(resources.map(item => JSON.stringify(item.spec.selector))).toEqual(selectors);
  });

  it("updates legacy mesh directly without requiring Helm or replacing Services", async () => {
    const { execute, resources } = mockInstallation(false, bin => { if (bin === "helm") throw new Error("Helm is not installed"); });
    const mesh = await inspectMeshInstallation(execute);
    expect(mesh.kind).toBe("legacy");
    await applyMeshImages(execute, mesh, { relay: "mirror.azurecr.io/agentmesh-relay-agt:latest" }, "chart");
    expect(recorded(execute).some(([bin]) => bin === "helm")).toBe(false);
    expect(recorded(execute).some(([, args]) => args[0] === "apply")).toBe(false);
    expect(recorded(execute).some(([, args]) => args.includes("relay=mirror.azurecr.io/agentmesh-relay-agt:latest"))).toBe(true);
    expect(resources.find(item => item.kind === "Service" && item.metadata.name === "agentmesh-relay")?.spec.selector)
      .toEqual({ app: "agentmesh-relay" });
  });

  it.each(["upgrade", "status"])("propagates failed Helm apply/rollout step %s", async step => {
    const { execute } = mockInstallation(true, (bin, args) => {
      if ((step === "upgrade" && bin === "helm" && args[0] === step)
        || (step === "status" && bin === "kubectl" && args[0] === "rollout" && args[1] === step)) throw new Error(`failed ${step}`);
    });
    await expect(applyPushedImages(execute, [{ name: "relay", image: "mirror.azurecr.io/relay:latest" }], "chart"))
      .rejects.toThrow(`failed ${step}`);
  });

  it("propagates a failed legacy image update", async () => {
    const { execute } = mockInstallation(false, (_bin, args) => { if (args[0] === "set") throw new Error("set image denied"); });
    await expect(applyPushedImages(execute, [{ name: "relay", image: "mirror.azurecr.io/relay:latest" }], "chart"))
      .rejects.toThrow("set image denied");
    expect(recorded(execute).some(([, args]) => args[0] === "rollout")).toBe(false);
  });

  it("rejects inventory failures before any mutation", async () => {
    const execute = vi.fn().mockRejectedValue(new Error("inventory denied")) as unknown as Execute;
    await expect(applyPushedImages(execute, [{ name: "relay", image: "mirror.azurecr.io/relay:latest" }], "chart"))
      .rejects.toThrow("inventory denied");
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it("rejects partial/mixed ownership instead of adopting a Service or deployment", async () => {
    const { execute, resources } = mockInstallation(true);
    resources[1].metadata.annotations = {};
    resources[1].metadata.labels = {};
    await expect(inspectMeshInstallation(execute)).rejects.toThrow("mixed or incomplete");
    expect(recorded(execute).some(([bin]) => bin === "helm")).toBe(false);
  });

  it("classifies another controller's installation as external", async () => {
    const { execute, resources } = mockInstallation(false);
    for (const resource of resources) resource.metadata.labels = { "app.kubernetes.io/managed-by": "another-controller" };
    expect((await inspectMeshInstallation(execute)).kind).toBe("external");
  });

  it("classifies Kubernetes-owned resources as external rather than unmanaged", async () => {
    const { execute, resources } = mockInstallation(false);
    resources[0].metadata.ownerReferences = [{ controller: true }];
    expect((await inspectMeshInstallation(execute)).kind).toBe("external");
    expect(recorded(execute).some(([, args]) => args[0] === "set" || args[0] === "upgrade")).toBe(false);
  });

  it("rejects an ownership change during image-build preparation", async () => {
    const { execute, resources } = mockInstallation(false);
    const original = await inspectMeshInstallation(execute);
    for (const resource of resources) {
      resource.metadata.labels = { "app.kubernetes.io/managed-by": "Helm" };
      resource.metadata.annotations = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" };
    }
    await expect(applyMeshImages(execute, original, { relay: "mirror.azurecr.io/relay:latest" }, "chart"))
      .rejects.toThrow("ownership changed");
    expect(recorded(execute).some(([, args]) => args[0] === "set" || args[0] === "upgrade" || args[0] === "apply")).toBe(false);
  });

  it("does not report an old/unavailable owned deployment as healthy", async () => {
    const { execute, resources } = mockInstallation(true);
    const mesh = await inspectMeshInstallation(execute);
    resources[0].status!.readyReplicas = 0;
    await expect(verifyMeshHealth(execute, mesh)).rejects.toThrow("healthy rollout");
  });

  it("requires actual image evidence, not just Available=True", async () => {
    const { execute } = mockInstallation(true);
    const mesh = await inspectMeshInstallation(execute);
    await expect(verifyMeshHealth(execute, mesh, { relay: "mirror.azurecr.io/relay:new" })).rejects.toThrow("selected image");
  });

  it("preserves absent maps but rejects values that imply ownership adoption", () => {
    expect(helmMeshEnabled({})).toBe(false);
    expect(helmMeshEnabled({ agentMesh: null })).toBe(false);
    expect(helmMeshEnabled({ agentMesh: { enabled: false } })).toBe(false);
    expect(() => helmMeshEnabled({ agentMesh: { enabled: "false" } })).toThrow("boolean");
    expect(() => assertMeshReleaseConsistency({ kind: "legacy", namespace: "agentmesh", deployments: [] },
      { agentMesh: { enabled: true } })).toThrow("ownership");
    expect(meshImageValueArgs({ registry: "mirror.azurecr.io/agentmesh-registry-agt:v1.2.3" }))
      .toContain("agentMesh.registry.image.tag=v1.2.3");
  });
});
