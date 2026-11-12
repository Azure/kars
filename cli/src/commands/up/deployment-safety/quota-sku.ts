// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { VmSku } from "../../../lib/vm-size.js";
import { asRecord, type AksPoolLogicalRole } from "./core.js";

export const TOTAL_REGIONAL_VCPU_QUOTA_NAME = "cores";
export const SYSTEM_POOL_NODE_COUNT = 2;
export const DEFAULT_SANDBOX_NODE_COUNT = 3;
export const MINIMUM_SANDBOX_NODE_COUNT = 1;
export const KATA_POOL_VM_SIZE = "Standard_D4as_v6";

export interface VmSkuCapacity {
  name: string;
  family: string;
  vcpus: number;
}

export function vmSkuValues(payload: unknown): VmSku[] {
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

export function additionalPoolVcpus(
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

export function quotaFailure(requirements: QuotaRequirement[]): Error {
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
