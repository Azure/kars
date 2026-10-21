// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import {
  classifyInfrastructureDeployment,
  detectInfrastructureCompleteness,
  requireCompleteSkipInfraDeployment,
} from "./deployment-safety.js";

describe("infrastructure deployment completeness", () => {
  const scope =
    "/subscriptions/sub-1/resourceGroups/kars-westus3/providers";
  const completePayload = {
    id: `${scope}/Microsoft.Resources/deployments/main`,
    provisioningState: "Succeeded",
    outputs: {
      acrLoginServer: { type: "String", value: "karsacr.azurecr.io" },
      acrName: { type: "String", value: "karsacr" },
      sandboxIdentityClientId: {
        type: "String",
        value: "00000000-0000-0000-0000-000000000001",
      },
      keyVaultName: { type: "String", value: "kars-kv-abc123" },
      openAiEndpoint: {
        type: "String",
        value: "https://kars-aoai.openai.azure.com/",
      },
    },
  };

  function successfulLiveResourceRunner(calls: string[][] = []) {
    return vi.fn(async (args: string[]) => {
      calls.push(args);
      if (args[0] === "deployment") return JSON.stringify(completePayload);
      if (args[0] === "acr") {
        return JSON.stringify({
          id: `${scope}/Microsoft.ContainerRegistry/registries/karsacr`,
          name: "karsacr",
          loginServer: "karsacr.azurecr.io",
        });
      }
      if (args[0] === "keyvault") {
        return JSON.stringify({
          id: `${scope}/Microsoft.KeyVault/vaults/kars-kv-abc123`,
          name: "kars-kv-abc123",
        });
      }
      if (args[0] === "identity") {
        return JSON.stringify([
          {
            id: `${scope}/Microsoft.ManagedIdentity/userAssignedIdentities/kars-aks-sandbox-wi`,
            name: "kars-aks-sandbox-wi",
            clientId: "00000000-0000-0000-0000-000000000001",
          },
        ]);
      }
      if (args[0] === "cognitiveservices") {
        return JSON.stringify([
          {
            id: `${scope}/Microsoft.CognitiveServices/accounts/kars-aoai`,
            name: "kars-aoai",
            kind: "OpenAI",
            endpoint: "https://kars-aoai.openai.azure.com/",
          },
        ]);
      }
      throw new Error(`Unexpected Azure command: ${args.join(" ")}`);
    });
  }

  it("accepts only a Succeeded deployment with every required ARM output value", () => {
    expect(classifyInfrastructureDeployment(completePayload)).toEqual({
      complete: true,
      diagnostic:
        "Resource-group deployment 'main' succeeded with all required outputs.",
    });
  });

  it("classifies Failed deployments and missing output values as incomplete", () => {
    expect(
      classifyInfrastructureDeployment({
        ...completePayload,
        provisioningState: "Failed",
      }),
    ).toMatchObject({ complete: false, diagnostic: expect.stringContaining("Failed") });
    const missingOutput = structuredClone(completePayload);
    delete (missingOutput.outputs as Partial<typeof missingOutput.outputs>).keyVaultName;
    const result = classifyInfrastructureDeployment(missingOutput);
    expect(result).toMatchObject({
      complete: false,
      diagnostic: expect.stringContaining("keyVaultName"),
    });
    expect(() => requireCompleteSkipInfraDeployment(result)).toThrowError(
      /Existing AKS infrastructure is incomplete.*missing ancillary resources.*Do not use --force-infra/s,
    );
  });

  it("verifies every retained output against exact read-only resource-group scoped commands", async () => {
    const calls: string[][] = [];
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        { subscriptionId: "sub-selected" },
        successfulLiveResourceRunner(calls),
      ),
    ).resolves.toMatchObject({
      complete: true,
      diagnostic: expect.stringContaining("resolved to live"),
    });

    expect(calls).toEqual([
      [
        "deployment",
        "group",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "main",
        "--query",
        "{id:id,provisioningState:properties.provisioningState,outputs:properties.outputs}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "acr",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "karsacr",
        "--query",
        "{id:id,name:name,loginServer:loginServer}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "keyvault",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "kars-kv-abc123",
        "--query",
        "{id:id,name:name}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "identity",
        "list",
        "--resource-group",
        "kars-westus3",
        "--query",
        "[].{id:id,name:name,clientId:clientId}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
      [
        "cognitiveservices",
        "account",
        "list",
        "--resource-group",
        "kars-westus3",
        "--query",
        "[].{id:id,name:name,kind:kind,endpoint:properties.endpoint}",
        "-o",
        "json",
        "--subscription",
        "sub-selected",
      ],
    ]);
  });

  it("uses a read-only deployment show query and treats deployment not-found as incomplete", async () => {
    const calls: string[][] = [];
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          calls.push(args);
          throw {
            stderr: "(DeploymentNotFound) Deployment 'main' was not found.",
          };
        },
      ),
    ).resolves.toMatchObject({ complete: false, diagnostic: expect.stringContaining("not found") });
    expect(calls).toEqual([
      [
        "deployment",
        "group",
        "show",
        "--resource-group",
        "kars-westus3",
        "--name",
        "main",
        "--query",
        "{id:id,provisioningState:properties.provisioningState,outputs:properties.outputs}",
        "-o",
        "json",
      ],
    ]);
  });

  it.each([
    ["Azure Container Registry", "acr"],
    ["Key Vault", "keyvault"],
    ["user-assigned identity", "identity"],
    ["Azure OpenAI account", "cognitiveservices"],
  ])("treats a missing live %s as incomplete", async (expected, missingCommand) => {
    const baseRunner = successfulLiveResourceRunner();
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          if (args[0] === missingCommand) {
            if (missingCommand === "identity" || missingCommand === "cognitiveservices") {
              return "[]";
            }
            throw { stderr: "(ResourceNotFound) resource was not found" };
          }
          return baseRunner(args);
        },
      ),
    ).resolves.toMatchObject({
      complete: false,
      diagnostic: expect.stringContaining(expected),
    });
  });

  it("rejects stale output values that do not match the live resource", async () => {
    const baseRunner = successfulLiveResourceRunner();
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          if (args[0] === "acr") {
            return JSON.stringify({
              id: `${scope}/Microsoft.ContainerRegistry/registries/karsacr`,
              name: "karsacr",
              loginServer: "replacement.azurecr.io",
            });
          }
          return baseRunner(args);
        },
      ),
    ).resolves.toMatchObject({
      complete: false,
      diagnostic: expect.stringContaining("does not match"),
    });
  });

  it("does not require a provisioned AI account when an external endpoint was supplied", async () => {
    const externalPayload = structuredClone(completePayload);
    externalPayload.outputs.openAiEndpoint.value = "";
    const baseRunner = successfulLiveResourceRunner();
    const calls: string[][] = [];
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {
          foundryEndpoint:
            "https://shared.services.ai.azure.com/api/projects/project",
        },
        async (args) => {
          calls.push(args);
          if (args[0] === "deployment") return JSON.stringify(externalPayload);
          return baseRunner(args);
        },
      ),
    ).resolves.toMatchObject({
      complete: true,
      diagnostic: expect.stringContaining("external AI"),
    });
    expect(calls.some((args) => args[0] === "cognitiveservices")).toBe(false);
  });

  it("fails closed on authorization and malformed Azure responses", async () => {
    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        async () => {
          throw { stderr: "(AuthorizationFailed) deployment access denied" };
        },
      ),
    ).rejects.toThrow(/Could not verify resource-group deployment.*AuthorizationFailed/);

    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async (args) => {
          if (args[0] === "deployment") return JSON.stringify(completePayload);
          throw { stderr: "(AuthorizationFailed) access denied" };
        },
      ),
    ).rejects.toThrow(/Could not verify Azure Container Registry.*AuthorizationFailed/);

    await expect(
      detectInfrastructureCompleteness(
        "kars-westus3",
        {},
        async () => "{not-json",
      ),
    ).rejects.toThrow(/malformed JSON/);
  });
});

