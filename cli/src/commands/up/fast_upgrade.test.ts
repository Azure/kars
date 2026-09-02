// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  const spinner = {
    start: vi.fn(),
    succeed: vi.fn(),
    warn: vi.fn(),
  };
  spinner.start.mockReturnValue(spinner);
  return {
    execa: vi.fn(),
    loadContext: vi.fn(),
    requireBundledAsset: vi.fn(() => "/bundled/deploy/helm/kars"),
    rolloutRestartAll: vi.fn(),
    spinner,
  };
});

vi.mock("execa", () => ({ execa: mocks.execa }));
vi.mock("ora", () => ({ default: vi.fn(() => mocks.spinner) }));
vi.mock("../../config.js", () => ({ loadContext: mocks.loadContext }));
vi.mock("../../lib/repo-assets.js", () => ({
  requireBundledAsset: mocks.requireBundledAsset,
}));
vi.mock("../../lib/version.js", () => ({ cliReleaseTag: () => "v-test" }));
vi.mock("../upgrade.js", () => ({
  rolloutRestartAll: mocks.rolloutRestartAll,
}));

import { runFastUpgrade } from "./fast_upgrade.js";

const cachedContext = {
  subscription: "cached-sub",
  resourceGroup: "kars-rg",
  aksCluster: "kars-aks",
  acrLoginServer: "kars.azurecr.io",
  foundryEndpoint: "https://foundry.openai.azure.com",
  oidcIssuerUrl: "https://oidc.example.test",
  identityName: "kars-wi",
  identityResourceGroup: "identity-rg",
};

function subscriptionOptions(args: readonly string[]): string[] {
  return args.filter(
    (arg) => arg === "--subscription" || arg.startsWith("--subscription="),
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.loadContext.mockReturnValue(cachedContext);
  mocks.execa.mockImplementation(
    async (file: string, args: readonly string[] = []) => {
      if (file === "az" && args.slice(0, 2).join(" ") === "account list") {
        return { stdout: JSON.stringify([{ id: "legacy-sub" }]) };
      }
      if (file === "az" && args.slice(0, 2).join(" ") === "aks list") {
        return {
          stdout: JSON.stringify([{
            name: cachedContext.aksCluster,
            resourceGroup: cachedContext.resourceGroup,
          }]),
        };
      }
      if (
        file === "az" &&
        args[0] === "cognitiveservices" &&
        args.includes("deployment")
      ) {
        return { stdout: "[]" };
      }
      if (file === "az" && args[0] === "cognitiveservices") {
        return { stdout: "foundry-rg\n" };
      }
      if (file === "az" && args[0] === "identity" && args[1] === "show") {
        return { stdout: "principal-id\n" };
      }
      if (file === "kubectl") {
        return {
          stdout: JSON.stringify({
            items: [{ metadata: { name: "sandbox-one" } }],
          }),
        };
      }
      return { stdout: "" };
    },
  );
});

