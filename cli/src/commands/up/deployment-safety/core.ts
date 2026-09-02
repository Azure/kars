// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execa } from "execa";

export type UnknownRecord = Record<string, unknown>;

export function asRecord(value: unknown): UnknownRecord | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as UnknownRecord)
    : undefined;
}

export function hasCliOption(flag: string, argv: string[] = process.argv): boolean {
  return argv.some((argument) => argument === flag || argument.startsWith(`${flag}=`));
}

export type AzTextRunner = (args: string[]) => Promise<string>;

export function azureCliErrorText(error: unknown): string {
  const record = asRecord(error);
  return [record?.stderr, record?.stdout, record?.shortMessage, record?.message]
    .filter((value): value is string => typeof value === "string" && value.trim().length > 0)
    .join("\n");
}

export function isAksNotFoundError(error: unknown): boolean {
  const message = azureCliErrorText(error);
  return (
    /\((?:ResourceNotFound|ResourceGroupNotFound|ManagedClusterNotFound|ParentResourceNotFound)\)/i.test(
      message,
    ) ||
    /["']code["']\s*:\s*["'](?:ResourceNotFound|ResourceGroupNotFound|ManagedClusterNotFound|ParentResourceNotFound)["']/i.test(
      message,
    )
  );
}

export async function defaultAzTextRunner(args: string[]): Promise<string> {
  const { stdout } = await execa("az", args, { stdio: "pipe", timeout: 20000 });
  return stdout;
}

/**
 * Check for an existing AKS cluster without mutating Azure. Only Azure's
 * explicit not-found codes mean absence; auth, transport, and malformed
 * responses fail closed.
 */
export type AksPoolLogicalRole = "system" | "sandbox" | "kata" | "other";

export interface AksAgentPoolProfile {
  name: string;
  count: number;
  vmSize: string;
  mode: string;
  provisioningState: string;
  nodeLabels: Record<string, string>;
  nodeTaints: string[];
  workloadRuntime?: string;
  logicalRole: AksPoolLogicalRole;
}

export interface ExistingAksCluster {
  exists: true;
  id: string;
  provisioningState: string;
  powerState: {
    code: string;
  };
  kubernetesVersion: string;
  supportPlan: string;
  sku: {
    name: string;
    tier: string;
  };
  autoUpgradeProfile: {
    upgradeChannel: string;
    nodeOSUpgradeChannel: string;
  };
  agentPoolProfiles: AksAgentPoolProfile[];
}

export type AksClusterDetection =
  | { exists: false }
  | ExistingAksCluster;

export type ExistingAksDisposition =
  | { action: "new" }
  | { action: "reuse"; cluster: ExistingAksCluster }
  | {
      action: "force-update";
      cluster: ExistingAksCluster;
      diagnostic: string;
    }
  | { action: "stopped"; cluster: ExistingAksCluster; diagnostic: string }
  | { action: "recover"; cluster: ExistingAksCluster; diagnostic: string };

export function resourceGroupFromId(resourceId: string): string {
  const segments = resourceId.split("/");
  const index = segments.findIndex(
    (segment) => segment.toLowerCase() === "resourcegroups",
  );
  return index >= 0 ? segments[index + 1] ?? "<resource-group>" : "<resource-group>";
}

export function resourceNameFromId(resourceId: string): string {
  return resourceId.split("/").filter(Boolean).at(-1) ?? "<cluster-name>";
}

export type AzJsonRunner = (args: string[]) => Promise<unknown>;

export async function defaultAzJsonRunner(args: string[]): Promise<unknown> {
  const { stdout } = await execa("az", args, { stdio: "pipe", timeout: 120000 });
  return JSON.parse(stdout);
}
