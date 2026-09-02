// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import {
  applyAzureDeploymentSafetyResult,
  calculateQuotaRequirements,
  classifyExistingAksCluster,
  classifyExistingNodeCountSelection,
  classifyInfrastructureDeployment,
  detectExistingAksCluster,
  detectInfrastructureCompleteness,
  hasCliOption,
  identifyAksPoolRoles,
  KATA_POOL_VM_SIZE,
  parseRegionalVmFamilyQuotas,
  parseVmSkuCapacities,
  requireCompleteSkipInfraDeployment,
  requireHealthyExistingAksPoolTopology,
  requireHealthySkipInfraCluster,
  requireSupportedExistingNodeCountWorkflow,
  requireTemplateSafeExistingAksMutation,
  resolveAzureDeploymentSafety,
  resolveSandboxNodeCountForQuota,
  selectAksKubernetesVersion,
  validateAutomaticAksNodeResourceGroupName,
  validateDerivedAzureResourceNames,
  type AksAgentPoolProfile,
  type RegionalQuota,
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

describe("validateDerivedAzureResourceNames", () => {
  it("accepts the 14-character baseName Key Vault boundary", () => {
    const names = validateDerivedAzureResourceNames("abcdefghijklmn-aks");
    expect(names.baseName).toBe("abcdefghijklmn");
    expect(names.keyVaultExample).toHaveLength(24);
  });

  it("rejects 15 characters with actionable cluster-name guidance", () => {
    expect(() =>
      validateDerivedAzureResourceNames("abcdefghijklmno"),
    ).toThrowError(
      /Key Vault.*25 characters.*at most 24.*--cluster-name.*at most 14/s,
    );
  });

  it("rejects an empty or syntactically invalid derived baseName", () => {
    expect(() => validateDerivedAzureResourceNames("-aks")).toThrowError(
      /Derived baseName.*invalid/,
    );
    expect(() => validateDerivedAzureResourceNames("Bad_Name")).toThrowError(
      /lowercase letters/,
    );
    expect(() => validateDerivedAzureResourceNames("1cluster")).toThrowError(
      /must start with a lowercase letter/,
    );
    expect(() => validateDerivedAzureResourceNames("bad--name")).toThrowError(
      /single internal hyphens/,
    );
  });
});

describe("validateAutomaticAksNodeResourceGroupName", () => {
  it("accepts the 80-character boundary", () => {
    const resourceGroup = "r".repeat(60);
    const name = validateAutomaticAksNodeResourceGroupName(
      resourceGroup,
      "kars-aks",
      "westus3",
    );

    expect(name).toHaveLength(80);
  });

  it("rejects an overlong custom region with actionable guidance", () => {
    const region = "customregion".repeat(6);
    expect(() =>
      validateAutomaticAksNodeResourceGroupName(
        `kars-${region}`,
        "kars-aks",
        region,
      ),
    ).toThrowError(
      /AKS automatic node resource group.*at most 80.*--resource-group.*--cluster-name.*--region/s,
    );
  });
});

