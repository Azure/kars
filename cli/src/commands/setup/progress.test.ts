// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { BetaExecute } from "../dev/kind-beta.js";
import { observeSetupCluster, summarizeSetupResources } from "./progress.js";
import type { SetupObservation } from "./types.js";

const tools = { kubectl: "/owned/kubectl", kind: "kind", helm: "helm", runtime: "podman", env: { KUBECONFIG: "/wrong/context" } };
const node = { kind: "Node", metadata: { name: "unit-control-plane", labels: { "kubernetes.io/arch": "arm64" } }, status: { conditions: [{ type: "Ready", status: "True" }] } };
const job = { kind: "Job", metadata: { name: "kars-schema-unit", namespace: "kars-system", annotations: { "helm.sh/hook": "pre-install", private: "secret" } },
  spec: { activeDeadlineSeconds: 1200, template: { spec: { nodeSelector: { "kubernetes.io/arch": "amd64" }, containers: [{ env: [{ value: "secret" }] }] } } },
  status: { failed: 1, conditions: [{ type: "Failed", status: "True", reason: "DeadlineExceeded", message: "secret" }] } };

describe("safe setup workload observations", () => {
  it("explains an active SRE hook and surfaces its current controller blocker without exposing arbitrary messages", () => {
    const source = { kind: "KarsSandbox", metadata: { name: "sre", namespace: "kars-system", generation: 1 },
      spec: { private: "private-spec" }, status: { conditions: [{ type: "Degraded", status: "True", observedGeneration: 1,
        message: "no inference endpoint configured (FOUNDRY_ENDPOINT or AZURE_OPENAI_ENDPOINT)" }] } };
    const hook = { kind: "Job", metadata: { name: "kars-sre-install-unit", namespace: "kars-system" }, status: { active: 1 } };
    const rows = summarizeSetupResources({ items: [hook, source] });
    expect(rows[0]).toMatchObject({ label: "SRE runtime", status: "blocked", detail: expect.stringContaining("GitHub Copilot") });
    expect(rows[1]).toMatchObject({ status: "running", detail: expect.stringContaining("waits for verified initial funding") });
    source.status.conditions[0].message = "private-controller-output";
    expect(summarizeSetupResources({ items: [source] })[0].detail).toContain("SRE prerequisite failure");
    expect(JSON.stringify(summarizeSetupResources({ items: [source] }))).not.toMatch(/private-spec|private-controller-output/);
    source.metadata.generation = 2;
    expect(summarizeSetupResources({ items: [source] })[0]).toMatchObject({ status: "running",
      detail: expect.stringContaining("current SRE configuration") });
  });
  it("does not mistake a funding reference for runtime readiness or expose another sandbox", () => {
    const source = { kind: "KarsSandbox", metadata: { name: "sre", namespace: "kars-system", generation: 1 },
      spec: { inferenceBudgetRef: {} }, status: { conditions: [{ type: "Ready", status: "False", observedGeneration: 1 }] } };
    expect(summarizeSetupResources({ items: [source] })[0]).toMatchObject({ status: "running",
      detail: expect.stringContaining("enrollment still verifies the active funding ledger") });
    source.status.conditions[0].status = "True";
    expect(summarizeSetupResources({ items: [source] })[0].status).toBe("passed");
    source.metadata.name = "private-sandbox";
    expect(summarizeSetupResources({ items: [source] })).toEqual([]);
  });
  it("reads SRE only after Core exists and uses the same owned read-only context", async () => {
    const deployment = { kind: "Deployment", metadata: { name: "kars-controller", namespace: "kars-system" } };
    const source = { kind: "KarsSandbox", metadata: { name: "sre", namespace: "kars-system", generation: 1 } };
    const execute = vi.fn<BetaExecute>(async (_file, args) => ({ stdout: JSON.stringify(
      args.includes("karssandboxes.kars.azure.com") ? source : { items: [deployment] }) }));
    const snapshots: SetupObservation[] = [];
    const observer = observeSetupCluster(tools, "unit", "/owned/kubeconfig", execute, value => snapshots.push(value));
    try {
      await observer.sample();
      expect(execute).toHaveBeenCalledWith("/owned/kubectl",
        ["--kubeconfig", "/owned/kubeconfig", "--context", "kind-unit", "--request-timeout=5s",
          "get", "karssandboxes.kars.azure.com", "sre", "-n", "kars-system", "--ignore-not-found", "-o", "json"],
        expect.objectContaining({ cancelSignal: expect.any(AbortSignal) }));
      expect(snapshots.at(-1)?.resources).toContainEqual(expect.objectContaining({ label: "SRE runtime", status: "running" }));
    } finally { observer.stop(); }
  });
  it("shows only allowlisted hook termination diagnostics, never the surrounding logs", () => {
    const pod = { kind: "Pod", metadata: { name: "kars-sre-install-unit-pod", namespace: "kars-system" },
      spec: { containers: [{ name: "sre-enrollment" }] }, status: { phase: "Failed",
        containerStatuses: [{ name: "sre-enrollment", ready: false, state: { terminated: {
          exitCode: 1, message: "private-credential\nKARS_CORE_HOOK_FAILURE ControllerChanged\nprivate-command-output",
        } } }] } };
    const [row] = summarizeSetupResources({ items: [pod] });
    expect(row).toMatchObject({ status: "failed", diagnosticCode: "ControllerChanged",
      detail: expect.stringContaining("Core controller intent changed") });
    expect(JSON.stringify(row)).not.toMatch(/private-credential|private-command-output/);
    pod.status.containerStatuses[0].state.terminated.message = "KARS_CORE_HOOK_FAILURE private-credential";
    expect(summarizeSetupResources({ items: [pod] })[0].diagnosticCode).toBeUndefined();
    pod.status.containerStatuses[0].state.terminated.message = "KARS_CORE_HOOK_FAILURE ControllerChanged";
    pod.metadata.namespace = "kars-sre";
    expect(summarizeSetupResources({ items: [pod] })[0].diagnosticCode).toBeUndefined();
  });
  it("retains the real scheduling cause after the deadline deletes a hook pod, without exposing specs or messages", () => {
    const rows = summarizeSetupResources({ items: [node, job, { kind: "Secret", metadata: { name: "private-key", namespace: "kars-system" } }] });
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({ status: "failed", label: "Job: kars-schema-unit", detail: expect.stringContaining("Job deadline exceeded") });
    expect(rows[0].detail).toContain("Requires amd64; available nodes have a different architecture");
    expect(rows[0].detail).toContain("Helm pre-install hook");
    expect(JSON.stringify(rows)).not.toMatch(/secret|private-key|containers|annotations/);
  });
  it("shows actual image-pull, container readiness and generation-aware deployment progress", () => {
    const pod = { kind: "Pod", metadata: { name: "kars-controller-unit", namespace: "kars-system" }, spec: { containers: [{}] },
      status: { containerStatuses: [{ ready: false, restartCount: 2, state: { waiting: { reason: "ImagePullBackOff", message: "private registry error" } } }] } };
    const deployment = { kind: "Deployment", metadata: { name: "kars-controller", namespace: "kars-system", generation: 2 },
      spec: { replicas: 1 }, status: { observedGeneration: 1, readyReplicas: 1, updatedReplicas: 1 } };
    const rows = summarizeSetupResources({ items: [pod, deployment] });
    expect(rows[0]).toMatchObject({ status: "blocked", detail: expect.stringContaining("ImagePullBackOff") });
    expect(rows[0].detail).toContain("0/1 containers ready");
    expect(rows[0].detail).toContain("2 restarts");
    expect(rows[1].status).toBe("running");
    expect(JSON.stringify(rows)).not.toContain("private registry");
    deployment.status.observedGeneration = 2;
    expect(summarizeSetupResources({ items: [deployment] })[0].status).toBe("passed");
  });
  it("pins read-only requests, prevents overlapping samples, and cancels without later publishing", async () => {
    let finish!: (value: { stdout: string }) => void;
    const execute = vi.fn<BetaExecute>(() => new Promise(resolve => { finish = resolve; }));
    const snapshots: SetupObservation[] = [];
    const observer = observeSetupCluster(tools, "unit", "/owned/kubeconfig", execute, value => snapshots.push(value));
    const first = observer.sample(), second = observer.sample();
    expect(execute).toHaveBeenCalledOnce();
    expect(execute.mock.calls[0]).toEqual(["/owned/kubectl",
      ["--kubeconfig", "/owned/kubeconfig", "--context", "kind-unit", "--request-timeout=5s", "get", "nodes,pods,jobs,deployments", "--all-namespaces", "-o", "json"],
      expect.objectContaining({ cancelSignal: expect.any(AbortSignal) })]);
    observer.stop();
    expect(execute.mock.calls[0][2]?.cancelSignal?.aborted).toBe(true);
    finish({ stdout: JSON.stringify({ items: [node, job] }) });
    await Promise.all([first, second]);
    expect(snapshots).toHaveLength(2);
    expect(snapshots.at(-1)?.status).toBe("stopped");
  });
  it("marks stale data explicitly when refreshing fails, retains evidence and stops polling", async () => {
    vi.useFakeTimers();
    const execute = vi.fn<BetaExecute>()
      .mockResolvedValueOnce({ stdout: JSON.stringify({ items: [node, job] }) })
      .mockRejectedValue(new Error("private-command-output"));
    const snapshots: SetupObservation[] = [];
    const observer = observeSetupCluster(tools, "unit", "/owned/kubeconfig", execute, value => snapshots.push(value));
    try {
      await observer.sample();
      await vi.advanceTimersByTimeAsync(3000);
      expect(snapshots.at(-1)).toMatchObject({ status: "unavailable", resources: expect.arrayContaining([expect.objectContaining({ status: "failed" })]) });
      expect(JSON.stringify(snapshots)).not.toContain("private-command-output");
      observer.stop();
      await vi.advanceTimersByTimeAsync(9000);
      expect(execute).toHaveBeenCalledTimes(3);
    } finally { observer.stop(); vi.useRealTimers(); }
  });
  it("takes a fresh final sample rather than reusing a poll started before the hook failed", async () => {
    let finish!: (value: { stdout: string }) => void;
    const execute = vi.fn<BetaExecute>()
      .mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }))
      .mockResolvedValueOnce({ stdout: JSON.stringify({ items: [node, job] }) });
    const snapshots: SetupObservation[] = [];
    const observer = observeSetupCluster(tools, "unit", "/owned/kubeconfig", execute, value => snapshots.push(value));
    try {
      const final = observer.sample();
      finish({ stdout: JSON.stringify({ items: [node] }) });
      await final;
      expect(snapshots.at(-1)?.resources).toContainEqual(expect.objectContaining({ status: "failed" }));
      expect(execute).toHaveBeenCalledTimes(2);
    } finally { observer.stop(); }
  });
});
