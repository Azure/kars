// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export type FeishuSecretReferenceState = "referenced" | "unreferenced" | "unknown";

export function classifyFeishuSecretReference(
  sandboxJson: string,
  secretName: string,
): FeishuSecretReferenceState {
  if (!sandboxJson.trim()) return "unreferenced";
  try {
    const sandbox = JSON.parse(sandboxJson) as {
      spec?: { channels?: Array<{ type?: string; credentialSecretRef?: { name?: string } }> };
    };
    if (!Array.isArray(sandbox.spec?.channels)) return "unknown";
    const channel = sandbox.spec.channels.find((candidate) => candidate.type === "Feishu");
    return channel?.credentialSecretRef?.name === secretName ? "referenced" : "unreferenced";
  } catch {
    return "unknown";
  }
}

export function shouldCleanupStagedFeishuSecret(
  stagedSecretName: string | undefined,
  referenceState: FeishuSecretReferenceState,
): boolean {
  return Boolean(stagedSecretName) && referenceState === "unreferenced";
}