describe("selectAksKubernetesVersion", () => {
  const response = {
    values: [
      {
        version: "1.35",
        capabilities: { supportPlan: ["KubernetesOfficial", "AKSLongTermSupport"] },
        patchVersions: {
          "1.35.4": {},
          "1.35.6": {},
        },
      },
      {
        version: "1.36",
        capabilities: { supportPlan: [{ name: "KubernetesOfficial" }] },
        patchVersions: {
          "1.36.1": {},
          "1.36.2": { isPreview: true },
        },
      },
      {
        version: "1.34",
        capabilities: { supportPlan: ["AKSLongTermSupport"] },
        patchVersions: { "1.34.9": {} },
      },
    ],
  };

  it("selects the newest stable KubernetesOfficial patch", () => {
    expect(selectAksKubernetesVersion(response)).toBe("1.36.1");
  });

  it("selects the highest numeric patch from the live Azure CLI schema", () => {
    const liveWestUs3Shape = {
      values: [
        {
          version: "1.36",
          isPreview: null,
          capabilities: {
            supportPlan: ["KubernetesOfficial", "AKSLongTermSupport"],
          },
          patchVersions: {
            "1.36.0": {},
            "1.36.3": {},
          },
        },
        {
          version: "1.33",
          isPreview: null,
          capabilities: { supportPlan: ["AKSLongTermSupport"] },
          patchVersions: { "1.33.13": {} },
        },
      ],
    };
    expect(selectAksKubernetesVersion(liveWestUs3Shape)).toBe("1.36.3");
    expect(() =>
      selectAksKubernetesVersion(liveWestUs3Shape, "1.33.13"),
    ).toThrowError(/not available.*KubernetesOfficial/);
  });

  it("supports the aks-preview valuesProperty live schema", () => {
    const liveAksPreviewShape = {
      valuesProperty: [
        {
          version: "1.36",
          capabilities: {
            supportPlan: ["KubernetesOfficial", "AKSLongTermSupport"],
          },
          patchVersions: {
            "1.36.1": {},
            "1.36.4": {},
          },
        },
        {
          version: "1.35",
          capabilities: { supportPlan: ["AKSLongTermSupport"] },
          patchVersions: {
            "1.35.9": {},
          },
        },
      ],
    };
    expect(selectAksKubernetesVersion(liveAksPreviewShape)).toBe("1.36.4");
    expect(() =>
      selectAksKubernetesVersion(liveAksPreviewShape, "1.35.9"),
    ).toThrowError(/not available.*KubernetesOfficial/);
  });

  it("accepts an explicit standard-support minor or patch", () => {
    expect(selectAksKubernetesVersion(response, "1.35")).toBe("1.35");
    expect(selectAksKubernetesVersion(response, "1.35.4")).toBe("1.35.4");
    expect(selectAksKubernetesVersion(response, "v1.35.4")).toBe("1.35.4");
  });

  it("rejects LTS-only, preview, and unknown explicit versions", () => {
    expect(() => selectAksKubernetesVersion(response, "1.34")).toThrowError(
      /not available.*KubernetesOfficial/,
    );
    expect(() => selectAksKubernetesVersion(response, "1.36.2")).toThrowError(
      /not available.*KubernetesOfficial/,
    );
    expect(() => selectAksKubernetesVersion(response, "1.37")).toThrowError(
      /Supported versions include/,
    );
  });

  it("fails closed when Azure reports no standard-support versions", () => {
    expect(() =>
      selectAksKubernetesVersion({
        values: [{ version: "1.33", capabilities: { supportPlan: ["AKSLongTermSupport"] } }],
      }),
    ).toThrowError(/no stable KubernetesOfficial/);
  });
});

describe("VM metadata and quota parsing", () => {
  it("extracts family/vCPU metadata and regional remaining quota", () => {
    const capacities = parseVmSkuCapacities(
      [
        {
          name: "Standard_D2s_v3",
          family: "standardDSv3Family",
          capabilities: [{ name: "vCPUs", value: "2" }],
        },
      ],
      ["Standard_D2s_v3"],
    );
    expect(capacities.get("standard_d2s_v3")).toEqual(system);

    const parsedQuotas = parseRegionalVmFamilyQuotas([
      {
        name: { value: "standardDSv3Family" },
        currentValue: "3",
        limit: "10",
      },
      {
        name: { value: "cores" },
        currentValue: "8",
        limit: "20",
      },
    ]);
    expect(parsedQuotas.get("standarddsv3family")?.remaining).toBe(7);
    expect(parsedQuotas.get("cores")?.remaining).toBe(12);
  });

  it("fails closed when confidential Kata pool metadata is absent", () => {
    expect(() =>
      parseVmSkuCapacities(
        [
          {
            name: system.name,
            family: system.family,
            capabilities: [{ name: "vCPUs", value: "2" }],
          },
        ],
        [system.name, KATA_POOL_VM_SIZE],
      ),
    ).toThrowError(
      /metadata did not include family\/vCPU details.*Standard_D4as_v6/,
    );
  });
});

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

