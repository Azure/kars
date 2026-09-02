// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execa } from "execa";
import {
  pickUsableVmSize,
  resolveVmSizes,
  SYSTEM_POOL_VM_PREFERENCES,
  usableSkuSet,
  USER_POOL_VM_PREFERENCES,
  type ResolvedVmSizes,
  type VmSku,
} from "../../lib/vm-size.js";

const KEY_VAULT_SUFFIX_LENGTH = 6;
export const MAX_AKS_NODE_RESOURCE_GROUP_NAME_LENGTH = 80;
export const TOTAL_REGIONAL_VCPU_QUOTA_NAME = "cores";
export const SYSTEM_POOL_NODE_COUNT = 2;
export const DEFAULT_SANDBOX_NODE_COUNT = 3;
export const MINIMUM_SANDBOX_NODE_COUNT = 1;
export const KATA_POOL_VM_SIZE = "Standard_D4as_v6";

export interface DerivedAzureResourceNames {
  baseName: string;
  aks: string;
  acrExample: string;
  keyVaultExample: string;
  azureOpenAi: string;
  logAnalytics: string;
  applicationInsights: string;
  sandboxIdentity: string;
}

/**
 * Validate every resource name derived by the deployment template. The Key
 * Vault name is the tightest constraint because its deterministic suffix leaves
 * only 14 characters for baseName.
 */
export function validateDerivedAzureResourceNames(
  clusterName: string,
): DerivedAzureResourceNames {
  const baseName = clusterName.replace(/-aks$/, "");
  const suffix = "0".repeat(KEY_VAULT_SUFFIX_LENGTH);
  const names: DerivedAzureResourceNames = {
    baseName,
    aks: `${baseName}-aks`,
    acrExample: `${baseName.replace(/-/g, "")}${suffix}`,
    keyVaultExample: `${baseName}-kv-${suffix}`,
    azureOpenAi: `${baseName}-aoai`,
    logAnalytics: `${baseName}-monitor-law`,
    applicationInsights: `${baseName}-monitor-ai`,
    sandboxIdentity: `${baseName}-aks-sandbox-wi`,
  };

  if (
    !/^[a-z](?:[a-z0-9-]*[a-z0-9])?$/.test(baseName) ||
    baseName.includes("--")
  ) {
    throw new Error(
      `Derived baseName '${baseName}' is invalid. It must start with a lowercase letter; use --cluster-name with lowercase letters, ` +
        "numbers, and single internal hyphens only (for example: --cluster-name kars-prod).",
    );
  }
  if (names.keyVaultExample.length > 24) {
    throw new Error(
      `Derived Key Vault name '${baseName}-kv-<6-char-suffix>' would be ` +
        `${names.keyVaultExample.length} characters; Azure Key Vault allows at most 24. ` +
        "Use --cluster-name with at most 14 characters before the optional '-aks' suffix " +
        "(for example: --cluster-name kars-prod).",
    );
  }
  if (!/^[a-z0-9]{5,50}$/.test(names.acrExample)) {
    throw new Error(
      `Derived ACR name '${names.acrExample}' is invalid. Use --cluster-name with enough ` +
        "letters or numbers to produce a 5-50 character alphanumeric registry name.",
    );
  }
  if (names.aks.length > 63) {
    throw new Error(`Derived AKS name '${names.aks}' exceeds Azure's 63-character limit.`);
  }
  if (names.azureOpenAi.length > 64) {
    throw new Error(
      `Derived Azure OpenAI name '${names.azureOpenAi}' exceeds Azure's 64-character limit.`,
    );
  }
  if (names.logAnalytics.length > 63) {
    throw new Error(
      `Derived Log Analytics name '${names.logAnalytics}' exceeds Azure's 63-character limit.`,
    );
  }
  if (names.sandboxIdentity.length > 128) {
    throw new Error(
      `Derived managed identity name '${names.sandboxIdentity}' exceeds Azure's 128-character limit.`,
    );
  }

  return names;
}

export function validateAutomaticAksNodeResourceGroupName(
  resourceGroup: string,
  aksName: string,
  region: string,
): string {
  const nodeResourceGroup = `MC_${resourceGroup}_${aksName}_${region}`;
  if (nodeResourceGroup.length > MAX_AKS_NODE_RESOURCE_GROUP_NAME_LENGTH) {
    throw new Error(
      `AKS automatic node resource group '${nodeResourceGroup}' would be ` +
        `${nodeResourceGroup.length} characters; Azure allows at most ` +
        `${MAX_AKS_NODE_RESOURCE_GROUP_NAME_LENGTH}. Shorten --resource-group or ` +
        "--cluster-name, or choose a shorter --region (for example: " +
        "--resource-group kars-rg --cluster-name kars --region westus3).",
    );
  }
  return nodeResourceGroup;
}

type UnknownRecord = Record<string, unknown>;

function asRecord(value: unknown): UnknownRecord | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as UnknownRecord)
    : undefined;
}

