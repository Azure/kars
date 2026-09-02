// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomUUID } from "node:crypto";

export type AzureRunner = (
  args: string[],
  options?: { timeout?: number },
) => Promise<{ stdout: string }>;

export function pinAzureSubscription(
  args: readonly string[],
  subscriptionId: string,
): string[] {
  const pinnedId = subscriptionId.trim();
  if (!pinnedId) {
    throw new Error("Azure subscription ID must not be empty");
  }

  const result = [...args];
  const explicitIndexes = result.flatMap((arg, index) =>
    arg === "--subscription" || arg.startsWith("--subscription=")
      ? [index]
      : [],
  );
  if (explicitIndexes.length === 0) {
    return [...result, "--subscription", pinnedId];
  }
  if (explicitIndexes.length > 1) {
    throw new Error("Azure command contains duplicate --subscription options");
  }

  const explicitIndex = explicitIndexes[0];
  const explicit = result[explicitIndex];
  const explicitId = explicit === "--subscription"
    ? result[explicitIndex + 1]
    : explicit.slice("--subscription=".length);
  if (!explicitId) {
    throw new Error("--subscription requires a subscription ID");
  }
  if (explicitId !== pinnedId) {
    throw new Error(
      `Azure command is scoped to subscription '${explicitId}', not the deployment subscription '${pinnedId}'`,
    );
  }
  return result;
}

export function createAzureRunner(
  execute: typeof import("execa").execa,
  subscriptionId: string,
): AzureRunner {
  return async (args, options) => {
    const { stdout } = await execute(
      "az",
      pinAzureSubscription(args, subscriptionId),
      { stdio: "pipe", ...options },
    );
    return { stdout: String(stdout) };
  };
}

export function createSubscriptionPinnedExeca(
  execute: typeof import("execa").execa,
  subscriptionId: string,
): typeof import("execa").execa {
  return ((file: string, args?: readonly string[], options?: unknown) =>
    execute(
      file,
      file === "az"
        ? pinAzureSubscription(args ?? [], subscriptionId)
        : args,
      options as never,
    )) as unknown as typeof import("execa").execa;
}

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

export interface BicepParameterOptions {
  location: string;
  baseName: string;
  recoverKeyVault?: boolean;
  vmSize: string;
  systemVmSize: string;
  kataVmSize: string;
  kubernetesVersion: string;
  systemNodeCount: number;
  nodeCount: number;
  kataNodeCount: number;
  systemPoolName: string;
  sandboxPoolName: string;
  kataPoolName: string;
}

export interface PoolNames {
  systemPoolName: string;
  sandboxPoolName: string;
  kataPoolName: string;
}

export interface ProjectedBicepParameterOptions {
  location: string;
  baseName: string;
  recoverKeyVault?: boolean;
  nodeVmSize?: string;
  systemVmSize?: string;
  kataVmSize?: string;
  kubernetesVersion?: string;
  systemNodeCount?: number;
  nodeCount?: number;
  kataNodeCount?: number;
  systemPoolName?: string;
  sandboxPoolName?: string;
  kataPoolName?: string;
}

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

export interface RecoverableDeletedKeyVault {
  name: string;
  location: string;
  vaultId: string;
  state: "soft-deleted";
  deletionDate?: string;
  scheduledPurgeDate?: string;
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

export function parsePositiveInteger(value: string): number {
  if (!/^[1-9]\d*$/.test(value)) {
    throw new Error("must be an integer from 1 to 100");
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed > 100) {
    throw new Error("must be an integer from 1 to 100");
  }
  return parsed;
}

export function validateInfrastructureMode(options: {
  skipInfra: boolean;
  forceInfra: boolean;
}): void {
  if (options.skipInfra && options.forceInfra) {
    throw new Error(
      "--skip-infra and --force-infra cannot be used together",
    );
  }
}

export function resolvePoolNames(options: {
  systemPoolName?: string;
  sandboxPoolName?: string;
  kataPoolName?: string;
}): PoolNames {
  return {
    systemPoolName: options.systemPoolName ?? "system",
    sandboxPoolName: options.sandboxPoolName ?? "clawpool",
    kataPoolName: options.kataPoolName ?? "katapool",
  };
}

export async function getActiveSubscriptionId(
  runAzure: AzureRunner,
): Promise<string> {
  const { stdout } = await runAzure([
    "account",
    "show",
    "--query",
    "id",
    "--output",
    "tsv",
  ]);
  const subscriptionId = stdout.trim();
  if (!subscriptionId) {
    throw new Error("Azure CLI returned an empty subscription ID");
  }
  return subscriptionId;
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

export function buildBicepParameters(
  options: BicepParameterOptions,
): string[] {
  if (!options.kubernetesVersion.trim()) {
    throw new Error("Preflight did not resolve a Kubernetes version");
  }
  if (!Number.isSafeInteger(options.nodeCount) || options.nodeCount <= 0) {
    throw new Error("Preflight did not resolve a positive node count");
  }
  if (
    !Number.isSafeInteger(options.systemNodeCount) ||
    options.systemNodeCount <= 0
  ) {
    throw new Error("Preflight did not resolve a positive system node count");
  }
  if (!options.kataVmSize.trim()) {
    throw new Error("Preflight did not resolve a Kata VM size");
  }
  if (
    !Number.isSafeInteger(options.kataNodeCount) ||
    options.kataNodeCount < 0
  ) {
    throw new Error("Preflight did not resolve a non-negative Kata node count");
  }
  return [
    `location=${options.location}`,
    `baseName=${options.baseName}`,
    `recoverKeyVault=${options.recoverKeyVault === true}`,
    `vmSize=${options.vmSize}`,
    `systemVmSize=${options.systemVmSize}`,
    `kataVmSize=${options.kataVmSize}`,
    `kubernetesVersion=${options.kubernetesVersion}`,
    `systemNodeCount=${options.systemNodeCount}`,
    `nodeCount=${options.nodeCount}`,
    `kataNodeCount=${options.kataNodeCount}`,
    `systemPoolName=${options.systemPoolName}`,
    `sandboxPoolName=${options.sandboxPoolName}`,
    `kataPoolName=${options.kataPoolName}`,
  ];
}

export function buildProjectedBicepParameters(
  options: ProjectedBicepParameterOptions,
): string[] {
  const vmSize = options.nodeVmSize?.trim();
  const systemVmSize = options.systemVmSize?.trim();
  const kataVmSize = options.kataVmSize?.trim();
  if (!vmSize || !systemVmSize || !kataVmSize) {
    throw new Error(
      "Preflight did not resolve sandbox, system, and Kata VM sizes",
    );
  }

  return buildBicepParameters({
    location: options.location,
    baseName: options.baseName,
    recoverKeyVault: options.recoverKeyVault,
    vmSize,
    systemVmSize,
    kataVmSize,
    kubernetesVersion: options.kubernetesVersion ?? "",
    systemNodeCount: options.systemNodeCount ?? Number.NaN,
    nodeCount: options.nodeCount ?? Number.NaN,
    kataNodeCount: options.kataNodeCount ?? Number.NaN,
    ...resolvePoolNames(options),
  });
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
