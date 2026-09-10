// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { execa } from "execa";
import type { Execute } from "./sre-authority.js";
import { listSreHelmReleases, sreHelmStageWait } from "./sre-helm.js";

const inventory = JSON.stringify([{ name: "kars", namespace: "workspace", status: "pending-upgrade" }]);
const unsupported = () => Object.assign(new Error("Helm flag rejected"), {
  exitCode: 1, stderr: "Error: unknown flag: --all\n",
});

afterEach(() => vi.restoreAllMocks());

describe("SRE Helm release inventory compatibility", () => {
  it("handles the installed Helm flag parser without contacting a cluster", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    // Help keeps listing offline, but still rejects unsupported flags.
    const execute: Execute = (file, args, options) =>
      execa(file, [...args, ...(args[0] === "list" ? ["--help"] : [])], options);
    const output = await listSreHelmReleases(execute, "workspace");
    expect(output).toContain("helm list");
    expect(output).toContain("--all-namespaces");
  });

  it("retains Helm 3 all-status discovery without an extra probe", async () => {
    const execute = vi.fn<Execute>().mockResolvedValue({ stdout: inventory });
    expect(await listSreHelmReleases(execute, "workspace")).toBe(inventory);
    expect(execute.mock.calls).toEqual([
      ["helm", ["list", "-n", "workspace", "--all", "-o", "json"], { stdio: "pipe" }],
    ]);
  });

  it.each(["v4.0.0", "v4.2.4", "v4.1.0-rc.1+build.2"])(
    "uses confirmed Helm %s all-status defaults and preserves executor context", async version => {
      vi.spyOn(console, "warn").mockImplementation(() => {});
      const execute = vi.fn<Execute>()
        .mockRejectedValueOnce(unsupported())
        .mockResolvedValueOnce({ stdout: version })
        .mockResolvedValueOnce({ stdout: inventory });
      const contextual: Execute = (file, args, options) =>
        execute(file, ["--kube-context", "test-context", ...args], options);
      expect(await listSreHelmReleases(contextual, "workspace")).toBe(inventory);
      expect(execute.mock.calls.map(([, args]) => args)).toEqual([
        ["--kube-context", "test-context", "list", "-n", "workspace", "--all", "-o", "json"],
        ["--kube-context", "test-context", "version", "--template", "{{.Version}}"],
        ["--kube-context", "test-context", "list", "-n", "workspace", "-o", "json"],
      ]);
    },
  );

  it.each([
    { exitCode: 1, stderr: "Forbidden: release storage access denied" },
    { exitCode: 1, stderr: "Error: unknown flag: --all-namespaces" },
    { exitCode: 1, stderr: "Error: unknown flag: --all\nanother failure" },
    { exitCode: 2, stderr: "Error: unknown flag: --all" },
  ])("does not retry another discovery failure: %j", async detail => {
    const error = Object.assign(new Error("Discovery failed"), detail);
    const execute = vi.fn<Execute>().mockRejectedValue(error);
    await expect(listSreHelmReleases(execute, "workspace")).rejects.toBe(error);
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it.each(["v3.19.0", "v5.0.0", "", "unexpected output"])(
    "does not assume default all-status semantics for %j", async version => {
      const execute = vi.fn<Execute>().mockRejectedValueOnce(unsupported())
        .mockResolvedValueOnce({ stdout: version });
      await expect(listSreHelmReleases(execute, "workspace")).rejects.toThrow(/only Helm 4|Unsupported Helm version/);
      expect(execute).toHaveBeenCalledTimes(2);
    },
  );

  it("propagates a failed Helm 4 retry rather than reporting an absent release", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const denied = new Error("Forbidden");
    const execute = vi.fn<Execute>().mockRejectedValueOnce(unsupported())
      .mockResolvedValueOnce({ stdout: "v4.2.4" }).mockRejectedValueOnce(denied);
    await expect(listSreHelmReleases(execute, "workspace")).rejects.toBe(denied);
    expect(execute).toHaveBeenCalledTimes(3);
  });

  describe("SRE staging waits only for built-ins before explicit enrollment", () => {
    it.each([["v3.16.4", "--wait"], ["v4.2.4", "--wait=legacy"], ["v4.0.0-rc.1", "--wait=legacy"]])(
      "selects %s's bounded built-in waiter", async (version, expected) => {
        const execute = vi.fn<Execute>().mockResolvedValue({ stdout: version });
        expect(await sreHelmStageWait(execute)).toBe(expected);
        expect(execute.mock.calls).toEqual([
          ["helm", ["version", "--template", "{{.Version}}"], { stdio: "pipe" }],
        ]);
      },
    );
    it.each(["", "v5.0.0", "v4.2", "v3.16.4\nwarning", "unknown"])("fails closed for %j", async version => {
      await expect(sreHelmStageWait(vi.fn<Execute>().mockResolvedValue({ stdout: version })))
        .rejects.toThrow("Unsupported Helm");
    });
    it("does not replace version errors with a default waiter", async () => {
      const error = new Error("Helm unavailable");
      await expect(sreHelmStageWait(vi.fn<Execute>().mockRejectedValue(error))).rejects.toBe(error);
    });
    it("passes the real installed Helm upgrade parser without a Kubernetes connection", async () => {
      const wait = await sreHelmStageWait(execa);
      const { stdout } = await execa("helm", ["upgrade", "kars", "chart", wait, "--timeout", "8m", "--help"], { stdio: "pipe" });
      expect(stdout).toContain("helm upgrade");
    });
  });
});
