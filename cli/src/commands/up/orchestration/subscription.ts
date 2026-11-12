// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
