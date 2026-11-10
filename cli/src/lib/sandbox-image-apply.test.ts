// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { inspectSandboxPlans, refreshSandboxImages, type SandboxImagePlan } from "./sandbox-image-apply.js";
import type { Execute } from "./deployment-target.js";

const image = { name: "runtime-hermes", image: `registry.example/hermes:latest@sha256:${"a".repeat(64)}` };
function fixture(requested: string | undefined = "run-1", completed: string | undefined = "run-1") {
  const task = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsTask", metadata: {
    name: "mission", namespace: "workspace", uid: "task-uid", annotations: {
      "kars.azure.com/run-requested": requested, "kars.azure.com/run-completed": completed,
    } as Record<string, string | undefined>,
  } };
  const cr: SandboxImagePlan["cr"] = {
    metadata: { name: "mission", namespace: "workspace", uid: "sandbox-uid", ownerReferences: [{
      apiVersion: task.apiVersion, kind: task.kind, name: task.metadata.name, uid: task.metadata.uid, controller: true,
    }] },
    spec: { runtime: { kind: "Hermes", hermes: {} } },
  };
  const deployment: SandboxImagePlan["deployment"] = {
    metadata: { name: "mission", namespace: "kars-mission", generation: 1,
      labels: { "kars.azure.com/sandbox": "mission", "kars.azure.com/parent-namespace": "workspace" } },
    spec: { replicas: 1, template: { spec: { containers: [
      { name: "agent", image: "registry.example/hermes:old" },
      { name: "inference-router", image: "registry.example/router:old" },
    ], initContainers: [{ name: "egress-guard", image: "registry.example/openclaw:old" }] } } },
    status: { observedGeneration: 1, updatedReplicas: 1, readyReplicas: 1, conditions: [{ type: "Available", status: "True" }] },
  };
  let onWait = () => { deployment.spec.template.spec.containers[0].image = image.image; };
  const execute = vi.fn(async (_bin: string, args: readonly string[]) => {
    if (args[0] === "get") {
      const value = args[1] === "deployments" ? { items: [deployment] }
        : args[1] === "karssandboxes" ? { items: [cr] }
          : args[1] === "karssandbox" ? cr : args[1] === "karstask" ? task : deployment;
      return { stdout: JSON.stringify(value) };
    }
    if (args[0] === "wait") onWait();
    return { stdout: "" };
  });
  return { task, cr, deployment, execute: execute as unknown as Execute, calls: execute.mock.calls,
    onWait: (action: () => void) => { onWait = action; } };
}

