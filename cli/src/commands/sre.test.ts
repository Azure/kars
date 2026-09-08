// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sreCommand } from "./sre.js";

const { execute } = vi.hoisted(() => ({
  execute: vi.fn<(file: string, args: readonly string[], options?: unknown) => Promise<{ stdout: string }>>(),
}));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("../lib/repo-assets.js", () => ({ requireBundledAsset: () => "/test/chart" }));

const releases = JSON.stringify([{ name: "kars", namespace: "kars-system" }]);
const controller = JSON.stringify({
  apiVersion: "apps/v1", kind: "Deployment",
  metadata: { name: "kars-controller", namespace: "kars-system", uid: "controller-uid", resourceVersion: "1" },
});

function authority(args: readonly string[]): { stdout: string } {
  const metadata = (name: string, uid = name) => ({ name, uid, resourceVersion: "1", generation: 1 });
  const getIndex = args.indexOf("get");
  const resource = getIndex < 0 ? undefined : args[getIndex + 1];
  const name = getIndex < 0 ? undefined : args[getIndex + 2];
  let value: unknown;
  if (args.includes("can-i")) return { stdout: "yes" };
  if (resource === "crd") value = { metadata: metadata("karssreregistrations.kars.azure.com") };
  if (resource === "karssreregistrations.kars.azure.com") value = {
    metadata: metadata("canonical"),
    spec: {
      enabled: true,
      controller: { namespace: { name: "kars-system", uid: "kars-system" },
        deployment: { name: "kars-controller", uid: "controller-uid" }, release: currentRelease },
      sandbox: { namespace: "kars-system", name: "sre", uid: "source" },
      runtimeNamespace: { name: "kars-sre", uid: "kars-sre" },
    },
    status: { phase: "Ready", observedGeneration: 1, privacyRevision: "kars.azure.com/sre-privacy/v2" },
  };
  if (resource === "namespace") value = name === "kars-sre"
    ? { metadata: { ...metadata("kars-sre"), annotations: {
      "kars.azure.com/namespace-claim-version": "v1",
      "kars.azure.com/sandbox-namespace": "kars-system",
      "kars.azure.com/sandbox-name": "sre",
      "kars.azure.com/sandbox-uid": "source",
    } } }
    : { metadata: metadata("kars-system") };
  if (resource === "karssandbox") value = {
    metadata: { ...metadata("sre", "source"), namespace: "kars-system",
      annotations: { "kars.azure.com/namespace-uid": "kars-sre" } },
  };
  if (resource === "clusterrolebindings" || resource === "rolebindings") value = { items: [] };
  return { stdout: value ? JSON.stringify(value) : "" };
}

let currentRelease = "kars";

beforeEach(() => {
  currentRelease = "kars";
  execute.mockReset();
  vi.spyOn(console, "log").mockImplementation(() => {});
});
afterEach(() => vi.restoreAllMocks());

describe("SRE controller upgrade namespace preflight", () => {
  it.each(["upgrade", "template"])("preflights %s mode in the selected context before mutations", async mode => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: mode === "upgrade" ? releases : "[]" };
      if (file === "kubectl" && args.includes("deployment")) {
        expect(args).toContain("--ignore-not-found");
        return { stdout: controller };
      }
      if (file === "kubectl" && args.includes("karssandboxes")) {
        expect(args.slice(0, 2)).toEqual(["--context", "test-context"]);
        return { stdout: '{"items":[]}' };
      }
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait", "--context", "test-context"]);
    const calls = execute.mock.calls;
    const inspected = calls.findIndex(([file, args]) => file === "kubectl" && args.includes("karssandboxes"));
    const mutation = calls.findIndex(([file, args]) => file === "helm" && args[0] === mode);
    expect(inspected).toBeGreaterThanOrEqual(0);
    expect(mutation).toBeGreaterThan(inspected);
  });

  it("stops an existing release upgrade when ownership cannot be inspected", async () => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: releases };
      throw new Error("Forbidden (403)");
    });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("403");
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "upgrade")).toBe(false);
  });

  it.each(["timeout", "Forbidden (403)"])("never treats controller discovery %s as a fresh install", async error => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: "[]" };
      throw new Error(error);
    });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow(error);
    expect(execute.mock.calls).toHaveLength(2);
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] !== "list")).toBe(false);
  });

  it("propagates Helm discovery errors without attempting another install path", async () => {
    execute.mockRejectedValue(new Error("Helm authorization failed"));
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("authorization");
    expect(execute.mock.calls).toHaveLength(1);
  });

  it.each(["{}", "null", '[{"name":"kars","namespace":"other"}]'])("rejects invalid release inventory %s", async stdout => {
    execute.mockResolvedValue({ stdout });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("invalid release inventory");
    expect(execute.mock.calls).toHaveLength(1);
  });

  it.each(["null", "{}", '{"kind":"Deployment","metadata":{"name":"foreign"}}'])("rejects invalid controller identity %s", async stdout => {
    execute.mockImplementation(async (file, args) => ({
      stdout: file === "helm" && args[0] === "list" ? "[]" : stdout,
    }));
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("invalid controller identity");
    expect(execute.mock.calls).toHaveLength(2);
  });

  it("permits fresh installation only after successful inventory and explicit controller absence", async () => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: "[]" };
      if (args[0] === "-n") return { stdout: "" };
      if (args.includes("deployment") && args.includes("kars-controller")) return { stdout: controller };
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]);
    expect(execute.mock.calls.slice(0, 2).map(([file, args]) => [file, args[0]])).toEqual([
      ["helm", "list"], ["kubectl", "-n"],
    ]);
    expect(execute.mock.calls[1][1]).toContain("--ignore-not-found");
    const installs = execute.mock.calls.filter(([file, args]) => file === "helm" && args[0] === "install");
    expect(installs).toHaveLength(1);
    expect(installs[0][1]).toContain("sre.enabled=false");
    expect(execute.mock.calls.some(([, args]) => args.includes("--take-ownership") || args.includes("--force-conflicts"))).toBe(false);
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "upgrade" && args.includes("sre.enabled=true"))).toBe(true);
  });

  it.each([
    { target: "kars.prod", names: ["kars.prod"] },
    { target: "kars", names: ["other.release", "kars"] },
    { target: "kars", names: ["other.release"] },
    { target: "a".repeat(53), names: ["a".repeat(53)] },
  ])("accepts Helm-compatible release inventory for $target", async ({ target, names }) => {
    currentRelease = target;
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") {
        return { stdout: JSON.stringify(names.map(name => ({ name, namespace: "kars-system" }))) };
      }
      if (file === "kubectl" && args.includes("deployment") && args.includes("kars-controller")) return { stdout: controller };
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait", "--release", target]);
    const mode = names.includes(target) ? "upgrade" : "template";
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === mode && args[1] === target))
      .toBe(true);
    expect(execute.mock.calls.some(([, args]) => args.includes("karssandboxes"))).toBe(true);
  });

  it.each(["", ".kars", "kars.", "kars..prod", "kars.-prod", "Kars", "a".repeat(54)])(
    "rejects the Helm-invalid release name %s without installation",
    async name => {
      execute.mockResolvedValue({ stdout: JSON.stringify([{ name, namespace: "kars-system" }]) });
      await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
        .rejects.toThrow("invalid release inventory");
      expect(execute.mock.calls).toHaveLength(1);
    },
  );
});