function supportPlanNames(value: unknown): string[] {
  if (typeof value === "string") return [value];
  if (Array.isArray(value)) return value.flatMap(supportPlanNames);
  const record = asRecord(value);
  if (!record) return [];
  return [record.name, record.value, record.displayName].flatMap(supportPlanNames);
}

function isKubernetesOfficial(record: UnknownRecord, inherited = false): boolean {
  const capabilities = asRecord(record.capabilities);
  const plans = supportPlanNames(
    record.supportPlan ?? record.supportPlans ?? capabilities?.supportPlan ?? capabilities?.supportPlans,
  );
  return plans.length === 0
    ? inherited
    : plans.some((plan) => plan.toLowerCase() === "kubernetesofficial");
}

function isStableVersion(record: UnknownRecord): boolean {
  const status = String(record.status ?? record.lifecycle ?? "").toLowerCase();
  return (
    record.isPreview !== true &&
    record.preview !== true &&
    !status.includes("preview") &&
    !status.includes("deprecated")
  );
}

interface AksVersionCandidate {
  version: string;
  official: boolean;
  stable: boolean;
}

function aksVersionCandidates(payload: unknown): AksVersionCandidate[] {
  const root = asRecord(payload);
  const rawValues = Array.isArray(payload)
    ? payload
    : Array.isArray(root?.values)
      ? root.values
      : Array.isArray(root?.valuesProperty)
        ? root.valuesProperty
      : Array.isArray(root?.orchestrators)
        ? root.orchestrators
        : [];
  const candidates: AksVersionCandidate[] = [];

  for (const value of rawValues) {
    const record = asRecord(value);
    if (!record) continue;
    const version = String(record.version ?? record.orchestratorVersion ?? "").trim();
    const official = isKubernetesOfficial(record);
    const stable = isStableVersion(record);
    if (version) candidates.push({ version, official, stable });

    const patchVersions = asRecord(record.patchVersions);
    if (patchVersions) {
      for (const [patchVersion, patchValue] of Object.entries(patchVersions)) {
        const patch = asRecord(patchValue) ?? {};
        candidates.push({
          version: patchVersion,
          official: isKubernetesOfficial(patch, official),
          stable: stable && isStableVersion(patch),
        });
      }
    }

    const patches = Array.isArray(record.patches) ? record.patches : [];
    for (const patchValue of patches) {
      const patch = asRecord(patchValue);
      if (!patch) continue;
      const patchVersion = String(
        patch.version ?? patch.orchestratorVersion ?? "",
      ).trim();
      if (!patchVersion) continue;
      candidates.push({
        version: patchVersion,
        official: isKubernetesOfficial(patch, official),
        stable: stable && isStableVersion(patch),
      });
    }
  }

  return candidates;
}

function versionParts(version: string): number[] | undefined {
  const match = version.replace(/^v/i, "").match(/^(\d+)\.(\d+)(?:\.(\d+))?$/);
  if (!match) return undefined;
  return [Number(match[1]), Number(match[2]), Number(match[3] ?? -1)];
}

function compareVersionsDescending(left: string, right: string): number {
  const a = versionParts(left) ?? [-1, -1, -1];
  const b = versionParts(right) ?? [-1, -1, -1];
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return b[i] - a[i];
  }
  return 0;
}

/**
 * Validate a requested version, or choose the newest stable patch offered under
 * AKS's KubernetesOfficial (standard) support plan.
 */
export function selectAksKubernetesVersion(
  payload: unknown,
  requestedVersion?: string,
): string {
  const candidates = aksVersionCandidates(payload);
  const supported = candidates.filter(
    (candidate) => candidate.official && candidate.stable && versionParts(candidate.version),
  );

  if (requestedVersion) {
    const normalized = requestedVersion.replace(/^v/i, "");
    const match = supported.find(
      (candidate) => candidate.version.replace(/^v/i, "") === normalized,
    );
    if (match) return normalized;

    const available = [...new Set(supported.map((candidate) => candidate.version))]
      .sort(compareVersionsDescending)
      .slice(0, 8);
    throw new Error(
      `Kubernetes version '${requestedVersion}' is not available in this region under the ` +
        "KubernetesOfficial (standard) support plan." +
        (available.length > 0
          ? ` Supported versions include: ${available.join(", ")}.`
          : " Azure returned no stable KubernetesOfficial versions.") +
        " Choose one shown by `az aks get-versions --location <region> -o table`.",
    );
  }

  const patchCandidates = supported.filter(
    (candidate) => (versionParts(candidate.version)?.[2] ?? -1) >= 0,
  );
  const selectable = patchCandidates.length > 0 ? patchCandidates : supported;
  const selected = selectable.sort((a, b) =>
    compareVersionsDescending(a.version, b.version),
  )[0];
  if (!selected) {
    throw new Error(
      "Azure returned no stable KubernetesOfficial AKS version for this region. " +
        "Try another region or inspect `az aks get-versions --location <region> -o table`.",
    );
  }
  return selected.version;
}

