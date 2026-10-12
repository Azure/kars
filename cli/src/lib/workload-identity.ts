// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { execa } from "execa";
import { createSubscriptionPinnedExeca } from "./azure-subscription.js";

const audience = "api://AzureADTokenExchange";
// Same built-in role as deploy/bicep/modules/sandbox-rbac.bicep.
const contributorRole = "e40ec5ca-96e0-45a2-b4ff-59039f2c2b59";
const guid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

type ObjectValue = Record<string, unknown>;
function object(value: unknown): value is ObjectValue {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export interface WorkloadIdentityConfig {
  subscriptionId: string;
  resourceGroup: string;
  identityName: string;
  issuer: string;
}

export function validateWorkloadIdentityConfig(config: WorkloadIdentityConfig): WorkloadIdentityConfig {
  if (typeof config.subscriptionId !== "string" || !guid.test(config.subscriptionId)
    || typeof config.resourceGroup !== "string" || !/^[\w().-]+$/.test(config.resourceGroup)
    || typeof config.identityName !== "string" || !/^[\w-]+$/.test(config.identityName)) {
    throw new Error("Workload Identity requires a subscription ID, resource group and managed identity name");
  }
  let url: URL;
  try { url = new URL(config.issuer); } catch { throw new Error("Workload Identity requires an HTTPS OIDC issuer"); }
  if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash
    || config.issuer !== config.issuer.trim() || /[,\s]/.test(config.issuer)) {
    throw new Error("Workload Identity requires an HTTPS OIDC issuer without credentials, query or fragment");
  }
  return config;
}

function validateSandboxNames(names: unknown[]): string[] {
  if (names.some(name => typeof name !== "string" || name.length > 58
    || !/^[a-z0-9](?:[-a-z0-9]*[a-z0-9])?$/.test(name))
    || new Set(names).size !== names.length) {
    throw new Error("Invalid or duplicate sandbox names in Workload Identity inventory");
  }
  return names as string[];
}

export function sandboxNamesForFederation(json: string): string[] {
  let value: unknown;
  try { value = JSON.parse(json); } catch { throw new Error("Invalid sandbox inventory for Workload Identity"); }
  if (!object(value) || !Array.isArray(value.items)) throw new Error("Missing sandbox inventory for Workload Identity");
  return validateSandboxNames(value.items.map(item => object(item) && object(item.metadata) ? item.metadata.name : undefined));
}

