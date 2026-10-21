// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export {
  createAzureRunner,
  createSubscriptionPinnedExeca,
  getActiveSubscriptionId,
  pinAzureSubscription,
  type AzureRunner,
} from "./orchestration/subscription.js";
export {
  ensureResourceGroup,
  RESOURCE_GROUP_ADOPTION_LOCK,
  RESOURCE_GROUP_OWNERSHIP_TAG,
  ResourceGroupOwnershipError,
  type EnsureResourceGroupOptions,
  type ResourceGroupOwnershipProof,
  type ResourceGroupResult,
} from "./orchestration/resource-group.js";
export {
  buildBicepParameters,
  buildProjectedBicepParameters,
  parsePositiveInteger,
  resolvePoolNames,
  validateInfrastructureMode,
  type BicepParameterOptions,
  type PoolNames,
  type ProjectedBicepParameterOptions,
} from "./orchestration/bicep-parameters.js";
export {
  findRecoverableDeletedKeyVault,
  type RecoverableDeletedKeyVault,
} from "./orchestration/key-vault-recovery.js";
export {
  cleanupCreatedResourceGroup,
  discoverCleanupNames,
  formatCleanupCompletion,
  formatRetainedResourceGuidance,
  maybeRollbackResourceGroup,
  releaseResourceGroupOwnership,
  verifyResourceGroupOwnership,
  type CleanupContext,
  type CleanupResult,
} from "./orchestration/resource-group-cleanup.js";