export interface VmSkuCapacity {
  name: string;
  family: string;
  vcpus: number;
}

function vmSkuValues(payload: unknown): VmSku[] {
  const root = asRecord(payload);
  const values = Array.isArray(payload)
    ? payload
    : Array.isArray(root?.value)
      ? root.value
      : [];
  return values.filter((value): value is VmSku => asRecord(value) !== undefined);
}

export function parseVmSkuCapacities(
  payload: unknown,
  selectedSizes: string[],
): Map<string, VmSkuCapacity> {
  const values = vmSkuValues(payload);
  const wanted = new Set(selectedSizes.map((size) => size.toLowerCase()));
  const result = new Map<string, VmSkuCapacity>();

  for (const value of values) {
    const record = asRecord(value);
    const name = typeof record?.name === "string" ? record.name : "";
    if (!name || !wanted.has(name.toLowerCase())) continue;
    const capabilities = Array.isArray(record?.capabilities)
      ? record.capabilities
      : [];
    const vcpuCapability = capabilities
      .map(asRecord)
      .find((capability) => String(capability?.name ?? "").toLowerCase() === "vcpus");
    const family = typeof record?.family === "string" ? record.family : "";
    const vcpus = Number(vcpuCapability?.value);
    if (family && Number.isFinite(vcpus) && vcpus > 0) {
      result.set(name.toLowerCase(), { name, family, vcpus });
    }
  }

  const missing = selectedSizes.filter((size) => !result.has(size.toLowerCase()));
  if (missing.length > 0) {
    throw new Error(
      `Azure VM SKU metadata did not include family/vCPU details for: ${missing.join(", ")}.`,
    );
  }
  return result;
}

export interface RegionalQuota {
  family: string;
  current: number;
  limit: number;
  remaining: number;
}

export function parseRegionalVmFamilyQuotas(payload: unknown): Map<string, RegionalQuota> {
  const root = asRecord(payload);
  const values = Array.isArray(payload)
    ? payload
    : Array.isArray(root?.value)
      ? root.value
      : [];
  const result = new Map<string, RegionalQuota>();

  for (const value of values) {
    const record = asRecord(value);
    const name = asRecord(record?.name);
    const family = String(name?.value ?? record?.name ?? "").trim();
    const current = Number(record?.currentValue);
    const limit = Number(record?.limit);
    if (!family || !Number.isFinite(current) || !Number.isFinite(limit)) continue;
    result.set(family.toLowerCase(), {
      family,
      current,
      limit,
      remaining: Math.max(0, limit - current),
    });
  }
  return result;
}

export interface QuotaPool {
  label: string;
  family: string;
  vcpusPerNode: number;
  count: number;
}

export interface QuotaRequirement {
  family: string;
  required: number;
  remaining: number;
  current: number;
  limit: number;
  pools: string[];
}

export interface NamedPoolFootprint extends QuotaPool {
  name: string;
  vmSize: string;
  logicalRole: AksPoolLogicalRole;
}

function additionalPoolVcpus(
  desired: NamedPoolFootprint,
  current: NamedPoolFootprint | undefined,
): number {
  const sameSku =
    current?.logicalRole === desired.logicalRole &&
    current.vmSize.toLowerCase() === desired.vmSize.toLowerCase();
  return current && sameSku
    ? Math.max(0, desired.count - current.count) * desired.vcpusPerNode
    : desired.count * desired.vcpusPerNode;
}

export function calculateQuotaRequirements(
  pools: QuotaPool[],
  quotas: Map<string, RegionalQuota>,
): QuotaRequirement[] {
  const requirements = new Map<string, { family: string; required: number; pools: string[] }>();
  const poolDescriptions = pools.map(
    (pool) => `${pool.label} ${pool.count} × ${pool.vcpusPerNode} vCPU`,
  );
  const totalRequired = pools.reduce(
    (sum, pool) => sum + pool.vcpusPerNode * pool.count,
    0,
  );
  const totalQuota = quotas.get(TOTAL_REGIONAL_VCPU_QUOTA_NAME);
  if (!totalQuota) {
    throw new Error(
      "Azure quota data did not include Total Regional vCPUs ('cores'); " +
        "cannot safely determine whether the deployment fits.",
    );
  }

  for (const pool of pools) {
    const key = pool.family.toLowerCase();
    const existing = requirements.get(key) ?? {
      family: pool.family,
      required: 0,
      pools: [],
    };
    existing.required += pool.vcpusPerNode * pool.count;
    existing.pools.push(`${pool.label} ${pool.count} × ${pool.vcpusPerNode} vCPU`);
    requirements.set(key, existing);
  }

  const familyRequirements = [...requirements.entries()].map(([key, requirement]) => {
    const quota = quotas.get(key);
    if (!quota) {
      throw new Error(
        `Azure quota data did not include VM family '${requirement.family}'; ` +
          "cannot safely determine whether the deployment fits.",
      );
    }
    return {
      ...requirement,
      family: quota.family,
      remaining: quota.remaining,
      current: quota.current,
      limit: quota.limit,
    };
  });
  return [
    {
      family: totalQuota.family,
      required: totalRequired,
      remaining: totalQuota.remaining,
      current: totalQuota.current,
      limit: totalQuota.limit,
      pools: poolDescriptions,
    },
    ...familyRequirements,
  ];
}

