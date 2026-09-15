// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { matchesReviewedExecution, reviewedOwner, templateDigest, type Execute } from "./private-activation.js";

const parent = {
  serviceAccountName: "kars-controller",
  containers: [{ name: "controller", image: "controller:latest", resources: {} }],
};
const automatic = ["node.kubernetes.io/not-ready", "node.kubernetes.io/unreachable"].map(key =>
  ({ key, operator: "Exists", effect: "NoExecute", tolerationSeconds: 300 }));
const memoryPressure = { key: "node.kubernetes.io/memory-pressure", operator: "Exists", effect: "NoSchedule" };

describe("private consumer execution comparison", () => {
  it.each([
    { containers: [{ name: "controller", image: "controller:latest", resources: { requests: { cpu: "100m" } } }] },
    { containers: [{ name: "controller", image: "controller:latest", resources: { limits: { memory: "128Mi" } } }] },
    { initContainers: [{ name: "init", image: "init:latest", resources: { requests: { memory: "1Mi" } } }] },
    { resources: { requests: { cpu: "1" } } },
  ])("recognizes the exact memory-pressure toleration for reviewed non-BestEffort resources %#", change => {
    const reviewed = { ...parent, ...change };
    const pod = { ...structuredClone(reviewed), tolerations: [...automatic, memoryPressure] };
    expect(matchesReviewedExecution(pod, reviewed, true)).toBe(true);
    expect(matchesReviewedExecution(pod, reviewed, false)).toBe(false);
    expect(pod.tolerations).toEqual([...automatic, memoryPressure]);
  });

  it.each([
    {},
    { resources: { requests: { cpu: "0", memory: "0" } } },
    { containers: [{ name: "controller", image: "controller:latest", resources: { requests: { "nvidia.com/gpu": "1" } } }] },
    { ephemeralContainers: [{ name: "debug", image: "debug:latest", resources: { requests: { cpu: "1" } } }] },
  ])("does not infer non-BestEffort authority from missing, zero or unrelated resources %#", change => {
    const reviewed = { ...parent, ...change };
    expect(matchesReviewedExecution({ ...reviewed, tolerations: [...automatic, memoryPressure] }, reviewed, true)).toBe(false);
  });

  it("retains exact reviewed memory-pressure policy and rejects altered or duplicated admission", () => {
    const reviewed = { ...parent, resources: { requests: { cpu: "1" } } };
    const explicit = { ...reviewed, tolerations: [memoryPressure] };
    expect(matchesReviewedExecution({ ...explicit, tolerations: [memoryPressure, ...automatic] }, explicit, true)).toBe(true);
    expect(matchesReviewedExecution({ ...reviewed, tolerations: automatic }, explicit, true)).toBe(false);
    for (const change of [
      { operator: "Equal" }, { effect: "NoExecute" }, { key: "node.kubernetes.io/disk-pressure" },
      { value: "unexpected" }, { tolerationSeconds: 300 },
    ]) {
      expect(matchesReviewedExecution({ ...reviewed, tolerations: [...automatic, { ...memoryPressure, ...change }] }, reviewed, true)).toBe(false);
    }
    expect(matchesReviewedExecution({ ...reviewed, tolerations: [...automatic, memoryPressure, memoryPressure] }, reviewed, true)).toBe(false);
    const wildcard = { ...reviewed, tolerations: [{ operator: "Exists" }] };
    expect(matchesReviewedExecution({ ...wildcard, tolerations: [...wildcard.tolerations, memoryPressure] }, wildcard, true)).toBe(false);
  });

  it("reviews a non-root consumer with exact QoS admission without treating it as the root controller", async () => {
    const spec = { ...parent, serviceAccountName: "sandbox",
      containers: [{ ...parent.containers[0], resources: { requests: { cpu: "100m", memory: "128Mi" } } }] };
    const deployment = { apiVersion: "apps/v1", kind: "Deployment",
      metadata: { name: "sandbox", namespace: "team", uid: "deployment-uid", resourceVersion: "1" },
      spec: { template: { metadata: { labels: { app: "sandbox" } }, spec } } };
    const rs = { apiVersion: "apps/v1", kind: "ReplicaSet",
      metadata: { name: "sandbox-rs", namespace: "team", uid: "rs-uid", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "Deployment", name: "sandbox", uid: "deployment-uid", controller: true }] },
      spec: { template: structuredClone(deployment.spec.template) } };
    const pod = { apiVersion: "v1", kind: "Pod",
      metadata: { name: "sandbox-pod", namespace: "team", uid: "pod-uid", resourceVersion: "1",
        ownerReferences: [{ apiVersion: "apps/v1", kind: "ReplicaSet", name: "sandbox-rs", uid: "rs-uid", controller: true }] },
      spec: { ...structuredClone(spec), tolerations: [...automatic, memoryPressure] } };
    const calls: string[][] = [];
    const execute: Execute = async args => {
      calls.push([...args]);
      expect(args[0]).toBe("get");
      expect(args).toContain("team");
      const value = args[1] === "replicasets.apps" ? rs : deployment;
      expect(args[2]).toBe(value.metadata.name);
      return JSON.stringify(value);
    };
    const approved = { kind: "Deployment", object: { name: "sandbox", uid: "deployment-uid", resourceVersion: "1" },
      templateDigest: templateDigest(deployment) };
    const root = {
      namespace: { name: "core", uid: "core-uid", resourceVersion: "1" },
      account: { name: "kars-controller", uid: "controller-sa", resourceVersion: "1" },
      deployment: { name: "kars-controller", uid: "controller-uid", resourceVersion: "1" },
      templateDigest: templateDigest(deployment), replicaIntent: 1,
    };
    await expect(reviewedOwner(execute, pod,
      { namespace: { name: "team", uid: "team-uid", resourceVersion: "1" }, consumers: [approved] }, undefined, root)).resolves.toEqual(approved);
    expect(calls).toHaveLength(2);
  });

  it("accepts only the standard admission-injected bounded tolerations", () => {
    const pod = { ...structuredClone(parent), tolerations: structuredClone(automatic), nodeName: "worker" };
    expect(matchesReviewedExecution(pod, parent, true)).toBe(true);
    expect(pod.tolerations).toEqual(automatic);
    expect(matchesReviewedExecution(pod, parent, false)).toBe(false);
  });

  it("does not discard changed, duplicated or unbounded scheduling exceptions", () => {
    for (const change of [
      { tolerationSeconds: 0 }, { tolerationSeconds: 301 },
      { operator: "Equal" }, { effect: "NoSchedule" }, { key: "unreviewed-taint" },
      { value: "unreviewed" },
    ]) {
      const pod = { ...parent, tolerations: [{ ...automatic[0], ...change }, automatic[1]] };
      expect(matchesReviewedExecution(pod, parent, true), JSON.stringify(change)).toBe(false);
    }
    expect(matchesReviewedExecution({ ...parent, tolerations: [...automatic, automatic[0]] }, parent, true))
      .toBe(false);
    const unbounded = { key: automatic[0]!.key, operator: "Exists", effect: "NoExecute" };
    expect(matchesReviewedExecution({ ...parent, tolerations: [unbounded, automatic[1]] }, parent, true))
      .toBe(false);
  });

  it("preserves explicitly reviewed tolerations and every execution field", () => {
    const reviewed = { ...parent, tolerations: [{ key: "dedicated", effect: "NoSchedule", operator: "Exists" }] };
    expect(matchesReviewedExecution({ ...reviewed, tolerations: [...reviewed.tolerations, ...automatic] }, reviewed, true))
      .toBe(true);
    expect(matchesReviewedExecution({ ...parent, tolerations: automatic }, reviewed, true)).toBe(false);
    for (const change of [
      { serviceAccountName: "another-account" }, { hostPID: true }, { automountServiceAccountToken: false },
      { containers: [{ name: "controller", image: "different", resources: {} }] },
      { initContainers: [{ name: "injected", image: "different" }] },
    ]) {
      expect(matchesReviewedExecution({ ...parent, ...change, tolerations: automatic }, parent, true))
        .toBe(false);
    }
  });

  it("does not replace an explicit same-key policy with an implicit default", () => {
    const reviewed = { ...parent, tolerations: [{ ...automatic[0], tolerationSeconds: 60 }] };
    expect(matchesReviewedExecution({ ...parent, tolerations: automatic }, reviewed, true)).toBe(false);
    expect(matchesReviewedExecution({ ...reviewed, tolerations: [...reviewed.tolerations, automatic[1]] }, reviewed, true))
      .toBe(true);
    expect(matchesReviewedExecution({ ...reviewed, tolerations: [...reviewed.tolerations, ...automatic] }, reviewed, true))
      .toBe(false);
  });

  it("matches the admission plugin's key and effect rules without swallowing wildcard drift", () => {
    const reviewed = { ...parent, tolerations: [{ ...automatic[0], effect: "NoSchedule" }] };
    expect(matchesReviewedExecution({ ...reviewed, tolerations: [...reviewed.tolerations, ...automatic] }, reviewed, true))
      .toBe(true);
    const wildcard = { ...parent, tolerations: [{ operator: "Exists" }] };
    expect(matchesReviewedExecution(wildcard, wildcard, true)).toBe(true);
    expect(matchesReviewedExecution({ ...wildcard, tolerations: [...wildcard.tolerations, ...automatic] }, wildcard, true))
      .toBe(false);
  });
});
