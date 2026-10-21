// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  pickUsableVmSize,
  resolveVmSizes,
  SYSTEM_POOL_VM_PREFERENCES,
  usableSkuSet,
  USER_POOL_VM_PREFERENCES,
  type ResolvedVmSizes,
} from "../../../lib/vm-size.js";
import {
  defaultAzJsonRunner,
  type AksAgentPoolProfile,
  type AksPoolLogicalRole,
  type AzJsonRunner,
  type ExistingAksCluster,
} from "./core.js";
import {
  classifyExistingNodeCountSelection,
  requireHealthyExistingAksPoolTopology,
  requireSupportedExistingNodeCountWorkflow,
  requireTemplateSafeExistingAksMutation,
  validateExistingKubernetesVersionSelection,
  validateExistingPoolVmSizeSelections,
} from "./aks-topology.js";
import {
  additionalPoolVcpus,
  calculateIncrementalQuotaRequirements,
  DEFAULT_SANDBOX_NODE_COUNT,
  KATA_POOL_VM_SIZE,
  MINIMUM_SANDBOX_NODE_COUNT,
  parseRegionalVmFamilyQuotas,
  parseVmSkuCapacities,
  quotaFailure,
  resolveSandboxNodeCountForQuota,
  SYSTEM_POOL_NODE_COUNT,
  vmSkuValues,
  type NamedPoolFootprint,
  type NodeCountResolution,
  type QuotaRequirement,
} from "./quota-sku.js";
import { selectAksKubernetesVersion } from "./versions.js";

export interface AzureDeploymentSafetyResult {
  kubernetesVersion: string;
  vmSizes: ResolvedVmSizes;
  systemNodeCount: number;
  kataVmSize: string;
  kataNodeCount: number;
  poolNames: {
    system: string;
    sandbox: string;
    kata: string;
  };
  additionalNodePools: Array<{ label: string; vmSize: string; count: number }>;
  nodeCount: number;
  adaptedNodeCount: boolean;
  quotaRequirements: QuotaRequirement[];
}

export interface AzureDeploymentSafetyProjection {
  kubernetesVersion?: string;
  nodeVmSize?: string;
  systemVmSize?: string;
  systemNodeCount?: number;
  nodeCount?: number;
  systemPoolName?: string;
  sandboxPoolName?: string;
  kataPoolName?: string;
  kataVmSize?: string;
  kataNodeCount?: number;
}

/**
 * Make the validated safety projection authoritative for deployment. Keeping
 * this assignment centralized prevents deployment from reverting to defaults
 * or independently derived pool names after preflight.
 */
export function applyAzureDeploymentSafetyResult(
  target: AzureDeploymentSafetyProjection,
  safety: AzureDeploymentSafetyResult,
): void {
  target.kubernetesVersion = safety.kubernetesVersion;
  target.nodeVmSize = safety.vmSizes.node;
  target.systemVmSize = safety.vmSizes.system;
  target.systemNodeCount = safety.systemNodeCount;
  target.nodeCount = safety.nodeCount;
  target.systemPoolName = safety.poolNames.system;
  target.sandboxPoolName = safety.poolNames.sandbox;
  target.kataPoolName = safety.poolNames.kata;
  target.kataVmSize = safety.kataVmSize;
  target.kataNodeCount = safety.kataNodeCount;
}

