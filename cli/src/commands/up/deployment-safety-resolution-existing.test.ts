// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import {
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

  it("preserves independent legacy Kata size/count for a confidential no-op", async () => {
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        isolation: "confidential",
        nodeCountExplicit: false,
        currentCluster: withLegacyKata(1),
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
    expect(result).toMatchObject({
      nodeCount: 3,
      kataNodeCount: 1,
      kataVmSize: legacyKata.name,
      additionalNodePools: [
        { label: "Kata sandbox", vmSize: legacyKata.name, count: 1 },
      ],
    });
    expect(result.quotaRequirements.every((requirement) => requirement.required === 0)).toBe(
      true,
    );
  });

  it("rejects explicit existing sandbox and Kata count changes before Azure discovery", async () => {
    const runAzJson = vi.fn();
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          isolation: "confidential",
          nodeCount: 5,
          nodeCountExplicit: true,
          currentCluster: withLegacyKata(1),
        },
        { runAzJson },
      ),
    ).rejects.toThrow(
      /az aks nodepool scale.*--name clawal --node-count 5.*az aks nodepool scale.*--name legacykata --node-count 5/s,
    );
    expect(runAzJson).not.toHaveBeenCalled();
  });

  it("requires manual migration for a failed restricted Kata pool", async () => {
    const cluster = withLegacyKata(1, "Failed");
    cluster.provisioningState = "Failed";
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          isolation: "confidential",
          nodeCountExplicit: false,
          currentCluster: cluster,
        },
        {
          runAzJson: azureDataWithLegacyKata(
            false,
            true,
            {
              [system.family]: 0,
              [legacyKata.family]: 4,
            },
            4,
          ),
        },
      ),
    ).rejects.toThrow(
      /cluster provisioningState=Failed.*pool 'legacykata' provisioningState=Failed.*az aks nodepool show/s,
    );
  });

  it("rejects an explicit different Kata SKU on an existing pool", async () => {
    const cluster = withLegacyKata(1);
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          isolation: "confidential",
          nodeCountExplicit: false,
          kataVmSize: KATA_POOL_VM_SIZE,
          kataVmSizeExplicit: true,
          currentCluster: cluster,
        },
        {
          runAzJson: azureDataWithLegacyKata(
            false,
            true,
            {
              [system.family]: 0,
              [kataSandbox.family]: 4,
            },
            4,
          ),
        },
      ),
    ).rejects.toThrow(
      /cannot resize an existing AKS node pool in place.*supported AKS tooling/s,
    );
  });

  it("allows a forced no-op with zero remaining quota", async () => {
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        nodeCountExplicit: false,
        nodeVmSizeExplicit: false,
        systemVmSizeExplicit: false,
        currentCluster: existingCluster(3),
      },
      {
        resolveSizes: async (_region, node, systemSize) => ({
          node: node!,
          system: systemSize!,
          checked: true,
        }),
        runAzJson: azureData({ [system.family]: 0 }, 0),
      },
    );
    expect(result.nodeCount).toBe(3);
    expect(result.adaptedNodeCount).toBe(false);
    expect(result.kubernetesVersion).toBe("1.35.6");
    expect(result.poolNames).toEqual({
      system: "sysal",
      sandbox: "clawal",
      kata: "katapool",
    });
    expect(result.quotaRequirements.map((requirement) => requirement.required)).toEqual([
      0,
      0,
    ]);
  });

  it("rejects an explicit Kubernetes version change on an existing cluster", async () => {
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          kubernetesVersion: "1.36.1",
          kubernetesVersionExplicit: true,
          nodeCountExplicit: false,
          currentCluster: existingCluster(3),
        },
        {
          runAzJson: azureData({ [system.family]: 0 }, 0),
        },
      ),
    ).rejects.toThrow(
      /cannot use --kubernetes-version.*supported AKS\/Kars upgrade tooling/s,
    );
  });

  it("allows an explicit exact-equal Kubernetes version on an existing cluster", async () => {
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        kubernetesVersion: "v1.35.6",
        kubernetesVersionExplicit: true,
        nodeCountExplicit: false,
        currentCluster: existingCluster(3),
      },
      {
        runAzJson: azureData({ [system.family]: 0 }, 0),
      },
    );
    expect(result.kubernetesVersion).toBe("1.35.6");
  });

  it("rejects an exact existing LTS-only patch on the mutation path", async () => {
    const cluster = existingCluster(3);
    cluster.kubernetesVersion = "1.33.13";
    const base = azureData({ [system.family]: 0 }, 0);
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          kubernetesVersion: "v1.33.13",
          kubernetesVersionExplicit: true,
          nodeCountExplicit: false,
          currentCluster: cluster,
        },
        {
          runAzJson: async (args) => {
            if (args[0] !== "aks") return base(args);
            return {
              values: [
                {
                  version: "1.33",
                  capabilities: { supportPlan: ["AKSLongTermSupport"] },
                  patchVersions: { "1.33.13": {} },
                },
                {
                  version: "1.36",
                  capabilities: { supportPlan: ["KubernetesOfficial"] },
                  patchVersions: { "1.36.1": {} },
                },
              ],
            };
          },
        },
      ),
    ).rejects.toThrow(/1\.33\.13.*not available.*KubernetesOfficial/);
  });

  it("rejects an explicit LTS-only Kubernetes patch for a new cluster", async () => {
    const base = azureData({ [system.family]: 20 }, 20);
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          kubernetesVersion: "1.33.13",
          kubernetesVersionExplicit: true,
          nodeCountExplicit: false,
        },
        {
          resolveSizes: async () => ({
            node: sandboxSameFamily.name,
            system: system.name,
            checked: true,
          }),
          runAzJson: async (args) => {
            if (args[0] !== "aks") return base(args);
            return {
              values: [
                {
                  version: "1.33",
                  capabilities: { supportPlan: ["AKSLongTermSupport"] },
                  patchVersions: { "1.33.13": {} },
                },
              ],
            };
          },
        },
      ),
    ).rejects.toThrow(/1\.33\.13.*not available.*KubernetesOfficial/);
  });

  it("preserves an existing healthy system-pool count without a capacity delta", async () => {
    const cluster = existingCluster(3);
    cluster.agentPoolProfiles[0].count = 3;
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        nodeCountExplicit: false,
        currentCluster: cluster,
      },
      { runAzJson: azureData({ [system.family]: 0 }, 0) },
    );

    expect(result.systemNodeCount).toBe(3);
    expect(result.quotaRequirements.every(({ required }) => required === 0)).toBe(
      true,
    );
  });

  it("rejects a failed existing system pool before Azure discovery", async () => {
    const cluster = existingCluster(3);
    cluster.agentPoolProfiles[0].provisioningState = "Failed";
    const runAzJson = vi.fn();
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          nodeCountExplicit: false,
          currentCluster: cluster,
        },
        { runAzJson },
      ),
    ).rejects.toThrow(/pool 'sysal' provisioningState=Failed.*az aks nodepool show/s);
    expect(runAzJson).not.toHaveBeenCalled();
  });

  it("rejects a missing existing system pool with child-pool add guidance", async () => {
    const cluster = existingCluster(3);
    cluster.agentPoolProfiles = cluster.agentPoolProfiles.filter(
      (pool) => pool.logicalRole !== "system",
    );
    const runAzJson = vi.fn();
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          nodeCountExplicit: false,
          currentCluster: cluster,
        },
        { runAzJson },
      ),
    ).rejects.toThrow(
      /az aks nodepool add --resource-group rg --cluster-name kars-aks --name system/s,
    );
    expect(runAzJson).not.toHaveBeenCalled();
  });

  it("preserves an existing sandbox count when --node-count is omitted", async () => {
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        nodeCountExplicit: false,
        currentCluster: existingCluster(5),
      },
      {
        resolveSizes: async (_region, node, systemSize) => ({
          node: node!,
          system: systemSize!,
          checked: true,
        }),
        runAzJson: azureData({ [system.family]: 0 }, 0),
      },
    );
    expect(result).toMatchObject({
      nodeCount: 5,
      adaptedNodeCount: false,
    });
  });

  it("rejects an explicit scale-up before Azure quota discovery", async () => {
    const runAzJson = vi.fn();
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          nodeCount: 5,
          nodeCountExplicit: true,
          currentCluster: existingCluster(3),
        },
        { runAzJson },
      ),
    ).rejects.toThrow(
      /az aks nodepool scale --resource-group rg --cluster-name kars-aks --name clawal --node-count 5/,
    );
    expect(runAzJson).not.toHaveBeenCalled();
  });

  it("never implicitly scales down an existing sandbox pool", async () => {
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        nodeCountExplicit: false,
        currentCluster: existingCluster(5),
      },
      {
        resolveSizes: async (_region, node, systemSize) => ({
          node: node!,
          system: systemSize!,
          checked: true,
        }),
        runAzJson: azureData({ [system.family]: 0 }, 0),
      },
    );
    expect(result.nodeCount).toBe(5);
    expect(result.adaptedNodeCount).toBe(false);
  });

  it("rejects an explicit different sandbox SKU on an existing pool", async () => {
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          nodeCount: 3,
          nodeCountExplicit: true,
          nodeVmSize: sandboxDifferentFamily.name,
          nodeVmSizeExplicit: true,
          currentCluster: existingCluster(3),
        },
        {
          runAzJson: azureData(
            {
              [system.family]: 0,
              [sandboxDifferentFamily.family]: 12,
            },
            12,
          ),
        },
      ),
    ).rejects.toThrow(
      /cannot resize an existing AKS node pool in place.*migrate\/replace.*supported AKS tooling/s,
    );
  });

  it("allows a healthy unavailable SKU when a no-op needs no capacity", async () => {
    const cluster = existingCluster(3);
    cluster.agentPoolProfiles[0].vmSize = unavailableSystem.name;
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        nodeCountExplicit: false,
        currentCluster: cluster,
      },
      {
        runAzJson: azureDataWithUnavailableSystem(
          {
            [unavailableSystem.family]: 0,
            [sandboxSameFamily.family]: 0,
          },
          0,
        ),
      },
    );
    expect(result.vmSizes.system).toBe(unavailableSystem.name);
    expect(result.quotaRequirements.every((requirement) => requirement.required === 0)).toBe(
      true,
    );
  });

  it("preserves a one-node unavailable system pool when no scale-up was requested", async () => {
    const cluster = existingCluster(3);
    cluster.agentPoolProfiles[0].count = 1;
    cluster.agentPoolProfiles[0].vmSize = unavailableSystem.name;
    const result = await resolveAzureDeploymentSafety(
      {
        region: "westus3",
        nodeCountExplicit: false,
        currentCluster: cluster,
      },
      {
        runAzJson: azureDataWithUnavailableSystem(
          {
            [unavailableSystem.family]: 0,
            [sandboxSameFamily.family]: 0,
          },
          0,
        ),
      },
    );

    expect(result.systemNodeCount).toBe(1);
    expect(result.quotaRequirements.every(({ required }) => required === 0)).toBe(true);
  });

  it.each([
    [
      "Failed cluster",
      (cluster: ExistingAksCluster) => {
        cluster.provisioningState = "Failed";
      },
      /cluster provisioningState=Failed.*az aks show/s,
    ],
    [
      "Failed governed pool",
      (cluster: ExistingAksCluster) => {
        cluster.agentPoolProfiles[1].provisioningState = "Failed";
      },
      /pool 'clawal' provisioningState=Failed.*az aks nodepool show/s,
    ],
    [
      "missing governed pool",
      (cluster: ExistingAksCluster) => {
        cluster.agentPoolProfiles = cluster.agentPoolProfiles.filter(
          ({ logicalRole }) => logicalRole !== "sandbox",
        );
      },
      /az aks nodepool add --resource-group rg --cluster-name kars-aks --name clawpool/s,
    ],
  ])("rejects a %s before Azure discovery", async (_label, mutate, diagnostic) => {
    const cluster = existingCluster(3);
    mutate(cluster);
    const runAzJson = vi.fn();
    await expect(
      resolveAzureDeploymentSafety(
        {
          region: "westus3",
          nodeCountExplicit: false,
          currentCluster: cluster,
        },
        { runAzJson },
      ),
    ).rejects.toThrow(diagnostic);
    expect(runAzJson).not.toHaveBeenCalled();
  });
});
