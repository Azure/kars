// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export * from "./deployment-safety/names.js";
export * from "./deployment-safety/versions.js";
export {
  DEFAULT_SANDBOX_NODE_COUNT,
  KATA_POOL_VM_SIZE,
  MINIMUM_SANDBOX_NODE_COUNT,
  SYSTEM_POOL_NODE_COUNT,
  TOTAL_REGIONAL_VCPU_QUOTA_NAME,
  calculateIncrementalQuotaRequirements,
  calculateQuotaRequirements,
  parseRegionalVmFamilyQuotas,
  parseVmSkuCapacities,
  resolveSandboxNodeCountForQuota,
  type NamedPoolFootprint,
  type NodeCountResolution,
  type QuotaPool,
  type QuotaRequirement,
  type RegionalQuota,
  type VmSkuCapacity,
} from "./deployment-safety/quota-sku.js";
export {
  hasCliOption,
  isAksNotFoundError,
  type AksAgentPoolProfile,
  type AksClusterDetection,
  type AksPoolLogicalRole,
  type AzJsonRunner,
  type AzTextRunner,
  type ExistingAksCluster,
  type ExistingAksDisposition,
} from "./deployment-safety/core.js";
export * from "./deployment-safety/aks-topology.js";
export * from "./deployment-safety/retained-infrastructure.js";
export * from "./deployment-safety/resolver.js";
