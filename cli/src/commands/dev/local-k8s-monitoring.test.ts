// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { startMonitoringPortForwards } from "./local-k8s.js";
import { startPortForwards } from "../up/headlamp_stack.js";
import { headlampCommand } from "../headlamp.js";
import type { SchemaExecute } from "../../lib/schema-documents.js";

const { execute, spawn, unref } = vi.hoisted(() => ({
  execute: vi.fn<SchemaExecute>(), spawn: vi.fn(), unref: vi.fn(),
}));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("node:child_process", () => ({ spawn }));
vi.mock("../../lib/headlamp-ownership.js", () => ({
  hasCoreHeadlamp: async () => true,
  HEADLAMP_OPERATOR_LOGIN: "Use the configured Core sign-in",
}));

beforeEach(() => {
  vi.useFakeTimers();
  spawn.mockReturnValue({ unref });
});
afterEach(() => {
  vi.useRealTimers();
  vi.resetAllMocks();
});

function discovered(stdout: string) {
  execute.mockImplementation(async (file, args) => {
    if (file === "lsof") throw new Error("No listener");
    expect(args.slice(0, 2)).toEqual(["--context", "kind-chosen"]);
    return { stdout };
  });
}

describe("optional monitoring forwards", () => {
  it("does not touch any port when no monitoring service exists", async () => {
    discovered("");
    expect(await startMonitoringPortForwards({ kubectl: "/tools/kubectl" }, "kind-chosen", 3000, 19091))
      .toEqual({ grafana: false, prometheus: false });
    expect(spawn).not.toHaveBeenCalled();
    expect(execute.mock.calls.every(([file]) => file === "/tools/kubectl")).toBe(true);
  });

  it("forwards only Prometheus on the selected context without touching Grafana's port", async () => {
    discovered("service/kps-kube-prometheus-stack-prometheus\n");
    const result = startMonitoringPortForwards({ kubectl: "/tools/kubectl" }, "kind-chosen", 3000, 19091);
    await vi.runAllTimersAsync();
    expect(await result).toEqual({ grafana: false, prometheus: true });
    expect(spawn).toHaveBeenCalledExactlyOnceWith("/tools/kubectl", [
      "--context", "kind-chosen", "port-forward", "-n", "monitoring",
      "service/kps-kube-prometheus-stack-prometheus", "19091:9090",
    ], { detached: true, stdio: "ignore" });
    expect(execute.mock.calls.some(([, args]) => args.includes(":3000"))).toBe(false);
  });

  it("retains both forwards when both services are installed", async () => {
    discovered("service/kps-grafana\nservice/kps-kube-prometheus-stack-prometheus\n");
    const result = startMonitoringPortForwards({ kubectl: "/tools/kubectl" }, "kind-chosen", 3000, 19091);
    await vi.runAllTimersAsync();
    expect(await result).toEqual({ grafana: true, prometheus: true });
    expect(spawn).toHaveBeenCalledTimes(2);
    expect(unref).toHaveBeenCalledTimes(2);
  });

  it("keeps the shared Headlamp stack from forwarding absent Grafana", async () => {
    discovered("service/kps-kube-prometheus-stack-prometheus\n");
    const result = startPortForwards({ context: "kind-chosen", headlampPort: 4466, grafanaPort: 3000, prometheusPort: 19091 });
    await vi.runAllTimersAsync();
    expect(await result).toEqual({ grafana: false, prometheus: true });
    expect(spawn).toHaveBeenCalledTimes(2);
    expect(spawn.mock.calls.every(([, args]) => !args.includes("service/kps-grafana"))).toBe(true);
    expect(execute.mock.calls.some(([, args]) => args.includes(":3000"))).toBe(false);
  });

  it("surfaces discovery failure before creating or replacing forwards", async () => {
    execute.mockRejectedValue(new Error("Forbidden"));
    await expect(startMonitoringPortForwards({ kubectl: "/tools/kubectl" }, "kind-chosen", 3000, 19091))
      .rejects.toThrow("Forbidden");
    expect(spawn).not.toHaveBeenCalled();
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["", false, false],
    ["service/kps-kube-prometheus-stack-prometheus\n", false, true],
    ["service/kps-grafana\nservice/kps-kube-prometheus-stack-prometheus\n", true, true],
  ])("prints only discovered monitoring URLs from kars headlamp (%s)", async (stdout, grafana, prometheus) => {
    discovered(stdout);
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    const result = headlampCommand().parseAsync(["node", "headlamp", "--context", "kind-chosen", "--no-browser"]);
    await vi.runAllTimersAsync();
    await result;
    const output = log.mock.calls.flat().join("\n");
    expect(output.includes("Grafana:")).toBe(grafana);
    expect(output.includes("Prometheus:")).toBe(prometheus);
    expect(spawn).toHaveBeenCalledTimes(1 + Number(grafana) + Number(prometheus));
    log.mockRestore();
  });
});
