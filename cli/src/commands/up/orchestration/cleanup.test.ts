// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";

import {
  cleanupCreatedResourceGroup,
  discoverCleanupNames,
  formatCleanupCompletion,
  formatRetainedResourceGuidance,
  parsePositiveInteger,
  releaseResourceGroupOwnership,
  RESOURCE_GROUP_ADOPTION_LOCK,
  RESOURCE_GROUP_OWNERSHIP_TAG,
  ResourceGroupOwnershipError,
  type AzureRunner,
  type ResourceGroupOwnershipProof,
} from "../orchestration.js";

const ownershipProof: ResourceGroupOwnershipProof = {
  resourceId: "/subscriptions/sub-1/resourceGroups/fresh-rg",
  token: "test-ownership-token",
  lockName: "kars-up-lease-testownershiptoken",
};

describe("up orchestration", () => {
  it("keeps preflight authoritative without legacy AKS or VM-size re-detection", () => {
    const source = readFileSync(new URL("../../up.ts", import.meta.url), "utf8");
    expect(source).not.toContain("options.skipInfra = true");
    expect(source).not.toContain("resolveVmSizes");
  });

  it("purges only discovered names matching this deployment's derived names", async () => {
    const calls: string[][] = [];
    const runAzure: AzureRunner = async (args) => {
      calls.push(args);
      if (args[0] === "keyvault" && args[1] === "list") {
        return {
          stdout:
            "safe-kv-a1b2c3\nsafe-kv-too-long7\nother-kv-a1b2c3\n",
        };
      }
      if (args[0] === "cognitiveservices" && args[2] === "list") {
        return { stdout: "safe-aoai\nother-aoai\n" };
      }
      if (args[0] === "group" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id: ownershipProof.resourceId,
            tags: {
              [RESOURCE_GROUP_OWNERSHIP_TAG]: ownershipProof.token,
            },
          }),
        };
      }
      if (args[0] === "lock" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id:
              `${ownershipProof.resourceId}/providers/` +
              `Microsoft.Authorization/locks/${ownershipProof.lockName}`,
            name: ownershipProof.lockName,
            notes: ownershipProof.token,
            level: "CanNotDelete",
          }),
        };
      }
      return { stdout: "" };
    };

    const result = await cleanupCreatedResourceGroup(
      {
        resourceGroup: "fresh-rg",
        baseName: "safe",
        clusterName: "safe",
        location: "westus3",
        subscriptionId: "sub-1",
        kubernetesVersion: "1.34.7",
        nodeCount: 1,
        ownershipProof,
      },
      runAzure,
    );

    expect(result.keyVaultNames).toEqual(["safe-kv-a1b2c3"]);
    expect(result.azureAiNames).toEqual(["safe-aoai"]);
    const verifyIndex = calls.findIndex(
      (args) => args[0] === "group" && args[1] === "show",
    );
    const deleteIndex = calls.findIndex(
      (args) =>
        args[0] === "group" && args[1] === "delete",
    );
    const lockVerifyIndex = calls.findIndex(
      (args) => args[0] === "lock" && args[1] === "show",
    );
    const leaseDeleteIndex = calls.findIndex(
      (args) => args[0] === "lock" && args[1] === "delete",
    );
    const waitIndex = calls.findIndex(
      (args) => args[0] === "group" && args[1] === "wait",
    );
    const purgeIndexes = calls
      .map((args, index) => ({ args, index }))
      .filter(({ args }) => args.includes("purge"))
      .map(({ index }) => index);
    expect([
      verifyIndex,
      lockVerifyIndex,
      leaseDeleteIndex,
      deleteIndex,
      waitIndex,
    ]).toEqual([2, 3, 4, 5, 6]);
    expect(calls[verifyIndex]).toEqual([
      "group",
      "show",
      "--name",
      "fresh-rg",
      "--subscription",
      "sub-1",
      "--query",
      "{id:id,tags:tags}",
      "--output",
      "json",
    ]);
    expect(calls[lockVerifyIndex]).toEqual([
      "lock",
      "show",
      "--name",
      ownershipProof.lockName,
      "--resource-group",
      "fresh-rg",
      "--subscription",
      "sub-1",
      "--query",
      "{id:id,name:name,notes:notes,level:level}",
      "--output",
      "json",
    ]);
    expect(calls[leaseDeleteIndex]).toEqual([
      "lock",
      "delete",
      "--name",
      ownershipProof.lockName,
      "--resource-group",
      "fresh-rg",
      "--subscription",
      "sub-1",
      "--output",
      "none",
    ]);
    expect(calls[deleteIndex]).toEqual([
      "group",
      "delete",
      "--name",
      "fresh-rg",
      "--subscription",
      "sub-1",
      "--yes",
      "--output",
      "none",
    ]);
    expect(calls[waitIndex]).toEqual([
      "group",
      "wait",
      "--deleted",
      "--resource-group",
      "fresh-rg",
      "--subscription",
      "sub-1",
      "--timeout",
      "3600",
      "--interval",
      "30",
      "--output",
      "none",
    ]);
    expect(purgeIndexes.every((index) => index > waitIndex)).toBe(true);
    expect(calls.some((args) => args[0] === "rest")).toBe(false);
    const discoveries = calls
      .slice(0, verifyIndex)
      .filter(
        (args) =>
          args[0] === "keyvault" || args[0] === "cognitiveservices",
      );
    expect(discoveries).toHaveLength(2);
    for (const discovery of discoveries) {
      expect(discovery).toContain("--resource-group");
      expect(discovery).toContain("fresh-rg");
      expect(discovery).toContain("--subscription");
      expect(discovery).toContain("sub-1");
    }
    expect(
      calls.some(
        (args) =>
          args[0] === "keyvault" &&
          args[1] === "purge" &&
          args.includes("safe-kv-a1b2c3"),
      ),
    ).toBe(true);
    expect(
      calls.some(
        (args) =>
          args[0] === "cognitiveservices" &&
          args[2] === "purge" &&
          args.includes("safe-aoai"),
      ),
    ).toBe(true);
    expect(calls.some((args) => args.includes("other-aoai"))).toBe(false);
    expect(calls.some((args) => args.includes("other-kv-a1b2c3"))).toBe(false);
  });

  it("surfaces concurrent adoption when its guard blocks group deletion and never purges", async () => {
    const calls: string[][] = [];
    const runAzure: AzureRunner = async (args) => {
      calls.push(args);
      if (args[0] === "keyvault") {
        return { stdout: "safe-kv-a1b2c3\n" };
      }
      if (args[0] === "cognitiveservices") {
        return { stdout: "safe-aoai\n" };
      }
      if (args[0] === "group" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id: ownershipProof.resourceId,
            tags: {
              [RESOURCE_GROUP_OWNERSHIP_TAG]: ownershipProof.token,
            },
          }),
        };
      }
      if (args[0] === "lock" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id:
              `${ownershipProof.resourceId}/providers/` +
              `Microsoft.Authorization/locks/${ownershipProof.lockName}`,
            name: ownershipProof.lockName,
            notes: ownershipProof.token,
            level: "CanNotDelete",
          }),
        };
      }
      if (args[0] === "group" && args[1] === "delete") {
        throw {
          stderr:
            "(ScopeLocked) The scope cannot be deleted because of lock kars-up-adopted.",
        };
      }
      return { stdout: "" };
    };

    const cleanup = cleanupCreatedResourceGroup(
      {
        resourceGroup: "fresh-rg",
        baseName: "safe",
        clusterName: "safe",
        location: "westus3",
        subscriptionId: "sub-1",
        kubernetesVersion: "1.34.7",
        nodeCount: 1,
        ownershipProof,
      },
      runAzure,
    );
    await expect(cleanup).rejects.toBeInstanceOf(
      ResourceGroupOwnershipError,
    );
    await expect(cleanup).rejects.toThrow(
      "adopted concurrently",
    );

    const deleteCall = calls.find(
      (args) => args[0] === "group" && args[1] === "delete",
    );
    expect(deleteCall).toContain("--yes");
    expect(
      calls.filter((args) => args[0] === "lock" && args[1] === "delete"),
    ).toEqual([
      [
        "lock",
        "delete",
        "--name",
        ownershipProof.lockName,
        "--resource-group",
        "fresh-rg",
        "--subscription",
        "sub-1",
        "--output",
        "none",
      ],
    ]);
    expect(calls.some((args) => args.includes("wait"))).toBe(false);
    expect(calls.some((args) => args.includes("purge"))).toBe(false);
    expect(
      calls.some((args) => args.includes(RESOURCE_GROUP_ADOPTION_LOCK)),
    ).toBe(false);
  });

  it("exports RG-scoped name discovery without deriving the Key Vault suffix", async () => {
    const runAzure = vi
      .fn<AzureRunner>()
      .mockResolvedValueOnce({ stdout: "safe-kv-f9e8d7\n" })
      .mockResolvedValueOnce({ stdout: "safe-aoai\n" });

    await expect(
      discoverCleanupNames(
        {
          resourceGroup: "fresh-rg",
          baseName: "safe",
          clusterName: "safe",
          location: "westus3",
          subscriptionId: "sub-1",
          kubernetesVersion: "1.34.7",
          nodeCount: 1,
          ownershipProof,
        },
        runAzure,
      ),
    ).resolves.toEqual({
      keyVaultNames: ["safe-kv-f9e8d7"],
      azureAiNames: ["safe-aoai"],
    });

    expect(runAzure.mock.calls).toHaveLength(2);
    for (const [args] of runAzure.mock.calls) {
      expect(args).toContain("--resource-group");
      expect(args).toContain("fresh-rg");
    }
  });

  it.each([
    ["Key Vault", 1],
    ["Cognitive Services", 2],
  ])(
    "does not delete the group when %s name discovery fails",
    async (_service, failingCall) => {
      const calls: string[][] = [];
      const discoveryError = new Error(`${_service} discovery failed`);
      let callNumber = 0;
      const runAzure: AzureRunner = async (args) => {
        calls.push(args);
        callNumber += 1;
        if (callNumber === failingCall) {
          throw discoveryError;
        }
        return { stdout: "safe-kv-a1b2c3\n" };
      };

      await expect(
        cleanupCreatedResourceGroup(
          {
            resourceGroup: "fresh-rg",
            baseName: "safe",
            clusterName: "safe",
            location: "westus3",
            subscriptionId: "sub-1",
            kubernetesVersion: "1.34.7",
            nodeCount: 1,
          },
          runAzure,
        ),
      ).rejects.toBe(discoveryError);

      expect(
        calls.some(
          (args) =>
            (args[0] === "group" && args[1] === "delete") ||
            (args[0] === "rest" && args[2] === "delete"),
        ),
      ).toBe(false);
    },
  );

  it("does not delete when the durable ownership marker changed", async () => {
    const calls: string[][] = [];
    const runAzure: AzureRunner = async (args) => {
      calls.push(args);
      if (args[0] === "keyvault") {
        return { stdout: "safe-kv-a1b2c3\n" };
      }
      if (args[0] === "cognitiveservices") {
        return { stdout: "safe-aoai\n" };
      }
      if (args[0] === "group" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id: ownershipProof.resourceId,
            tags: {
              [RESOURCE_GROUP_OWNERSHIP_TAG]: "different-token",
            },
          }),
        };
      }
      return { stdout: "" };
    };

    await expect(
      cleanupCreatedResourceGroup(
        {
          resourceGroup: "fresh-rg",
          baseName: "safe",
          clusterName: "safe",
          location: "westus3",
          subscriptionId: "sub-1",
          kubernetesVersion: "1.34.7",
          nodeCount: 1,
          ownershipProof,
        },
        runAzure,
      ),
    ).rejects.toThrow("durable ownership marker no longer matches");
    expect(
      calls.some(
        (args) =>
          (args[0] === "group" && args[1] === "delete") ||
          (args[0] === "rest" && args[2] === "delete"),
      ),
    ).toBe(false);
  });

  it("requires the exact transient lock name, notes, type, and resource ID before rollback", async () => {
    const calls: string[][] = [];
    const runAzure: AzureRunner = async (args) => {
      calls.push(args);
      if (args[0] === "keyvault" || args[0] === "cognitiveservices") {
        return { stdout: "" };
      }
      if (args[0] === "group" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id: ownershipProof.resourceId,
            tags: {
              [RESOURCE_GROUP_OWNERSHIP_TAG]: ownershipProof.token,
            },
          }),
        };
      }
      if (args[0] === "lock" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id:
              `${ownershipProof.resourceId}/providers/` +
              `Microsoft.Authorization/locks/${ownershipProof.lockName}`,
            name: ownershipProof.lockName,
            notes: `${ownershipProof.token}-not-exact`,
            level: "CanNotDelete",
          }),
        };
      }
      return { stdout: "" };
    };

    await expect(
      cleanupCreatedResourceGroup(
        {
          resourceGroup: "fresh-rg",
          baseName: "safe",
          clusterName: "safe",
          location: "westus3",
          subscriptionId: "sub-1",
          kubernetesVersion: "1.34.7",
          nodeCount: 1,
          ownershipProof,
        },
        runAzure,
      ),
    ).rejects.toThrow("rollback lease no longer exactly matches");
    expect(
      calls.some(
        (args) =>
          (args[0] === "lock" && args[1] === "delete") ||
          (args[0] === "group" && args[1] === "delete"),
      ),
    ).toBe(false);
  });

  it("releases a successful fresh run by deleting only its token tag and transient lock", async () => {
    const runAzure = vi.fn<AzureRunner>(async (args) => {
      if (args[0] === "group") {
        return {
          stdout: JSON.stringify({
            id: ownershipProof.resourceId,
            tags: {
              [RESOURCE_GROUP_OWNERSHIP_TAG]: ownershipProof.token,
            },
          }),
        };
      }
      if (args[0] === "lock" && args[1] === "show") {
        return {
          stdout: JSON.stringify({
            id:
              `${ownershipProof.resourceId}/providers/` +
              `Microsoft.Authorization/locks/${ownershipProof.lockName}`,
            name: ownershipProof.lockName,
            notes: ownershipProof.token,
            level: "CanNotDelete",
          }),
        };
      }
      return { stdout: "" };
    });

    await expect(
      releaseResourceGroupOwnership(
        "fresh-rg",
        "sub-1",
        ownershipProof,
        runAzure,
      ),
    ).resolves.toEqual({ released: true });

    expect(runAzure.mock.calls.map(([args]) => args)).toEqual([
      [
        "group",
        "show",
        "--name",
        "fresh-rg",
        "--subscription",
        "sub-1",
        "--query",
        "{id:id,tags:tags}",
        "--output",
        "json",
      ],
      [
        "lock",
        "show",
        "--name",
        ownershipProof.lockName,
        "--resource-group",
        "fresh-rg",
        "--subscription",
        "sub-1",
        "--query",
        "{id:id,name:name,notes:notes,level:level}",
        "--output",
        "json",
      ],
      [
        "tag",
        "update",
        "--resource-id",
        ownershipProof.resourceId,
        "--operation",
        "Delete",
        "--tags",
        `${RESOURCE_GROUP_OWNERSHIP_TAG}=${ownershipProof.token}`,
        "--subscription",
        "sub-1",
        "--output",
        "none",
      ],
      [
        "lock",
        "delete",
        "--name",
        ownershipProof.lockName,
        "--resource-group",
        "fresh-rg",
        "--subscription",
        "sub-1",
        "--output",
        "none",
      ],
    ]);
    expect(
      runAzure.mock.calls.some(([args]) =>
        args.includes(RESOURCE_GROUP_ADOPTION_LOCK),
      ),
    ).toBe(false);
  });

  it("leaves the transient lock in place when selective token-tag removal fails", async () => {
    const runAzure = vi
      .fn<AzureRunner>()
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: ownershipProof.resourceId,
          tags: {
            [RESOURCE_GROUP_OWNERSHIP_TAG]: ownershipProof.token,
          },
        }),
      })
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id:
            `${ownershipProof.resourceId}/providers/` +
            `Microsoft.Authorization/locks/${ownershipProof.lockName}`,
          name: ownershipProof.lockName,
          notes: ownershipProof.token,
          level: "CanNotDelete",
        }),
      })
      .mockRejectedValueOnce(new Error("tag update unavailable"));

    const result = await releaseResourceGroupOwnership(
      "fresh-rg",
      "sub-1",
      ownershipProof,
      runAzure,
    );

    expect(result.released).toBe(false);
    expect(result.warning).toContain("was left in place");
    expect(runAzure).toHaveBeenCalledTimes(3);
    expect(runAzure.mock.calls[2][0].slice(0, 2)).toEqual([
      "tag",
      "update",
    ]);
  });

  it("does not run any release mutation for an existing resource group", async () => {
    const runAzure = vi.fn<AzureRunner>();

    await expect(
      releaseResourceGroupOwnership(
        "existing-rg",
        "sub-1",
        undefined,
        runAzure,
      ),
    ).resolves.toEqual({ released: true });
    expect(runAzure).not.toHaveBeenCalled();
  });

  it("contains no conditional resource-group update or delete implementation", () => {
    const orchestrationSource = [
      readFileSync(new URL("./resource-group.ts", import.meta.url), "utf8"),
      readFileSync(
        new URL("./resource-group-cleanup.ts", import.meta.url),
        "utf8",
      ),
    ].join("\n");
    expect(orchestrationSource).not.toMatch(/\betag\b/i);
    expect(orchestrationSource.toLowerCase()).not.toContain(
      '"--method",\n      "patch"',
    );
    expect(orchestrationSource).not.toContain("If-Match");
  });

  it("accepts only Bicep-supported node counts", () => {
    expect(parsePositiveInteger("2")).toBe(2);
    expect(parsePositiveInteger("100")).toBe(100);
    for (const invalid of ["0", "-1", "1.5", "101", "abc", "9007199254740992"]) {
      expect(() => parsePositiveInteger(invalid)).toThrow(
        "must be an integer from 1 to 100",
      );
    }
  });

  it("prints concrete retry and cleanup commands for a retained fresh group", () => {
    const guidance = formatRetainedResourceGuidance({
      resourceGroup: "fresh-rg",
      baseName: "safe",
      clusterName: "safe-aks",
      location: "westus3",
      subscriptionId: "00000000-0000-0000-0000-000000000001",
      kubernetesVersion: "1.34.7",
      nodeCount: 1,
    });

    expect(guidance).toContain(
      "rerun the complete original command with the same flags",
    );
    expect(guidance).toContain(
      "retained resource group is now pre-existing",
    );
    expect(guidance).toContain(
      "--rollback-on-failure on a retry intentionally cannot delete it",
    );
    expect(guidance).toContain(
      "retry discovers its exact Azure-assigned name and recovers it automatically",
    );
    expect(guidance).toContain(
      "purge protection can keep manual purge blocked until the retention period ends",
    );
    expect(guidance).not.toContain("kars up --resource-group");
    expect(guidance).not.toContain("Future automated cleanup");
    expect(guidance).toContain(
      "az group delete --name 'fresh-rg' --subscription '00000000-0000-0000-0000-000000000001' --yes",
    );
    expect(guidance).toContain('--query "[?starts_with(name, \'safe-kv-\')].name | [0]"');
    expect(guidance).not.toContain('\\"');
  });

  it("reports purge-protected Key Vault cleanup as immediately recoverable", () => {
    const messages = formatCleanupCompletion("fresh-rg", {
      keyVaultNames: ["safe-kv-a1b2c3"],
      azureAiNames: [],
      purgeFailures: ["Key Vault safe-kv-a1b2c3"],
    });

    expect(messages.join("\n")).toContain("cleanup is incomplete");
    expect(messages.join("\n")).toContain(
      "identify and recover the matching soft-deleted Key Vault",
    );
    expect(messages.join("\n")).toContain(
      "Purge protection may continue blocking purge",
    );
    expect(messages.join("\n")).not.toContain(
      "soft-deleted resources were purged where available",
    );
  });
});
