// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { controllerEnv, coreImageValues, PUSH_COMPONENTS, pushImageTag } from "./image-targets.js";
import { inspectManagedMcpPlans, refreshManagedMcpImages } from "./managed-mcp-image-apply.js";
import type { Execute } from "./deployment-target.js";

const image = `registry.azurecr.io/mcp-everything:latest@sha256:${"a".repeat(64)}`;
const source = {
  metadata: { name: "tools", namespace: "workspace", uid: "source", resourceVersion: "1", generation: 2 },
  spec: { managed: { preset: "everything" }, allowedTools: ["echo"] },
  status: { phase: "Ready", observedGeneration: 2, workloadImage: image, workloadGeneration: 3,
    workloadRef: "kars-mcp/mcp-tools", managedNamespaceUid: "managed-ns" },
};
function fixture(preset = "everything") {
  const image = `registry.azurecr.io/mcp-${preset}:latest@sha256:${"a".repeat(64)}`;
  const current = structuredClone(source);
  current.spec.managed.preset = preset;
  current.status.workloadImage = image;
  const inventory = [current];
  const annotations = { "kars.azure.com/mcp-source-namespace": "workspace", "kars.azure.com/mcp-source-name": "tools",
    "kars.azure.com/mcp-source-uid": "source", "kars.azure.com/mcp-namespace-uid": "managed-ns" };
  const metadata = { name: "mcp-tools", namespace: "kars-mcp", uid: "resource", resourceVersion: "1", generation: 3,
    annotations, labels: { "app.kubernetes.io/managed-by": "kars-controller" } };
  const selector = { "kars.azure.com/mcp-source-uid": "source" };
  const resources: Record<string, any> = {
    mcpserver: current,
    deployment: { metadata: structuredClone(metadata), spec: { selector: { matchLabels: selector },
      template: { spec: { containers: [{ name: "mcp", image }] } } },
    status: { observedGeneration: 3, availableReplicas: 1, updatedReplicas: 1 } },
    service: { metadata: structuredClone(metadata), spec: { selector: structuredClone(selector) } },
  };
  const execute = vi.fn(async (_file: string, args: readonly string[]) => {
    if (args[0] === "get" && args[1] === "mcpservers") return { stdout: JSON.stringify({ items: inventory }) };
    if (args[0] === "get" && args[1] === "namespace") return { stdout: JSON.stringify(args[2] === "kars-system"
      ? { metadata: { name: "kars-system", uid: "system", resourceVersion: "1" } }
      : { metadata: { name: "kars-mcp", uid: "managed-ns", resourceVersion: "1", annotations: {
          "kars.azure.com/mcp-namespace-claim": "v1", "kars.azure.com/mcp-controller-namespace": "kars-system",
          "kars.azure.com/mcp-controller-namespace-uid": "system" } } }) };
    if (args[0] === "get" && resources[args[1]]) return { stdout: JSON.stringify(resources[args[1]]) };
    if (["patch", "wait"].includes(args[0])) return { stdout: "" };
    throw new Error("Unexpected command");
  }) as unknown as Execute & ReturnType<typeof vi.fn>;
  return { execute, resources, image, inventory };
}

describe("managed MCP image application", () => {
  it.each(["everything", "playwright"])("maps %s to its owning Helm value and controller environment", preset => {
    const name = `mcp-${preset}`;
    expect(PUSH_COMPONENTS).toContain(name);
    expect(pushImageTag(name)).toBe(`${name}:latest`);
    expect(controllerEnv(name)).toBe(`MCP_${preset.toUpperCase()}_IMAGE`);
    expect(coreImageValues([{ name, image }])).toEqual({ [`managedMcp.${preset}Image`]: image });
  });
  it("leaves unrelated component inventories untouched", async () => {
    const { execute } = fixture();
    expect(await inspectManagedMcpPlans(execute, [{ name: "controller", image }])).toEqual([]);
    expect(execute).not.toHaveBeenCalled();
  });
  it.each(["everything", "playwright"])("CAS-triggers only %s and verifies its actual artifact, selector and probed generation", async preset => {
    const { execute, image } = fixture(preset);
    const plans = await inspectManagedMcpPlans(execute, [{ name: `mcp-${preset}`, image }]);
    expect(await refreshManagedMcpImages(execute, plans, "kars-system")).toBe(1);
    const calls = execute.mock.calls.map(call => call[1] as string[]);
    const patch = calls.find(args => args[0] === "patch")!;
    expect(JSON.parse(patch.at(-1)!).metadata).toMatchObject({ uid: "source", resourceVersion: "1" });
    expect(calls.some(args => args[0] === "patch" && args[1] === "deployment")).toBe(false);
  });
  it("selects preset-specific images without including unrelated servers", async () => {
    const { execute, inventory } = fixture();
    const browser = structuredClone(source);
    browser.metadata.name = "browser";
    browser.metadata.uid = "browser-source";
    browser.spec.managed.preset = "playwright";
    inventory.push(browser);
    const browserImage = image.replace("mcp-everything", "mcp-playwright");
    const onlyBrowser = await inspectManagedMcpPlans(execute, [{ name: "mcp-playwright", image: browserImage }]);
    expect(onlyBrowser.map(plan => [plan.source.metadata.name, plan.image])).toEqual([["browser", browserImage]]);
    const both = await inspectManagedMcpPlans(execute, [
      { name: "mcp-everything", image }, { name: "mcp-playwright", image: browserImage },
    ]);
    expect(both.map(plan => [plan.source.metadata.name, plan.image])).toEqual([["tools", image], ["browser", browserImage]]);
  });
  it.each(["everything", "playwright"].flatMap(preset =>
    ["selector", "image", "probe", "owner"].map(field => [preset, field]),
  ))("rejects %s %s mismatches instead of reporting applied", async (preset, field) => {
    const { execute, resources, image } = fixture(preset);
    const plans = await inspectManagedMcpPlans(execute, [{ name: `mcp-${preset}`, image }]);
    if (field === "selector") resources.service.spec.selector = { app: "foreign" };
    if (field === "image") resources.deployment.spec.template.spec.containers[0].image = "old:latest";
    if (field === "probe") resources.mcpserver.status.workloadGeneration = 1;
    if (field === "owner") resources.deployment.metadata.annotations["kars.azure.com/mcp-source-uid"] = "foreign";
    await expect(refreshManagedMcpImages(execute, plans, "kars-system")).rejects.toThrow("Managed MCP");
  });
});
