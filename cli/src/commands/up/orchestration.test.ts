// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";

import * as orchestration from "./orchestration.js";

describe("up orchestration barrel", () => {
  it("preserves the orchestration public surface", () => {
    expect(Object.keys(orchestration).sort()).toEqual([
      "RESOURCE_GROUP_ADOPTION_LOCK",
      "RESOURCE_GROUP_OWNERSHIP_TAG",
      "ResourceGroupOwnershipError",
      "buildBicepParameters",
      "buildProjectedBicepParameters",
      "cleanupCreatedResourceGroup",
      "createAzureRunner",
      "createSubscriptionPinnedExeca",
      "discoverCleanupNames",
      "ensureResourceGroup",
      "findRecoverableDeletedKeyVault",
      "formatCleanupCompletion",
      "formatRetainedResourceGuidance",
      "getActiveSubscriptionId",
      "maybeRollbackResourceGroup",
      "parsePositiveInteger",
      "pinAzureSubscription",
      "releaseResourceGroupOwnership",
      "resolvePoolNames",
      "validateInfrastructureMode",
      "verifyResourceGroupOwnership",
    ]);
  });
});
