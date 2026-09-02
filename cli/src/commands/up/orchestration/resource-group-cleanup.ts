// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  RESOURCE_GROUP_OWNERSHIP_TAG,
  ResourceGroupOwnershipError,
  type ResourceGroupOwnershipProof,
} from "./resource-group.js";
import type { AzureRunner } from "./subscription.js";

export interface CleanupContext {
  resourceGroup: string;
  baseName: string;
  clusterName: string;
  location: string;
  subscriptionId: string;
  kubernetesVersion: string;
  nodeCount: number;
  ownershipProof?: ResourceGroupOwnershipProof;
}

export interface CleanupResult {
  keyVaultNames: string[];
  azureAiNames: string[];
  purgeFailures: string[];
}

function errorText(error: unknown): string {
  if (error && typeof error === "object") {
    const candidate = error as { stderr?: unknown; message?: unknown };
    return String(candidate.stderr ?? candidate.message ?? error);
  }
  return String(error);
}

function isDeletionLockConflict(error: unknown): boolean {
  return /ScopeLocked|CanNotDelete|cannot be deleted because of the following lock|locked/i.test(
    errorText(error),
  );
}

function parseJsonObject(
  stdout: string,
  operation: string,
): Record<string, unknown> {
  try {
    const parsed = JSON.parse(stdout) as unknown;
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // Fall through to the actionable error below.
  }
  throw new Error(`${operation} returned an invalid JSON response`);
}

interface ResourceGroupSnapshot {
  id: string;
  location: string;
  tags: Record<string, unknown>;
}

function parseResourceGroupSnapshot(
  stdout: string,
  operation: string,
): ResourceGroupSnapshot {
  const resourceGroup = parseJsonObject(stdout, operation);
  const tags =
    resourceGroup.tags &&
    typeof resourceGroup.tags === "object" &&
    !Array.isArray(resourceGroup.tags)
      ? (resourceGroup.tags as Record<string, unknown>)
      : {};
  const id = String(resourceGroup.id ?? "").trim();
  if (!id) {
    throw new Error(`${operation} returned a resource group without an ID`);
  }
  return {
    id,
    location: String(resourceGroup.location ?? "").trim(),
    tags,
  };
}

function lockResourceId(resourceId: string, lockName: string): string {
  return `${resourceId}/providers/Microsoft.Authorization/locks/${lockName}`;
}

function parseLockSnapshot(
  stdout: string,
  operation: string,
): { id: string; name: string; notes: string; level: string } {
  const lock = parseJsonObject(stdout, operation);
  return {
    id: String(lock.id ?? "").trim(),
    name: String(lock.name ?? "").trim(),
    notes: String(lock.notes ?? ""),
    level: String(lock.level ?? "").trim(),
  };
}