/**
 * Quota usage already includes pools allocated to an existing cluster,
 * including a partially provisioned recovery target. Check only positive
 * additions. Callers must reject unsupported in-place VM-size changes before
 * using this calculation.
 */
export function calculateIncrementalQuotaRequirements(
  desiredPools: NamedPoolFootprint[],
  currentPools: NamedPoolFootprint[],
  quotas: Map<string, RegionalQuota>,
): QuotaRequirement[] {
  const currentByName = new Map(
    currentPools.map((pool) => [pool.name.toLowerCase(), pool]),
  );
  const additions = desiredPools.map((desired) => {
    const current = currentByName.get(desired.name.toLowerCase());
    const required = additionalPoolVcpus(desired, current);
    return {
      ...desired,
      required,
      description:
        required === 0
          ? `${desired.label} no additional vCPU`
          : `${desired.label} +${required} vCPU`,
    };
  });
  const totalQuota = quotas.get(TOTAL_REGIONAL_VCPU_QUOTA_NAME);
  if (!totalQuota) {
    throw new Error(
      "Azure quota data did not include Total Regional vCPUs ('cores'); " +
        "cannot safely determine whether the update fits.",
    );
  }
  const familyAdditions = new Map<
    string,
    { family: string; required: number; pools: string[] }
  >();
  for (const addition of additions) {
    const key = addition.family.toLowerCase();
    const aggregate = familyAdditions.get(key) ?? {
      family: addition.family,
      required: 0,
      pools: [],
    };
    aggregate.required += addition.required;
    aggregate.pools.push(addition.description);
    familyAdditions.set(key, aggregate);
  }
  const familyRequirements = [...familyAdditions.entries()].map(
    ([key, requirement]) => {
      const quota = quotas.get(key);
      if (!quota) {
        throw new Error(
          `Azure quota data did not include VM family '${requirement.family}'; ` +
            "cannot safely determine whether the update fits.",
        );
      }
      return {
        ...requirement,
        family: quota.family,
        remaining: quota.remaining,
        current: quota.current,
        limit: quota.limit,
      };
    },
  );
  return [
    {
      family: totalQuota.family,
      required: additions.reduce((sum, addition) => sum + addition.required, 0),
      remaining: totalQuota.remaining,
      current: totalQuota.current,
      limit: totalQuota.limit,
      pools: additions.map((addition) => addition.description),
    },
    ...familyRequirements,
  ];
}

export interface NodeCountResolution {
  nodeCount: number;
  adapted: boolean;
  requirements: QuotaRequirement[];
}

function quotaFailure(requirements: QuotaRequirement[]): Error {
  const insufficient = requirements.filter(
    (requirement) => requirement.required > requirement.remaining,
  );
  return new Error(
    "Insufficient regional VM-family vCPU quota: " +
      insufficient
        .map(
          (requirement) =>
            `${requirement.family} requires ${requirement.required} vCPU, ` +
            `${requirement.remaining} vCPU remaining`,
        )
        .join("; ") +
      ". Reduce --node-count, choose different VM sizes/region, or request a quota increase.",
  );
}

/**
 * Preserve the two-node system pool. An implicit three-node sandbox pool may
 * adapt to one node, but an explicit count is never silently changed.
 */
export function resolveSandboxNodeCountForQuota(input: {
  requestedNodeCount?: number;
  nodeCountExplicit: boolean;
  system: VmSkuCapacity;
  sandbox: VmSkuCapacity;
  additionalSandboxPools?: Array<{
    label: string;
    capacity: VmSkuCapacity;
  }>;
  quotas: Map<string, RegionalQuota>;
}): NodeCountResolution {
  const nodeCount = input.requestedNodeCount ?? DEFAULT_SANDBOX_NODE_COUNT;
  if (!Number.isInteger(nodeCount) || nodeCount < MINIMUM_SANDBOX_NODE_COUNT) {
    throw new Error("--node-count must be an integer of at least 1.");
  }

  const requirementsFor = (sandboxCount: number) => {
    const pools: QuotaPool[] = [
        {
          label: "system",
          family: input.system.family,
          vcpusPerNode: input.system.vcpus,
          count: SYSTEM_POOL_NODE_COUNT,
        },
        {
          label: "sandbox",
          family: input.sandbox.family,
          vcpusPerNode: input.sandbox.vcpus,
          count: sandboxCount,
        },
      ];
    for (const additional of input.additionalSandboxPools ?? []) {
      pools.push({
        label: additional.label,
        family: additional.capacity.family,
        vcpusPerNode: additional.capacity.vcpus,
        count: sandboxCount,
      });
    }
    return calculateQuotaRequirements(pools, input.quotas);
  };

  const requestedRequirements = requirementsFor(nodeCount);
  if (
    requestedRequirements.every(
      (requirement) => requirement.required <= requirement.remaining,
    )
  ) {
    return { nodeCount, adapted: false, requirements: requestedRequirements };
  }

  if (
    !input.nodeCountExplicit &&
    nodeCount === DEFAULT_SANDBOX_NODE_COUNT
  ) {
    const minimumRequirements = requirementsFor(MINIMUM_SANDBOX_NODE_COUNT);
    if (
      minimumRequirements.every(
        (requirement) => requirement.required <= requirement.remaining,
      )
    ) {
      return {
        nodeCount: MINIMUM_SANDBOX_NODE_COUNT,
        adapted: true,
        requirements: minimumRequirements,
      };
    }
    throw quotaFailure(minimumRequirements);
  }

  throw quotaFailure(requestedRequirements);
}

