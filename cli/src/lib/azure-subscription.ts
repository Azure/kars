// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Shared subscription pinning from the deployment-safety implementation (#531).
export function pinAzureSubscription(args: readonly string[], subscriptionId: string): string[] {
  const pinnedId = subscriptionId.trim();
  if (!pinnedId) throw new Error("Azure subscription ID must not be empty");
  const result = [...args];
  const indexes = result.flatMap((arg, index) =>
    arg === "--subscription" || arg.startsWith("--subscription=") ? [index] : []);
  if (indexes.length === 0) return [...result, "--subscription", pinnedId];
  if (indexes.length > 1) throw new Error("Azure command contains duplicate --subscription options");
  const index = indexes[0];
  const explicit = result[index] === "--subscription"
    ? result[index + 1] : result[index].slice("--subscription=".length);
  if (!explicit) throw new Error("--subscription requires a subscription ID");
  if (explicit !== pinnedId) {
    throw new Error(`Azure command is scoped to subscription '${explicit}', not the deployment subscription '${pinnedId}'`);
  }
  return result;
}

export function createSubscriptionPinnedExeca(
  execute: typeof import("execa").execa,
  subscriptionId: string,
): typeof import("execa").execa {
  return ((file: string, args?: readonly string[], options?: unknown) =>
    execute(file, file === "az" ? pinAzureSubscription(args ?? [], subscriptionId) : args, options as never)
  ) as unknown as typeof import("execa").execa;
}
