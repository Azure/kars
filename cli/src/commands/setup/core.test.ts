// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { inspectSetupCore } from "./core.js";
import type { BetaExecute } from "../dev/kind-beta.js";

function fixture() {
  const namespace: any = { metadata: { uid: "namespace-uid" } };
  const controller: any = { metadata: { uid: "controller-uid", resourceVersion: "12",
    labels: { "app.kubernetes.io/managed-by": "Helm" },
    annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" } } };
  const releases: any[] = [{ name: "kars", namespace: "kars-system", chart: "kars-0.2.0-beta.6", status: "deployed" }];
  const values: any = { controller: { inferenceDefaults: { provider: "anthropic", model: "claude-sonnet-4-5" } }, apiKey: "private-value", privateConfig: { token: "private-token" } };
  const calls: Array<{ tool: string; args: string[] }> = [];
  let change: (() => void) | undefined;
  const execute: BetaExecute = async (tool, args) => {
    calls.push({ tool, args });
    expect(args.slice(0, 2)).toEqual([tool === "helm" ? "--kube-context" : "--context", "selected-aks"]);
    if (args.includes("namespace")) return { stdout: namespace.metadata ? JSON.stringify(namespace) : "" };
    if (args.includes("deployment")) return { stdout: controller.metadata ? JSON.stringify(controller) : "" };
    if (args.includes("list")) return { stdout: JSON.stringify(releases) };
    if (args.includes("values")) { change?.(); return { stdout: JSON.stringify(values) }; }
    throw Error("Unexpected inventory operation");
  };
  return { namespace, controller, releases, values, calls, execute, setChange: (callback: () => void) => { change = callback; } };
}

describe("existing Core discovery", () => {
  it("returns only owned release identities and public defaults, without inspecting credential Secrets", async () => {
    const f = fixture();
    const result = await inspectSetupCore("selected-aks", f.execute);
    expect(result).toEqual({ context: "selected-aks", status: "installed", namespaceUid: "namespace-uid", controllerUid: "controller-uid", release: "kars", chart: "kars-0.2.0-beta.6", provider: "anthropic", model: "claude-sonnet-4-5" });
    expect(JSON.stringify(result)).not.toContain("private");
    expect(f.calls.flatMap(call => call.args)).not.toContain("secret");
    expect(f.calls.every(call => call.args.includes("get") || call.args.includes("list"))).toBe(true);
  });
  it("does not equate namespace presence with installed Core", async () => {
    const f = fixture(); delete f.controller.metadata;
    expect((await inspectSetupCore("selected-aks", f.execute)).status).toBe("unmanaged");
    expect(f.calls.some(call => call.tool === "helm")).toBe(false);
  });
  it("recognizes an absent namespace without masking authorization errors", async () => {
    const f = fixture(); delete f.namespace.metadata;
    expect(await inspectSetupCore("selected-aks", f.execute)).toEqual({ context: "selected-aks", status: "absent" });
    expect(f.calls).toHaveLength(1);
    await expect(inspectSetupCore("selected-aks", async () => { throw Error("Forbidden"); })).rejects.toThrow("Forbidden");
  });
  it.each(["manager", "release", "namespace", "terminating", "version"])("does not adopt a controller with foreign %s", async field => {
    const f = fixture();
    if (field === "manager") f.controller.metadata.labels["app.kubernetes.io/managed-by"] = "Other";
    if (field === "release") f.controller.metadata.annotations["meta.helm.sh/release-name"] = "other";
    if (field === "namespace") f.controller.metadata.annotations["meta.helm.sh/release-namespace"] = "other";
    if (field === "terminating") f.controller.metadata.deletionTimestamp = "2026-11-13T00:00:00Z";
    if (field === "version") delete f.controller.metadata.resourceVersion;
    expect((await inspectSetupCore("selected-aks", f.execute)).status).toBe("unmanaged");
  });
  it.each(["failed", "pending-install", "uninstalled"])("blocks a %s release without reading its values", async status => {
    const f = fixture(); f.releases[0].status = status;
    await expect(inspectSetupCore("selected-aks", f.execute)).rejects.toThrow("not deployed");
    expect(f.calls.some(call => call.args.includes("values"))).toBe(false);
  });
  it.each(["missing", "duplicate", "foreign-chart"])("rejects %s releases", async mismatch => {
    const f = fixture();
    if (mismatch === "missing") f.releases.length = 0;
    if (mismatch === "duplicate") f.releases.push(f.releases[0]);
    if (mismatch === "foreign-chart") f.releases[0].chart = "unrelated-1.0.0";
    await expect(inspectSetupCore("selected-aks", f.execute)).rejects.toThrow("Core release");
  });
  it.each(["namespace", "controller", "version", "deleted"])("rejects %s changes during the read", async changed => {
    const f = fixture(); f.setChange(() => {
      if (changed === "namespace") f.namespace.metadata.uid = "replacement";
      if (changed === "controller") f.controller.metadata.uid = "replacement";
      if (changed === "version") f.controller.metadata.resourceVersion = "13";
      if (changed === "deleted") delete f.controller.metadata;
    });
    await expect(inspectSetupCore("selected-aks", f.execute)).rejects.toThrow("changed during discovery");
  });
  it("does not invent defaults or expose unexpected values", async () => {
    const f = fixture(); f.values.controller.inferenceDefaults = { provider: { apiKey: "private" }, model: "<script>private</script>" };
    expect(await inspectSetupCore("selected-aks", f.execute)).toMatchObject({ status: "installed", provider: undefined, model: undefined });
  });
});