export function hasCliOption(flag: string, argv: string[] = process.argv): boolean {
  return argv.some((argument) => argument === flag || argument.startsWith(`${flag}=`));
}

export type AzTextRunner = (args: string[]) => Promise<string>;

function azureCliErrorText(error: unknown): string {
  const record = asRecord(error);
  return [record?.stderr, record?.stdout, record?.shortMessage, record?.message]
    .filter((value): value is string => typeof value === "string" && value.trim().length > 0)
    .join("\n");
}

export function isAksNotFoundError(error: unknown): boolean {
  const message = azureCliErrorText(error);
  return (
    /\((?:ResourceNotFound|ResourceGroupNotFound|ManagedClusterNotFound|ParentResourceNotFound)\)/i.test(
      message,
    ) ||
    /["']code["']\s*:\s*["'](?:ResourceNotFound|ResourceGroupNotFound|ManagedClusterNotFound|ParentResourceNotFound)["']/i.test(
      message,
    )
  );
}

async function defaultAzTextRunner(args: string[]): Promise<string> {
  const { stdout } = await execa("az", args, { stdio: "pipe", timeout: 20000 });
  return stdout;
}

/**
 * Check for an existing AKS cluster without mutating Azure. Only Azure's
 * explicit not-found codes mean absence; auth, transport, and malformed
 * responses fail closed.
 */
export type AksPoolLogicalRole = "system" | "sandbox" | "kata" | "other";

export interface AksAgentPoolProfile {
  name: string;
  count: number;
  vmSize: string;
  mode: string;
  provisioningState: string;
  nodeLabels: Record<string, string>;
  nodeTaints: string[];
  workloadRuntime?: string;
  logicalRole: AksPoolLogicalRole;
}

export interface ExistingAksCluster {
  exists: true;
  id: string;
  provisioningState: string;
  powerState: {
    code: string;
  };
  kubernetesVersion: string;
  supportPlan: string;
  sku: {
    name: string;
    tier: string;
  };
  autoUpgradeProfile: {
    upgradeChannel: string;
    nodeOSUpgradeChannel: string;
  };
  agentPoolProfiles: AksAgentPoolProfile[];
}

export type AksClusterDetection =
  | { exists: false }
  | ExistingAksCluster;

export type ExistingAksDisposition =
  | { action: "new" }
  | { action: "reuse"; cluster: ExistingAksCluster }
  | {
      action: "force-update";
      cluster: ExistingAksCluster;
      diagnostic: string;
    }
  | { action: "stopped"; cluster: ExistingAksCluster; diagnostic: string }
  | { action: "recover"; cluster: ExistingAksCluster; diagnostic: string };

function resourceGroupFromId(resourceId: string): string {
  const segments = resourceId.split("/");
  const index = segments.findIndex(
    (segment) => segment.toLowerCase() === "resourcegroups",
  );
  return index >= 0 ? segments[index + 1] ?? "<resource-group>" : "<resource-group>";
}

function resourceNameFromId(resourceId: string): string {
  return resourceId.split("/").filter(Boolean).at(-1) ?? "<cluster-name>";
}

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
    if (isAksNotFoundError(error)) return { exists: false };
    const detail = azureCliErrorText(error).split("\n")[0] || String(error);
    throw new Error(
      `Could not determine whether AKS cluster '${clusterName}' exists: ${detail}`,
      { cause: error },
    );
  }
}

export interface InfrastructureCompleteness {
  complete: boolean;
  diagnostic: string;
}

export interface InfrastructureCompletenessOptions {
  foundryEndpoint?: string;
  openAiEndpoint?: string;
  subscriptionId?: string;
}

const REQUIRED_INFRASTRUCTURE_OUTPUTS = [
  "acrLoginServer",
  "sandboxIdentityClientId",
  "keyVaultName",
] as const;

function infrastructureOutput(
  outputs: UnknownRecord | undefined,
  name: string,
): string {
  const output = asRecord(outputs?.[name]);
  return typeof output?.value === "string" ? output.value.trim() : "";
}

