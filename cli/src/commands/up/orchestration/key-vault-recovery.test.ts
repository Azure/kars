// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";

import {
  findRecoverableDeletedKeyVault,
  type AzureRunner,
} from "../orchestration.js";

describe("up orchestration", () => {
  describe("soft-deleted Key Vault recovery discovery", () => {
    const context = {
      resourceGroup: "fresh-rg",
      baseName: "safe",
      location: "westus3",
      subscriptionId: "sub-1",
    };
    const deletedVault = (
      name: string,
      resourceGroup = "fresh-rg",
      location = "westus3",
    ) => ({
      name,
      properties: {
        location,
        vaultId:
          `/subscriptions/sub-1/resourceGroups/${resourceGroup}` +
          `/providers/Microsoft.KeyVault/vaults/${name}`,
        deletionDate: "2026-09-01T12:00:00Z",
        scheduledPurgeDate: "2026-10-01T12:00:00Z",
      },
    });

    it("returns no recovery state when the deleted-vault inventory is empty", async () => {
      const runAzure = vi
        .fn<AzureRunner>()
        .mockResolvedValue({ stdout: "[]" });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).resolves.toBeUndefined();
      expect(runAzure).toHaveBeenCalledWith([
        "keyvault",
        "list-deleted",
        "--subscription",
        "sub-1",
        "--output",
        "json",
      ]);
    });

    it("returns the exact Azure-assigned name and soft-deleted state", async () => {
      const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
        stdout: JSON.stringify([deletedVault("safe-kv-a1b2c3")]),
      });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).resolves.toEqual({
        name: "safe-kv-a1b2c3",
        location: "westus3",
        vaultId:
          "/subscriptions/sub-1/resourceGroups/fresh-rg/providers/Microsoft.KeyVault/vaults/safe-kv-a1b2c3",
        state: "soft-deleted",
        deletionDate: "2026-09-01T12:00:00Z",
        scheduledPurgeDate: "2026-10-01T12:00:00Z",
      });
    });

    it("ignores unrelated names, other resource groups, and other locations", async () => {
      const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
        stdout: JSON.stringify([
          deletedVault("unrelated-kv-a1b2c3"),
          deletedVault("safe-kv-a1b2c3", "other-rg"),
          deletedVault("safe-kv-d4e5f6", "fresh-rg", "eastus2"),
        ]),
      });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).resolves.toBeUndefined();
    });

    it("fails closed when more than one deleted vault matches", async () => {
      const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
        stdout: JSON.stringify([
          deletedVault("safe-kv-a1b2c3"),
          deletedVault("safe-kv-d4e5f6"),
        ]),
      });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).rejects.toThrow(
        /multiple soft-deleted vaults.*safe-kv-a1b2c3, safe-kv-d4e5f6/,
      );
    });

    it("fails closed when Azure cannot list deleted vaults", async () => {
      const listError = new Error("not authorized to list deleted vaults");
      const runAzure = vi.fn<AzureRunner>().mockRejectedValue(listError);

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).rejects.toBe(listError);
    });
  });

});
