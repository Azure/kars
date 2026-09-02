// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import {
  buildBicepParameters,
  buildProjectedBicepParameters,
  cleanupCreatedResourceGroup,
  createAzureRunner,
  createSubscriptionPinnedExeca,
  discoverCleanupNames,
  ensureResourceGroup,
  findRecoverableDeletedKeyVault,
  formatCleanupCompletion,
  formatRetainedResourceGuidance,
  maybeRollbackResourceGroup,
  parsePositiveInteger,
  pinAzureSubscription,
  releaseResourceGroupOwnership,
  RESOURCE_GROUP_ADOPTION_LOCK,
  RESOURCE_GROUP_OWNERSHIP_TAG,
  ResourceGroupOwnershipError,
  resolvePoolNames,
  type AzureRunner,
  type ResourceGroupOwnershipProof,
  validateInfrastructureMode,
} from "./orchestration.js";

vi.mock("./preflight.js", () => ({
  isValidAzureHost: () => true,
}));

const ownershipProof: ResourceGroupOwnershipProof = {
  resourceId: "/subscriptions/sub-1/resourceGroups/fresh-rg",
  token: "test-ownership-token",
  lockName: "kars-up-lease-testownershiptoken",
};

describe("up orchestration", () => {
  it("pins Azure commands exactly once to the preflight-selected subscription", async () => {
    expect(pinAzureSubscription(["aks", "show"], "sub-1")).toEqual([
      "aks",
      "show",
      "--subscription",
      "sub-1",
    ]);
    expect(
      pinAzureSubscription(
        ["group", "show", "--subscription", "sub-1"],
        "sub-1",
      ),
    ).toEqual(["group", "show", "--subscription", "sub-1"]);
    expect(
      pinAzureSubscription(
        ["group", "show", "--subscription=sub-1"],
        "sub-1",
      ),
    ).toEqual(["group", "show", "--subscription=sub-1"]);
    expect(() =>
      pinAzureSubscription(
        ["group", "show", "--subscription", "other-sub"],
        "sub-1",
      ),
    ).toThrow("not the deployment subscription");
    expect(() =>
      pinAzureSubscription(
        [
          "group",
          "show",
          "--subscription",
          "sub-1",
          "--subscription=sub-1",
        ],
        "sub-1",
      ),
    ).toThrow("duplicate --subscription");
  });

  it("automatically pins every command executed by an Azure runner", async () => {
    const execute = vi.fn().mockResolvedValue({ stdout: "ok" });
    const runAzure = createAzureRunner(
      execute as unknown as typeof import("execa").execa,
      "preflight-sub",
    );

    await expect(
      runAzure(["deployment", "group", "create"], { timeout: 1234 }),
    ).resolves.toEqual({ stdout: "ok" });
    expect(execute).toHaveBeenCalledWith(
      "az",
      [
        "deployment",
        "group",
        "create",
        "--subscription",
        "preflight-sub",
      ],
      { stdio: "pipe", timeout: 1234 },
    );
  });

  it("pins Azure calls made by injected helper dependencies", async () => {
    const execute = vi.fn().mockResolvedValue({ stdout: "{}" });
    const scoped = createSubscriptionPinnedExeca(
      execute as unknown as typeof import("execa").execa,
      "preflight-sub",
    );

    await scoped("az", ["rest", "--method", "get", "--url", "/resource"]);
    await scoped("kubectl", ["get", "pods"]);

    expect(execute.mock.calls[0][1]).toEqual([
      "rest",
      "--method",
      "get",
      "--url",
      "/resource",
      "--subscription",
      "preflight-sub",
    ]);
    expect(execute.mock.calls[1][1]).toEqual(["get", "pods"]);
  });

  it("has no direct unscoped Azure CLI calls in downstream up modules", () => {
    const downstream = [
      "../up.js",
      "./images.js",
      "./sandbox_bringup.js",
      "./agentmesh_deploy.js",
    ];
    for (const modulePath of downstream) {
      const sourcePath = new URL(modulePath.replace(/\.js$/, ".ts"), import.meta.url);
      const source = readFileSync(sourcePath, "utf8");
      expect(source, modulePath).not.toMatch(/\bexeca\(\s*["']az["']/);
    }
  });

  it("exposes the deployment-safety flags with early node-count parsing", async () => {
    const { upCommand } = await import("../up.js");
    const command = upCommand();
    const help = command.helpInformation();

    expect(help).toContain("--kubernetes-version <version>");
    expect(help).toContain("--node-count <count>");
    expect(help).toContain("--rollback-on-failure");
    expect(help).toMatch(
      /Use a unique resource group generated for this\s+invocation/,
    );
    expect(help).toMatch(
      /Cannot be combined with\s+an explicit or cached resource group/,
    );
    expect(help).toContain("--kata-vm-size <sku>");
    expect(help).not.toContain("--kata-node-count");
    expect(help).not.toContain("--system-pool-name");
    expect(help).not.toContain("--sandbox-pool-name");
    expect(help).not.toContain("--kata-pool-name");

    const option = command.options.find(
      (candidate) => candidate.long === "--node-count",
    );
    expect(option?.parseArg?.("2", undefined)).toBe(2);
    expect(() => option?.parseArg?.("0", undefined)).toThrow(
      "must be an integer from 1 to 100",
    );
    expect(() => option?.parseArg?.("101", undefined)).toThrow(
      "must be an integer from 1 to 100",
    );
  });

  it("rejects contradictory CLI infrastructure flags before Azure work", async () => {
    const { upCommand } = await import("../up.js");
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });
    try {
      await expect(
        upCommand().parseAsync([
          "node",
          "kars",
          "--skip-infra",
          "--force-infra",
        ]),
      ).rejects.toThrow("process.exit(1)");
      expect(consoleError).toHaveBeenCalledWith(
        expect.stringContaining(
          "--skip-infra and --force-infra cannot be used together",
        ),
      );
    } finally {
      exit.mockRestore();
      consoleError.mockRestore();
    }
  });

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
    const upSource = readFileSync(new URL("../up.ts", import.meta.url), "utf8");
    const orchestrationSource = readFileSync(
      new URL("./orchestration.ts", import.meta.url),
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

  it("forwards resolved Kubernetes version and node count to Bicep", () => {
    expect(
      buildBicepParameters({
        location: "westus3",
        baseName: "safe",
        vmSize: "Standard_D4s_v3",
        systemVmSize: "Standard_D2s_v3",
        kataVmSize: "Standard_D4as_v6",
        kubernetesVersion: "1.34.7",
        systemNodeCount: 3,
        nodeCount: 2,
        kataNodeCount: 2,
        systemPoolName: "systemlegacy",
        sandboxPoolName: "userlegacy",
        kataPoolName: "katalegacy",
      }),
    ).toEqual([
      "location=westus3",
      "baseName=safe",
      "recoverKeyVault=false",
      "vmSize=Standard_D4s_v3",
      "systemVmSize=Standard_D2s_v3",
      "kataVmSize=Standard_D4as_v6",
      "kubernetesVersion=1.34.7",
      "systemNodeCount=3",
      "nodeCount=2",
      "kataNodeCount=2",
      "systemPoolName=systemlegacy",
      "sandboxPoolName=userlegacy",
      "kataPoolName=katalegacy",
    ]);
  });

  it("rejects contradictory infrastructure modes", () => {
    expect(() =>
      validateInfrastructureMode({
        skipInfra: true,
        forceInfra: true,
      }),
    ).toThrow("--skip-infra and --force-infra cannot be used together");

    expect(() =>
      validateInfrastructureMode({
        skipInfra: true,
        forceInfra: false,
      }),
    ).not.toThrow();
    expect(() =>
      validateInfrastructureMode({
        skipInfra: false,
        forceInfra: true,
      }),
    ).not.toThrow();
  });

  it("preserves resolved pool names and defaults only absent names", () => {
    expect(
      resolvePoolNames({
        systemPoolName: "systemlegacy",
        sandboxPoolName: "userlegacy",
        kataPoolName: "katalegacy",
      }),
    ).toEqual({
      systemPoolName: "systemlegacy",
      sandboxPoolName: "userlegacy",
      kataPoolName: "katalegacy",
    });
    expect(resolvePoolNames({})).toEqual({
      systemPoolName: "system",
      sandboxPoolName: "clawpool",
      kataPoolName: "katapool",
    });
  });

  it("builds deployment parameters from preflight-projected SKUs and pool names", () => {
    expect(
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        nodeVmSize: "Standard_Restricted_User_SKU",
        systemVmSize: "Standard_Restricted_System_SKU",
        kataVmSize: "Standard_Restricted_Kata_SKU",
        kubernetesVersion: "1.35.3",
        systemNodeCount: 4,
        nodeCount: 1,
        kataNodeCount: 2,
        systemPoolName: "legacysys",
        sandboxPoolName: "legacyuser",
        kataPoolName: "legacykata",
      }),
    ).toEqual([
      "location=eastus2",
      "baseName=safe",
      "recoverKeyVault=false",
      "vmSize=Standard_Restricted_User_SKU",
      "systemVmSize=Standard_Restricted_System_SKU",
      "kataVmSize=Standard_Restricted_Kata_SKU",
      "kubernetesVersion=1.35.3",
      "systemNodeCount=4",
      "nodeCount=1",
      "kataNodeCount=2",
      "systemPoolName=legacysys",
      "sandboxPoolName=legacyuser",
      "kataPoolName=legacykata",
    ]);
    expect(() =>
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        kubernetesVersion: "1.35.3",
        nodeCount: 1,
        kataNodeCount: 1,
      }),
    ).toThrow("Preflight did not resolve sandbox, system, and Kata VM sizes");
    expect(() =>
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        nodeVmSize: "Standard_D4s_v3",
        systemVmSize: "Standard_D2s_v3",
        kataVmSize: "Standard_D4as_v6",
        kubernetesVersion: "1.35.3",
        systemNodeCount: 2,
        nodeCount: 1,
      }),
    ).toThrow(
      "Preflight did not resolve a non-negative Kata node count",
    );
  });

  it("forwards the Key Vault recovery decision through projected Bicep parameters", () => {
    expect(
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        recoverKeyVault: true,
        nodeVmSize: "Standard_D4s_v3",
        systemVmSize: "Standard_D2s_v3",
        kataVmSize: "Standard_D4as_v6",
        kubernetesVersion: "1.35.3",
        systemNodeCount: 2,
        nodeCount: 1,
        kataNodeCount: 0,
      }),
    ).toContain("recoverKeyVault=true");
  });

  it("declares disabled-by-default recovery and forwards it to the Key Vault module", () => {
    const source = readFileSync(
      new URL("../../../../deploy/bicep/main.bicep", import.meta.url),
      "utf8",
    );
    expect(source).toContain("param recoverKeyVault bool = false");
    expect(source).toContain("recover: recoverKeyVault");
  });

  describe("soft-deleted Key Vault recovery discovery", () => {
    const context = {
      resourceGroup: "fresh-rg",
      baseName: "safe",
      location: "westus3",
      subscriptionId: "sub-1",
    };
    const deletedVault = (
      name: string,
      resourceGroup = "fresh-rg",
      location = "westus3",
    ) => ({
      name,
      properties: {
        location,
        vaultId:
          `/subscriptions/sub-1/resourceGroups/${resourceGroup}` +
          `/providers/Microsoft.KeyVault/vaults/${name}`,
        deletionDate: "2026-09-01T12:00:00Z",
        scheduledPurgeDate: "2026-10-01T12:00:00Z",
      },
    });

    it("returns no recovery state when the deleted-vault inventory is empty", async () => {
      const runAzure = vi
        .fn<AzureRunner>()
        .mockResolvedValue({ stdout: "[]" });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).resolves.toBeUndefined();
      expect(runAzure).toHaveBeenCalledWith([
        "keyvault",
        "list-deleted",
        "--subscription",
        "sub-1",
        "--output",
        "json",
      ]);
    });

    it("returns the exact Azure-assigned name and soft-deleted state", async () => {
      const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
        stdout: JSON.stringify([deletedVault("safe-kv-a1b2c3")]),
      });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).resolves.toEqual({
        name: "safe-kv-a1b2c3",
        location: "westus3",
        vaultId:
          "/subscriptions/sub-1/resourceGroups/fresh-rg/providers/Microsoft.KeyVault/vaults/safe-kv-a1b2c3",
        state: "soft-deleted",
        deletionDate: "2026-09-01T12:00:00Z",
        scheduledPurgeDate: "2026-10-01T12:00:00Z",
      });
    });

    it("ignores unrelated names, other resource groups, and other locations", async () => {
      const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
        stdout: JSON.stringify([
          deletedVault("unrelated-kv-a1b2c3"),
          deletedVault("safe-kv-a1b2c3", "other-rg"),
          deletedVault("safe-kv-d4e5f6", "fresh-rg", "eastus2"),
        ]),
      });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).resolves.toBeUndefined();
    });

    it("fails closed when more than one deleted vault matches", async () => {
      const runAzure = vi.fn<AzureRunner>().mockResolvedValue({
        stdout: JSON.stringify([
          deletedVault("safe-kv-a1b2c3"),
          deletedVault("safe-kv-d4e5f6"),
        ]),
      });

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).rejects.toThrow(
        /multiple soft-deleted vaults.*safe-kv-a1b2c3, safe-kv-d4e5f6/,
      );
    });

    it("fails closed when Azure cannot list deleted vaults", async () => {
      const listError = new Error("not authorized to list deleted vaults");
      const runAzure = vi.fn<AzureRunner>().mockRejectedValue(listError);

      await expect(
        findRecoverableDeletedKeyVault(context, runAzure),
      ).rejects.toBe(listError);
    });
  });

  it("keeps preflight authoritative without legacy AKS or VM-size re-detection", () => {
    const source = readFileSync(new URL("../up.ts", import.meta.url), "utf8");
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
    const orchestrationSource = readFileSync(
      new URL("./orchestration.ts", import.meta.url),
      "utf8",
    );
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
