// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  asRecord,
  azureCliErrorText,
  defaultAzTextRunner,
  type AzTextRunner,
  type UnknownRecord,
} from "./core.js";

export interface InfrastructureCompleteness {
  complete: boolean;
  diagnostic: string;
}

export interface InfrastructureCompletenessOptions {
  foundryEndpoint?: string;
  openAiEndpoint?: string;
  subscriptionId?: string;
}

const REQUIRED_INFRASTRUCTURE_OUTPUTS = [
  "acrLoginServer",
  "sandboxIdentityClientId",
  "keyVaultName",
] as const;

function infrastructureOutput(
  outputs: UnknownRecord | undefined,
  name: string,
): string {
  const output = asRecord(outputs?.[name]);
  return typeof output?.value === "string" ? output.value.trim() : "";
}

function usesExternalAi(
  options: InfrastructureCompletenessOptions,
): boolean {
  return Boolean(
    options.foundryEndpoint?.trim() || options.openAiEndpoint?.trim(),
  );
}

export function classifyInfrastructureDeployment(
  payload: unknown,
  options: InfrastructureCompletenessOptions = {},
): InfrastructureCompleteness {
  const record = asRecord(payload);
  const provisioningState =
    typeof record?.provisioningState === "string"
      ? record.provisioningState.trim()
      : "";
  if (provisioningState.toLowerCase() !== "succeeded") {
    return {
      complete: false,
      diagnostic: provisioningState
        ? `Resource-group deployment 'main' is ${provisioningState}, not Succeeded.`
        : "Resource-group deployment 'main' returned no valid provisioning state.",
    };
  }

  const outputs = asRecord(record?.outputs);
  const requiredOutputs = usesExternalAi(options)
    ? REQUIRED_INFRASTRUCTURE_OUTPUTS
    : [...REQUIRED_INFRASTRUCTURE_OUTPUTS, "openAiEndpoint"];
  const missing = requiredOutputs.filter(
    (name) => infrastructureOutput(outputs, name).length === 0,
  );
  if (missing.length > 0) {
    return {
      complete: false,
      diagnostic:
        `Resource-group deployment 'main' is missing valid output value(s): ${missing.join(", ")}.`,
    };
  }
  return {
    complete: true,
    diagnostic: "Resource-group deployment 'main' succeeded with all required outputs.",
  };
}

export function isInfrastructureDeploymentNotFoundError(
  error: unknown,
): boolean {
  const message = azureCliErrorText(error);
  return (
    /\((?:DeploymentNotFound|ResourceNotFound|ResourceGroupNotFound)\)/i.test(
      message,
    ) ||
    /["']code["']\s*:\s*["'](?:DeploymentNotFound|ResourceNotFound|ResourceGroupNotFound)["']/i.test(
      message,
    )
  );
}

interface AzureResourceScope {
  subscriptionId: string;
  resourceGroup: string;
}

function parseAzureResourceScope(id: string): AzureResourceScope | undefined {
  const match = id.match(
    /^\/subscriptions\/([^/]+)\/resourceGroups\/([^/]+)\/providers\//i,
  );
  return match
    ? { subscriptionId: match[1], resourceGroup: match[2] }
    : undefined;
}

function sameAzureResourceScope(
  left: AzureResourceScope,
  right: AzureResourceScope,
): boolean {
  return (
    left.subscriptionId.toLowerCase() === right.subscriptionId.toLowerCase() &&
    left.resourceGroup.toLowerCase() === right.resourceGroup.toLowerCase()
  );
}

function parseJsonResponse(payload: string, description: string): unknown {
  try {
    return JSON.parse(payload);
  } catch (error) {
    throw new Error(
      `Could not verify ${description}: Azure CLI returned malformed JSON.`,
      { cause: error },
    );
  }
}