describe("existing node-count selection", () => {
  const cluster: ExistingAksCluster = {
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
        name: "system",
        count: 2,
        vmSize: system.name,
        mode: "System",
        provisioningState: "Succeeded",
        nodeLabels: {},
        nodeTaints: [],
        logicalRole: "system",
      },
      {
        name: "clawpool",
        count: 3,
        vmSize: sandboxSameFamily.name,
        mode: "User",
        provisioningState: "Succeeded",
        nodeLabels: { "kars.azure.com/pool": "sandbox" },
        nodeTaints: [],
        logicalRole: "sandbox",
      },
      {
        name: "katapool",
        count: 1,
        vmSize: KATA_POOL_VM_SIZE,
        mode: "User",
        provisioningState: "Succeeded",
        nodeLabels: { "kars.azure.com/pool": "sandbox-kata" },
        nodeTaints: [],
        logicalRole: "kata",
      },
    ],
  };

  it("directs a differing standard sandbox count to the AKS child-pool workflow", () => {
    const selection = classifyExistingNodeCountSelection({
      cluster,
      isolation: "standard",
      nodeCount: 5,
      nodeCountExplicit: true,
    });
    expect(selection).toMatchObject({
      action: "update",
      differences: [
        { logicalRole: "sandbox", name: "clawpool", current: 3, desired: 5 },
      ],
    });
    expect(() =>
      requireSupportedExistingNodeCountWorkflow(selection, cluster),
    ).toThrowError(
      /az aks nodepool scale --resource-group rg --cluster-name kars-aks --name clawpool --node-count 5.*rerun kars up without --node-count/s,
    );
  });

  it("treats an exact-equal standard sandbox count as a reuse no-op", () => {
    expect(
      classifyExistingNodeCountSelection({
        cluster,
        isolation: "standard",
        nodeCount: 3,
        nodeCountExplicit: true,
      }),
    ).toEqual({ action: "reuse" });
  });

  it.each([
    ["Kata", 3, "kata", 1],
    ["regular sandbox", 1, "sandbox", 3],
  ])(
    "routes a confidential %s count difference through update",
    (_label, nodeCount, logicalRole, current) => {
      expect(
        classifyExistingNodeCountSelection({
          cluster,
          isolation: "confidential",
          nodeCount,
          nodeCountExplicit: true,
        }),
      ).toMatchObject({
        action: "update",
        differences: [
          {
            logicalRole,
            current,
            desired: nodeCount,
          },
        ],
      });
    },
  );
});

describe("existing governed pool topology gate", () => {
  const healthy: ExistingAksCluster = {
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
        name: "system",
        count: 2,
        vmSize: system.name,
        mode: "System",
        provisioningState: "Succeeded",
        nodeLabels: {},
        nodeTaints: [],
        logicalRole: "system",
      },
      {
        name: "clawpool",
        count: 3,
        vmSize: sandboxSameFamily.name,
        mode: "User",
        provisioningState: "Succeeded",
        nodeLabels: { "kars.azure.com/pool": "sandbox" },
        nodeTaints: [],
        logicalRole: "sandbox",
      },
    ],
  };

  it("accepts a healthy standard topology", () => {
    expect(() =>
      requireHealthyExistingAksPoolTopology(healthy, "standard"),
    ).not.toThrow();
  });

  it("gives an AKS child-pool add command for a missing confidential Kata pool", () => {
    expect(() =>
      requireHealthyExistingAksPoolTopology(healthy, "confidential"),
    ).toThrowError(
      /az aks nodepool add --resource-group rg --cluster-name kars-aks --name katapool.*--workload-runtime KataVmIsolation.*then rerun kars up/s,
    );
  });

  it.each(["Failed", "Creating"])(
    "gives repair guidance for a %s governed pool",
    (provisioningState) => {
      const cluster = structuredClone(healthy);
      cluster.agentPoolProfiles[1].provisioningState = provisioningState;
      expect(() =>
        requireHealthyExistingAksPoolTopology(cluster, "standard"),
      ).toThrowError(
        new RegExp(
          `pool 'clawpool' provisioningState=${provisioningState}.*` +
            "az aks nodepool show --resource-group rg --cluster-name kars-aks --name clawpool",
          "s",
        ),
      );
    },
  );
});

