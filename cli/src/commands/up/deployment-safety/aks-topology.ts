// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  azureCliErrorText,
  defaultAzTextRunner,
  isAksNotFoundError,
  isMissingSubscriptionRegistrationError,
  resourceGroupFromId,
  resourceNameFromId,
  asRecord,
  type AksAgentPoolProfile,
  type AksClusterDetection,
  type AksPoolLogicalRole,
  type AzTextRunner,
  type ExistingAksCluster,
  type ExistingAksDisposition,
} from "./core.js";
import {
  DEFAULT_SANDBOX_NODE_COUNT,
  KATA_POOL_VM_SIZE,
  MINIMUM_SANDBOX_NODE_COUNT,
  SYSTEM_POOL_NODE_COUNT,
} from "./quota-sku.js";

export function classifyExistingAksCluster(
  detection: AksClusterDetection,
  forceInfra: boolean,
  isolation: string,
): ExistingAksDisposition {
  if (!detection.exists) return { action: "new" };
  if (detection.powerState.code.toLowerCase() !== "running") {
    return {
      action: "stopped",
      cluster: detection,
      diagnostic:
        `AKS cluster '${detection.id}' is stopped (powerState=${detection.powerState.code}). ` +
        `Start it with \`az aks start --resource-group ${resourceGroupFromId(detection.id)} ` +
        `--name ${resourceNameFromId(detection.id)}\`, wait until it is Running, then rerun.`,
    };
  }
  const systemPools = detection.agentPoolProfiles.filter(
    (pool) => pool.logicalRole === "system",
  );
  const sandboxPools = detection.agentPoolProfiles.filter(
    (pool) => pool.logicalRole === "sandbox",
  );
  const kataPools = detection.agentPoolProfiles.filter(
    (pool) => pool.logicalRole === "kata",
  );
  const confidential = isolation === "confidential";
  const governedPools = [
    ...systemPools,
    ...sandboxPools,
    ...(confidential ? kataPools : []),
  ];
  const issues: string[] = [];
  if (detection.provisioningState.toLowerCase() !== "succeeded") {
    issues.push(`cluster=${detection.provisioningState}`);
  }

  if (systemPools.length < 1) {
    issues.push(`system pools=${systemPools.length}`);
  }
  if (sandboxPools.length !== 1) {
    issues.push(`Kars sandbox pools=${sandboxPools.length}`);
  }
  if (confidential && kataPools.length !== 1) {
    issues.push(`Kars Kata pools=${kataPools.length}`);
  }
  for (const pool of governedPools) {
    if (pool.provisioningState.toLowerCase() !== "succeeded") {
      issues.push(`${pool.name}=${pool.provisioningState || "unknown"}`);
    }
  }
  if (issues.length > 0) {
    return {
      action: "recover",
      cluster: detection,
      diagnostic:
        `AKS cluster '${detection.id}' is not healthy (${issues.join(", ")}). ` +
        "Repair the AKS cluster or its node pools with supported AKS tooling, wait until " +
        "the cluster and governed pools are Succeeded, then rerun.",
    };
  }

  return forceInfra
    ? {
        action: "force-update",
        cluster: detection,
        diagnostic:
          `--force-infra cannot be used because AKS cluster '${detection.id}' already exists. ` +
          "Kars will not run the full managedClusters Bicep template against an existing cluster " +
          "because properties not modeled by the template, including autoscaling and availability " +
          "zones, could be reset. Remove --force-infra; a healthy cluster with complete surrounding " +
          "infrastructure is reused automatically. Repair incomplete infrastructure manually. " +
          "--force-infra is valid only when the AKS cluster does not exist.",
      }
    : { action: "reuse", cluster: detection };
}