async function queryLiveResource(
  description: string,
  args: string[],
  runAzText: AzTextRunner,
): Promise<{ found: true; payload: unknown } | { found: false }> {
  let payload: string;
  try {
    payload = await runAzText(args);
  } catch (error) {
    if (isInfrastructureDeploymentNotFoundError(error)) {
      return { found: false };
    }
    const detail = azureCliErrorText(error).split("\n")[0] || String(error);
    throw new Error(`Could not verify ${description}: ${detail}`, {
      cause: error,
    });
  }
  return {
    found: true,
    payload: parseJsonResponse(payload, description),
  };
}

interface LiveResourceRecord {
  id: string;
  name: string;
  record: UnknownRecord;
}

function parseLiveResourceRecord(
  value: unknown,
  description: string,
): LiveResourceRecord {
  const record = asRecord(value);
  const id = typeof record?.id === "string" ? record.id.trim() : "";
  const name = typeof record?.name === "string" ? record.name.trim() : "";
  if (!record || !id || !name || !parseAzureResourceScope(id)) {
    throw new Error(
      `Could not verify ${description}: Azure CLI returned malformed resource data.`,
    );
  }
  return { id, name, record };
}

function isLiveResourceInScope(
  resource: LiveResourceRecord,
  expectedScope: AzureResourceScope,
): boolean {
  const actualScope = parseAzureResourceScope(resource.id);
  return Boolean(
    actualScope && sameAzureResourceScope(actualScope, expectedScope),
  );
}

function normalizedEndpoint(value: string): string {
  return value.trim().replace(/\/+$/, "").toLowerCase();
}

/**
 * Verify that the retained resource-group deployment completed and produced
 * the values later deployment stages consume, then resolve those values to
 * live resources in the deployment's resource group and subscription. Only
 * explicit not-found errors represent an incomplete deployment;
 * authorization, transport, and malformed-response failures fail closed.
 */
