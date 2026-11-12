// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { CleanupContext } from "./resource-group-cleanup.js";
import type { AzureRunner } from "./subscription.js";

export interface RecoverableDeletedKeyVault {
  name: string;
  location: string;
  vaultId: string;
  state: "soft-deleted";
  deletionDate?: string;
  scheduledPurgeDate?: string;
}

function parseJsonArray(stdout: string, operation: string): unknown[] {
  try {
    const parsed = JSON.parse(stdout) as unknown;
    if (Array.isArray(parsed)) {
      return parsed;
    }
  } catch {
    // Fall through to the actionable error below.
  }
  throw new Error(`${operation} returned an invalid JSON response`);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export async function findRecoverableDeletedKeyVault(
  context: Pick<
    CleanupContext,
    "resourceGroup" | "baseName" | "location" | "subscriptionId"
  >,
  runAzure: AzureRunner,
): Promise<RecoverableDeletedKeyVault | undefined> {
  const { stdout } = await runAzure([
    "keyvault",
    "list-deleted",
    "--subscription",
    context.subscriptionId,
    "--output",
    "json",
  ]);
  const deletedVaults = parseJsonArray(
    stdout,
    "Soft-deleted Key Vault lookup",
  );
  const expectedName = new RegExp(
    `^${escapeRegExp(context.baseName)}-kv-[a-z0-9]{6}$`,
    "i",
  );
  const matches: RecoverableDeletedKeyVault[] = [];

  for (const value of deletedVaults) {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      continue;
    }
    const vault = value as Record<string, unknown>;
    const properties =
      vault.properties &&
      typeof vault.properties === "object" &&
      !Array.isArray(vault.properties)
        ? (vault.properties as Record<string, unknown>)
        : {};
    const name = String(vault.name ?? "").trim();
    const location = String(
      properties.location ?? vault.location ?? "",
    ).trim();
    const vaultId = String(properties.vaultId ?? "").trim();
    if (!expectedName.test(name)) {
      continue;
    }
    const expectedVaultId =
      `/subscriptions/${context.subscriptionId}` +
      `/resourceGroups/${context.resourceGroup}` +
      `/providers/Microsoft.KeyVault/vaults/${name}`;
    if (
      vaultId.toLowerCase() !== expectedVaultId.toLowerCase() ||
      location.toLowerCase() !== context.location.toLowerCase()
    ) {
      continue;
    }
    const deletionDate = String(properties.deletionDate ?? "").trim();
    const scheduledPurgeDate = String(
      properties.scheduledPurgeDate ?? "",
    ).trim();
    matches.push({
      name,
      location,
      vaultId,
      state: "soft-deleted",
      ...(deletionDate ? { deletionDate } : {}),
      ...(scheduledPurgeDate ? { scheduledPurgeDate } : {}),
    });
  }

  if (matches.length > 1) {
    throw new Error(
      `Refusing automatic Key Vault recovery: Azure returned multiple soft-deleted vaults matching deployment prefix '${context.baseName}-kv-', resource group '${context.resourceGroup}', and location '${context.location}': ${matches.map(({ name }) => name).join(", ")}.`,
    );
  }
  return matches[0];
}