export async function resolveAzureDeploymentSafety(
  input: {
    region: string;
    subscriptionId?: string;
    kubernetesVersion?: string;
    kubernetesVersionExplicit?: boolean;
    nodeCount?: number;
    nodeCountExplicit: boolean;
    nodeVmSize?: string;
    nodeVmSizeExplicit?: boolean;
    kataVmSize?: string;
    kataVmSizeExplicit?: boolean;
    systemVmSize?: string;
    systemVmSizeExplicit?: boolean;
    isolation?: string;
    currentCluster?: ExistingAksCluster;
  },
  dependencies: {
    runAzJson?: AzJsonRunner;
    resolveSizes?: typeof resolveVmSizes;
  } = {},
): Promise<AzureDeploymentSafetyResult> {
  const runAzJson = dependencies.runAzJson ?? defaultAzJsonRunner;
  const resolveSizes = dependencies.resolveSizes ?? resolveVmSizes;
  const currentSystem = input.currentCluster?.agentPoolProfiles.find(
    (pool) => pool.logicalRole === "system",
  );
  const currentSandbox = input.currentCluster?.agentPoolProfiles.find(
    (pool) => pool.logicalRole === "sandbox",
  );
  const currentKata = input.currentCluster?.agentPoolProfiles.find(
    (pool) => pool.logicalRole === "kata",
  );
  const systemNodeCount = currentSystem?.count ?? SYSTEM_POOL_NODE_COUNT;
  if (input.currentCluster) {
    requireHealthyExistingAksPoolTopology(
      input.currentCluster,
      input.isolation ?? "standard",
      {
        nodeCount: input.nodeCount,
        nodeVmSize: input.nodeVmSize,
        systemVmSize: input.systemVmSize,
        kataVmSize: input.kataVmSize,
      },
    );
    requireSupportedExistingNodeCountWorkflow(
      classifyExistingNodeCountSelection({
        cluster: input.currentCluster,
        isolation: input.isolation,
        nodeCount: input.nodeCount,
        nodeCountExplicit: input.nodeCountExplicit,
      }),
      input.currentCluster,
    );
    requireTemplateSafeExistingAksMutation(
      input.currentCluster,
      input.isolation ?? "standard",
    );
    validateExistingKubernetesVersionSelection({
      cluster: input.currentCluster,
      kubernetesVersion: input.kubernetesVersion,
      kubernetesVersionExplicit: input.kubernetesVersionExplicit,
    });
    validateExistingPoolVmSizeSelections({
      cluster: input.currentCluster,
      isolation: input.isolation,
      nodeVmSize: input.nodeVmSize,
      nodeVmSizeExplicit: input.nodeVmSizeExplicit,
      systemVmSize: input.systemVmSize,
      systemVmSizeExplicit: input.systemVmSizeExplicit,
      kataVmSize: input.kataVmSize,
      kataVmSizeExplicit: input.kataVmSizeExplicit,
    });
  }
  const poolNames = {
    system: currentSystem?.name ?? "system",
    sandbox: currentSandbox?.name ?? "clawpool",
    kata: currentKata?.name ?? "katapool",
  };
  const requestedNodeVmSize =
    currentSandbox && !input.nodeVmSizeExplicit
      ? currentSandbox.vmSize
      : input.nodeVmSize;
  const requestedSystemVmSize =
    currentSystem && !input.systemVmSizeExplicit
      ? currentSystem.vmSize
      : input.systemVmSize;
  const requestedNodeCount =
    currentSandbox && !input.nodeCountExplicit
      ? currentSandbox.count
      : input.nodeCount;

  const newClusterSizes = input.currentCluster
    ? Promise.resolve<ResolvedVmSizes | undefined>(undefined)
    : resolveSizes(
        input.region,
        requestedNodeVmSize,
        requestedSystemVmSize,
        input.subscriptionId,
      );
  const scopedAzArgs = (args: string[]) =>
    input.subscriptionId
      ? [...args, "--subscription", input.subscriptionId]
      : args;
  const [versions, skuPayload, quotaPayload, resolvedNewClusterSizes] =
    await Promise.all([
    runAzJson(scopedAzArgs([
      "aks",
      "get-versions",
      "--location",
      input.region,
      "-o",
      "json",
    ])),
    runAzJson(scopedAzArgs([
      "vm",
      "list-skus",
      "--location",
      input.region,
      "--resource-type",
      "virtualMachines",
      "--all",
      "-o",
      "json",
    ])),
    runAzJson(scopedAzArgs([
      "vm",
      "list-usage",
      "--location",
      input.region,
      "-o",
      "json",
    ])),
    newClusterSizes,
  ]);
  const usableVmSizes = usableSkuSet(vmSkuValues(skuPayload));
  const chooseExistingSize = (selection: {
    current?: AksAgentPoolProfile;
    requested?: string;
    explicit?: boolean;
    preferences: string[];
    poolLabel: string;
    flagName: string;
  }): string => {
    if (selection.explicit && !selection.requested) {
      throw new Error(`${selection.flagName} requires a VM size.`);
    }
    if (
      selection.current &&
      (!selection.explicit ||
        selection.requested?.toLowerCase() ===
          selection.current.vmSize.toLowerCase())
    ) {
      return selection.current.vmSize;
    }
    return pickUsableVmSize({
      usable: usableVmSizes,
      preferences: selection.preferences,
      poolLabel: selection.poolLabel,
      flagName: selection.flagName,
      requested: selection.requested,
    });
  };
  const vmSizes: ResolvedVmSizes = input.currentCluster
    ? {
        node: chooseExistingSize({
          current: currentSandbox,
          requested: input.nodeVmSize,
          explicit: input.nodeVmSizeExplicit,
          preferences: USER_POOL_VM_PREFERENCES,
          poolLabel: "sandbox",
          flagName: "--node-vm-size",
        }),
        system: chooseExistingSize({
          current: currentSystem,
          requested: input.systemVmSize,
          explicit: input.systemVmSizeExplicit,
          preferences: SYSTEM_POOL_VM_PREFERENCES,
          poolLabel: "system",
          flagName: "--system-vm-size",
        }),
        checked: true,
      }
    : resolvedNewClusterSizes!;
  if (!vmSizes.checked) {
    throw new Error(
      "Could not query VM sizes available to this subscription. Azure VM discovery " +
        "must succeed before provisioning new infrastructure.",
    );
  }

  const kubernetesVersion = input.currentCluster
    ? selectAksKubernetesVersion(
        versions,
        input.currentCluster.kubernetesVersion,
      )
    : selectAksKubernetesVersion(versions, input.kubernetesVersion);
  const confidential = input.isolation === "confidential";
  let kataVmSize =
    currentKata && !input.kataVmSizeExplicit
      ? currentKata.vmSize
      : input.kataVmSize ?? KATA_POOL_VM_SIZE;
  if (
    confidential &&
    input.kataVmSizeExplicit &&
    (!currentKata ||
      input.kataVmSize?.toLowerCase() !== currentKata.vmSize.toLowerCase())
  ) {
    kataVmSize = pickUsableVmSize({
      usable: usableVmSizes,
      preferences: [KATA_POOL_VM_SIZE, ...USER_POOL_VM_PREFERENCES],
      poolLabel: "Kata sandbox",
      flagName: "--kata-vm-size",
      requested: input.kataVmSize,
    });
  }
  const rejectExistingPoolResize = (
    current: AksAgentPoolProfile | undefined,
    desired: string,
    label: string,
  ): void => {
    if (!current || current.vmSize.toLowerCase() === desired.toLowerCase()) return;
    throw new Error(
      `Existing ${label} AKS node pool '${current.name}' uses VM size '${current.vmSize}', ` +
        `but '${desired}' was requested. Kars cannot resize an existing AKS node pool in place; ` +
        "migrate/replace that pool using supported AKS tooling, then rerun.",
    );
  };
  rejectExistingPoolResize(currentSystem, vmSizes.system, "system");
  rejectExistingPoolResize(currentSandbox, vmSizes.node, "sandbox");
  if (confidential) {
    rejectExistingPoolResize(currentKata, kataVmSize, "Kata");
  }
  const additionalNodePool =
    confidential
      ? { label: "Kata sandbox", vmSize: kataVmSize }
      : undefined;
  const capacities = parseVmSkuCapacities(skuPayload, [
    vmSizes.system,
    vmSizes.node,
    ...(additionalNodePool ? [additionalNodePool.vmSize] : []),
  ]);
  if (
    !input.currentCluster &&
    additionalNodePool &&
    !input.kataVmSizeExplicit &&
    !usableVmSizes.has(kataVmSize.toLowerCase())
  ) {
    throw new Error(
      `Required Kata pool VM size '${kataVmSize}' is not currently allocatable in ` +
        `${input.region}. The Kata pool size is fixed; choose another Azure region.`,
    );
  }
  const regionalQuotas = parseRegionalVmFamilyQuotas(quotaPayload);
  let count: NodeCountResolution;
  let kataNodeCount = 0;
  if (input.currentCluster) {
    const desiredNodeCount =
      requestedNodeCount ?? DEFAULT_SANDBOX_NODE_COUNT;
    const desiredKataNodeCount = input.nodeCountExplicit
      ? desiredNodeCount
      : currentKata?.count ?? desiredNodeCount;
    if (
      !Number.isInteger(desiredNodeCount) ||
      desiredNodeCount < MINIMUM_SANDBOX_NODE_COUNT
    ) {
      throw new Error("--node-count must be an integer of at least 1.");
    }
    const footprint = (
      name: string,
      label: string,
      logicalRole: AksPoolLogicalRole,
      vmSize: string,
      poolCount: number,
    ): NamedPoolFootprint => {
      const capacity = capacities.get(vmSize.toLowerCase())!;
      return {
        name,
        label,
        logicalRole,
        vmSize,
        family: capacity.family,
        vcpusPerNode: capacity.vcpus,
        count: poolCount,
      };
    };
    const desiredPools = [
      footprint(
        poolNames.system,
        "system",
        "system",
        vmSizes.system,
        systemNodeCount,
      ),
      footprint(
        poolNames.sandbox,
        "sandbox",
        "sandbox",
        vmSizes.node,
        desiredNodeCount,
      ),
      ...(additionalNodePool
        ? [
            footprint(
          poolNames.kata,
              additionalNodePool.label,
          "kata",
              additionalNodePool.vmSize,
              desiredKataNodeCount,
            ),
          ]
        : []),
    ];
    const currentPools = input.currentCluster.agentPoolProfiles
      .filter(
        (pool) => pool.provisioningState.toLowerCase() === "succeeded",
      )
      .map((pool) => ({
        name: pool.name,
        label: pool.name,
        logicalRole: pool.logicalRole,
        vmSize: pool.vmSize,
        family: "",
        vcpusPerNode: 0,
        count: pool.count,
      }));
    const currentByName = new Map(
      currentPools.map((pool) => [pool.name.toLowerCase(), pool]),
    );
    for (const desired of desiredPools) {
      const additionalVcpus = additionalPoolVcpus(
        desired,
        currentByName.get(desired.name.toLowerCase()),
      );
      if (
        additionalVcpus > 0 &&
        !usableVmSizes.has(desired.vmSize.toLowerCase())
      ) {
        const existingPool = input.currentCluster.agentPoolProfiles.find(
          (pool) => pool.name.toLowerCase() === desired.name.toLowerCase(),
        );
        if (existingPool) {
          throw new Error(
            `Existing ${desired.label} AKS node pool '${desired.name}' uses VM size ` +
              `'${desired.vmSize}', which is not currently allocatable in ` +
              `${input.region}, but this update requires ${additionalVcpus} additional vCPU. ` +
              "Kars cannot resize an existing AKS node pool in place; migrate/replace that pool " +
              "using supported AKS tooling, then rerun.",
          );
        }
        throw new Error(
          `Required new ${desired.label} pool VM size '${desired.vmSize}' is not currently ` +
            `allocatable in ${input.region}. Choose an available VM size or another region.`,
        );
      }
    }
    const requirements = calculateIncrementalQuotaRequirements(
      desiredPools,
      currentPools,
      regionalQuotas,
    );
    if (
      requirements.some(
        (requirement) => requirement.required > requirement.remaining,
      )
    ) {
      throw quotaFailure(requirements);
    }
    count = {
      nodeCount: desiredNodeCount,
      adapted: false,
      requirements,
    };
    kataNodeCount = additionalNodePool ? desiredKataNodeCount : 0;
  } else {
    count = resolveSandboxNodeCountForQuota({
      requestedNodeCount,
      nodeCountExplicit: input.nodeCountExplicit,
      system: capacities.get(vmSizes.system.toLowerCase())!,
      sandbox: capacities.get(vmSizes.node.toLowerCase())!,
      additionalSandboxPools: additionalNodePool
        ? [{
            label: additionalNodePool.label,
            capacity: capacities.get(additionalNodePool.vmSize.toLowerCase())!,
          }]
        : [],
      quotas: regionalQuotas,
    });
    kataNodeCount = additionalNodePool ? count.nodeCount : 0;
  }
  const additionalNodePools = additionalNodePool
    ? [{ ...additionalNodePool, count: kataNodeCount }]
    : [];

  return {
    kubernetesVersion,
    vmSizes,
    systemNodeCount,
    kataVmSize,
    kataNodeCount,
    poolNames,
    additionalNodePools,
    nodeCount: count.nodeCount,
    adaptedNodeCount: count.adapted,
    quotaRequirements: count.requirements,
  };
}
