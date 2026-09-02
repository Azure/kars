// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  classifyExistingAksCluster,
  detectExistingAksCluster,
  identifyAksPoolRoles,
  KATA_POOL_VM_SIZE,
  requireHealthySkipInfraCluster,
  requireTemplateSafeExistingAksMutation,
  type AksAgentPoolProfile,
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
