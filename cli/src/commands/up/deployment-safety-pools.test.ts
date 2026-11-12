// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  classifyExistingNodeCountSelection,
  KATA_POOL_VM_SIZE,
  requireHealthyExistingAksPoolTopology,
  requireSupportedExistingNodeCountWorkflow,
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

