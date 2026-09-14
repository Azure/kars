// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { matchesReviewedExecution } from "./private-activation.js";

const parent = {
  serviceAccountName: "kars-controller",
  containers: [{ name: "controller", image: "controller:latest", resources: {} }],
};
const automatic = ["node.kubernetes.io/not-ready", "node.kubernetes.io/unreachable"].map(key =>
  ({ key, operator: "Exists", effect: "NoExecute", tolerationSeconds: 300 }));

describe("private consumer execution comparison", () => {
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
