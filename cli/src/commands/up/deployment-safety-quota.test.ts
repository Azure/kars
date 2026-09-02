// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  calculateQuotaRequirements,
  hasCliOption,
  KATA_POOL_VM_SIZE,
  resolveSandboxNodeCountForQuota,
  type RegionalQuota,
  type VmSkuCapacity,
} from "./deployment-safety.js";

const system: VmSkuCapacity = {
  name: "Standard_D2s_v3",
  family: "standardDSv3Family",
  vcpus: 2,
};
const sandboxSameFamily: VmSkuCapacity = {
  name: "Standard_D4s_v3",
  family: "standardDSv3Family",
  vcpus: 4,
};
const sandboxDifferentFamily: VmSkuCapacity = {
  name: "Standard_D4as_v5",
  family: "standardDASv5Family",
  vcpus: 4,
};
const kataSandbox: VmSkuCapacity = {
  name: KATA_POOL_VM_SIZE,
  family: "standardDASv6Family",
  vcpus: 4,
};

function quotas(
  entries: Array<[family: string, remaining: number]>,
  totalRemaining = 100,
): Map<string, RegionalQuota> {
  const result = new Map(
    entries.map(([family, remaining]) => [
      family.toLowerCase(),
      { family, current: 0, limit: remaining, remaining },
    ]),
  );
  result.set("cores", {
    family: "cores",
    current: 0,
    limit: totalRemaining,
    remaining: totalRemaining,
  });
  return result;
}

describe("quota accounting and adaptive node count", () => {
  it("combines system and sandbox requirements in the same family", () => {
    const result = calculateQuotaRequirements(
      [
        { label: "system", family: system.family, vcpusPerNode: 2, count: 2 },
        { label: "sandbox", family: system.family, vcpusPerNode: 4, count: 3 },
      ],
      quotas([[system.family, 20]]),
    );
    expect(result).toHaveLength(2);
    expect(result.find((requirement) => requirement.family === "cores")).toMatchObject({
      required: 16,
      remaining: 100,
    });
    expect(
      result.find((requirement) => requirement.family === system.family),
    ).toMatchObject({ required: 16, remaining: 20 });
  });

  it("accounts for different VM families independently", () => {
    const result = resolveSandboxNodeCountForQuota({
      requestedNodeCount: 3,
      nodeCountExplicit: false,
      system,
      sandbox: sandboxDifferentFamily,
      quotas: quotas([
        [system.family, 4],
        [sandboxDifferentFamily.family, 12],
      ]),
    });

    expect(result.adapted).toBe(false);
    expect(result.requirements.map((r) => r.required)).toEqual([16, 4, 12]);
  });

  it("accounts for both clawpool and katapool in confidential mode", () => {
    const result = resolveSandboxNodeCountForQuota({
      requestedNodeCount: 1,
      nodeCountExplicit: true,
      system,
      sandbox: sandboxSameFamily,
      additionalSandboxPools: [
        { label: "Kata sandbox", capacity: kataSandbox },
      ],
      quotas: quotas(
        [
          [system.family, 8],
          [kataSandbox.family, 4],
        ],
        12,
      ),
    });

    expect(result.requirements).toEqual([
      expect.objectContaining({ family: "cores", required: 12, remaining: 12 }),
      expect.objectContaining({
        family: system.family,
        required: 8,
        remaining: 8,
      }),
      expect.objectContaining({
        family: kataSandbox.family,
        required: 4,
        remaining: 4,
      }),
    ]);
    expect(result.requirements[0].pools).toEqual([
      "system 2 × 2 vCPU",
      "sandbox 1 × 4 vCPU",
      "Kata sandbox 1 × 4 vCPU",
    ]);
  });

  it("adapts an implicit three-node sandbox pool to one when it fits", () => {
    const result = resolveSandboxNodeCountForQuota({
      requestedNodeCount: 3,
      nodeCountExplicit: false,
      system,
      sandbox: sandboxSameFamily,
      quotas: quotas([[system.family, 10]]),
    });
    expect(result).toMatchObject({ nodeCount: 1, adapted: true });
    expect(
      result.requirements.find((requirement) => requirement.family === system.family),
    ).toMatchObject({ required: 8, remaining: 10 });
  });

  it("adapts based on Total Regional vCPUs even when family quotas fit", () => {
    const result = resolveSandboxNodeCountForQuota({
      requestedNodeCount: 3,
      nodeCountExplicit: false,
      system,
      sandbox: sandboxDifferentFamily,
      quotas: quotas(
        [
          [system.family, 20],
          [sandboxDifferentFamily.family, 20],
        ],
        10,
      ),
    });
    expect(result).toMatchObject({ nodeCount: 1, adapted: true });
    expect(result.requirements[0]).toMatchObject({
      family: "cores",
      required: 8,
      remaining: 10,
    });
  });

  it("fails an explicit footprint against Total Regional vCPUs", () => {
    expect(() =>
      resolveSandboxNodeCountForQuota({
        requestedNodeCount: 3,
        nodeCountExplicit: true,
        system,
        sandbox: sandboxDifferentFamily,
        quotas: quotas(
          [
            [system.family, 20],
            [sandboxDifferentFamily.family, 20],
          ],
          15,
        ),
      }),
    ).toThrowError(/cores requires 16 vCPU, 15 vCPU remaining/);
  });

  it("does not reduce an explicit node count and reports exact capacity", () => {
    expect(() =>
      resolveSandboxNodeCountForQuota({
        requestedNodeCount: 3,
        nodeCountExplicit: true,
        system,
        sandbox: sandboxSameFamily,
        quotas: quotas([[system.family, 10]]),
      }),
    ).toThrowError(
      /standardDSv3Family requires 16 vCPU, 10 vCPU remaining/,
    );
  });

  it("fails with minimum-footprint required and remaining quota", () => {
    expect(() =>
      resolveSandboxNodeCountForQuota({
        requestedNodeCount: 3,
        nodeCountExplicit: false,
        system,
        sandbox: sandboxSameFamily,
        quotas: quotas([[system.family, 7]]),
      }),
    ).toThrowError(
      /standardDSv3Family requires 8 vCPU, 7 vCPU remaining/,
    );
  });

  it("reports every insufficient family", () => {
    expect(() =>
      resolveSandboxNodeCountForQuota({
        requestedNodeCount: 3,
        nodeCountExplicit: true,
        system,
        sandbox: sandboxDifferentFamily,
        quotas: quotas([
          [system.family, 3],
          [sandboxDifferentFamily.family, 11],
        ]),
      }),
    ).toThrowError(
      /standardDSv3Family requires 4 vCPU, 3 vCPU remaining; standardDASv5Family requires 12 vCPU, 11 vCPU remaining/,
    );
  });

  it("fails closed when a selected family's quota is absent", () => {
    expect(() =>
      resolveSandboxNodeCountForQuota({
        requestedNodeCount: 1,
        nodeCountExplicit: true,
        system,
        sandbox: sandboxDifferentFamily,
        quotas: quotas([[system.family, 10]]),
      }),
    ).toThrowError(/quota data did not include VM family 'standardDASv5Family'/);
  });
});

describe("hasCliOption", () => {
  it("recognizes separate and equals-form options", () => {
    expect(hasCliOption("--node-count", ["node", "kars", "--node-count", "1"])).toBe(true);
    expect(hasCliOption("--node-count", ["node", "kars", "--node-count=1"])).toBe(true);
    expect(hasCliOption("--node-count", ["node", "kars"])).toBe(false);
  });
});

