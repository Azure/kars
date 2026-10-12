// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { hasCoreHeadlamp } from "./headlamp-ownership.js";

const run = vi.hoisted(() => vi.fn());
vi.mock("execa", () => ({ execa: run }));

describe("Core Headlamp ownership", () => {
  beforeEach(() => run.mockReset());

  it("recognizes only the Helm-owned packaged plugin and preserves explicit targeting", async () => {
    run.mockResolvedValue({ stdout: JSON.stringify({ kind: "Deployment", metadata: {
      annotations: { "meta.helm.sh/release-name": "core", "kars.azure.com/headlamp-plugin": "bundled" },
    } }) });
    expect(await hasCoreHeadlamp("kind-isolated", "headlamp", "/tools/kubectl")).toBe(true);
    expect(run).toHaveBeenCalledWith("/tools/kubectl", [
      "--context", "kind-isolated", "get", "deployment", "headlamp",
      "-n", "headlamp", "--ignore-not-found", "-o", "json",
    ], { stdio: "pipe" });
  });

  it("retains standalone behavior for absence, external charts and unowned annotations", async () => {
    for (const stdout of [
      "",
      JSON.stringify({ kind: "Deployment", metadata: {} }),
      JSON.stringify({ kind: "Deployment", metadata: { annotations: { "meta.helm.sh/release-name": "external" } } }),
      JSON.stringify({ kind: "Deployment", metadata: { annotations: { "kars.azure.com/headlamp-plugin": "bundled" } } }),
    ]) {
      run.mockResolvedValueOnce({ stdout });
      expect(await hasCoreHeadlamp("kind-isolated")).toBe(false);
    }
  });

  it("recognizes Core template-and-apply ownership without inventing a Helm release", async () => {
    run.mockResolvedValue({ stdout: JSON.stringify({ kind: "Deployment", metadata: {
      annotations: { "kars.azure.com/headlamp-owner": "kars-system/kars", "kars.azure.com/headlamp-plugin": "bundled" },
    } }) });
    expect(await hasCoreHeadlamp("kind-isolated")).toBe(true);
  });

  it("refuses conflicting or malformed Core ownership rather than replacing the deployment", async () => {
    for (const annotations of [
      { "kars.azure.com/headlamp-owner": "broken" },
      { "kars.azure.com/headlamp-owner": "kars-system/kars", "meta.helm.sh/release-name": "another", "meta.helm.sh/release-namespace": "kars-system" },
    ]) {
      run.mockResolvedValueOnce({ stdout: JSON.stringify({ kind: "Deployment", metadata: {
        annotations: { ...annotations, "kars.azure.com/headlamp-plugin": "bundled" },
      } }) });
      await expect(hasCoreHeadlamp("kind-isolated")).rejects.toThrow(/Headlamp owner/);
    }
  });

  it("does not reinterpret denied reads or invalid responses as permission to replace Headlamp", async () => {
    run.mockRejectedValueOnce(new Error("Forbidden"));
    await expect(hasCoreHeadlamp("kind-isolated")).rejects.toThrow("Forbidden");
    run.mockResolvedValueOnce({ stdout: "not json" });
    await expect(hasCoreHeadlamp("kind-isolated")).rejects.toThrow();
    run.mockResolvedValueOnce({ stdout: JSON.stringify({ kind: "Status", metadata: {} }) });
    await expect(hasCoreHeadlamp("kind-isolated")).rejects.toThrow();
  });

  it("never applies manifests, mounts another plugin or mints a token for the Core-owned deployment", async () => {
    run.mockResolvedValue({ stdout: JSON.stringify({ kind: "Deployment", metadata: {
      annotations: { "meta.helm.sh/release-name": "core", "kars.azure.com/headlamp-plugin": "bundled" },
    } }) });
    const { installHeadlamp, installKarsPlugin, mintHeadlampToken } = await import("../commands/up/headlamp_stack.js");
    await installHeadlamp({ context: "kind-isolated" });
    await installKarsPlugin({ context: "kind-isolated" });
    expect(await mintHeadlampToken("kind-isolated")).toBeNull();
    expect(run).toHaveBeenCalledTimes(3);
    for (const call of run.mock.calls) expect(call[1].slice(2, 4)).toEqual(["get", "deployment"]);
  });
});
