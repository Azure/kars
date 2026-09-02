// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";

import {
  ensureResourceGroup,
  maybeRollbackResourceGroup,
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
  it("reads an unmarked existing group once without mutating customer tags", async () => {
    const customerTags = {
      environment: "production",
      owner: "customer@example.test",
      "cost-center": "cc-42",
    };
    const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
      stdout: JSON.stringify({
        id: "/subscriptions/sub-1/resourceGroups/existing-rg",
        location: "westus3",
        tags: customerTags,
      }),
    });

    await expect(
      ensureResourceGroup("existing-rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: false,
      }),
    ).resolves.toEqual({ created: false, location: "westus3" });

    expect(runAzure).toHaveBeenCalledTimes(1);
    expect(runAzure.mock.calls[0][0]).toEqual([
      "group",
      "show",
      "--name",
      "existing-rg",
      "--subscription",
      "sub-1",
      "--query",
      "{id:id,location:location,tags:tags}",
      "--output",
      "json",
    ]);
    expect(runAzure.mock.calls[0][0][0]).not.toBe("rest");
    expect(customerTags).toEqual({
      environment: "production",
      owner: "customer@example.test",
      "cost-center": "cc-42",
    });
  });

  it("protects a marked existing group with a persistent adoption guard without mutating tags", async () => {
    const observedTags = {
      environment: "production",
      [RESOURCE_GROUP_OWNERSHIP_TAG]: "first-run-token",
      "Case-Sensitive-Customer-Key": "preserve-me",
    };
    const resourceId =
      "/subscriptions/sub-1/resourceGroups/existing-rg";
    const adopterToken = "second-run-token";
    const runAzure = vi
      .fn<AzureRunner>()
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: resourceId,
          location: "westus3",
          tags: observedTags,
        }),
      })
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: `${resourceId}/providers/Microsoft.Authorization/locks/${RESOURCE_GROUP_ADOPTION_LOCK}`,
          name: RESOURCE_GROUP_ADOPTION_LOCK,
          notes: `kars up adopter token=${adopterToken}`,
          level: "CanNotDelete",
        }),
      });

    await expect(
      ensureResourceGroup("existing-rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: true,
        adopterToken,
      }),
    ).resolves.toEqual({ created: false, location: "westus3" });

    expect(runAzure).toHaveBeenCalledTimes(2);
    expect(runAzure.mock.calls[1][0]).toEqual([
      "lock",
      "create",
      "--name",
      RESOURCE_GROUP_ADOPTION_LOCK,
      "--resource-group",
      "existing-rg",
      "--lock-type",
      "CanNotDelete",
      "--notes",
      `kars up adopter token=${adopterToken}`,
      "--subscription",
      "sub-1",
      "--query",
      "{id:id,name:name,notes:notes,level:level}",
      "--output",
      "json",
    ]);
    expect(observedTags).toEqual({
      environment: "production",
      [RESOURCE_GROUP_OWNERSHIP_TAG]: "first-run-token",
      "Case-Sensitive-Customer-Key": "preserve-me",
    });
    expect(
      runAzure.mock.calls.some(
        ([args]) => args[0] === "tag" || args[0] === "rest",
      ),
    ).toBe(false);
  });

  it("fails closed before deployment resources when the adoption guard cannot be created", async () => {
    const runAzure = vi
      .fn<AzureRunner>()
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: "/subscriptions/sub-1/resourceGroups/existing-rg",
          location: "westus3",
          tags: {
            customer: "preserved",
            [RESOURCE_GROUP_OWNERSHIP_TAG]: "first-run-token",
          },
        }),
      })
      .mockRejectedValueOnce({
        stderr: "ScopeLocked or resource group deletion in progress",
      });

    const adoption = ensureResourceGroup(
      "existing-rg",
      "eastus2",
      "sub-1",
      runAzure,
      { createIfMissing: true, adopterToken: "adopter-token" },
    );
    await expect(adoption).rejects.toBeInstanceOf(
      ResourceGroupOwnershipError,
    );
    await expect(adoption).rejects.toThrow(
      "persistent adoption guard could not be created",
    );
    expect(runAzure).toHaveBeenCalledTimes(2);
    expect(runAzure.mock.calls[1][0].slice(0, 2)).toEqual([
      "lock",
      "create",
    ]);
    expect(
      runAzure.mock.calls.some(
        ([args]) => args[0] === "deployment",
      ),
    ).toBe(false);
  });

  it("creates a normal default group without rollback ownership, tags, or locks", async () => {
    const runAzure = vi
      .fn<AzureRunner>()
      .mockRejectedValueOnce({
        stderr:
          "(ResourceGroupNotFound) Resource group 'new-rg' could not be found.",
      })
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: "/subscriptions/sub-1/resourceGroups/new-rg",
          location: "eastus2",
          tags: {},
        }),
      });

    await expect(
      ensureResourceGroup("new-rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: true,
        ownershipToken: "must-be-ignored",
      }),
    ).resolves.toEqual({
      created: true,
      location: "eastus2",
    });

    expect(runAzure).toHaveBeenCalledTimes(2);
    expect(runAzure.mock.calls[1][0]).toEqual([
      "group",
      "create",
      "--name",
      "new-rg",
      "--location",
      "eastus2",
      "--subscription",
      "sub-1",
      "--query",
      "{id:id,location:location,tags:tags}",
      "--output",
      "json",
    ]);
    expect(runAzure.mock.calls.flatMap(([args]) => args)).not.toContain(
      "If-None-Match=*",
    );
    expect(runAzure.mock.calls.flatMap(([args]) => args)).not.toContain(
      "--tags",
    );
    expect(
      runAzure.mock.calls.some(
        ([args]) => args[0] === "rest" || args[0] === "lock",
      ),
    ).toBe(false);
  });

  it("returns no rollback proof for explicit and cached unmarked groups", async () => {
    for (const createIfMissing of [false, true]) {
      const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
        stdout: JSON.stringify({
          id: "/subscriptions/sub-1/resourceGroups/existing-rg",
          location: "westus3",
          tags: { owner: "customer" },
        }),
      });

      const result = await ensureResourceGroup(
        "existing-rg",
        "eastus2",
        "sub-1",
        runAzure,
        { createIfMissing },
      );

      expect(result).toEqual({ created: false, location: "westus3" });
      expect(result.ownershipProof).toBeUndefined();
      expect(
        runAzure.mock.calls.some(
          ([args]) => args.includes("--tags") || args[0] === "lock",
        ),
      ).toBe(false);
    }
  });

  it("creates a generated rollback group with an ownership marker and unique lease", async () => {
    const runAzure = vi
      .fn<AzureRunner>()
      .mockRejectedValueOnce({
        stderr:
          "(ResourceGroupNotFound) Resource group 'new-rg' could not be found.",
      })
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: "/subscriptions/sub-1/resourceGroups/new-rg",
          name: "new-rg",
          location: "eastus2",
          tags: {
            [RESOURCE_GROUP_OWNERSHIP_TAG]: "atomic-token",
          },
        }),
      })
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id:
            "/subscriptions/sub-1/resourceGroups/new-rg/providers/" +
            "Microsoft.Authorization/locks/kars-up-lease-atomictoken",
          name: "kars-up-lease-atomictoken",
          notes: "atomic-token",
          level: "CanNotDelete",
        }),
      });

    await expect(
      ensureResourceGroup("new-rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: true,
        generatedForRollback: true,
        ownershipToken: "atomic-token",
      }),
    ).resolves.toEqual({
      created: true,
      location: "eastus2",
      ownershipProof: {
        resourceId: "/subscriptions/sub-1/resourceGroups/new-rg",
        token: "atomic-token",
        lockName: "kars-up-lease-atomictoken",
      },
    });

    expect(runAzure).toHaveBeenCalledTimes(3);
    expect(runAzure.mock.calls[1][0]).toEqual([
      "group",
      "create",
      "--name",
      "new-rg",
      "--location",
      "eastus2",
      "--subscription",
      "sub-1",
      "--tags",
      `${RESOURCE_GROUP_OWNERSHIP_TAG}=atomic-token`,
      "--query",
      "{id:id,location:location,tags:tags}",
      "--output",
      "json",
    ]);
    expect(runAzure.mock.calls.flatMap(([args]) => args)).not.toContain(
      "If-None-Match=*",
    );
    expect(
      runAzure.mock.calls.some(([args]) => args[0] === "rest"),
    ).toBe(false);
    expect(runAzure.mock.calls[2][0]).toEqual([
      "lock",
      "create",
      "--name",
      "kars-up-lease-atomictoken",
      "--resource-group",
      "new-rg",
      "--lock-type",
      "CanNotDelete",
      "--notes",
      "atomic-token",
      "--subscription",
      "sub-1",
      "--query",
      "{id:id,name:name,notes:notes,level:level}",
      "--output",
      "json",
    ]);
  });

  it("aborts after fresh creation when its transient rollback lock cannot be created", async () => {
    const runAzure = vi
      .fn<AzureRunner>()
      .mockRejectedValueOnce({
        stderr:
          "(ResourceGroupNotFound) Resource group 'new-rg' could not be found.",
      })
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: "/subscriptions/sub-1/resourceGroups/new-rg",
          location: "eastus2",
          tags: {
            [RESOURCE_GROUP_OWNERSHIP_TAG]: "atomic-token",
          },
        }),
      })
      .mockRejectedValueOnce(new Error("lock authorization denied"));

    await expect(
      ensureResourceGroup("new-rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: true,
        generatedForRollback: true,
        ownershipToken: "atomic-token",
      }),
    ).rejects.toThrow("rollback lease");
    expect(runAzure).toHaveBeenCalledTimes(3);
    expect(
      runAzure.mock.calls.some(([args]) => args[0] === "deployment"),
    ).toBe(false);
  });

  it("rejects an unexpectedly existing generated rollback group without mutation", async () => {
    const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
      stdout: JSON.stringify({
        id: "/subscriptions/sub-1/resourceGroups/existing-rg",
        location: "westus3",
        tags: {
          [RESOURCE_GROUP_OWNERSHIP_TAG]: "another-run",
        },
      }),
    });

    await expect(
      ensureResourceGroup(
        "existing-rg",
        "eastus2",
        "sub-1",
        runAzure,
        {
          createIfMissing: true,
          generatedForRollback: true,
          ownershipToken: "new-run",
        },
      ),
    ).rejects.toThrow(/already exists.*cannot claim rollback ownership/s);

    expect(runAzure).toHaveBeenCalledTimes(1);
    expect(runAzure.mock.calls[0][0].slice(0, 2)).toEqual([
      "group",
      "show",
    ]);
  });

  it("fails closed when generated creation does not return its ownership marker", async () => {
    const runAzure = vi
      .fn<AzureRunner>()
      .mockRejectedValueOnce({
        stderr:
          "(ResourceGroupNotFound) Resource group 'new-rg' could not be found.",
      })
      .mockResolvedValueOnce({
        stdout: JSON.stringify({
          id: "/subscriptions/sub-1/resourceGroups/new-rg",
          location: "eastus2",
          tags: {},
        }),
      });

    await expect(
      ensureResourceGroup("new-rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: true,
        generatedForRollback: true,
        ownershipToken: "unverified-token",
      }),
    ).rejects.toThrow("did not return a verifiable ownership marker");
    expect(runAzure).toHaveBeenCalledTimes(2);
    expect(runAzure.mock.calls[1][0].slice(0, 2)).toEqual([
      "group",
      "create",
    ]);
    expect(
      runAzure.mock.calls.some(([args]) => args[0] === "lock"),
    ).toBe(false);
  });

  it("does not turn an arbitrary group-show failure into a create", async () => {
    const failure = { stderr: "AADSTS50076: interaction required" };
    const runAzure = vi.fn<AzureRunner>().mockRejectedValue(failure);

    await expect(
      ensureResourceGroup("rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: true,
      }),
    ).rejects.toBe(failure);
    expect(runAzure).toHaveBeenCalledTimes(1);
  });

  it("requires an existing group for --skip-infra and never creates a missing one", async () => {
    const runAzure = vi.fn<AzureRunner>().mockRejectedValue({
      stderr:
        "(ResourceGroupNotFound) Resource group 'missing-rg' could not be found.",
    });

    await expect(
      ensureResourceGroup("missing-rg", "eastus2", "sub-1", runAzure, {
        createIfMissing: false,
      }),
    ).rejects.toThrow(
      "--skip-infra only reuses existing infrastructure and will not create a resource group",
    );

    expect(runAzure).toHaveBeenCalledTimes(1);
    expect(runAzure.mock.calls[0][0].slice(0, 2)).toEqual(["group", "show"]);
  });

  it("never rolls back a resource group that this invocation did not create", async () => {
    const cleanup = vi.fn().mockResolvedValue(undefined);

    await expect(
      maybeRollbackResourceGroup({
        ownershipProof: undefined,
        cleanup,
      }),
    ).resolves.toBe("protected-existing");

    expect(cleanup).not.toHaveBeenCalled();
  });

  it("rolls back a generated group when ownership was established", async () => {
    const cleanup = vi.fn().mockResolvedValue(undefined);

    await expect(
      maybeRollbackResourceGroup({
        ownershipProof,
        cleanup,
      }),
    ).resolves.toBe("cleaned");

    expect(cleanup).toHaveBeenCalledOnce();
  });

  it("surfaces lease cleanup guidance without an interactive rollback branch", () => {
    const upSource = readFileSync(new URL("../../up.ts", import.meta.url), "utf8");
    const orchestrationSource = readFileSync(
      new URL("./resource-group.ts", import.meta.url),
      "utf8",
    );

    expect(upSource).toContain(
      "Rollback lease '${resourceGroupOwnership.lockName}'",
    );
    expect(upSource).toContain(
      "kars destroy --all --yes --resource-group ${rg} --subscription ${subscriptionId}",
    );
    expect(upSource).not.toContain("confirmCleanup");
    expect(orchestrationSource).not.toContain("isAtomicCreateConflict");
  });

});