describe("detectExistingAksCluster", () => {
  const clusterPayload = (provisioningState: string) => ({
    id: "/subscriptions/sub/resourceGroups/kars-westus3/providers/Microsoft.ContainerService/managedClusters/kars-aks",
    provisioningState,
    powerState: { code: "Running" },
    kubernetesVersion: "1.35.6",
    supportPlan: "KubernetesOfficial",
    sku: { name: "Base", tier: "Free" },
    autoUpgradeProfile: {
      upgradeChannel: "stable",
      nodeOSUpgradeChannel: "SecurityPatch",
    },
  });
  const nodePoolPayload = (
    sandboxState = "Succeeded",
  ): Array<Omit<AksAgentPoolProfile, "logicalRole">> => [
    {
      name: "sysal",
      count: 2,
      vmSize: system.name,
      mode: "System",
      provisioningState: "Succeeded",
      nodeLabels: {},
      nodeTaints: ["CriticalAddonsOnly=true:NoSchedule"],
    },
    {
      name: "clawal",
      count: 3,
      vmSize: sandboxSameFamily.name,
      mode: "User",
      provisioningState: sandboxState,
      nodeLabels: { "kars.azure.com/pool": "sandbox" },
      nodeTaints: ["kars.azure.com/sandbox=true:NoSchedule"],
    },
  ];

  it("returns typed state for a successful lookup and uses a read-only az command", async () => {
    const calls: string[][] = [];
    const detection = await detectExistingAksCluster(
      "kars-westus3",
      "kars-aks",
      async (args) => {
        calls.push(args);
        return JSON.stringify(
          args[1] === "nodepool"
            ? nodePoolPayload()
            : clusterPayload("Succeeded"),
        );
      },
      "sub-selected",
    );
    expect(detection).toMatchObject({
      exists: true,
      ...clusterPayload("Succeeded"),
      agentPoolProfiles: [
        { name: "sysal", logicalRole: "system" },
        { name: "clawal", logicalRole: "sandbox" },
      ],
    });
    expect(calls).toEqual([
      [
        "aks",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "kars-aks",
        "--query",
        "{id:id, provisioningState:provisioningState, powerState:{code:powerState.code}, kubernetesVersion:kubernetesVersion, supportPlan:supportPlan, sku:{name:sku.name,tier:sku.tier}, autoUpgradeProfile:{upgradeChannel:autoUpgradeProfile.upgradeChannel,nodeOSUpgradeChannel:autoUpgradeProfile.nodeOSUpgradeChannel}}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "aks",
        "nodepool",
        "list",
        "--resource-group",
        "kars-westus3",
        "--cluster-name",
        "kars-aks",
        "--query",
        "[].{name:name,count:count,vmSize:vmSize,mode:mode,provisioningState:provisioningState,nodeLabels:nodeLabels,nodeTaints:nodeTaints,workloadRuntime:workloadRuntime}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
    ]);
  });

  it("returns absent only for an explicit Azure not-found code", async () => {
    await expect(
      detectExistingAksCluster("missing-rg", "kars-aks", async () => {
        throw {
          stderr:
            "(ResourceGroupNotFound) Resource group 'missing-rg' could not be found.",
        };
      }),
    ).resolves.toEqual({ exists: false });
  });

  it("rejects --skip-infra when the cluster is absent or unhealthy", () => {
    expect(() =>
      requireHealthySkipInfraCluster({ exists: false }, "standard"),
    ).toThrowError(
      /--skip-infra.*not found/,
    );
    const failed: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Failed"),
      agentPoolProfiles: [],
    };
    expect(() => requireHealthySkipInfraCluster(failed, "standard")).toThrowError(
      /--skip-infra.*not healthy.*cluster=Failed/,
    );
  });

  it("accepts --skip-infra only for a healthy reuse disposition", () => {
    const healthy: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      agentPoolProfiles: [
        {
          ...nodePoolPayload()[0],
          logicalRole: "system",
        },
        {
          ...nodePoolPayload()[1],
          logicalRole: "sandbox",
        },
      ],
    };
    expect(requireHealthySkipInfraCluster(healthy, "standard")).toBe(healthy);
  });

  it("reuses only Succeeded clusters and routes Failed/Creating to recovery", () => {
    const succeeded: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      agentPoolProfiles: [
        {
          ...nodePoolPayload()[0],
          workloadRuntime: undefined,
          logicalRole: "system",
        },
        {
          ...nodePoolPayload()[1],
          workloadRuntime: undefined,
          logicalRole: "sandbox",
        },
      ],
    };
    expect(classifyExistingAksCluster(succeeded, false, "standard").action).toBe("reuse");
    expect(classifyExistingAksCluster(succeeded, true, "standard")).toMatchObject(
      {
        action: "force-update",
        diagnostic: expect.stringMatching(
          /full managedClusters Bicep template.*autoscaling.*availability zones.*valid only when the AKS cluster does not exist/s,
        ),
      },
    );
    for (const state of ["Failed", "Creating"]) {
      const disposition = classifyExistingAksCluster(
        { ...succeeded, provisioningState: state },
        false,
        "standard",
      );
      expect(disposition.action).toBe("recover");
      expect(
        disposition.action === "recover" && disposition.diagnostic,
      ).toContain(`cluster=${state}`);
    }
  });

  it("allows healthy reuse with multiple system pools but rejects template mutation", () => {
    const healthy: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      agentPoolProfiles: identifyAksPoolRoles([
        ...nodePoolPayload(),
        {
          ...nodePoolPayload()[0],
          name: "sysbackup",
        },
      ]),
    };

    expect(classifyExistingAksCluster(healthy, false, "standard").action).toBe(
      "reuse",
    );
    expect(classifyExistingAksCluster(healthy, true, "standard").action).toBe(
      "force-update",
    );
    expect(() =>
      requireTemplateSafeExistingAksMutation(healthy, "standard"),
    ).toThrowError(/2 system pools.*template can preserve only one/);
  });

  it("allows healthy LTS reuse but rejects forced or recovery mutation", () => {
    const lts: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      supportPlan: "AKSLongTermSupport",
      sku: { name: "Base", tier: "Premium" },
      agentPoolProfiles: identifyAksPoolRoles(nodePoolPayload()),
    };

    expect(classifyExistingAksCluster(lts, false, "standard").action).toBe(
      "reuse",
    );
    expect(() =>
      requireTemplateSafeExistingAksMutation(lts, "standard"),
    ).toThrowError(/supportPlan=AKSLongTermSupport.*sku.tier=Premium/);

    lts.provisioningState = "Failed";
    expect(classifyExistingAksCluster(lts, false, "standard").action).toBe(
      "recover",
    );
    expect(() =>
      requireTemplateSafeExistingAksMutation(lts, "standard"),
    ).toThrowError(/supportPlan=AKSLongTermSupport/);
  });

  it.each([
    [
      "custom Kubernetes channel",
      { upgradeChannel: "rapid", nodeOSUpgradeChannel: "SecurityPatch" },
      /upgradeChannel=rapid/,
    ],
    [
      "custom node OS channel",
      { upgradeChannel: "stable", nodeOSUpgradeChannel: "NodeImage" },
      /nodeOSUpgradeChannel=NodeImage/,
    ],
  ])("rejects %s before template mutation", (_label, profile, diagnostic) => {
    const cluster: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      autoUpgradeProfile: profile,
      agentPoolProfiles: identifyAksPoolRoles(nodePoolPayload()),
    };
    expect(() =>
      requireTemplateSafeExistingAksMutation(cluster, "standard"),
    ).toThrowError(diagnostic);
  });

  it("fails closed on authorization and network errors", async () => {
    await expect(
      detectExistingAksCluster("kars-westus3", "kars-aks", async () => {
        throw { stderr: "(AuthorizationFailed) The client is not authorized." };
      }),
    ).rejects.toThrow(/Could not determine.*AuthorizationFailed/);
    await expect(
      detectExistingAksCluster("kars-westus3", "kars-aks", async () => {
        throw new Error("Connection reset by peer");
      }),
    ).rejects.toThrow(/Could not determine.*Connection reset by peer/);
  });

  it("fails closed on an incomplete successful response", async () => {
    await expect(
      detectExistingAksCluster(
        "kars-westus3",
        "kars-aks",
        async (args) =>
          JSON.stringify(
            args[1] === "nodepool"
              ? {}
              : { id: "id", provisioningState: "Creating" },
          ),
      ),
    ).rejects.toThrow(/incomplete state/);
  });

  it("routes a Succeeded cluster with a Failed sandbox pool to recovery", () => {
    const cluster: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      agentPoolProfiles: [
        {
          ...nodePoolPayload("Failed")[0],
          workloadRuntime: undefined,
          logicalRole: "system",
        },
        {
          ...nodePoolPayload("Failed")[1],
          workloadRuntime: undefined,
          logicalRole: "sandbox",
        },
      ],
    };
    const disposition = classifyExistingAksCluster(cluster, false, "standard");
    expect(disposition.action).toBe("recover");
    expect(
      disposition.action === "recover" && disposition.diagnostic,
    ).toContain("clawal=Failed");
  });

  it("requires Running power state for reuse and gives start guidance", () => {
    const stopped: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      powerState: { code: "Stopped" },
      agentPoolProfiles: identifyAksPoolRoles(nodePoolPayload()),
    };
    const disposition = classifyExistingAksCluster(
      stopped,
      false,
      "standard",
    );
    expect(disposition.action).toBe("stopped");
    expect(
      disposition.action === "stopped" && disposition.diagnostic,
    ).toMatch(/powerState=Stopped.*az aks start.*kars-westus3.*kars-aks/);
    expect(() =>
      requireHealthySkipInfraCluster(stopped, "standard"),
    ).toThrowError(/az aks start/);
  });

  it("routes a standard cluster to recovery when confidential isolation is requested", () => {
    const cluster: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      agentPoolProfiles: identifyAksPoolRoles(nodePoolPayload()),
    };
    const disposition = classifyExistingAksCluster(
      cluster,
      false,
      "confidential",
    );
    expect(disposition.action).toBe("recover");
    expect(
      disposition.action === "recover" && disposition.diagnostic,
    ).toContain("Kars Kata pools=0");
    expect(() =>
      requireHealthySkipInfraCluster(cluster, "confidential"),
    ).toThrowError(/Repair the existing cluster.*do not use --force-infra/);
  });

  it("routes a failed Kata pool to confidential recovery", () => {
    const kataPool: Omit<AksAgentPoolProfile, "logicalRole"> = {
      name: "isolatedal",
      count: 3,
      vmSize: KATA_POOL_VM_SIZE,
      mode: "User",
      provisioningState: "Failed",
      nodeLabels: { "kars.azure.com/pool": "sandbox-kata" },
      nodeTaints: [],
      workloadRuntime: "KataMshvVmIsolation",
    };
    const cluster: ExistingAksCluster = {
      exists: true,
      ...clusterPayload("Succeeded"),
      agentPoolProfiles: identifyAksPoolRoles([
        ...nodePoolPayload(),
        kataPool,
      ]),
    };
    const disposition = classifyExistingAksCluster(
      cluster,
      false,
      "confidential",
    );
    expect(disposition.action).toBe("recover");
    expect(
      disposition.action === "recover" && disposition.diagnostic,
    ).toContain("isolatedal=Failed");
  });

  it("identifies an arbitrarily named Kata pool by label/runtime", async () => {
    const detection = await detectExistingAksCluster(
      "kars-westus3",
      "kars-aks",
      async (args) =>
        JSON.stringify(
          args[1] === "nodepool"
            ? [
                ...nodePoolPayload(),
                {
                  name: "isolatedal",
                  count: 3,
                  vmSize: KATA_POOL_VM_SIZE,
                  mode: "User",
                  provisioningState: "Succeeded",
                  nodeLabels: { "kars.azure.com/pool": "sandbox-kata" },
                  nodeTaints: [],
                  workloadRuntime: "KataMshvVmIsolation",
                },
              ]
            : clusterPayload("Succeeded"),
        ),
    );
    expect(
      detection.exists &&
        detection.agentPoolProfiles.find((pool) => pool.name === "isolatedal")
          ?.logicalRole,
    ).toBe("kata");
    expect(classifyExistingAksCluster(detection, false, "confidential").action).toBe(
      "reuse",
    );
  });

  it("uses mode=User fallback only for one unambiguous sandbox pool", () => {
    const unlabeled = nodePoolPayload().map((pool) => ({
      ...pool,
      nodeLabels: {},
    }));
    expect(
      identifyAksPoolRoles(unlabeled).find((pool) => pool.name === "clawal")
        ?.logicalRole,
    ).toBe("sandbox");

    const ambiguous = identifyAksPoolRoles([
      ...unlabeled,
      {
        name: "otheral",
        count: 1,
        vmSize: sandboxSameFamily.name,
        mode: "User",
        provisioningState: "Succeeded",
        nodeLabels: {},
        nodeTaints: [],
      },
    ]);
    expect(
      ambiguous.filter((pool) => pool.logicalRole === "sandbox"),
    ).toHaveLength(0);
  });
});

