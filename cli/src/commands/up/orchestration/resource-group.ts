// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomUUID } from "node:crypto";

import type { AzureRunner } from "./subscription.js";

export const RESOURCE_GROUP_OWNERSHIP_TAG =
  "kars-runtime-creation-token";
export const RESOURCE_GROUP_ADOPTION_LOCK = "kars-up-adopted";
const RESOURCE_GROUP_LEASE_PREFIX = "kars-up-lease-";

export interface ResourceGroupOwnershipProof {
  resourceId: string;
  token: string;
  lockName: string;
}

export class ResourceGroupOwnershipError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ResourceGroupOwnershipError";
  }
}

export interface ResourceGroupResult {
  created: boolean;
  location: string;
  ownershipProof?: ResourceGroupOwnershipProof;
}

export interface EnsureResourceGroupOptions {
  createIfMissing: boolean;
  generatedForRollback?: boolean;
  ownershipToken?: string;
  adopterToken?: string;
}


function errorText(error: unknown): string {
  if (error && typeof error === "object") {
    const candidate = error as { stderr?: unknown; message?: unknown };
    return String(candidate.stderr ?? candidate.message ?? error);
  }
  return String(error);
}

function isMissingResourceGroup(error: unknown): boolean {
  return /ResourceGroupNotFound|resource group .+ could not be found/i.test(
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

async function showResourceGroup(
  resourceGroup: string,
  subscriptionId: string,
  runAzure: AzureRunner,
): Promise<ResourceGroupSnapshot> {
  const { stdout } = await runAzure([
    "group",
    "show",
    "--name",
    resourceGroup,
    "--subscription",
    subscriptionId,
    "--query",
    "{id:id,location:location,tags:tags}",
    "--output",
    "json",
  ]);
  return parseResourceGroupSnapshot(stdout, "Resource-group lookup");
}

function leaseLockName(token: string): string {
  const fragment = token.toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 48);
  if (!fragment) {
    throw new Error(
      "Resource-group ownership token must contain an ASCII letter or digit",
    );
  }
  return `${RESOURCE_GROUP_LEASE_PREFIX}${fragment}`;
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

async function createResourceGroupLock(
  resourceGroup: string,
  subscriptionId: string,
  resourceId: string,
  lockName: string,
  notes: string,
  runAzure: AzureRunner,
): Promise<void> {
  const { stdout } = await runAzure([
    "lock",
    "create",
    "--name",
    lockName,
    "--resource-group",
    resourceGroup,
    "--lock-type",
    "CanNotDelete",
    "--notes",
    notes,
    "--subscription",
    subscriptionId,
    "--query",
    "{id:id,name:name,notes:notes,level:level}",
    "--output",
    "json",
  ]);
  const lock = parseLockSnapshot(stdout, "Resource-group lock creation");
  if (
    lock.id.toLowerCase() !==
      lockResourceId(resourceId, lockName).toLowerCase() ||
    lock.name !== lockName ||
    lock.notes !== notes ||
    lock.level.toLowerCase() !== "cannotdelete"
  ) {
    throw new Error(
      `Azure did not confirm the expected CanNotDelete lock '${lockName}'`,
    );
  }
}

async function adoptExistingResourceGroup(
  resourceGroup: string,
  fallbackLocation: string,
  subscriptionId: string,
  observed: ResourceGroupSnapshot,
  runAzure: AzureRunner,
  adopterToken: string,
): Promise<ResourceGroupResult> {
  const result = {
    created: false,
    location: observed.location || fallbackLocation,
  };
  if (
    !Object.prototype.hasOwnProperty.call(
      observed.tags,
      RESOURCE_GROUP_OWNERSHIP_TAG,
    )
  ) {
    return result;
  }

  const notes = `kars up adopter token=${adopterToken}`;
  try {
    await createResourceGroupLock(
      resourceGroup,
      subscriptionId,
      observed.id,
      RESOURCE_GROUP_ADOPTION_LOCK,
      notes,
      runAzure,
    );
  } catch (error) {
    throw new ResourceGroupOwnershipError(
      `Refusing to adopt resource group '${resourceGroup}': its persistent adoption guard could not be created (${errorText(error)}).`,
    );
  }
  return result;
}


export async function ensureResourceGroup(
  resourceGroup: string,
  location: string,
  subscriptionId: string,
  runAzure: AzureRunner,
  options: EnsureResourceGroupOptions,
): Promise<ResourceGroupResult> {
  const adopterToken = options.adopterToken ?? randomUUID();
  try {
    const existing = await showResourceGroup(
      resourceGroup,
      subscriptionId,
      runAzure,
    );
    if (options.generatedForRollback) {
      throw new ResourceGroupOwnershipError(
        `Resource group '${resourceGroup}' already exists, so this invocation cannot claim rollback ownership. ` +
          "Rerun kars up --rollback-on-failure to generate a different unique group, or omit the flag and clean up manually after a failure.",
      );
    }
    return adoptExistingResourceGroup(
      resourceGroup,
      location,
      subscriptionId,
      existing,
      runAzure,
      adopterToken,
    );
  } catch (error) {
    if (!isMissingResourceGroup(error)) {
      throw error;
    }
  }

  if (!options.createIfMissing) {
    throw new Error(
      `Resource group '${resourceGroup}' does not exist. --skip-infra only reuses existing infrastructure and will not create a resource group. ` +
        "Pass --resource-group for an existing deployment, or remove --skip-infra to provision new infrastructure.",
    );
  }

  const token = options.generatedForRollback
    ? options.ownershipToken ?? randomUUID()
    : undefined;
  if (token !== undefined && !token.trim()) {
    throw new Error("Resource-group ownership token must not be empty");
  }
  const resourceId =
    `/subscriptions/${subscriptionId}/resourceGroups/${resourceGroup}`;
  const createArgs = [
    "group",
    "create",
    "--name",
    resourceGroup,
    "--location",
    location,
    "--subscription",
    subscriptionId,
  ];
  if (token !== undefined) {
    createArgs.push(
      "--tags",
      `${RESOURCE_GROUP_OWNERSHIP_TAG}=${token}`,
    );
  }
  createArgs.push(
    "--query",
    "{id:id,location:location,tags:tags}",
    "--output",
    "json",
  );
  const { stdout: createOutput } = await runAzure(createArgs);

  const created = parseJsonObject(
    createOutput,
    "Resource-group creation",
  );
  const responseTags =
    created.tags && typeof created.tags === "object" && !Array.isArray(created.tags)
      ? (created.tags as Record<string, unknown>)
      : {};
  const returnedId = String(created.id ?? "");
  if (returnedId.toLowerCase() !== resourceId.toLowerCase()) {
    throw new Error(
      `Resource-group creation for '${resourceGroup}' returned an unexpected resource ID.`,
    );
  }
  const createdLocation =
    String(created.location ?? "").trim() || location;
  if (token === undefined) {
    return { created: true, location: createdLocation };
  }
  if (responseTags[RESOURCE_GROUP_OWNERSHIP_TAG] !== token) {
    throw new Error(
      `Creation of unique rollback resource group '${resourceGroup}' did not return a verifiable ownership marker. ` +
        "Automatic rollback is disabled; inspect the resource group before cleanup.",
    );
  }

  const lockName = leaseLockName(token);
  try {
    await createResourceGroupLock(
      resourceGroup,
      subscriptionId,
      returnedId,
      lockName,
      token,
      runAzure,
    );
  } catch (error) {
    throw new ResourceGroupOwnershipError(
      `Resource group '${resourceGroup}' was created, but its rollback lease '${lockName}' could not be established (${errorText(error)}). No deployment resources were created.`,
    );
  }

  return {
    created: true,
    location: createdLocation,
    ownershipProof: { resourceId: returnedId, token, lockName },
  };
}
