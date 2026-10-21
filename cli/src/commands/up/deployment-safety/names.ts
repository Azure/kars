// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

const KEY_VAULT_SUFFIX_LENGTH = 6;
export const MAX_AKS_NODE_RESOURCE_GROUP_NAME_LENGTH = 80;

export interface DerivedAzureResourceNames {
  baseName: string;
  aks: string;
  acrExample: string;
  keyVaultExample: string;
  azureOpenAi: string;
  logAnalytics: string;
  applicationInsights: string;
  sandboxIdentity: string;
}

/**
 * Validate every resource name derived by the deployment template. The Key
 * Vault name is the tightest constraint because its deterministic suffix leaves
 * only 14 characters for baseName.
 */
export function validateDerivedAzureResourceNames(
  clusterName: string,
): DerivedAzureResourceNames {
  const baseName = clusterName.replace(/-aks$/, "");
  const suffix = "0".repeat(KEY_VAULT_SUFFIX_LENGTH);
  const names: DerivedAzureResourceNames = {
    baseName,
    aks: `${baseName}-aks`,
    acrExample: `${baseName.replace(/-/g, "")}${suffix}`,
    keyVaultExample: `${baseName}-kv-${suffix}`,
    azureOpenAi: `${baseName}-aoai`,
    logAnalytics: `${baseName}-monitor-law`,
    applicationInsights: `${baseName}-monitor-ai`,
    sandboxIdentity: `${baseName}-aks-sandbox-wi`,
  };

  if (
    !/^[a-z](?:[a-z0-9-]*[a-z0-9])?$/.test(baseName) ||
    baseName.includes("--")
  ) {
    throw new Error(
      `Derived baseName '${baseName}' is invalid. It must start with a lowercase letter; use --cluster-name with lowercase letters, ` +
        "numbers, and single internal hyphens only (for example: --cluster-name kars-prod).",
    );
  }
  if (names.keyVaultExample.length > 24) {
    throw new Error(
      `Derived Key Vault name '${baseName}-kv-<6-char-suffix>' would be ` +
        `${names.keyVaultExample.length} characters; Azure Key Vault allows at most 24. ` +
        "Use --cluster-name with at most 14 characters before the optional '-aks' suffix " +
        "(for example: --cluster-name kars-prod).",
    );
  }
  if (!/^[a-z0-9]{5,50}$/.test(names.acrExample)) {
    throw new Error(
      `Derived ACR name '${names.acrExample}' is invalid. Use --cluster-name with enough ` +
        "letters or numbers to produce a 5-50 character alphanumeric registry name.",
    );
  }
  if (names.aks.length > 63) {
    throw new Error(`Derived AKS name '${names.aks}' exceeds Azure's 63-character limit.`);
  }
  if (names.azureOpenAi.length > 64) {
    throw new Error(
      `Derived Azure OpenAI name '${names.azureOpenAi}' exceeds Azure's 64-character limit.`,
    );
  }
  if (names.logAnalytics.length > 63) {
    throw new Error(
      `Derived Log Analytics name '${names.logAnalytics}' exceeds Azure's 63-character limit.`,
    );
  }
  if (names.sandboxIdentity.length > 128) {
    throw new Error(
      `Derived managed identity name '${names.sandboxIdentity}' exceeds Azure's 128-character limit.`,
    );
  }

  return names;
}

export function validateAutomaticAksNodeResourceGroupName(
  resourceGroup: string,
  aksName: string,
  region: string,
): string {
  const nodeResourceGroup = `MC_${resourceGroup}_${aksName}_${region}`;
  if (nodeResourceGroup.length > MAX_AKS_NODE_RESOURCE_GROUP_NAME_LENGTH) {
    throw new Error(
      `AKS automatic node resource group '${nodeResourceGroup}' would be ` +
        `${nodeResourceGroup.length} characters; Azure allows at most ` +
        `${MAX_AKS_NODE_RESOURCE_GROUP_NAME_LENGTH}. Shorten --resource-group or ` +
        "--cluster-name, or choose a shorter --region (for example: " +
        "--resource-group kars-rg --cluster-name kars --region westus3).",
    );
  }
  return nodeResourceGroup;
}