function usesExternalAi(
  options: InfrastructureCompletenessOptions,
): boolean {
  return Boolean(
    options.foundryEndpoint?.trim() || options.openAiEndpoint?.trim(),
  );
}

export function classifyInfrastructureDeployment(
  payload: unknown,
  options: InfrastructureCompletenessOptions = {},
): InfrastructureCompleteness {
  const record = asRecord(payload);
  const provisioningState =
    typeof record?.provisioningState === "string"
      ? record.provisioningState.trim()
      : "";
  if (provisioningState.toLowerCase() !== "succeeded") {
    return {
      complete: false,
      diagnostic: provisioningState
        ? `Resource-group deployment 'main' is ${provisioningState}, not Succeeded.`
        : "Resource-group deployment 'main' returned no valid provisioning state.",
    };
  }

  const outputs = asRecord(record?.outputs);
  const requiredOutputs = usesExternalAi(options)
    ? REQUIRED_INFRASTRUCTURE_OUTPUTS
    : [...REQUIRED_INFRASTRUCTURE_OUTPUTS, "openAiEndpoint"];
  const missing = requiredOutputs.filter(
    (name) => infrastructureOutput(outputs, name).length === 0,
  );
  if (missing.length > 0) {
    return {
      complete: false,
      diagnostic:
        `Resource-group deployment 'main' is missing valid output value(s): ${missing.join(", ")}.`,
    };
  }
  return {
    complete: true,
    diagnostic: "Resource-group deployment 'main' succeeded with all required outputs.",
  };
}

export function isInfrastructureDeploymentNotFoundError(
  error: unknown,
): boolean {
  const message = azureCliErrorText(error);
  return (
    /\((?:DeploymentNotFound|ResourceNotFound|ResourceGroupNotFound)\)/i.test(
      message,
    ) ||
    /["']code["']\s*:\s*["'](?:DeploymentNotFound|ResourceNotFound|ResourceGroupNotFound)["']/i.test(
      message,
    )
  );
}

interface AzureResourceScope {
  subscriptionId: string;
  resourceGroup: string;
}

function parseAzureResourceScope(id: string): AzureResourceScope | undefined {
  const match = id.match(
    /^\/subscriptions\/([^/]+)\/resourceGroups\/([^/]+)\/providers\//i,
  );
  return match
    ? { subscriptionId: match[1], resourceGroup: match[2] }
    : undefined;
}

function sameAzureResourceScope(
  left: AzureResourceScope,
  right: AzureResourceScope,
): boolean {
  return (
    left.subscriptionId.toLowerCase() === right.subscriptionId.toLowerCase() &&
    left.resourceGroup.toLowerCase() === right.resourceGroup.toLowerCase()
  );
}

function parseJsonResponse(payload: string, description: string): unknown {
  try {
    return JSON.parse(payload);
  } catch (error) {
    throw new Error(
      `Could not verify ${description}: Azure CLI returned malformed JSON.`,
      { cause: error },
    );
  }
}

async function queryLiveResource(
  description: string,
  args: string[],
  runAzText: AzTextRunner,
): Promise<{ found: true; payload: unknown } | { found: false }> {
  let payload: string;
  try {
    payload = await runAzText(args);
  } catch (error) {
    if (isInfrastructureDeploymentNotFoundError(error)) {
      return { found: false };
    }
    const detail = azureCliErrorText(error).split("\n")[0] || String(error);
    throw new Error(`Could not verify ${description}: ${detail}`, {
      cause: error,
    });
  }
  return {
    found: true,
    payload: parseJsonResponse(payload, description),
  };
}

interface LiveResourceRecord {
  id: string;
  name: string;
  record: UnknownRecord;
}

function parseLiveResourceRecord(
  value: unknown,
  description: string,
): LiveResourceRecord {
  const record = asRecord(value);
  const id = typeof record?.id === "string" ? record.id.trim() : "";
  const name = typeof record?.name === "string" ? record.name.trim() : "";
  if (!record || !id || !name || !parseAzureResourceScope(id)) {
    throw new Error(
      `Could not verify ${description}: Azure CLI returned malformed resource data.`,
    );
  }
  return { id, name, record };
}

function isLiveResourceInScope(
  resource: LiveResourceRecord,
  expectedScope: AzureResourceScope,
): boolean {
  const actualScope = parseAzureResourceScope(resource.id);
  return Boolean(
    actualScope && sameAzureResourceScope(actualScope, expectedScope),
  );
}

function normalizedEndpoint(value: string): string {
  return value.trim().replace(/\/+$/, "").toLowerCase();
}

/**
 * Verify that the retained resource-group deployment completed and produced
 * the values later deployment stages consume, then resolve those values to
 * live resources in the deployment's resource group and subscription. Only
 * explicit not-found errors represent an incomplete deployment;
 * authorization, transport, and malformed-response failures fail closed.
 */
