// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import {
  applyAzureDeploymentSafetyResult,
  KATA_POOL_VM_SIZE,
  resolveAzureDeploymentSafety,
  type ExistingAksCluster,
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
const unavailableSystem: VmSkuCapacity = {
  name: "Standard_D2ads_v5",
  family: "standardDADSv5Family",
  vcpus: 2,
};
const legacyKata: VmSkuCapacity = {
  name: "Standard_DC4as_v5",
  family: "standardDCASv5Family",
  vcpus: 4,
};

describe("resolveAzureDeploymentSafety", () => {
  const existingCluster = (
    sandboxCount: number,
    sandboxVmSize = sandboxSameFamily.name,
  ): ExistingAksCluster => ({
    exists: true,
    id: "/subscriptions/sub/resourceGroups/rg/providers/Microsoft.ContainerService/managedClusters/kars-aks",
    provisioningState: "Succeeded",
    powerState: { code: "Running" },
    kubernetesVersion: "1.35.6",
    supportPlan: "KubernetesOfficial",
    sku: { name: "Base", tier: "Free" },
    autoUpgradeProfile: {
      upgradeChannel: "stable",
      nodeOSUpgradeChannel: "SecurityPatch",
    },
    agentPoolProfiles: [
      {
        name: "sysal",
        count: 2,
        vmSize: system.name,
        mode: "System",
        provisioningState: "Succeeded",
        nodeLabels: {},
        nodeTaints: [],
        logicalRole: "system",
      },
      {
        name: "clawal",
        count: sandboxCount,
        vmSize: sandboxVmSize,
        mode: "User",
        provisioningState: "Succeeded",
        nodeLabels: { "kars.azure.com/pool": "sandbox" },
        nodeTaints: ["kars.azure.com/sandbox=true:NoSchedule"],
        logicalRole: "sandbox",
      },
    ],
  });

  const withLegacyKata = (
    kataCount: number,
    provisioningState = "Succeeded",
  ): ExistingAksCluster => {
    const cluster = existingCluster(3);
    cluster.autoUpgradeProfile.nodeOSUpgradeChannel = "NodeImage";
    cluster.agentPoolProfiles.push({
      name: "legacykata",
      count: kataCount,
      vmSize: legacyKata.name,
      mode: "User",
      provisioningState,
      nodeLabels: { "kars.azure.com/pool": "sandbox-kata" },
      nodeTaints: [],
      workloadRuntime: "KataMshvVmIsolation",
      logicalRole: "kata",
    });
    return cluster;
  };

  describe("applyAzureDeploymentSafetyResult", () => {
    it("applies the complete validated deployment projection authoritatively", () => {
      const target = {
        kubernetesVersion: "old",
        nodeVmSize: "old-node",
        systemVmSize: "old-system",
        systemNodeCount: 99,
        nodeCount: 99,
        systemPoolName: "old-system-pool",
        sandboxPoolName: "old-sandbox-pool",
        kataPoolName: "old-kata-pool",
        kataVmSize: "old-kata-size",
        kataNodeCount: 99,
      };
      applyAzureDeploymentSafetyResult(target, {
        kubernetesVersion: "1.36.3",
        vmSizes: {
          node: "Standard_D4s_v6",
          system: "Standard_D2as_v7",
          checked: true,
        },
        systemNodeCount: 5,
        poolNames: {
          system: "sysal",
          sandbox: "clawal",
          kata: "kataal",
        },
        kataVmSize: "Standard_D4as_v6",
        kataNodeCount: 1,
        additionalNodePools: [
          { label: "Kata sandbox", vmSize: "Standard_D4as_v6", count: 1 },
        ],
        nodeCount: 3,
        adaptedNodeCount: false,
        quotaRequirements: [],
      });

      expect(target).toEqual({
        kubernetesVersion: "1.36.3",
        nodeVmSize: "Standard_D4s_v6",
        systemVmSize: "Standard_D2as_v7",
        systemNodeCount: 5,
        nodeCount: 3,
        systemPoolName: "sysal",
        sandboxPoolName: "clawal",
        kataPoolName: "kataal",
        kataVmSize: "Standard_D4as_v6",
        kataNodeCount: 1,
      });
    });
  });

  const azureData = (
    familyRemaining: Record<string, number>,
    totalRemaining: number,
  ) => async (args: string[]) => {
    if (args[0] === "aks") {
      return {
        values: [
          {
            version: "1.35",
            capabilities: { supportPlan: ["KubernetesOfficial"] },
            patchVersions: { "1.35.6": {} },
          },
          {
            version: "1.36",
            capabilities: { supportPlan: ["KubernetesOfficial"] },
            patchVersions: { "1.36.1": {} },
          },
        ],
      };
    }
    if (args[1] === "list-skus") {
      return [
        {
          name: system.name,
          family: system.family,
          capabilities: [{ name: "vCPUs", value: "2" }],
        },
        {
          name: sandboxSameFamily.name,
          family: sandboxSameFamily.family,
          capabilities: [{ name: "vCPUs", value: "4" }],
        },
        {
          name: sandboxDifferentFamily.name,
          family: sandboxDifferentFamily.family,
          capabilities: [{ name: "vCPUs", value: "4" }],
        },
      ];
    }
    return [
      ...Object.entries(familyRemaining).map(([family, remaining]) => ({
        name: { value: family },
        currentValue: "100",
        limit: String(100 + remaining),
      })),
      {
        name: { value: "cores" },
        currentValue: "100",
        limit: String(100 + totalRemaining),
      },
    ];
  };

  const azureDataWithUnavailableSystem = (
    familyRemaining: Record<string, number>,
    totalRemaining: number,
  ) => {
    const base = azureData(familyRemaining, totalRemaining);
    return async (args: string[]) => {
      const result = await base(args);
      if (args[1] !== "list-skus") return result;
      return [
        ...(result as unknown[]),
        {
          name: unavailableSystem.name,
          family: unavailableSystem.family,
          capabilities: [{ name: "vCPUs", value: "2" }],
          restrictions: [
            {
              type: "Location",
              reasonCode: "NotAvailableForSubscription",
            },
          ],
        },
      ];
    };
  };

  const azureDataWithKata = (
    allocatable: boolean,
    familyRemaining: Record<string, number>,
    totalRemaining: number,
  ) => {
    const base = azureData(familyRemaining, totalRemaining);
    return async (args: string[]) => {
      const result = await base(args);
      if (args[1] !== "list-skus") return result;
      return [
        ...(result as unknown[]),
        {
          name: KATA_POOL_VM_SIZE,
          family: kataSandbox.family,
          capabilities: [{ name: "vCPUs", value: "4" }],
          restrictions: allocatable
            ? []
            : [{ type: "Location", reasonCode: "NotAvailableForSubscription" }],
        },
      ];
    };
  };

  const azureDataWithLegacyKata = (
    legacyAllocatable: boolean,
    replacementAllocatable: boolean,
    familyRemaining: Record<string, number>,
    totalRemaining: number,
  ) => {
    const base = azureDataWithKata(
      replacementAllocatable,
      familyRemaining,
      totalRemaining,
    );
    return async (args: string[]) => {
      const result = await base(args);
      if (args[1] !== "list-skus") return result;
      return [
        ...(result as unknown[]),
        {
          name: legacyKata.name,
          family: legacyKata.family,
          capabilities: [{ name: "vCPUs", value: "4" }],
          restrictions: legacyAllocatable
            ? []
            : [{ type: "Location", reasonCode: "NotAvailableForSubscription" }],
        },
      ];
    };
  };

  it("uses injectable Azure discovery and returns the resolved footprint", async () => {
    const calls: string[][] = [];
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        subscriptionId: "sub-selected",
        nodeCountExplicit: false,
      },
      {
        resolveSizes: async (
          region,
          requestedNode,
          requestedSystem,
          subscriptionId,
        ) => {
          expect([
            region,
            requestedNode,
            requestedSystem,
            subscriptionId,
          ]).toEqual(["westus3", undefined, undefined, "sub-selected"]);
          return {
            node: sandboxSameFamily.name,
            system: system.name,
            checked: true,
          };
        },
        runAzJson: async (args) => {
          calls.push(args);
          if (args[0] === "aks") {
            return {
              values: [
                {
                  version: "1.36",
                  capabilities: { supportPlan: ["KubernetesOfficial"] },
                  patchVersions: { "1.36.1": {} },
                },
              ],
            };
          }
          if (args[1] === "list-skus") {
            return [
              {
                name: system.name,
                family: system.family,
                capabilities: [{ name: "vCPUs", value: "2" }],
              },
              {
                name: sandboxSameFamily.name,
                family: sandboxSameFamily.family,
                capabilities: [{ name: "vCPUs", value: "4" }],
              },
            ];
          }
          return [
            {
              name: { value: system.family },
              currentValue: "0",
              limit: "10",
            },
            {
              name: { value: "cores" },
              currentValue: "0",
              limit: "10",
            },
          ];
        },
      },
    );

    expect(result).toMatchObject({
      kubernetesVersion: "1.36.1",
      systemNodeCount: 2,
      nodeCount: 1,
      adaptedNodeCount: true,
    });

    expect(calls).toContainEqual([
      "aks",
      "get-versions",
      "--location",
      "westus3",
      "-o",
      "json",
      "--subscription",
      "sub-selected",
    ]);
    expect(calls).toContainEqual([
      "vm",
      "list-skus",
      "--location",
      "westus3",
      "--resource-type",
      "virtualMachines",
      "--all",
      "-o",
      "json",
      "--subscription",
      "sub-selected",
    ]);
    expect(calls).toContainEqual([
      "vm",
      "list-usage",
      "--location",
      "westus3",
      "-o",
      "json",
      "--subscription",
      "sub-selected",
    ]);
  });

  it("rejects a new confidential deployment when the fixed Kata SKU is restricted", async () => {
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          isolation: "confidential",
          nodeCountExplicit: false,
        },
        {
          resolveSizes: async () => ({
            node: sandboxSameFamily.name,
            system: system.name,
            checked: true,
          }),
          runAzJson: azureDataWithKata(
            false,
            {
              [system.family]: 16,
              [kataSandbox.family]: 12,
            },
            28,
          ),
        },
      ),
    ).rejects.toThrow(
      /Required Kata pool VM size 'Standard_D4as_v6'.*fixed; choose another Azure region/,
    );
  });

  it("accepts a new confidential deployment when the fixed Kata SKU is available", async () => {
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        isolation: "confidential",
        nodeCountExplicit: false,
      },
      {
        resolveSizes: async () => ({
          node: sandboxSameFamily.name,
          system: system.name,
          checked: true,
        }),
        runAzJson: azureDataWithKata(
          true,
          {
            [system.family]: 16,
            [kataSandbox.family]: 12,
          },
          28,
        ),
      },
    );
    expect(result.nodeCount).toBe(3);
    expect(result.additionalNodePools).toEqual([
      { label: "Kata sandbox", vmSize: KATA_POOL_VM_SIZE, count: 3 },
    ]);
    expect(result.quotaRequirements.map((requirement) => requirement.required)).toEqual([
      28,
      16,
      12,
    ]);
  });

  it("preserves a healthy restricted Kata SKU for a confidential no-op", async () => {
    const cluster = existingCluster(3);
    cluster.autoUpgradeProfile.nodeOSUpgradeChannel = "NodeImage";
    cluster.agentPoolProfiles.push({
      name: "kataal",
      count: 3,
      vmSize: KATA_POOL_VM_SIZE,
      mode: "User",
      provisioningState: "Succeeded",
      nodeLabels: { "kars.azure.com/pool": "sandbox-kata" },
      nodeTaints: [],
      workloadRuntime: "KataMshvVmIsolation",
      logicalRole: "kata",
    });
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        isolation: "confidential",
        nodeCountExplicit: false,
        currentCluster: cluster,
      },
      {
        runAzJson: azureDataWithKata(
          false,
          {
            [system.family]: 0,
            [kataSandbox.family]: 0,
          },
          0,
        ),
      },
    );
    expect(result.poolNames.kata).toBe("kataal");
    expect(result.quotaRequirements.every((requirement) => requirement.required === 0)).toBe(
      true,
    );
  });

  it("allows explicit exact-equal system, sandbox, and Kata SKUs on an existing cluster", async () => {
    const cluster = withLegacyKata(1);
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        isolation: "confidential",
        nodeCountExplicit: false,
        systemVmSize: system.name,
        systemVmSizeExplicit: true,
        nodeVmSize: sandboxSameFamily.name,
        nodeVmSizeExplicit: true,
        kataVmSize: legacyKata.name,
        kataVmSizeExplicit: true,
        currentCluster: cluster,
      },
      {
        runAzJson: azureDataWithLegacyKata(
          false,
          true,
          {
            [system.family]: 0,
            [legacyKata.family]: 0,
          },
          0,
        ),
      },
    );
    expect(result.vmSizes).toMatchObject({
      system: system.name,
      node: sandboxSameFamily.name,
    });
    expect(result.kataVmSize).toBe(legacyKata.name);
    expect(result.quotaRequirements.every((requirement) => requirement.required === 0)).toBe(
      true,
    );
  });


});