export function requireHealthySkipInfraCluster(
  detection: AksClusterDetection,
  isolation: string,
): ExistingAksCluster {
  const disposition = classifyExistingAksCluster(detection, false, isolation);
  if (disposition.action === "reuse") return disposition.cluster;
  if (disposition.action === "new") {
    throw new Error(
      "--skip-infra requires an existing healthy AKS cluster, but the cluster was not found. " +
        "Remove --skip-infra to provision it.",
    );
  }
  if (disposition.action === "recover") {
    throw new Error(
      `--skip-infra requires an existing healthy AKS cluster. ${disposition.diagnostic} ` +
        "Repair the existing cluster with supported AKS tooling; do not use --force-infra.",
    );
  }
  if (disposition.action === "stopped") {
    throw new Error(
      `--skip-infra requires a Running AKS cluster. ${disposition.diagnostic}`,
    );
  }
  throw new Error("--skip-infra cannot be combined with a forced infrastructure update.");
}

/**
 * Bicep manages one system pool and writes the cluster-wide support, SKU, and
 * upgrade settings below. Reuse skips Bicep and may preserve any valid AKS
 * topology, but update/recovery must reject state the template cannot preserve.
 */
export function requireTemplateSafeExistingAksMutation(
  cluster: ExistingAksCluster,
  isolation: string,
): void {
  const systemPools = cluster.agentPoolProfiles.filter(
    (pool) => pool.logicalRole === "system",
  );
  const expectedNodeOSUpgradeChannel =
    isolation === "confidential" ? "NodeImage" : "SecurityPatch";
  const differences: string[] = [];
  const shown = (value: string) => value || "<unset>";

  if (systemPools.length > 1) {
    differences.push(
      `${systemPools.length} system pools (the template can preserve only one)`,
    );
  }
  if (cluster.supportPlan.toLowerCase() !== "kubernetesofficial") {
    differences.push(`supportPlan=${shown(cluster.supportPlan)}`);
  }
  if (cluster.sku.name.toLowerCase() !== "base") {
    differences.push(`sku.name=${shown(cluster.sku.name)}`);
  }
  if (cluster.sku.tier.toLowerCase() !== "free") {
    differences.push(`sku.tier=${shown(cluster.sku.tier)}`);
  }
  if (
    cluster.autoUpgradeProfile.upgradeChannel.toLowerCase() !== "stable"
  ) {
    differences.push(
      `upgradeChannel=${shown(cluster.autoUpgradeProfile.upgradeChannel)}`,
    );
  }
  if (
    cluster.autoUpgradeProfile.nodeOSUpgradeChannel.toLowerCase() !==
    expectedNodeOSUpgradeChannel.toLowerCase()
  ) {
    differences.push(
      `nodeOSUpgradeChannel=${shown(cluster.autoUpgradeProfile.nodeOSUpgradeChannel)}`,
    );
  }

  if (differences.length === 0) return;
  throw new Error(
    `Existing AKS cluster cannot be safely updated or recovered by the current template: ` +
      `${differences.join(", ")}. A healthy, complete cluster may still be reused without ` +
      "--force-infra; otherwise migrate these settings/topology with supported AKS tooling first.",
  );
}

interface ParsedPoolWithoutRole
  extends Omit<AksAgentPoolProfile, "logicalRole"> {}

function isKataPool(pool: ParsedPoolWithoutRole): boolean {
  const label = pool.nodeLabels["kars.azure.com/pool"]?.toLowerCase();
  return (
    label === "sandbox-kata" ||
    (pool.workloadRuntime ?? "").toLowerCase().includes("kata") ||
    (pool.workloadRuntime ?? "").toLowerCase().includes("vminsolation")
  );
}

export function identifyAksPoolRoles(
  pools: ParsedPoolWithoutRole[],
): AksAgentPoolProfile[] {
  const kataPools = new Set(pools.filter(isKataPool));
  const labeledSandboxPools = new Set(
    pools.filter(
      (pool) =>
        pool.nodeLabels["kars.azure.com/pool"]?.toLowerCase() === "sandbox",
    ),
  );
  const fallbackCandidates =
    labeledSandboxPools.size === 0
      ? pools.filter(
          (pool) =>
            pool.mode.toLowerCase() === "user" && !kataPools.has(pool),
        )
      : [];
  const fallbackSandbox =
    fallbackCandidates.length === 1 ? fallbackCandidates[0] : undefined;

  return pools.map((pool) => ({
    ...pool,
    logicalRole:
      pool.mode.toLowerCase() === "system"
        ? "system"
        : kataPools.has(pool)
          ? "kata"
          : labeledSandboxPools.has(pool) || pool === fallbackSandbox
            ? "sandbox"
            : "other",
  }));
}