export async function detectInfrastructureCompleteness(
  resourceGroup: string,
  optionsOrRunner: InfrastructureCompletenessOptions | AzTextRunner = {},
  runAzText: AzTextRunner = defaultAzTextRunner,
): Promise<InfrastructureCompleteness> {
  const options =
    typeof optionsOrRunner === "function" ? {} : optionsOrRunner;
  const resourceRunner =
    typeof optionsOrRunner === "function" ? optionsOrRunner : runAzText;
  const scopedResourceRunner: AzTextRunner = (args) =>
    resourceRunner(
      options.subscriptionId
        ? [...args, "--subscription", options.subscriptionId]
        : args,
    );
  let payload: string;
  try {
    payload = await scopedResourceRunner([
      "deployment",
      "group",
      "show",
      "--resource-group",
      resourceGroup,
      "--name",
      "main",
      "--query",
      "{id:id,provisioningState:properties.provisioningState,outputs:properties.outputs}",
      "-o",
      "json",
    ]);
  } catch (error) {
    if (isInfrastructureDeploymentNotFoundError(error)) {
      return {
        complete: false,
        diagnostic: "Resource-group deployment 'main' was not found.",
      };
    }
    const detail = azureCliErrorText(error).split("\n")[0] || String(error);
    throw new Error(
      `Could not verify resource-group deployment 'main': ${detail}`,
      { cause: error },
    );
  }

  const deploymentPayload = parseJsonResponse(
    payload,
    "resource-group deployment 'main'",
  );
  const deploymentRecord = asRecord(deploymentPayload);
  if (
    !deploymentRecord ||
    typeof deploymentRecord.provisioningState !== "string" ||
    !deploymentRecord.provisioningState.trim()
  ) {
    throw new Error(
      "Could not verify resource-group deployment 'main': Azure CLI returned malformed deployment data.",
    );
  }
  const deploymentCompleteness = classifyInfrastructureDeployment(
    deploymentPayload,
    options,
  );
  if (!deploymentCompleteness.complete) return deploymentCompleteness;

  const deploymentId =
    typeof deploymentRecord.id === "string" ? deploymentRecord.id.trim() : "";
  const expectedScope = parseAzureResourceScope(deploymentId);
  if (
    !expectedScope ||
    expectedScope.resourceGroup.toLowerCase() !== resourceGroup.toLowerCase()
  ) {
    throw new Error(
      "Could not verify resource-group deployment 'main': Azure CLI returned malformed deployment scope.",
    );
  }
  const outputs = asRecord(deploymentRecord.outputs);
  const acrLoginServer = infrastructureOutput(outputs, "acrLoginServer");
  const explicitAcrName = infrastructureOutput(outputs, "acrName");
  const derivedAcrName = acrLoginServer.split(".", 1)[0];
  const acrName = explicitAcrName || derivedAcrName;
  if (!/^[a-zA-Z0-9]{5,50}$/.test(acrName)) {
    return {
      complete: false,
      diagnostic:
        "Resource-group deployment 'main' has stale or invalid ACR outputs.",
    };
  }

  const acrDescription = `Azure Container Registry '${acrName}'`;
  const acrResult = await queryLiveResource(
    acrDescription,
    [
      "acr",
      "show",
      "--resource-group",
      resourceGroup,
      "--name",
      acrName,
      "--query",
      "{id:id,name:name,loginServer:loginServer}",
      "-o",
      "json",
    ],
    scopedResourceRunner,
  );
  if (!acrResult.found) {
    return {
      complete: false,
      diagnostic: `${acrDescription} from deployment outputs was not found in resource group '${resourceGroup}'.`,
    };
  }
  const acr = parseLiveResourceRecord(acrResult.payload, acrDescription);
  const liveLoginServer =
    typeof acr.record.loginServer === "string"
      ? acr.record.loginServer.trim()
      : "";
  if (!liveLoginServer) {
    throw new Error(
      `Could not verify ${acrDescription}: Azure CLI returned malformed resource data.`,
    );
  }
  if (
    !isLiveResourceInScope(acr, expectedScope) ||
    acr.name.toLowerCase() !== acrName.toLowerCase() ||
    liveLoginServer.toLowerCase() !== acrLoginServer.toLowerCase()
  ) {
    return {
      complete: false,
      diagnostic: `${acrDescription} does not match the retained deployment outputs in resource group '${resourceGroup}'.`,
    };
  }

  const keyVaultName = infrastructureOutput(outputs, "keyVaultName");
  const keyVaultDescription = `Key Vault '${keyVaultName}'`;
  const keyVaultResult = await queryLiveResource(
    keyVaultDescription,
    [
      "keyvault",
      "show",
      "--resource-group",
      resourceGroup,
      "--name",
      keyVaultName,
      "--query",
      "{id:id,name:name}",
      "-o",
      "json",
    ],
    scopedResourceRunner,
  );
  if (!keyVaultResult.found) {
    return {
      complete: false,
      diagnostic: `${keyVaultDescription} from deployment outputs was not found in resource group '${resourceGroup}'.`,
    };
  }
  const keyVault = parseLiveResourceRecord(
    keyVaultResult.payload,
    keyVaultDescription,
  );
  if (
    !isLiveResourceInScope(keyVault, expectedScope) ||
    keyVault.name.toLowerCase() !== keyVaultName.toLowerCase()
  ) {
    return {
      complete: false,
      diagnostic: `${keyVaultDescription} does not match the retained deployment outputs in resource group '${resourceGroup}'.`,
    };
  }

  const identityClientId = infrastructureOutput(
    outputs,
    "sandboxIdentityClientId",
  );
  const identityDescription = `user-assigned identity with client ID '${identityClientId}'`;
  const identityResult = await queryLiveResource(
    identityDescription,
    [
      "identity",
      "list",
      "--resource-group",
      resourceGroup,
      "--query",
      "[].{id:id,name:name,clientId:clientId}",
      "-o",
      "json",
    ],
    scopedResourceRunner,
  );
  if (!identityResult.found || !Array.isArray(identityResult.payload)) {
    if (!identityResult.found) {
      return {
        complete: false,
        diagnostic: `The ${identityDescription} was not found in resource group '${resourceGroup}'.`,
      };
    }
    throw new Error(
      `Could not verify ${identityDescription}: Azure CLI returned malformed resource data.`,
    );
  }
  const identities = identityResult.payload.map((value) =>
    parseLiveResourceRecord(value, identityDescription),
  );
  const identity = identities.find((candidate) => {
    const clientId =
      typeof candidate.record.clientId === "string"
        ? candidate.record.clientId.trim()
        : "";
    if (!clientId) {
      throw new Error(
        `Could not verify ${identityDescription}: Azure CLI returned malformed resource data.`,
      );
    }
    return (
      clientId.toLowerCase() === identityClientId.toLowerCase() &&
      isLiveResourceInScope(candidate, expectedScope)
    );
  });
  if (!identity) {
    return {
      complete: false,
      diagnostic: `The ${identityDescription} was not found in resource group '${resourceGroup}'.`,
    };
  }

  if (!usesExternalAi(options)) {
    const openAiEndpoint = infrastructureOutput(outputs, "openAiEndpoint");
    const accountDescription = `Azure OpenAI account for endpoint '${openAiEndpoint}'`;
    const accountResult = await queryLiveResource(
      accountDescription,
      [
        "cognitiveservices",
        "account",
        "list",
        "--resource-group",
        resourceGroup,
        "--query",
        "[].{id:id,name:name,kind:kind,endpoint:properties.endpoint}",
        "-o",
        "json",
      ],
      scopedResourceRunner,
    );
    if (!accountResult.found || !Array.isArray(accountResult.payload)) {
      if (!accountResult.found) {
        return {
          complete: false,
          diagnostic: `${accountDescription} was not found in resource group '${resourceGroup}'.`,
        };
      }
      throw new Error(
        `Could not verify ${accountDescription}: Azure CLI returned malformed resource data.`,
      );
    }
    const accounts = accountResult.payload.map((value) =>
      parseLiveResourceRecord(value, accountDescription),
    );
    const account = accounts.find((candidate) => {
      const endpoint =
        typeof candidate.record.endpoint === "string"
          ? candidate.record.endpoint
          : "";
      const kind =
        typeof candidate.record.kind === "string"
          ? candidate.record.kind.trim()
          : "";
      if (!endpoint.trim() || !kind) {
        throw new Error(
          `Could not verify ${accountDescription}: Azure CLI returned malformed resource data.`,
        );
      }
      return (
        kind.toLowerCase() === "openai" &&
        normalizedEndpoint(endpoint) === normalizedEndpoint(openAiEndpoint) &&
        isLiveResourceInScope(candidate, expectedScope)
      );
    });
    if (!account) {
      return {
        complete: false,
        diagnostic: `${accountDescription} was not found in resource group '${resourceGroup}'.`,
      };
    }
  }

  return {
    complete: true,
    diagnostic: usesExternalAi(options)
      ? "Resource-group deployment 'main' resolved to live ACR, Key Vault, and managed identity resources; external AI configuration is in use."
      : "Resource-group deployment 'main' resolved to live ACR, Key Vault, managed identity, and Azure OpenAI resources.",
  };
}

export function requireCompleteSkipInfraDeployment(
  completeness: InfrastructureCompleteness,
): void {
  if (completeness.complete) return;
  throw new Error(
    `Existing AKS infrastructure is incomplete. ${completeness.diagnostic} ` +
      "Kars will not run the full managedClusters Bicep template against an existing cluster " +
      "because unmodeled AKS properties could be reset. Repair or recreate the missing ancillary " +
      "resources (such as ACR, Key Vault, managed identity, and Azure AI resources) manually, or " +
      "deploy into a new resource group with a new cluster name. Do not use --force-infra.",
  );
}
