// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { installMonitoring } from "./monitoring.js";
import { planMonitoringSchemas } from "../../lib/monitoring-schemas.js";

const { execute } = vi.hoisted(() => ({
  execute: vi.fn<(file: string, args: string[], options?: unknown) => Promise<{ stdout: string }>>(),
}));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("../../lib/monitoring-schemas.js", async importOriginal => ({
  ...await importOriginal<typeof import("../../lib/monitoring-schemas.js")>(),
  planMonitoringSchemas: vi.fn(),
}));
const owned = {
  "kars.azure.com/monitoring-owner": "kars-system/kars",
  "meta.helm.sh/release-name": "kars",
  "meta.helm.sh/release-namespace": "kars-system",
};
function namespace(annotations: Partial<Record<string, string>>) {
  execute.mockResolvedValueOnce({ stdout: JSON.stringify({
    kind: "Namespace", metadata: { name: "monitoring", annotations },
  }) });
}
beforeEach(() => { vi.resetAllMocks(); });

describe("CLI monitoring ownership", () => {
  it("reuses Core monitoring without Helm, apply, namespace relabeling or image changes", async () => {
    namespace(owned);
    await installMonitoring("fresh", { kubectl: "/tools/kubectl", helm: "/tools/helm" });
    expect(execute).toHaveBeenCalledTimes(1);
    expect(execute).toHaveBeenCalledWith("/tools/kubectl",
      ["--context", "fresh", "get", "namespace", "monitoring", "--ignore-not-found", "-o", "json"],
      expect.anything());
  });
  it.each([
    { ...owned, "meta.helm.sh/release-name": "foreign" },
    { "meta.helm.sh/release-name": "other", "meta.helm.sh/release-namespace": "elsewhere" },
    { "kars.azure.com/monitoring-owner": "undefined/undefined" },
  ])("refuses external or ambiguous ownership without mutation", async annotations => {
    namespace(annotations);
    await expect(installMonitoring("fresh")).rejects.toThrow("ownership");
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("does not bypass Core's monitoring opt-out or overwrite an older Core installation", async () => {
    execute.mockResolvedValueOnce({ stdout: "" });
    execute.mockResolvedValueOnce({ stdout: JSON.stringify({
      kind: "Deployment", metadata: { name: "kars-controller" },
    }) });
    await expect(installMonitoring("fresh")).rejects.toThrow("reviewed Core upgrade");
    expect(execute).toHaveBeenCalledTimes(2);
  });
  it("propagates permission and transport failures instead of treating them as absence", async () => {
    execute.mockRejectedValueOnce(new Error("Forbidden"));
    await expect(installMonitoring("fresh")).rejects.toThrow("Forbidden");
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("refuses malformed discovery responses", async () => {
    execute.mockResolvedValueOnce({ stdout: '{"kind":"Namespace","metadata":{"name":"other"}}' });
    await expect(installMonitoring("fresh")).rejects.toThrow("Invalid monitoring namespace");
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("does not relabel an unowned monitoring namespace", async () => {
    namespace({});
    await expect(installMonitoring("fresh")).rejects.toThrow("no proven Kars ownership");
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("publishes schemas before Helm runs certificate hooks, then applies addons and waits for real workloads", async () => {
    let published = false;
    vi.mocked(planMonitoringSchemas).mockResolvedValue(async () => { published = true; });
    execute.mockImplementation(async (file, args) => {
      const command = args;
      if (file === "/tools/helm" && command[0] === "template") {
        return { stdout: JSON.stringify({ apiVersion: "v1", kind: "ConfigMap",
          metadata: { name: command[1] === "kps" ? "upstream" : "addon", namespace: "monitoring" } }) };
      }
      if (command[0] === "upgrade") expect(published).toBe(true);
      return { stdout: "" };
    });
    await installMonitoring("fresh", { kubectl: "/tools/kubectl", helm: "/tools/helm" });
    const calls = execute.mock.calls;
    const helm = calls.findIndex(([file, args]) => file === "/tools/helm" && args?.[0] === "upgrade");
    const addon = calls.findIndex(([file, args]) => file === "/tools/kubectl" && args?.includes("apply"));
    expect(helm).toBeGreaterThan(0);
    expect(addon).toBeGreaterThan(helm);
    expect(calls[helm][1]).toEqual(expect.arrayContaining(["--kube-context", "fresh", "--skip-crds", "--wait"]));
    expect(calls.filter(([, args]) => args?.includes("apply"))).toHaveLength(1);
    expect(calls.some(([, args]) => args?.some(arg => arg.startsWith("--force")))).toBe(false);
    expect(calls.at(-1)![1]).toEqual(expect.arrayContaining([
      "rollout", "status", "statefulset/prometheus-kps-kube-prometheus-stack-prometheus",
    ]));
  });
  it("does not mutate the namespace or run Helm if schema review fails", async () => {
    execute.mockResolvedValue({ stdout: "" });
    vi.mocked(planMonitoringSchemas).mockRejectedValue(new Error("dependency mismatch"));
    await expect(installMonitoring("fresh")).rejects.toThrow("dependency mismatch");
    expect(execute.mock.calls.some(([, args]) => args?.some(arg => ["upgrade", "create", "apply"].includes(arg)))).toBe(false);
  });
});