function parseAksCluster(
  clusterPayload: unknown,
  poolsPayload: unknown,
  clusterName: string,
): ExistingAksCluster {
  const record = asRecord(clusterPayload);
  const id = typeof record?.id === "string" ? record.id.trim() : "";
  const provisioningState =
    typeof record?.provisioningState === "string"
      ? record.provisioningState.trim()
      : "";
  const kubernetesVersion =
    typeof record?.kubernetesVersion === "string"
      ? record.kubernetesVersion.trim()
      : "";
  const supportPlan =
    typeof record?.supportPlan === "string" ? record.supportPlan.trim() : "";
  const sku = asRecord(record?.sku);
  const skuName = typeof sku?.name === "string" ? sku.name.trim() : "";
  const skuTier = typeof sku?.tier === "string" ? sku.tier.trim() : "";
  const autoUpgradeProfile = asRecord(record?.autoUpgradeProfile);
  const upgradeChannel =
    typeof autoUpgradeProfile?.upgradeChannel === "string"
      ? autoUpgradeProfile.upgradeChannel.trim()
      : "";
  const nodeOSUpgradeChannel =
    typeof autoUpgradeProfile?.nodeOSUpgradeChannel === "string"
      ? autoUpgradeProfile.nodeOSUpgradeChannel.trim()
      : "";
  const powerState = asRecord(record?.powerState);
  const powerStateCode =
    typeof powerState?.code === "string" ? powerState.code.trim() : "";
  if (
    !id ||
    !provisioningState ||
    !powerStateCode ||
    !kubernetesVersion ||
    !Array.isArray(poolsPayload)
  ) {
    throw new Error(
      `Azure returned incomplete state while checking AKS cluster '${clusterName}'.`,
    );
  }
  const parsedPools: ParsedPoolWithoutRole[] = poolsPayload.map((value) => {
    const pool = asRecord(value);
    const name = typeof pool?.name === "string" ? pool.name.trim() : "";
    const count = Number(pool?.count);
    const vmSize = typeof pool?.vmSize === "string" ? pool.vmSize.trim() : "";
    const mode = typeof pool?.mode === "string" ? pool.mode.trim() : "";
    const poolState =
      typeof pool?.provisioningState === "string"
        ? pool.provisioningState.trim()
        : "";
    const rawLabels = asRecord(pool?.nodeLabels) ?? {};
    const nodeLabels = Object.fromEntries(
      Object.entries(rawLabels)
        .filter((entry): entry is [string, string] => typeof entry[1] === "string"),
    );
    const nodeTaints = Array.isArray(pool?.nodeTaints)
      ? pool.nodeTaints.filter(
          (taint): taint is string => typeof taint === "string",
        )
      : [];
    const workloadRuntime =
      typeof pool?.workloadRuntime === "string"
        ? pool.workloadRuntime.trim()
        : undefined;
    if (!name || !Number.isInteger(count) || count < 0 || !vmSize) {
      throw new Error(
        `Azure returned invalid agent-pool state while checking AKS cluster '${clusterName}'.`,
      );
    }
    return {
      name,
      count,
      vmSize,
      mode,
      provisioningState: poolState,
      nodeLabels,
      nodeTaints,
      workloadRuntime,
    };
  });
  return {
    exists: true,
    id,
    provisioningState,
    powerState: { code: powerStateCode },
    kubernetesVersion,
    supportPlan,
    sku: { name: skuName, tier: skuTier },
    autoUpgradeProfile: { upgradeChannel, nodeOSUpgradeChannel },
    agentPoolProfiles: identifyAksPoolRoles(parsedPools),
  };
}