/** Verify ARM configuration; this does not prove token exchange or RBAC propagation. */
export async function ensureWorkloadIdentity(
  execute: typeof execa,
  config: WorkloadIdentityConfig,
  sandboxNames: string[],
): Promise<void> {
  validateWorkloadIdentityConfig(config);
  validateSandboxNames(sandboxNames);
  const run = createSubscriptionPinnedExeca(execute, config.subscriptionId);
  const identityArgs = ["--identity-name", config.identityName, "--resource-group", config.resourceGroup];
  const scope = `/subscriptions/${config.subscriptionId}/resourceGroups/${config.resourceGroup}/providers/Microsoft.ManagedIdentity/userAssignedIdentities/${config.identityName}`;
  const roleId = `/subscriptions/${config.subscriptionId}/providers/Microsoft.Authorization/roleDefinitions/${contributorRole}`;
  const expected = [
    { name: "kars-controller-sa", subject: "system:serviceaccount:kars-system:kars-controller" },
    ...sandboxNames.map(name => ({ name: `kars-${name}`, subject: `system:serviceaccount:kars-${name}:sandbox` })),
  ];
  if (new Set(expected.map(item => item.name)).size !== expected.length) {
    throw new Error("Sandbox federated credential name conflicts with the controller credential");
  }
  const call = async (args: string[], label: string): Promise<string> => {
    try {
      const { stdout } = await run("az", args, { stdio: "pipe", timeout: 30000 });
      return stdout;
    } catch (cause) {
      throw new Error(`Workload Identity ${label} failed; configuration is incomplete`, { cause });
    }
  };
  const read = async (args: string[], label: string): Promise<unknown> => {
    const text = await call([...args, "--output", "json"], label);
    try { return JSON.parse(text); } catch { throw new Error(`Invalid Workload Identity ${label} response`); }
  };
  const readPrincipal = async (): Promise<string> => {
    const identity = await read(["identity", "show", "--name", config.identityName, "--resource-group", config.resourceGroup], "identity read");
    if (!object(identity) || typeof identity.id !== "string" || identity.id.toLowerCase() !== scope.toLowerCase()
      || typeof identity.principalId !== "string" || !guid.test(identity.principalId)) {
      throw new Error("Workload Identity identity read does not match the selected resource or principal");
    }
    return identity.principalId;
  };
  const principalId = await readPrincipal();
  const readCredentials = async (): Promise<ObjectValue[]> => {
    const value = await read(["identity", "federated-credential", "list", ...identityArgs], "credential inventory");
    if (!Array.isArray(value) || value.some(item => !object(item) || typeof item.name !== "string"
      || typeof item.issuer !== "string" || typeof item.subject !== "string"
      || !Array.isArray(item.audiences) || item.audiences.some((entry: unknown) => typeof entry !== "string"))
      || new Set(value.map(item => item.name)).size !== value.length) {
      throw new Error("Invalid Workload Identity credential inventory");
    }
    return value;
  };
  const matches = (credential: ObjectValue, subject: string): boolean => credential.issuer === config.issuer
    && credential.subject === subject && Array.isArray(credential.audiences)
    && credential.audiences.length === 1 && credential.audiences[0] === audience;
  const credentials = await readCredentials();
  for (const item of expected) {
    const existing = credentials.find(credential => credential.name === item.name);
    if (existing && !matches(existing, item.subject)) throw new Error(`Conflicting Workload Identity credential '${item.name}'; review its issuer, subject and audience`);
  }
  const readRole = async (): Promise<boolean> => {
    const value = await read(["role", "assignment", "list", "--assignee-object-id", principalId,
      "--scope", scope, "--fill-principal-name", "false", "--fill-role-definition-name", "false"], "role inventory");
    if (!Array.isArray(value) || value.some(item => !object(item)
      || typeof item.principalId !== "string" || typeof item.scope !== "string" || typeof item.roleDefinitionId !== "string")) {
      throw new Error("Invalid Workload Identity role inventory");
    }
    const matching = value.filter(item => item.principalId.toLowerCase() === principalId.toLowerCase()
      && item.scope.toLowerCase() === scope.toLowerCase() && item.roleDefinitionId.toLowerCase() === roleId.toLowerCase());
    if (matching.some(item => item.condition != null && item.condition !== "")) {
      throw new Error("Conditional Managed Identity Contributor assignment requires review; refusing to broaden it");
    }
    return matching.length > 0;
  };
  const roleExists = await readRole();
  for (const item of expected) {
    if (!credentials.some(credential => credential.name === item.name)) {
      await call(["identity", "federated-credential", "create", ...identityArgs,
        "--name", item.name, "--issuer", config.issuer, "--subject", item.subject,
        "--audiences", audience, "--output", "none"], `credential '${item.name}' creation`);
    }
  }
  if (!roleExists) {
    await call(["role", "assignment", "create", "--assignee-object-id", principalId,
      "--assignee-principal-type", "ServicePrincipal", "--role", contributorRole,
      "--scope", scope, "--output", "none"], "Managed Identity Contributor assignment");
  }
  const verified = await readCredentials();
  for (const item of expected) {
    const credential = verified.find(entry => entry.name === item.name);
    if (!credential || !matches(credential, item.subject)) throw new Error(`Workload Identity credential '${item.name}' failed readback verification`);
  }
  if (!await readRole()) throw new Error("Managed Identity Contributor assignment failed readback verification");
  if ((await readPrincipal()).toLowerCase() !== principalId.toLowerCase()) {
    throw new Error("Workload Identity principal changed during configuration; verification failed");
  }
}