function splitNames(stdout: string): string[] {
  return stdout
    .split(/\r?\n/)
    .map((name) => name.trim())
    .filter(Boolean);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export async function discoverCleanupNames(
  context: CleanupContext,
  runAzure: AzureRunner,
): Promise<{ keyVaultNames: string[]; azureAiNames: string[] }> {
  const { stdout: keyVaultOutput } = await runAzure([
    "keyvault",
    "list",
    "--resource-group",
    context.resourceGroup,
    "--subscription",
    context.subscriptionId,
    "--query",
    "[].name",
    "--output",
    "tsv",
  ]);
  const expectedKeyVault = new RegExp(
    `^${escapeRegExp(context.baseName)}-kv-[a-z0-9]{6}$`,
  );
  const keyVaultNames = splitNames(keyVaultOutput).filter((name) =>
    expectedKeyVault.test(name),
  );

  const { stdout: cognitiveServicesOutput } = await runAzure([
    "cognitiveservices",
    "account",
    "list",
    "--resource-group",
    context.resourceGroup,
    "--subscription",
    context.subscriptionId,
    "--query",
    "[].name",
    "--output",
    "tsv",
  ]);
  const expectedAzureAi = `${context.baseName}-aoai`;
  const azureAiNames = splitNames(cognitiveServicesOutput).filter(
    (name) => name === expectedAzureAi,
  );

  return { keyVaultNames, azureAiNames };
}

export async function cleanupCreatedResourceGroup(
  context: CleanupContext,
  runAzure: AzureRunner,
): Promise<CleanupResult> {
  const names = await discoverCleanupNames(context, runAzure);
  await verifyResourceGroupOwnership(context, runAzure);
  const ownership = context.ownershipProof!;

  await runAzure([
    "lock",
    "delete",
    "--name",
    ownership.lockName,
    "--resource-group",
    context.resourceGroup,
    "--subscription",
    context.subscriptionId,
    "--output",
    "none",
  ]);
  try {
    await runAzure([
      "group",
      "delete",
      "--name",
      context.resourceGroup,
      "--subscription",
      context.subscriptionId,
      "--yes",
      "--output",
      "none",
    ]);
  } catch (error) {
    if (isDeletionLockConflict(error)) {
      throw new ResourceGroupOwnershipError(
        `Resource group '${context.resourceGroup}' was adopted concurrently and its CanNotDelete guard blocked rollback. It was not deleted.`,
      );
    }
    throw error;
  }

  await runAzure([
    "group",
    "wait",
    "--deleted",
    "--resource-group",
    context.resourceGroup,
    "--subscription",
    context.subscriptionId,
    "--timeout",
    "3600",
    "--interval",
    "30",
    "--output",
    "none",
  ]);

  const purgeFailures: string[] = [];
  for (const name of names.keyVaultNames) {
    try {
      await runAzure([
        "keyvault",
        "purge",
        "--name",
        name,
        "--location",
        context.location,
        "--subscription",
        context.subscriptionId,
        "--output",
        "none",
      ]);
    } catch {
      purgeFailures.push(`Key Vault ${name}`);
    }
  }
  for (const name of names.azureAiNames) {
    try {
      await runAzure([
        "cognitiveservices",
        "account",
        "purge",
        "--name",
        name,
        "--resource-group",
        context.resourceGroup,
        "--location",
        context.location,
        "--subscription",
        context.subscriptionId,
        "--output",
        "none",
      ]);
    } catch {
      purgeFailures.push(`Azure AI account ${name}`);
    }
  }

  return { ...names, purgeFailures };
}

export async function verifyResourceGroupOwnership(
  context: Pick<
    CleanupContext,
    "resourceGroup" | "subscriptionId" | "ownershipProof"
  >,
  runAzure: AzureRunner,
): Promise<ResourceGroupOwnershipProof> {
  if (!context.ownershipProof) {
    throw new ResourceGroupOwnershipError(
      `Refusing to delete resource group '${context.resourceGroup}': this invocation has no rollback ownership proof.`,
    );
  }

  const { stdout } = await runAzure([
    "group",
    "show",
    "--name",
    context.resourceGroup,
    "--subscription",
    context.subscriptionId,
    "--query",
    "{id:id,tags:tags}",
    "--output",
    "json",
  ]);
  const current = parseResourceGroupSnapshot(
    stdout,
    "Resource-group ownership verification",
  );
  if (
    current.id.toLowerCase() !==
      context.ownershipProof.resourceId.toLowerCase() ||
    current.tags[RESOURCE_GROUP_OWNERSHIP_TAG] !==
      context.ownershipProof.token
  ) {
    throw new ResourceGroupOwnershipError(
      `Refusing to delete resource group '${context.resourceGroup}': its durable ownership marker no longer matches this invocation.`,
    );
  }

  let lock: { id: string; name: string; notes: string; level: string };
  try {
    const { stdout: lockOutput } = await runAzure([
      "lock",
      "show",
      "--name",
      context.ownershipProof.lockName,
      "--resource-group",
      context.resourceGroup,
      "--subscription",
      context.subscriptionId,
      "--query",
      "{id:id,name:name,notes:notes,level:level}",
      "--output",
      "json",
    ]);
    lock = parseLockSnapshot(
      lockOutput,
      "Resource-group rollback lease verification",
    );
  } catch (error) {
    throw new ResourceGroupOwnershipError(
      `Refusing to delete resource group '${context.resourceGroup}': its rollback lease could not be verified (${errorText(error)}).`,
    );
  }
  if (
    lock.id.toLowerCase() !==
      lockResourceId(
        context.ownershipProof.resourceId,
        context.ownershipProof.lockName,
      ).toLowerCase() ||
    lock.name !== context.ownershipProof.lockName ||
    lock.notes !== context.ownershipProof.token ||
    lock.level.toLowerCase() !== "cannotdelete"
  ) {
    throw new ResourceGroupOwnershipError(
      `Refusing to delete resource group '${context.resourceGroup}': its rollback lease no longer exactly matches this invocation.`,
    );
  }
  return context.ownershipProof;
}

export async function releaseResourceGroupOwnership(
  resourceGroup: string,
  subscriptionId: string,
  ownershipProof: ResourceGroupOwnershipProof | undefined,
  runAzure: AzureRunner,
): Promise<{ released: boolean; warning?: string }> {
  if (!ownershipProof) {
    return { released: true };
  }

  try {
    await verifyResourceGroupOwnership(
      { resourceGroup, subscriptionId, ownershipProof },
      runAzure,
    );
  } catch (error) {
    return {
      released: false,
      warning:
        `Could not verify this run's resource-group ownership before release (${errorText(error)}). ` +
        `The safety lock '${ownershipProof.lockName}' was left in place.`,
    };
  }

  try {
    await runAzure([
      "tag",
      "update",
      "--resource-id",
      ownershipProof.resourceId,
      "--operation",
      "Delete",
      "--tags",
      `${RESOURCE_GROUP_OWNERSHIP_TAG}=${ownershipProof.token}`,
      "--subscription",
      subscriptionId,
      "--output",
      "none",
    ]);
  } catch (error) {
    return {
      released: false,
      warning:
        `Could not remove the internal resource-group ownership tag (${errorText(error)}). ` +
        `The safety lock '${ownershipProof.lockName}' was left in place.`,
    };
  }

  try {
    await runAzure([
      "lock",
      "delete",
      "--name",
      ownershipProof.lockName,
      "--resource-group",
      resourceGroup,
      "--subscription",
      subscriptionId,
      "--output",
      "none",
    ]);
  } catch (error) {
    return {
      released: false,
      warning:
        `Could not release safety lock '${ownershipProof.lockName}' (${errorText(error)}). ` +
        "The lock was left in place and may be removed manually after inspection.",
    };
  }
  return { released: true };
}

export async function maybeRollbackResourceGroup(options: {
  ownershipProof?: ResourceGroupOwnershipProof;
  cleanup: () => Promise<void>;
}): Promise<"protected-existing" | "cleaned"> {
  if (!options.ownershipProof) {
    return "protected-existing";
  }

  await options.cleanup();
  return "cleaned";
}

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

export function formatRetainedResourceGuidance(
  context: CleanupContext,
): string {
  const rg = shellQuote(context.resourceGroup);
  const sub = shellQuote(context.subscriptionId);
  const location = shellQuote(context.location);
  const baseName = shellQuote(context.baseName);
  const kvPrefix = shellQuote(`${context.baseName}-kv-`);
  const aoaiName = shellQuote(`${context.baseName}-aoai`);

  return [
    `  Resource group ${rg} was created by this run and was retained.`,
    "  Correct the failure, then rerun the complete original command with the same flags.",
    "  This retained resource group is now pre-existing, so --rollback-on-failure on a retry intentionally cannot delete it.",
    "  If cleanup soft-deletes the matching Key Vault, the retry discovers its exact Azure-assigned name and recovers it automatically.",
    "  Key Vault purge protection can keep manual purge blocked until the retention period ends; recovery does not require waiting for purge.",
    "",
    "  Or clean up only this failed fresh deployment (capture names before deleting the group):",
    `    KARS_KV_NAME="$(az keyvault list --resource-group ${rg} --subscription ${sub} --query "[?starts_with(name, ${kvPrefix})].name | [0]" --output tsv)"`,
    `    KARS_AOAI_NAME="$(az cognitiveservices account list --resource-group ${rg} --subscription ${sub} --query "[?name == ${aoaiName}].name | [0]" --output tsv)"`,
    `    az group delete --name ${rg} --subscription ${sub} --yes`,
    `    [ -z "$KARS_KV_NAME" ] || az keyvault purge --name "$KARS_KV_NAME" --location ${location} --subscription ${sub}`,
    `    [ -z "$KARS_AOAI_NAME" ] || az cognitiveservices account purge --name "$KARS_AOAI_NAME" --resource-group ${rg} --location ${location} --subscription ${sub}`,
    "",
    `  Derived deployment base name: ${baseName}`,
  ].join("\n");
}

export function formatCleanupCompletion(
  resourceGroup: string,
  result: CleanupResult,
): string[] {
  if (result.purgeFailures.length === 0) {
    return [
      `Resource group '${resourceGroup}' deleted. Derived soft-deleted resources were purged where available.`,
    ];
  }

  const keyVaultFailures = result.purgeFailures.filter((failure) =>
    failure.startsWith("Key Vault "),
  );
  const messages = [
    `Resource group '${resourceGroup}' deleted, but cleanup is incomplete. Purge remains blocked or failed for: ${result.purgeFailures.join(", ")}.`,
  ];
  if (keyVaultFailures.length > 0) {
    messages.push(
      "Retry the same kars up command now: it will identify and recover the matching soft-deleted Key Vault. Purge protection may continue blocking purge until Azure's retention period ends.",
    );
  } else {
    messages.push(
      "Retry cleanup after Azure finishes deleting the retained service state.",
    );
  }
  return messages;
}