export async function detectExistingAksCluster(
  resourceGroup: string,
  clusterName: string,
  runAzTextOrSubscription: AzTextRunner | string = defaultAzTextRunner,
  subscriptionId?: string,
): Promise<AksClusterDetection> {
  const runAzText =
    typeof runAzTextOrSubscription === "function"
      ? runAzTextOrSubscription
      : defaultAzTextRunner;
  const selectedSubscriptionId =
    typeof runAzTextOrSubscription === "string"
      ? runAzTextOrSubscription
      : subscriptionId;
  const scoped = (args: string[]) =>
    runAzText(
      selectedSubscriptionId
        ? [...args, "--subscription", selectedSubscriptionId]
        : args,
    );
  try {
    const clusterPayload = await scoped([
      "aks",
      "show",
      "--resource-group",
      resourceGroup,
      "--name",
      clusterName,
      "--query",
      "{id:id, provisioningState:provisioningState, powerState:{code:powerState.code}, kubernetesVersion:kubernetesVersion, supportPlan:supportPlan, sku:{name:sku.name,tier:sku.tier}, autoUpgradeProfile:{upgradeChannel:autoUpgradeProfile.upgradeChannel,nodeOSUpgradeChannel:autoUpgradeProfile.nodeOSUpgradeChannel}}",
      "-o",
      "json",
    ]);
    const poolsPayload = await scoped([
      "aks",
      "nodepool",
      "list",
      "--resource-group",
      resourceGroup,
      "--cluster-name",
      clusterName,
      "--query",
      "[].{name:name,count:count,vmSize:vmSize,mode:mode,provisioningState:provisioningState,nodeLabels:nodeLabels,nodeTaints:nodeTaints,workloadRuntime:workloadRuntime}",
      "-o",
      "json",
    ]);
    return parseAksCluster(
      JSON.parse(clusterPayload),
      JSON.parse(poolsPayload),
      clusterName,
    );
  } catch (error) {
    if (
      isAksNotFoundError(error) ||
      isMissingSubscriptionRegistrationError(error)
    ) {
      return { exists: false };
    }
    const detail = azureCliErrorText(error).split("\n")[0] || String(error);
    throw new Error(
      `Could not determine whether AKS cluster '${clusterName}' exists: ${detail}`,
      { cause: error },
    );
  }
}

export function validateExistingKubernetesVersionSelection(input: {
  cluster: ExistingAksCluster;
  kubernetesVersion?: string;
  kubernetesVersionExplicit?: boolean;
}): void {
  if (!input.kubernetesVersionExplicit || !input.kubernetesVersion) return;
  const requested = input.kubernetesVersion.replace(/^v/i, "");
  const current = input.cluster.kubernetesVersion.replace(/^v/i, "");
  if (requested === current) return;
  throw new Error(
    `Existing AKS cluster uses Kubernetes version '${input.cluster.kubernetesVersion}', ` +
      `but '${input.kubernetesVersion}' was requested. Kars cannot use --kubernetes-version ` +
      "as an unvalidated existing-cluster upgrade path; use supported AKS/Kars upgrade tooling, then rerun.",
  );
}

export type ExistingNodeCountSelection =
  | { action: "reuse" }
  | {
      action: "update";
      diagnostic: string;
      differences: Array<{
        logicalRole: "sandbox" | "kata";
        name: string;
        current: number;
        desired: number;
      }>;
    };

export function classifyExistingNodeCountSelection(input: {
  cluster: ExistingAksCluster;
  isolation?: string;
  nodeCount?: number;
  nodeCountExplicit?: boolean;
}): ExistingNodeCountSelection {
  if (!input.nodeCountExplicit) return { action: "reuse" };
  if (
    !Number.isInteger(input.nodeCount) ||
    input.nodeCount! < MINIMUM_SANDBOX_NODE_COUNT
  ) {
    throw new Error("--node-count must be an integer of at least 1.");
  }

  const affectedRoles: Array<"sandbox" | "kata"> = [
    "sandbox",
    ...(input.isolation === "confidential" ? (["kata"] as const) : []),
  ];
  const differences = affectedRoles.flatMap((logicalRole) => {
    const pool = input.cluster.agentPoolProfiles.find(
      (candidate) => candidate.logicalRole === logicalRole,
    );
    return pool && pool.count !== input.nodeCount
      ? [{
          logicalRole,
          name: pool.name,
          current: pool.count,
          desired: input.nodeCount!,
        }]
      : [];
  });
  if (differences.length === 0) return { action: "reuse" };

  return {
    action: "update",
    differences,
    diagnostic:
      `Explicit --node-count ${input.nodeCount} differs from existing ` +
      differences
        .map(
          (difference) =>
            `${difference.logicalRole} pool '${difference.name}' count ${difference.current}`,
        )
        .join(" and ") +
      "; infrastructure update and quota validation are required.",
  };
}

