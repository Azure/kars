// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { sandboxKey } from "./helpers.js";

describe("sandboxKey", () => {
  // Regression: the operator's per-sandbox maps (securityStates,
  // egressByAgent) were keyed by bare `name`. The same agent name can exist
  // simultaneously as a docker container, a kind pod, and an AKS pod — so a
  // stale/empty same-named entry overwrote the live one and the operator
  // showed no DID / no audit for the shadowed agent (observed for "analyst"
  // when docker + kind + aks all had an "analyst"). The key must disambiguate
  // by origin (kubeContext for K8s, runtime for docker) + namespace.
  it("disambiguates same-named agents across runtimes and clusters", () => {
    const docker = sandboxKey({
      name: "analyst",
      namespace: "kars-analyst",
      runtime: "docker",
    });
    const kind = sandboxKey({
      name: "analyst",
      namespace: "kars-analyst",
      runtime: "aks",
      kubeContext: "kind-kars-dev",
    });
    const aks = sandboxKey({
      name: "analyst",
      namespace: "kars-analyst",
      runtime: "aks",
      kubeContext: "kars-aks",
    });

    const keys = new Set([docker, kind, aks]);
    expect(keys.size).toBe(3);
    expect(docker).not.toBe(kind);
    expect(kind).not.toBe(aks);
    expect(docker).not.toBe(aks);
  });

  it("is stable for the same sandbox identity", () => {
    const a = sandboxKey({ name: "viz", namespace: "kars-viz", runtime: "docker" });
    const b = sandboxKey({ name: "viz", namespace: "kars-viz", runtime: "docker" });
    expect(a).toBe(b);
  });

  it("falls back to runtime then 'local' when kubeContext is absent", () => {
    expect(sandboxKey({ name: "x", namespace: "kars-x", runtime: "docker" })).toContain("docker");
    expect(sandboxKey({ name: "x", namespace: "kars-x" })).toContain("local");
  });
});