export async function detectInfrastructureCompleteness(
  resourceGroup: string,
  optionsOrRunner: InfrastructureCompletenessOptions | AzTextRunner = {},
  runAzText: AzTextRunner = defaultAzTextRunner,
): Promise<InfrastructureCompleteness> {
  const options =
    typeof optionsOrRunner === "function" ? {} : optionsOrRunner;
  const resourceRunner =
    typeof optionsOrRunner === "function" ? optionsOrRunner : runAzText;
  const scopedResourceRunner: AzTextRunner = (args) =>
    resourceRunner(
      options.subscriptionId
        ? [...args, "--subscription", options.subscriptionId]
        : args,
    );
  let payload: string;
  try {
    payload = await scopedResourceRunner([
      "deployment",
      "group",
      "show",
      "--resource-group",
      resourceGroup,
      "--name",
      "main",
      "--query",
      "{id:id,provisioningState:properties.provisioningState,outputs:properties.outputs}",
      "-o",
      "json",
    ]);
  } catch (error) {
    if (isInfrastructureDeploymentNotFoundError(error)) {
      return {
        complete: false,
        diagnostic: "Resource-group deployment 'main' was not found.",
      };
    }
    const detail = azureCliErrorText(error).split("\n")[0] || String(error);
    throw new Error(
      `Could not verify resource-group deployment 'main': ${detail}`,
      { cause: error },
    );
  }

  const deploymentPayload = parseJsonResponse(
    payload,
    "resource-group deployment 'main'",
  );
  const deploymentRecord = asRecord(deploymentPayload);
  if (
    !deploymentRecord ||
    typeof deploymentRecord.provisioningState !== "string" ||
    !deploymentRecord.provisioningState.trim()
  ) {
    throw new Error(
      "Could not verify resource-group deployment 'main': Azure CLI returned malformed deployment data.",
    );
  }
  const deploymentCompleteness = classifyInfrastructureDeployment(
    deploymentPayload,
    options,
  );
  if (!deploymentCompleteness.complete) return deploymentCompleteness;

  const deploymentId =
    typeof deploymentRecord.id === "string" ? deploymentRecord.id.trim() : "";
  const expectedScope = parseAzureResourceScope(deploymentId);
  if (
    !expectedScope ||
    expectedScope.resourceGroup.toLowerCase() !== resourceGroup.toLowerCase()
  ) {
    throw new Error(
      "Could not verify resource-group deployment 'main': Azure CLI returned malformed deployment scope.",
    );
  }
  const outputs = asRecord(deploymentRecord.outputs);
  const acrLoginServer = infrastructureOutput(outputs, "acrLoginServer");
  const explicitAcrName = infrastructureOutput(outputs, "acrName");
  const derivedAcrName = acrLoginServer.split(".", 1)[0];
  const acrName = explicitAcrName || derivedAcrName;
  if (!/^[a-zA-Z0-9]{5,50}$/.test(acrName)) {
    return {
      complete: false,
      diagnostic:
        "Resource-group deployment 'main' has stale or invalid ACR outputs.",
    };
  }

  const acrDescription = `Azure Container Registry '${acrName}'`;
  const acrResult = await queryLiveResource(
    acrDescription,
    [
      "acr",
      "show",
      "--resource-group",
      resourceGroup,
      "--name",
      acrName,
      "--query",
      "{id:id,name:name,loginServer:loginServer}",
      "-o",
      "json",
    ],
    scopedResourceRunner,
  );
  if (!acrResult.found) {
    return {
      complete: false,
      diagnostic: `${acrDescription} from deployment outputs was not found in resource group '${resourceGroup}'.`,
    };
  }
  const acr = parseLiveResourceRecord(acrResult.payload, acrDescription);
  const liveLoginServer =
    typeof acr.record.loginServer === "string"
      ? acr.record.loginServer.trim()
      : "";
  if (!liveLoginServer) {
    throw new Error(
      `Could not verify ${acrDescription}: Azure CLI returned malformed resource data.`,
    );
  }
  if (
    !isLiveResourceInScope(acr, expectedScope) ||
    acr.name.toLowerCase() !== acrName.toLowerCase() ||
    liveLoginServer.toLowerCase() !== acrLoginServer.toLowerCase()
  ) {
    return {
      complete: false,
      diagnostic: `${acrDescription} does not match the retained deployment outputs in resource group '${resourceGroup}'.`,
    };
  }

  const keyVaultName = infrastructureOutput(outputs, "keyVaultName");
  const keyVaultDescription = `Key Vault '${keyVaultName}'`;
  const keyVaultResult = await queryLiveResource(
    keyVaultDescription,
    [
      "keyvault",
      "show",
      "--resource-group",
      resourceGroup,
      "--name",
      keyVaultName,
      "--query",
      "{id:id,name:name}",
      "-o",
      "json",
    ],
    scopedResourceRunner,
  );
  if (!keyVaultResult.found) {
    return {
      complete: false,
      diagnostic: `${keyVaultDescription} from deployment outputs was not found in resource group '${resourceGroup}'.`,
    };
  }
  const keyVault = parseLiveResourceRecord(
    keyVaultResult.payload,
    keyVaultDescription,
  );
  if (
    !isLiveResourceInScope(keyVault, expectedScope) ||
    keyVault.name.toLowerCase() !== keyVaultName.toLowerCase()
  ) {
    return {
      complete: false,
      diagnostic: `${keyVaultDescription} does not match the retained deployment outputs in resource group '${resourceGroup}'.`,
    };
  }

  const identityClientId = infrastructureOutput(
    outputs,
    "sandboxIdentityClientId",
  );
  const identityDescription = `user-assigned identity with client ID '${identityClientId}'`;
  const identityResult = await queryLiveResource(
    identityDescription,
    [
      "identity",
      "list",
      "--resource-group",
      resourceGroup,
      "--query",
      "[].{id:id,name:name,clientId:clientId}",
      "-o",
      "json",
    ],
    scopedResourceRunner,
  );
  if (!identityResult.found || !Array.isArray(identityResult.payload)) {
    if (!identityResult.found) {
      return {
        complete: false,
        diagnostic: `The ${identityDescription} was not found in resource group '${resourceGroup}'.`,
      };
    }
    throw new Error(
      `Could not verify ${identityDescription}: Azure CLI returned malformed resource data.`,
    );
  }
  const identities = identityResult.payload.map((value) =>
    parseLiveResourceRecord(value, identityDescription),
  );
  const identity = identities.find((candidate) => {
    const clientId =
      typeof candidate.record.clientId === "string"
        ? candidate.record.clientId.trim()
        : "";
    if (!clientId) {
      throw new Error(
        `Could not verify ${identityDescription}: Azure CLI returned malformed resource data.`,
      );
    }
    return (
      clientId.toLowerCase() === identityClientId.toLowerCase() &&
      isLiveResourceInScope(candidate, expectedScope)
    );
  });
  if (!identity) {
    return {
      complete: false,
      diagnostic: `The ${identityDescription} was not found in resource group '${resourceGroup}'.`,
    };
  }

  if (!usesExternalAi(options)) {
    const openAiEndpoint = infrastructureOutput(outputs, "openAiEndpoint");
    const accountDescription = `Azure OpenAI account for endpoint '${openAiEndpoint}'`;
    const accountResult = await queryLiveResource(
      accountDescription,
      [
        "cognitiveservices",
        "account",
        "list",
        "--resource-group",
        resourceGroup,
        "--query",
        "[].{id:id,name:name,kind:kind,endpoint:properties.endpoint}",
        "-o",
        "json",
      ],
      scopedResourceRunner,
    );
    if (!accountResult.found || !Array.isArray(accountResult.payload)) {
      if (!accountResult.found) {
        return {
          complete: false,
          diagnostic: `${accountDescription} was not found in resource group '${resourceGroup}'.`,
        };
      }
      throw new Error(
        `Could not verify ${accountDescription}: Azure CLI returned malformed resource data.`,
      );
    }
    const accounts = accountResult.payload.map((value) =>
      parseLiveResourceRecord(value, accountDescription),
    );
    const account = accounts.find((candidate) => {
      const endpoint =
        typeof candidate.record.endpoint === "string"
          ? candidate.record.endpoint
          : "";
      const kind =
        typeof candidate.record.kind === "string"
          ? candidate.record.kind.trim()
          : "";
      if (!endpoint.trim() || !kind) {
        throw new Error(
          `Could not verify ${accountDescription}: Azure CLI returned malformed resource data.`,
        );
      }
      return (
        kind.toLowerCase() === "openai" &&
        normalizedEndpoint(endpoint) === normalizedEndpoint(openAiEndpoint) &&
        isLiveResourceInScope(candidate, expectedScope)
      );
    });
    if (!account) {
      return {
        complete: false,
        diagnostic: `${accountDescription} was not found in resource group '${resourceGroup}'.`,
      };
    }
  }

  return {
    complete: true,
    diagnostic: usesExternalAi(options)
      ? "Resource-group deployment 'main' resolved to live ACR, Key Vault, and managed identity resources; external AI configuration is in use."
      : "Resource-group deployment 'main' resolved to live ACR, Key Vault, managed identity, and Azure OpenAI resources.",
  };
}

export function requireCompleteSkipInfraDeployment(
  completeness: InfrastructureCompleteness,
): void {
  if (completeness.complete) return;
  throw new Error(
    `Existing AKS infrastructure is incomplete. ${completeness.diagnostic} ` +
      "Kars will not run the full managedClusters Bicep template against an existing cluster " +
      "because unmodeled AKS properties could be reset. Repair or recreate the missing ancillary " +
      "resources (such as ACR, Key Vault, managed identity, and Azure AI resources) manually, or " +
      "deploy into a new resource group with a new cluster name. Do not use --force-infra.",
  );
}

export type AzJsonRunner = (args: string[]) => Promise<unknown>;

async function defaultAzJsonRunner(args: string[]): Promise<unknown> {
  const { stdout } = await execa("az", args, { stdio: "pipe", timeout: 120000 });
  return JSON.parse(stdout);
}

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