export function requireSupportedExistingNodeCountWorkflow(
  selection: ExistingNodeCountSelection,
  cluster: ExistingAksCluster,
): void {
  if (selection.action === "reuse") return;
  const resourceGroup = resourceGroupFromId(cluster.id);
  const clusterName = resourceNameFromId(cluster.id);
  const commands = selection.differences
    .map(
      ({ name, desired }) =>
        `az aks nodepool scale --resource-group ${resourceGroup} ` +
        `--cluster-name ${clusterName} --name ${name} --node-count ${desired}`,
    )
    .map((command) => `\`${command}\``)
    .join("; ");
  throw new Error(
    `${selection.diagnostic} Existing AKS node-pool counts are not changed through ` +
      "managedClusters.agentPoolProfiles. Scale the existing pool with the supported " +
      `AKS child-resource workflow: ${commands}. Wait until each pool provisioningState ` +
      "is Succeeded, then rerun kars up without --node-count.",
  );
}

export interface ExistingAksTopologyGuidance {
  nodeCount?: number;
  nodeVmSize?: string;
  systemVmSize?: string;
  kataVmSize?: string;
}

export function requireHealthyExistingAksPoolTopology(
  cluster: ExistingAksCluster,
  isolation: string,
  guidance: ExistingAksTopologyGuidance = {},
): void {
  const resourceGroup = resourceGroupFromId(cluster.id);
  const clusterName = resourceNameFromId(cluster.id);
  const systemPools = cluster.agentPoolProfiles.filter(
    (pool) => pool.logicalRole === "system",
  );
  const sandboxPools = cluster.agentPoolProfiles.filter(
    (pool) => pool.logicalRole === "sandbox",
  );
  const kataPools = cluster.agentPoolProfiles.filter(
    (pool) => pool.logicalRole === "kata",
  );
  const confidential = isolation === "confidential";
  const governedPools = [
    ...systemPools,
    ...sandboxPools,
    ...(confidential ? kataPools : []),
  ];
  const problems: string[] = [];

  if (cluster.provisioningState.toLowerCase() !== "succeeded") {
    problems.push(
      `cluster provisioningState=${cluster.provisioningState || "unknown"}. Inspect and ` +
        "repair the cluster with " +
        `\`az aks show --resource-group ${resourceGroup} --name ${clusterName} ` +
        "--query provisioningState -o tsv`",
    );
  }

  const desiredCount =
    guidance.nodeCount ?? sandboxPools[0]?.count ?? DEFAULT_SANDBOX_NODE_COUNT;
  if (systemPools.length === 0) {
    problems.push(
      "the system pool is missing. Add it through the AKS child-resource workflow: " +
        `\`az aks nodepool add --resource-group ${resourceGroup} --cluster-name ${clusterName} ` +
        `--name system --mode System --node-count ${SYSTEM_POOL_NODE_COUNT} ` +
        `--node-vm-size ${guidance.systemVmSize ?? "<supported-system-vm-size>"} ` +
        "--os-sku AzureLinux`",
    );
  }
  if (sandboxPools.length === 0) {
    problems.push(
      "the Kars sandbox pool is missing. Add it through the AKS child-resource workflow: " +
        `\`az aks nodepool add --resource-group ${resourceGroup} --cluster-name ${clusterName} ` +
        `--name clawpool --mode User --node-count ${desiredCount} ` +
        `--node-vm-size ${guidance.nodeVmSize ?? "<supported-sandbox-vm-size>"} ` +
        "--os-sku AzureLinux --labels kars.azure.com/pool=sandbox " +
        "--node-taints kars.azure.com/sandbox=true:NoSchedule`",
    );
  } else if (sandboxPools.length > 1) {
    problems.push(
      `${sandboxPools.length} Kars sandbox pools were detected. Inspect them with ` +
        `\`az aks nodepool list --resource-group ${resourceGroup} --cluster-name ${clusterName} ` +
        "-o table` " +
        "and reconcile the duplicate topology through the AKS child-resource workflow",
    );
  }
  if (confidential && kataPools.length === 0) {
    problems.push(
      "the confidential Kars Kata pool is missing. Add it through the AKS child-resource workflow: " +
        `\`az aks nodepool add --resource-group ${resourceGroup} --cluster-name ${clusterName} ` +
        `--name katapool --mode User --node-count ${desiredCount} ` +
        `--node-vm-size ${guidance.kataVmSize ?? KATA_POOL_VM_SIZE} ` +
        "--os-sku AzureLinux --workload-runtime KataVmIsolation " +
        "--labels kars.azure.com/pool=sandbox-kata " +
        "--node-taints kars.azure.com/sandbox=true:NoSchedule`",
    );
  } else if (confidential && kataPools.length > 1) {
    problems.push(
      `${kataPools.length} Kars Kata pools were detected. Inspect them with ` +
        `\`az aks nodepool list --resource-group ${resourceGroup} --cluster-name ${clusterName} ` +
        "-o table` " +
        "and reconcile the duplicate topology through the AKS child-resource workflow",
    );
  }

  for (const pool of governedPools) {
    if (pool.provisioningState.toLowerCase() === "succeeded") continue;
    problems.push(
      `pool '${pool.name}' provisioningState=${pool.provisioningState || "unknown"}. ` +
        "Inspect and repair or recreate it through the AKS child-resource workflow with " +
        `\`az aks nodepool show --resource-group ${resourceGroup} --cluster-name ${clusterName} ` +
        `--name ${pool.name} --query provisioningState -o tsv\``,
    );
  }

  if (problems.length === 0) return;
  throw new Error(
    `Existing AKS pool topology cannot be created, scaled, or repaired through ` +
      `managedClusters.agentPoolProfiles: ${problems.join("; ")}. Wait until the AKS ` +
      "cluster and every governed pool report provisioningState Succeeded, then rerun kars up.",
  );
}