describe("infrastructure deployment completeness", () => {
  const scope =
    "/subscriptions/sub-1/resourceGroups/kars-westus3/providers";
  const completePayload = {
    id: `${scope}/Microsoft.Resources/deployments/main`,
    provisioningState: "Succeeded",
    outputs: {
      acrLoginServer: { type: "String", value: "karsacr.azurecr.io" },
      acrName: { type: "String", value: "karsacr" },
      sandboxIdentityClientId: {
        type: "String",
        value: "00000000-0000-0000-0000-000000000001",
      },
      keyVaultName: { type: "String", value: "kars-kv-abc123" },
      openAiEndpoint: {
        type: "String",
        value: "https://kars-aoai.openai.azure.com/",
      },
    },
  };

  function successfulLiveResourceRunner(calls: string[][] = []) {
    return vi.fn(async (args: string[]) => {
      calls.push(args);
      if (args[0] === "deployment") return JSON.stringify(completePayload);
      if (args[0] === "acr") {
        return JSON.stringify({
          id: `${scope}/Microsoft.ContainerRegistry/registries/karsacr`,
          name: "karsacr",
          loginServer: "karsacr.azurecr.io",
        });
      }
      if (args[0] === "keyvault") {
        return JSON.stringify({
          id: `${scope}/Microsoft.KeyVault/vaults/kars-kv-abc123`,
          name: "kars-kv-abc123",
        });
      }
      if (args[0] === "identity") {
        return JSON.stringify([
          {
            id: `${scope}/Microsoft.ManagedIdentity/userAssignedIdentities/kars-aks-sandbox-wi`,
            name: "kars-aks-sandbox-wi",
            clientId: "00000000-0000-0000-0000-000000000001",
          },
        ]);
      }
      if (args[0] === "cognitiveservices") {
        return JSON.stringify([
          {
            id: `${scope}/Microsoft.CognitiveServices/accounts/kars-aoai`,
            name: "kars-aoai",
            kind: "OpenAI",
            endpoint: "https://kars-aoai.openai.azure.com/",
          },
        ]);
      }
      throw new Error(`Unexpected Azure command: ${args.join(" ")}`);
    });
  }

  it("accepts only a Succeeded deployment with every required ARM output value", () => {
    expect(classifyInfrastructureDeployment(completePayload)).toEqual({
      complete: true,
      diagnostic:
        "Resource-group deployment 'main' succeeded with all required outputs.",
    });
  });

  it("classifies Failed deployments and missing output values as incomplete", () => {
    expect(
      classifyInfrastructureDeployment({
        ...completePayload,
        provisioningState: "Failed",
      }),
    ).toMatchObject({ complete: false, diagnostic: expect.stringContaining("Failed") });
    const missingOutput = structuredClone(completePayload);
    delete (missingOutput.outputs as Partial<typeof missingOutput.outputs>).keyVaultName;
    const result = classifyInfrastructureDeployment(missingOutput);
    expect(result).toMatchObject({
      complete: false,
      diagnostic: expect.stringContaining("keyVaultName"),
    });
    expect(() => requireCompleteSkipInfraDeployment(result)).toThrowError(
      /Existing AKS infrastructure is incomplete.*missing ancillary resources.*Do not use --force-infra/s,
    );
  });

  it("verifies every retained output against exact read-only resource-group scoped commands", async () => {
    const calls: string[][] = [];
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        { subscriptionId: "sub-selected" },
        successfulLiveResourceRunner(calls),
      ),
    ).resolves.toMatchObject({
      complete: true,
      diagnostic: expect.stringContaining("resolved to live"),
    });

    expect(calls).toEqual([
      [
        "deployment",
        "group",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "main",
        "--query",
        "{id:id,provisioningState:properties.provisioningState,outputs:properties.outputs}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "acr",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "karsacr",
        "--query",
        "{id:id,name:name,loginServer:loginServer}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "keyvault",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "kars-kv-abc123",
        "--query",
        "{id:id,name:name}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "identity",
        "list",
        "--resource-group",
        "kars-westus3",
        "--query",
        "[].{id:id,name:name,clientId:clientId}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "cognitiveservices",
        "account",
        "list",
        "--resource-group",
        "kars-westus3",
        "--query",
        "[].{id:id,name:name,kind:kind,endpoint:properties.endpoint}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
    ]);
  });

  it("uses a read-only deployment show query and treats deployment not-found as incomplete", async () => {
    const calls: string[][] = [];
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          calls.push(args);
          throw {
            stderr: "(DeploymentNotFound) Deployment 'main' was not found.",
          };
        },
      ),
    ).resolves.toMatchObject({ complete: false, diagnostic: expect.stringContaining("not found") });
    expect(calls).toEqual([
      [
        "deployment",
        "group",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "main",
        "--query",
        "{id:id,provisioningState:properties.provisioningState,outputs:properties.outputs}",
        "-o",
        "json",
      ],
    ]);
  });

  it.each([
    ["Azure Container Registry", "acr"],
    ["Key Vault", "keyvault"],
    ["user-assigned identity", "identity"],
    ["Azure OpenAI account", "cognitiveservices"],
  ])("treats a missing live %s as incomplete", async (expected, missingCommand) => {
    const baseRunner = successfulLiveResourceRunner();
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          if (args[0] === missingCommand) {
            if (missingCommand === "identity" || missingCommand === "cognitiveservices") {
              return "[]";
            }
            throw { stderr: "(ResourceNotFound) resource was not found" };
          }
          return baseRunner(args);
        },
      ),
    ).resolves.toMatchObject({
      complete: false,
      diagnostic: expect.stringContaining(expected),
    });
  });

  it("rejects stale output values that do not match the live resource", async () => {
    const baseRunner = successfulLiveResourceRunner();
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          if (args[0] === "acr") {
            return JSON.stringify({
              id: `${scope}/Microsoft.ContainerRegistry/registries/karsacr`,
              name: "karsacr",
              loginServer: "replacement.azurecr.io",
            });
          }
          return baseRunner(args);
        },
      ),
    ).resolves.toMatchObject({
      complete: false,
      diagnostic: expect.stringContaining("does not match"),
    });
  });

  it("does not require a provisioned AI account when an external endpoint was supplied", async () => {
    const externalPayload = structuredClone(completePayload);
    externalPayload.outputs.openAiEndpoint.value = "";
    const baseRunner = successfulLiveResourceRunner();
    const calls: string[][] = [];
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {
          foundryEndpoint:
            "https://shared.services.ai.azure.com/api/projects/project",
        },
        async (args) => {
          calls.push(args);
          if (args[0] === "deployment") return JSON.stringify(externalPayload);
          return baseRunner(args);
        },
      ),
    ).resolves.toMatchObject({
      complete: true,
      diagnostic: expect.stringContaining("external AI"),
    });
    expect(calls.some((args) => args[0] === "cognitiveservices")).toBe(false);
  });

  it("fails closed on authorization and malformed Azure responses", async () => {
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        async () => {
          throw { stderr: "(AuthorizationFailed) deployment access denied" };
        },
      ),
    ).rejects.toThrow(/Could not verify resource-group deployment.*AuthorizationFailed/);

    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          if (args[0] === "deployment") return JSON.stringify(completePayload);
          throw { stderr: "(AuthorizationFailed) access denied" };
        },
      ),
    ).rejects.toThrow(/Could not verify Azure Container Registry.*AuthorizationFailed/);

    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async () => "{not-json",
      ),
    ).rejects.toThrow(/malformed JSON/);
  });
});

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