describe("terminal Mission image preservation", () => {
  it("preserves all containers of an exact completed run without claiming an image pin or restart", async () => {
    const f = fixture();
    const original = structuredClone({ cr: f.cr, task: f.task, deployment: f.deployment });
    const plans = await inspectSandboxPlans(f.execute, [image, { ...image, name: "router" }, { ...image, name: "sandbox" }]);
    expect(plans[0].expected).toEqual([]);
    expect(plans[0].pinned).toBe(false);
    expect(await refreshSandboxImages(f.execute, plans)).toBe(0);
    expect(f.calls.every(([, args]) => args[0] === "get")).toBe(true);
    expect({ cr: f.cr, task: f.task, deployment: f.deployment }).toEqual(original);
  });

  it.each([[undefined, undefined], ["", ""], ["run-2", "run-1"], ["run-1", ""], ["", "run-1"]])(
    "does not treat missing, empty or previous completion as terminal (%s/%s)", async (requested, completed) => {
      const f = fixture();
      f.task.metadata.annotations["kars.azure.com/run-requested"] = requested;
      f.task.metadata.annotations["kars.azure.com/run-completed"] = completed;
      const plans = await inspectSandboxPlans(f.execute, [image]);
      expect(plans[0].expected).toHaveLength(1);
      expect(await refreshSandboxImages(f.execute, plans)).toBe(1);
      expect(f.calls.some(([, args]) => args[0] === "rollout" && args[1] === "status")).toBe(true);
      expect(f.calls.every(([, args]) => ["get", "wait"].includes(args[0])
        || (args[0] === "rollout" && args[1] === "status"))).toBe(true);
    },
  );

  it("leaves ordinary unowned sandboxes eligible without reading a Task", async () => {
    const f = fixture(); delete f.cr.metadata.ownerReferences;
    const plans = await inspectSandboxPlans(f.execute, [image]);
    expect(await refreshSandboxImages(f.execute, plans)).toBe(1);
    expect(f.calls.some(([, args]) => args[1] === "karstask")).toBe(false);
  });

  it("verifies an already installed digest without writing a protected workload", async () => {
    const f = fixture(); delete f.cr.metadata.ownerReferences;
    f.deployment.spec.template.spec.containers[0].image = image.image;
    const original = structuredClone(f.deployment);
    const plans = await inspectSandboxPlans(f.execute, [image]);
    expect(await refreshSandboxImages(f.execute, plans)).toBe(1);
    expect(f.deployment).toEqual(original);
    expect(f.calls.every(([, args]) => ["get", "wait"].includes(args[0])
      || (args[0] === "rollout" && args[1] === "status"))).toBe(true);
  });

  it("rejects a floating tag rather than claiming a fresh artifact without a rollout", async () => {
    const f = fixture(); delete f.cr.metadata.ownerReferences;
    const plans = await inspectSandboxPlans(f.execute, [{ ...image, image: "registry.example/hermes:latest" }]);
    await expect(refreshSandboxImages(f.execute, plans)).rejects.toThrow("requires immutable artifact digests");
    expect(f.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });

  it("retains unhealthy deployment failures after controller-owned convergence", async () => {
    const f = fixture(); delete f.cr.metadata.ownerReferences;
    const plans = await inspectSandboxPlans(f.execute, [image]);
    f.deployment.status!.readyReplicas = 0;
    await expect(refreshSandboxImages(f.execute, plans)).rejects.toThrow("is not healthy");
    expect(f.calls.some(([, args]) => args[0] === "rollout" && args[1] === "restart")).toBe(false);
  });

  it.each(["uid", "name", "namespace"] as const)("rejects a changed Task %s before workload mutation", async field => {
    const f = fixture(); f.task.metadata[field] = "replacement";
    await expect(inspectSandboxPlans(f.execute, [image])).rejects.toThrow("Mission owner changed");
    expect(f.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });

  it("rejects ambiguous controlling owners", async () => {
    const f = fixture(); f.cr.metadata.ownerReferences!.push({ ...f.cr.metadata.ownerReferences![0] });
    await expect(inspectSandboxPlans(f.execute, [image])).rejects.toThrow("Cannot verify Mission owner");
  });

  it("rechecks completion after planning", async () => {
    const f = fixture("run-1", ""); const plans = await inspectSandboxPlans(f.execute, [image]);
    f.task.metadata.annotations["kars.azure.com/run-completed"] = "run-1";
    expect(await refreshSandboxImages(f.execute, plans)).toBe(0);
    expect(f.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });

  it.each([false, true])("preserves a Mission completing during the convergence wait (wait failure=%s)", async fail => {
    const f = fixture("run-1", ""); const plans = await inspectSandboxPlans(f.execute, [image]);
    f.onWait(() => {
      f.task.metadata.annotations["kars.azure.com/run-completed"] = "run-1";
      if (fail) throw new Error("convergence timeout");
    });
    expect(await refreshSandboxImages(f.execute, plans)).toBe(0);
    expect(f.calls.some(([, args]) => args[0] === "rollout")).toBe(false);
  });

  it("retains a genuine convergence failure for a nonterminal run", async () => {
    const f = fixture("run-1", ""); const plans = await inspectSandboxPlans(f.execute, [image]);
    f.onWait(() => { throw new Error("convergence timeout"); });
    await expect(refreshSandboxImages(f.execute, plans)).rejects.toThrow("convergence timeout");
  });

  it.each(["uid", "owner"])("rejects changed sandbox %s before verification or restart", async field => {
    const f = fixture("run-1", ""); const plans = await inspectSandboxPlans(f.execute, [image]);
    if (field === "uid") f.cr.metadata.uid = "replacement";
    else f.cr.metadata.ownerReferences![0].uid = "replacement";
    await expect(refreshSandboxImages(f.execute, plans)).rejects.toThrow("changed during image preparation");
    expect(f.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });
});