export function validateExistingPoolVmSizeSelections(input: {
  cluster: ExistingAksCluster;
  isolation?: string;
  nodeVmSize?: string;
  nodeVmSizeExplicit?: boolean;
  systemVmSize?: string;
  systemVmSizeExplicit?: boolean;
  kataVmSize?: string;
  kataVmSizeExplicit?: boolean;
}): void {
  const rejectResize = (
    logicalRole: AksPoolLogicalRole,
    requested: string | undefined,
    explicit: boolean | undefined,
    label: string,
  ): void => {
    const current = input.cluster.agentPoolProfiles.find(
      (pool) => pool.logicalRole === logicalRole,
    );
    if (
      !current ||
      !explicit ||
      !requested ||
      current.vmSize.toLowerCase() === requested.toLowerCase()
    ) {
      return;
    }
    throw new Error(
      `Existing ${label} AKS node pool '${current.name}' uses VM size '${current.vmSize}', ` +
        `but '${requested}' was requested. Kars cannot resize an existing AKS node pool in place; ` +
        "migrate/replace that pool using supported AKS tooling, then rerun.",
    );
  };

  rejectResize(
    "system",
    input.systemVmSize,
    input.systemVmSizeExplicit,
    "system",
  );
  rejectResize(
    "sandbox",
    input.nodeVmSize,
    input.nodeVmSizeExplicit,
    "sandbox",
  );
  if (input.isolation === "confidential") {
    rejectResize(
      "kata",
      input.kataVmSize,
      input.kataVmSizeExplicit,
      "Kata",
    );
  }
}
