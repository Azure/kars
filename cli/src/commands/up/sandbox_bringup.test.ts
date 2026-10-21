// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { saveFinalDeploymentContext } from "./sandbox_bringup.js";

const context = {
  subscription: "sub-1",
  resourceGroup: "rg-1",
  aksCluster: "kars-aks",
  phase: "complete" as const,
};

describe("saveFinalDeploymentContext", () => {
  it("persists successful deployment context", () => {
    const persist = vi.fn();
    saveFinalDeploymentContext(context, true, persist);
    expect(persist).toHaveBeenCalledWith(context);
  });

  it("fails rollback-enabled deployments when context cannot be saved", () => {
    expect(() =>
      saveFinalDeploymentContext(context, true, () => {
        throw new Error("disk full");
      }),
    ).toThrow(/could not be saved.*rolling back/s);
  });

  it("preserves historical best-effort behavior without rollback", () => {
    expect(() =>
      saveFinalDeploymentContext(context, false, () => {
        throw new Error("disk full");
      }),
    ).not.toThrow();
  });
});
