// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./deployment-target.js";

export async function restartController(execute: Execute): Promise<void> {
  await execute("kubectl", ["rollout", "restart", "deployment/kars-controller", "-n", "kars-system"], { stdio: "pipe" });
  await execute("kubectl", ["rollout", "status", "deployment/kars-controller", "-n", "kars-system", "--timeout=300s"], { stdio: "pipe" });
}

export async function restartSandboxes(execute: Execute): Promise<void> {
  const { stdout } = await execute("kubectl", [
    "get", "deployments", "-A", "-l", "kars.azure.com/component=sandbox", "-o", "json",
  ], { stdio: "pipe" });
  const list = JSON.parse(String(stdout)) as { items?: Array<{ metadata?: { name?: string; namespace?: string } }> };
  if (!Array.isArray(list.items)) throw new Error("Sandbox deployment inventory returned invalid JSON");
  for (const deployment of list.items) {
    const { name, namespace } = deployment.metadata ?? {};
    if (!name || !namespace) throw new Error("Sandbox deployment inventory omitted its name or namespace");
    await execute("kubectl", ["rollout", "restart", `deployment/${name}`, "-n", namespace], { stdio: "pipe" });
    await execute("kubectl", ["rollout", "status", `deployment/${name}`, "-n", namespace, "--timeout=300s"], { stdio: "pipe" });
  }
}