describe("runFastUpgrade Azure subscription pinning", () => {
  it("uses the cached deployment subscription for every Azure command", async () => {
    await runFastUpgrade({});

    const azureCalls = mocks.execa.mock.calls.filter(
      ([file]) => file === "az",
    );
    expect(azureCalls).not.toHaveLength(0);
    expect(
      azureCalls.filter(([, args]) =>
        (args as string[]).slice(0, 2).join(" ") === "account show"
      ),
    ).toHaveLength(0);

    for (const [, rawArgs] of azureCalls) {
      const args = rawArgs as string[];
      expect(subscriptionOptions(args)).toEqual(["--subscription"]);
      expect(args[args.indexOf("--subscription") + 1]).toBe("cached-sub");
    }

    const helmCall = mocks.execa.mock.calls.find(([file]) => file === "helm");
    expect(helmCall?.[1]).toContain("fedcred.subscriptionId=cached-sub");
    const roleCall = azureCalls.find(([, args]) =>
      (args as string[]).slice(0, 3).join(" ") === "role assignment create"
    );
    expect(roleCall?.[1]).toContain(
      "/subscriptions/cached-sub/resourceGroups/identity-rg/providers/Microsoft.ManagedIdentity/userAssignedIdentities/kars-wi",
    );

    for (const [file, rawArgs] of mocks.execa.mock.calls.filter(
      ([file]) => file !== "az",
    )) {
      expect(file).not.toBe("az");
      expect(subscriptionOptions((rawArgs ?? []) as string[])).toEqual([]);
    }
  });

  it("uniquely discovers a legacy deployment and pins every later Azure command", async () => {
    mocks.loadContext.mockReturnValue({
      ...cachedContext,
      subscription: undefined,
      foundryEndpoint: undefined,
      oidcIssuerUrl: undefined,
      identityName: undefined,
    });

    await runFastUpgrade({});

    const azureCalls = mocks.execa.mock.calls.filter(
      ([file]) => file === "az",
    );
    const accountCalls = azureCalls.filter(([, args]) =>
      (args as string[]).slice(0, 2).join(" ") === "account list"
    );
    expect(accountCalls).toHaveLength(1);
    expect(subscriptionOptions(accountCalls[0][1] as string[])).toEqual([]);

    const clusterLookups = azureCalls.filter(([, args]) =>
      (args as string[]).slice(0, 2).join(" ") === "aks list"
    );
    expect(clusterLookups).toHaveLength(1);
    expect(clusterLookups[0][1]).toContain("legacy-sub");

    const subsequentCalls = azureCalls.filter((call) =>
      call !== accountCalls[0] && call !== clusterLookups[0]
    );
    expect(subsequentCalls).not.toHaveLength(0);
    for (const [, rawArgs] of [...clusterLookups, ...subsequentCalls]) {
      const args = rawArgs as string[];
      expect(subscriptionOptions(args)).toEqual(["--subscription"]);
      expect(args[args.indexOf("--subscription") + 1]).toBe("legacy-sub");
    }
  });

  it.each([
    {
      name: "zero matching subscriptions",
      clusters: (_subscription: string) => [],
      message: "No enabled Azure subscription contains cached AKS cluster",
    },
    {
      name: "same-name clusters in multiple subscriptions",
      clusters: (_subscription: string) => [{
        name: cachedContext.aksCluster,
        resourceGroup: cachedContext.resourceGroup,
      }],
      message: "exists in multiple enabled Azure subscriptions",
    },
  ])("fails closed for legacy context with $name", async ({
    clusters,
    message,
  }) => {
    mocks.loadContext.mockReturnValue({
      ...cachedContext,
      subscription: undefined,
    });
    mocks.execa.mockImplementation(
      async (file: string, args: readonly string[] = []) => {
        if (file === "az" && args.slice(0, 2).join(" ") === "account list") {
          return {
            stdout: JSON.stringify([{ id: "sub-1" }, { id: "sub-2" }]),
          };
        }
        if (file === "az" && args.slice(0, 2).join(" ") === "aks list") {
          return {
            stdout: JSON.stringify(clusters(
              args[args.indexOf("--subscription") + 1],
            )),
          };
        }
        return { stdout: "" };
      },
    );

    await expect(runFastUpgrade({})).rejects.toThrow(message);

    const calls = mocks.execa.mock.calls.map(([file, args]) => ({
      file: file as string,
      args: (args ?? []) as string[],
    }));
    const lookups = calls.filter(({ file, args }) =>
      file === "az" && args.slice(0, 2).join(" ") === "aks list"
    );
    expect(lookups).toHaveLength(2);
    for (const { args } of lookups) {
      expect(subscriptionOptions(args)).toEqual(["--subscription"]);
    }
    expect(calls.some(({ file, args }) =>
      file === "helm" ||
      (file === "az" && (
        args.slice(0, 2).join(" ") === "aks get-credentials" ||
        args[0] === "identity" ||
        args[0] === "role"
      ))
    )).toBe(false);
  });

  it("persists the selected bring-up subscription in completed context", () => {
    const source = readFileSync(
      new URL("./sandbox_bringup.ts", import.meta.url),
      "utf8",
    );
    expect(source).toMatch(
      /saveFinalDeploymentContext\(\{\s*subscription: subscriptionId,\s*region:/,
    );
  });
});
