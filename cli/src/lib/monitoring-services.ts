// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execa } from "execa";
import type { SchemaExecute } from "./schema-documents.js";

export async function monitoringServices(
  context: string,
  kubectl = "kubectl",
  execute: SchemaExecute = execa,
): Promise<{ grafana: boolean; prometheus: boolean }> {
  if (!context.trim()) throw new Error("Monitoring discovery requires an explicit Kubernetes context");
  const grafana = "service/kps-grafana";
  const prometheus = "service/kps-kube-prometheus-stack-prometheus";
  const { stdout } = await execute(kubectl, [
    "--context", context, "get", "service", "kps-grafana", "kps-kube-prometheus-stack-prometheus",
    "-n", "monitoring", "--ignore-not-found", "-o", "name",
  ], { stdio: "pipe" });
  const names = stdout.trim() ? stdout.trim().split(/\r?\n/).map(name => name.trim()) : [];
  if (new Set(names).size !== names.length || names.some(name => name !== grafana && name !== prometheus)) {
    throw new Error("Invalid monitoring service discovery response");
  }
  return { grafana: names.includes(grafana), prometheus: names.includes(prometheus) };
}
