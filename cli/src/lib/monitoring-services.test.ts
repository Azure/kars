// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { monitoringServices } from "./monitoring-services.js";
import type { SchemaExecute } from "./schema-documents.js";

afterEach(() => vi.restoreAllMocks());

describe("monitoring service discovery", () => {
  it.each([
    ["", false, false],
    ["service/kps-grafana\n", true, false],
    ["service/kps-kube-prometheus-stack-prometheus\n", false, true],
    ["service/kps-grafana\r\nservice/kps-kube-prometheus-stack-prometheus\r\n", true, true],
  ])("reports installed services, not assumed defaults (%s)", async (stdout, grafana, prometheus) => {
    const execute = vi.fn<SchemaExecute>().mockResolvedValue({ stdout });
    expect(await monitoringServices("chosen-context", "/tools/kubectl", execute)).toEqual({ grafana, prometheus });
    expect(execute).toHaveBeenCalledExactlyOnceWith("/tools/kubectl", [
      "--context", "chosen-context", "get", "service", "kps-grafana",
      "kps-kube-prometheus-stack-prometheus", "-n", "monitoring", "--ignore-not-found", "-o", "name",
    ], { stdio: "pipe" });
  });

  it.each(["service/foreign", "service/kps-grafana\nservice/kps-grafana", "{}"])(
    "rejects an invalid service response (%s)", async stdout => {
      const execute = vi.fn<SchemaExecute>().mockResolvedValue({ stdout });
      await expect(monitoringServices("chosen", "kubectl", execute)).rejects.toThrow("Invalid monitoring");
    },
  );

  it.each(["Forbidden", "connection refused"])("does not report %s as an absent service", async message => {
    const failure = new Error(message);
    const execute = vi.fn<SchemaExecute>().mockRejectedValue(failure);
    await expect(monitoringServices("chosen", "kubectl", execute)).rejects.toBe(failure);
  });

  it("refuses ambient context discovery", async () => {
    const execute = vi.fn<SchemaExecute>();
    await expect(monitoringServices(" ", "kubectl", execute)).rejects.toThrow("explicit Kubernetes context");
    expect(execute).not.toHaveBeenCalled();
  });
});
