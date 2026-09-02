// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ExistingAksCluster } from "./deployment-safety.js";
import { validateAutomaticAksNodeResourceGroupName } from "./deployment-safety.js";
import {
  AZURE_REGION_CHOICES,
  resolveResourceGroupForInvocation,
  runPreflight,
  validateRollbackResourceGroupSelection,
  type UpOptionsForPreflight,
} from "./preflight.js";

const azureMock = vi.hoisted(() => ({
  calls: [] as string[][],
  subscriptions: [
    { id: "sub-1", name: "Test subscription", isDefault: true },
  ],
  selectedSubscriptionId: "sub-1",
}));
const contextMock = vi.hoisted(() => ({
  value: null as null | {
    region?: string;
    resourceGroup?: string;
    phase?: "complete";
  },
}));

vi.mock("../../config.js", () => ({
  loadContext: () => contextMock.value,
}));

vi.mock("execa", () => ({
  execa: vi.fn(async (_command: string, args: string[]) => {
    azureMock.calls.push(args);
    if (
      args[0] === "account" &&
      args[1] === "show" &&
      args.includes("json")
    ) {
      return {
        stdout: JSON.stringify({
          id: "sub-1",
          name: "Test subscription",
        }),
      };
    }
    if (args[0] === "account" && args[1] === "list") {
      return {
        stdout: JSON.stringify(azureMock.subscriptions),
      };
    }
    return { stdout: "" };
  }),
}));

vi.mock("inquirer", () => ({
  default: {
    Separator: class Separator {},
    prompt: vi.fn(async (questions: Array<{ name: string }>) => {
      if (questions[0]?.name === "subId") {
        return { subId: azureMock.selectedSubscriptionId };
      }
      throw new Error(`Unexpected prompt: ${questions[0]?.name ?? "<none>"}`);
    }),
  },
}));

function options(skipInfra: boolean): UpOptionsForPreflight {
  return {
    name: "agent",
    model: "gpt-5",
    region: "westus3",
    isolation: "standard",
    resourceGroup: "kars-westus3",
    build: false,
    release: true,
    sourceAcr: "ghcr.io/azure/kars",
    dryRun: false,
    skipInfra,
    forceInfra: false,
    skipPreflight: false,
    clusterName: "kars",
    fromScratch: true,
    yes: true,
  };
}

const stoppedCluster: ExistingAksCluster = {
  exists: true,
  id: "/subscriptions/sub-1/resourceGroups/kars-westus3/providers/Microsoft.ContainerService/managedClusters/kars-aks",
  provisioningState: "Succeeded",
  powerState: { code: "Stopped" },
  kubernetesVersion: "1.35.6",
  supportPlan: "KubernetesOfficial",
  sku: { name: "Base", tier: "Free" },
  autoUpgradeProfile: {
    upgradeChannel: "stable",
    nodeOSUpgradeChannel: "SecurityPatch",
  },
  agentPoolProfiles: [],
};

const healthyCluster: ExistingAksCluster = {
  ...stoppedCluster,
  powerState: { code: "Running" },
  agentPoolProfiles: [
    {
      name: "system",
      count: 2,
      vmSize: "Standard_D2s_v3",
      mode: "System",
      provisioningState: "Succeeded",
      nodeLabels: {},
      nodeTaints: [],
      logicalRole: "system",
    },
    {
      name: "clawpool",
      count: 3,
      vmSize: "Standard_D4s_v3",
      mode: "User",
      provisioningState: "Succeeded",
      nodeLabels: { "kars.azure.com/pool": "sandbox" },
      nodeTaints: [],
      logicalRole: "sandbox",
    },
  ],
};

beforeEach(() => {
  contextMock.value = null;
  azureMock.calls.length = 0;
});

describe("runPreflight rollback resource-group selection", () => {
  it("keeps the historical regional default for normal deployments", () => {
    const input = {
      region: "westus3",
      rollbackOnFailure: false,
      resourceGroup: undefined,
      resourceGroupGeneratedForRollback: undefined,
    };

    expect(resolveResourceGroupForInvocation(input)).toBe("kars-westus3");
    expect(input.resourceGroupGeneratedForRollback).toBe(false);
  });

  it("generates two unique 12-character lowercase hexadecimal suffixes", () => {
    const first = {
      region: "westus3",
      rollbackOnFailure: true,
      resourceGroup: undefined,
      resourceGroupGeneratedForRollback: undefined,
    };
    const second = {
      region: "westus3",
      rollbackOnFailure: true,
      resourceGroup: undefined,
      resourceGroupGeneratedForRollback: undefined,
    };

    const firstName = resolveResourceGroupForInvocation(first);
    const secondName = resolveResourceGroupForInvocation(second);

    expect(firstName).toMatch(
      /^kars-westus3-[0-9a-f]{12}$/,
    );
    expect(secondName).not.toBe(firstName);
    expect(first.resourceGroup).toBe(firstName);
    expect(first.resourceGroupGeneratedForRollback).toBe(true);
  });

  it("uses the injected cryptographic byte source", () => {
    const input = {
      region: "westus3",
      rollbackOnFailure: true,
      resourceGroup: undefined,
      resourceGroupGeneratedForRollback: undefined,
    };
    const bytes = vi.fn(() =>
      Buffer.from([0x00, 0x10, 0xab, 0xcd, 0xef, 0xff]),
    );

    expect(resolveResourceGroupForInvocation(input, bytes)).toBe(
      "kars-westus3-0010abcdefff",
    );
    expect(bytes).toHaveBeenCalledOnce();
    expect(bytes).toHaveBeenCalledWith(6);
  });

  it("keeps all picker regions within the AKS automatic node-group limit", () => {
    expect(AZURE_REGION_CHOICES.map(({ value }) => value)).toEqual(
      expect.arrayContaining([
        "southeastasia",
        "australiaeast",
        "germanywestcentral",
      ]),
    );

    for (const { value: region } of AZURE_REGION_CHOICES) {
      const input = {
        region,
        rollbackOnFailure: true,
        resourceGroup: undefined,
        resourceGroupGeneratedForRollback: undefined,
      };
      const resourceGroup = resolveResourceGroupForInvocation(
        input,
        () => Buffer.alloc(6, 0xab),
      );

      expect(() =>
        validateAutomaticAksNodeResourceGroupName(
          resourceGroup,
          "kars-aks",
          region,
        ),
      ).not.toThrow();
    }
  });

  it("rejects --skip-infra with rollback before any Azure command", async () => {
    const input = {
      ...options(true),
      resourceGroup: undefined,
      rollbackOnFailure: true,
    };
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    const consoleLog = vi
      .spyOn(console, "log")
      .mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    try {
      await expect(runPreflight(input)).rejects.toThrow("process.exit(1)");
      expect(azureMock.calls).toEqual([]);
      expect(consoleError).toHaveBeenCalledWith(
        expect.stringMatching(
          /--skip-infra cannot be combined with --rollback-on-failure.*new.*resource group/s,
        ),
      );
    } finally {
      exit.mockRestore();
      consoleError.mockRestore();
      consoleLog.mockRestore();
    }
  });

  it("warns that from-scratch rollback retains a complete cached deployment", async () => {
    contextMock.value = {
      region: "centralus",
      resourceGroup: "retained-rg",
      phase: "complete",
    };
    const input = {
      ...options(false),
      dryRun: true,
      resourceGroup: undefined,
      rollbackOnFailure: true,
    };
    const consoleLog = vi
      .spyOn(console, "log")
      .mockImplementation(() => undefined);
    const consoleWarn = vi
      .spyOn(console, "warn")
      .mockImplementation(() => undefined);

    try {
      await expect(
        runPreflight(input, {
          randomBytes: () => Buffer.from("010203040506", "hex"),
        }),
      ).resolves.toBeNull();
      expect(input.resourceGroup).toBe("kars-westus3-010203040506");
      expect(consoleWarn).toHaveBeenCalledWith(
        expect.stringMatching(
          /creates a second deployment.*complete cached deployment in 'retained-rg' will remain unchanged/s,
        ),
      );
    } finally {
      consoleWarn.mockRestore();
      consoleLog.mockRestore();
    }
  });

  it.each([
    ["new infrastructure", false, false],
    ["explicit --skip-infra", true, false],
    ["dry run", false, true],
  ])(
    "rejects an overlong custom region on the %s path before deployment discovery",
    async (_path, skipInfra, dryRun) => {
      const region = "customregion".repeat(6);
      const input = {
        ...options(skipInfra),
        dryRun,
        region,
        resourceGroup: undefined,
      };
      const detectAks = vi.fn();
      const runChecks = vi.fn();
      const resolveSafety = vi.fn();
      const consoleError = vi
        .spyOn(console, "error")
        .mockImplementation(() => undefined);
      const consoleLog = vi
        .spyOn(console, "log")
        .mockImplementation(() => undefined);
      const exit = vi
        .spyOn(process, "exit")
        .mockImplementation((code): never => {
          throw new Error(`process.exit(${String(code)})`);
        });

      try {
        await expect(
          runPreflight(input, {
            detectExistingAksCluster: detectAks,
            runPreflightChecks: runChecks,
            resolveAzureDeploymentSafety: resolveSafety,
          }),
        ).rejects.toThrow("process.exit(1)");
        expect(detectAks).not.toHaveBeenCalled();
        expect(runChecks).not.toHaveBeenCalled();
        expect(resolveSafety).not.toHaveBeenCalled();
        expect(consoleError).toHaveBeenCalledWith(
          expect.stringMatching(
            /AKS automatic node resource group.*at most 80.*shorter --region/is,
          ),
        );
      } finally {
        exit.mockRestore();
        consoleError.mockRestore();
        consoleLog.mockRestore();
      }
    },
  );

  it("rejects direct rollback ownership of an explicit resource group", () => {
    expect(() =>
      validateRollbackResourceGroupSelection({
        resourceGroup: "customer-rg",
        rollbackOnFailure: true,
      }),
    ).toThrow(/explicit or cached resource group.*Omit --rollback-on-failure/s);
  });

  it("rejects an explicit resource group before any Azure command", async () => {
    const input = {
      ...options(false),
      resourceGroup: "customer-rg",
      rollbackOnFailure: true,
    };
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    const consoleLog = vi
      .spyOn(console, "log")
      .mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    try {
      await expect(runPreflight(input)).rejects.toThrow("process.exit(1)");
      expect(azureMock.calls).toEqual([]);
      expect(input.resourceGroupGeneratedForRollback).not.toBe(true);
      expect(consoleError).toHaveBeenCalledWith(
        expect.stringMatching(/Omit --rollback-on-failure.*clean up.*manually/s),
      );
    } finally {
      exit.mockRestore();
      consoleError.mockRestore();
      consoleLog.mockRestore();
    }
  });

  it("rejects a cached resource group before any Azure command", async () => {
    contextMock.value = {
      region: "centralus",
      resourceGroup: "cached-rg",
    };
    const input = {
      ...options(false),
      region: "eastus2",
      resourceGroup: undefined,
      rollbackOnFailure: true,
      fromScratch: false,
    };
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    const consoleLog = vi
      .spyOn(console, "log")
      .mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    try {
      await expect(runPreflight(input)).rejects.toThrow("process.exit(1)");
      expect(azureMock.calls).toEqual([]);
      expect(input.resourceGroupGeneratedForRollback).not.toBe(true);
      expect(consoleError).toHaveBeenCalledWith(
        expect.stringMatching(
          /explicit or cached resource group.*clean up that group manually/s,
        ),
      );
    } finally {
      exit.mockRestore();
      consoleError.mockRestore();
      consoleLog.mockRestore();
    }
  });
});

describe("runPreflight stopped-cluster guard", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    azureMock.calls.length = 0;
    azureMock.subscriptions = [
      { id: "sub-1", name: "Test subscription", isDefault: true },
    ];
    azureMock.selectedSubscriptionId = "sub-1";
  });

  it.each([
    ["automatic reuse", false],
    ["explicit --skip-infra", true],
  ])(
    "exits before checks or safety resolution on the %s path",
    async (_path, skipInfra) => {
      const input = options(skipInfra);
      const detectInfrastructure = vi.fn();
      const runChecks = vi.fn();
      const resolveSafety = vi.fn();
      const consoleLog = vi
        .spyOn(console, "log")
        .mockImplementation(() => undefined);
      const consoleError = vi
        .spyOn(console, "error")
        .mockImplementation(() => undefined);
      const exit = vi
        .spyOn(process, "exit")
        .mockImplementation((code): never => {
          throw new Error(`process.exit(${String(code)})`);
        });

      await expect(
        runPreflight(input, {
          detectExistingAksCluster: vi.fn().mockResolvedValue(stoppedCluster),
          detectInfrastructureCompleteness: detectInfrastructure,
          runPreflightChecks: runChecks,
          resolveAzureDeploymentSafety: resolveSafety,
        }),
      ).rejects.toThrow("process.exit(1)");

      expect(detectInfrastructure).not.toHaveBeenCalled();
      expect(runChecks).not.toHaveBeenCalled();
      expect(resolveSafety).not.toHaveBeenCalled();
      expect(input.forceInfra).toBe(false);
      expect(consoleLog).toHaveBeenCalledWith(
        expect.stringContaining(
          "az aks start --resource-group kars-westus3 --name kars-aks",
        ),
      );

      exit.mockRestore();
      consoleError.mockRestore();
      consoleLog.mockRestore();
    },
  );
});

describe("runPreflight retained-infrastructure completeness", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    azureMock.calls.length = 0;
    azureMock.subscriptions = [
      { id: "sub-1", name: "Test subscription", isDefault: true },
    ];
    azureMock.selectedSubscriptionId = "sub-1";
  });

  it("automatically reuses a healthy cluster only when deployment outputs are complete", async () => {
    const input = options(false);
    input.foundryEndpoint =
      "https://shared.services.ai.azure.com/api/projects/project";
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    const detectInfrastructure = vi.fn().mockResolvedValue({
      complete: true,
      diagnostic: "complete",
    });
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    vi.spyOn(console, "error").mockImplementation(() => undefined);

    await expect(
      runPreflight(input, {
        detectExistingAksCluster: vi.fn().mockResolvedValue(healthyCluster),
        detectInfrastructureCompleteness: detectInfrastructure,
        runPreflightChecks: runChecks,
        resolveAzureDeploymentSafety: resolveSafety,
      }),
    ).resolves.toEqual({
      rg: "kars-westus3",
      subscriptionId: "sub-1",
    });

    expect(input.skipInfra).toBe(true);
    expect(input.forceInfra).toBe(false);
    expect(detectInfrastructure).toHaveBeenCalledWith("kars-westus3", {
      foundryEndpoint:
        "https://shared.services.ai.azure.com/api/projects/project",
      openAiEndpoint: undefined,
      subscriptionId: "sub-1",
    });
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
  });

  it("rejects a healthy cluster with incomplete ancillary resources before safety or Bicep resolution", async () => {
    const input = options(false);
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    const output: string[] = [];
    vi.spyOn(console, "log").mockImplementation((message) => {
      output.push(String(message));
    });
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    await expect(
      runPreflight(input, {
        detectExistingAksCluster: vi.fn().mockResolvedValue(healthyCluster),
        detectInfrastructureCompleteness: vi.fn().mockResolvedValue({
          complete: false,
          diagnostic: "Resource-group deployment 'main' is Failed.",
        }),
        runPreflightChecks: runChecks,
        resolveAzureDeploymentSafety: resolveSafety,
      }),
    ).rejects.toThrow("process.exit(1)");

    expect(input.skipInfra).toBe(false);
    expect(input.forceInfra).toBe(false);
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
    expect(output.join("\n")).toMatch(
      /full managedClusters Bicep template.*Repair or recreate the missing ancillary resources/s,
    );
    expect(output.join("\n")).toContain("Do not use --force-infra");
    exit.mockRestore();
  });

  it("rejects --force-infra for an existing healthy cluster before infrastructure discovery or safety resolution", async () => {
    const input = { ...options(false), forceInfra: true };
    const detectInfrastructure = vi.fn();
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    const output: string[] = [];
    vi.spyOn(console, "log").mockImplementation((message) => {
      output.push(String(message));
    });
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    await expect(
      runPreflight(input, {
        detectExistingAksCluster: vi.fn().mockResolvedValue(healthyCluster),
        detectInfrastructureCompleteness: detectInfrastructure,
        runPreflightChecks: runChecks,
        resolveAzureDeploymentSafety: resolveSafety,
      }),
    ).rejects.toThrow("process.exit(1)");

    expect(input.forceInfra).toBe(true);
    expect(input.skipInfra).toBe(false);
    expect(detectInfrastructure).not.toHaveBeenCalled();
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
    expect(output.join("\n")).toMatch(
      /--force-infra cannot be used.*already exists.*autoscaling.*availability zones/s,
    );
    expect(output.join("\n")).toContain(
      "--force-infra is valid only when the AKS cluster does not exist",
    );
    exit.mockRestore();
  });

  it("rejects an explicit differing node count before safety or Bicep resolution", async () => {
    const originalArgv = process.argv;
    process.argv = [...originalArgv, "--node-count", "5"];
    const input = { ...options(false), nodeCount: 5 };
    const detectInfrastructure = vi.fn();
    const runChecks = vi.fn().mockResolvedValue({ ok: true });
    const resolveSafety = vi.fn();
    const output: string[] = [];
    vi.spyOn(console, "error").mockImplementation((message) => {
      output.push(String(message));
    });
    const consoleLog = vi
      .spyOn(console, "log")
      .mockImplementation((message) => {
        output.push(String(message));
      });
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    try {
      await expect(
        runPreflight(input, {
          detectExistingAksCluster: vi.fn().mockResolvedValue(healthyCluster),
          detectInfrastructureCompleteness: detectInfrastructure,
          runPreflightChecks: runChecks,
          resolveAzureDeploymentSafety: resolveSafety,
        }),
      ).rejects.toThrow("process.exit(1)");
    } finally {
      process.argv = originalArgv;
      exit.mockRestore();
    }

    expect(input.skipInfra).toBe(false);
    expect(input.forceInfra).toBe(false);
    expect(detectInfrastructure).not.toHaveBeenCalled();
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
    expect(output.join("\n")).toContain(
      "az aks nodepool scale --resource-group kars-westus3 --cluster-name kars-aks --name clawpool --node-count 5",
    );
    expect(output.join("\n")).toContain("rerun kars up without --node-count");
    consoleLog.mockRestore();
  });

  it("rejects a differing node count with explicit --skip-infra", async () => {
    const originalArgv = process.argv;
    process.argv = [...originalArgv, "--node-count", "5"];
    const input = { ...options(true), nodeCount: 5 };
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const consoleLog = vi
      .spyOn(console, "log")
      .mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    try {
      await expect(
        runPreflight(input, {
          detectExistingAksCluster: vi.fn().mockResolvedValue(healthyCluster),
          detectInfrastructureCompleteness: vi.fn(),
          runPreflightChecks: runChecks,
          resolveAzureDeploymentSafety: resolveSafety,
        }),
      ).rejects.toThrow("process.exit(1)");
    } finally {
      process.argv = originalArgv;
      exit.mockRestore();
    }

    expect(input.forceInfra).toBe(false);
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
    expect(consoleLog).toHaveBeenCalledWith(
      expect.stringContaining(
        "az aks nodepool scale --resource-group kars-westus3 --cluster-name kars-aks --name clawpool --node-count 5",
      ),
    );
  });

  it.each([
    [
      "a missing sandbox pool",
      {
        ...healthyCluster,
        agentPoolProfiles: [healthyCluster.agentPoolProfiles[0]],
      },
      "standard",
      "az aks nodepool add --resource-group kars-westus3 --cluster-name kars-aks --name clawpool",
    ],
    [
      "a Failed sandbox pool",
      {
        ...healthyCluster,
        agentPoolProfiles: healthyCluster.agentPoolProfiles.map((pool) =>
          pool.logicalRole === "sandbox"
            ? { ...pool, provisioningState: "Failed" }
            : pool,
        ),
      },
      "standard",
      "az aks nodepool show --resource-group kars-westus3 --cluster-name kars-aks --name clawpool",
    ],
    [
      "a Creating sandbox pool",
      {
        ...healthyCluster,
        agentPoolProfiles: healthyCluster.agentPoolProfiles.map((pool) =>
          pool.logicalRole === "sandbox"
            ? { ...pool, provisioningState: "Creating" }
            : pool,
        ),
      },
      "standard",
      "az aks nodepool show --resource-group kars-westus3 --cluster-name kars-aks --name clawpool",
    ],
    [
      "a missing Kata pool during confidential transition",
      healthyCluster,
      "confidential",
      "az aks nodepool add --resource-group kars-westus3 --cluster-name kars-aks --name katapool",
    ],
    [
      "a Failed cluster",
      { ...healthyCluster, provisioningState: "Failed" },
      "standard",
      "az aks show --resource-group kars-westus3 --name kars-aks",
    ],
  ])(
    "rejects %s before retained-resource discovery, safety, or Bicep resolution",
    async (_label, cluster, isolation, guidance) => {
      const input = { ...options(false), isolation };
      const detectInfrastructure = vi.fn();
      const runChecks = vi.fn();
      const resolveSafety = vi.fn();
      const output: string[] = [];
      vi.spyOn(console, "error").mockImplementation(() => undefined);
      vi.spyOn(console, "log").mockImplementation((message) => {
        output.push(String(message));
      });
      const exit = vi
        .spyOn(process, "exit")
        .mockImplementation((code): never => {
          throw new Error(`process.exit(${String(code)})`);
        });

      await expect(
        runPreflight(input, {
          detectExistingAksCluster: vi.fn().mockResolvedValue(cluster),
          detectInfrastructureCompleteness: detectInfrastructure,
          runPreflightChecks: runChecks,
          resolveAzureDeploymentSafety: resolveSafety,
        }),
      ).rejects.toThrow("process.exit(1)");

      expect(input.forceInfra).toBe(false);
      expect(input.skipInfra).toBe(false);
      expect(detectInfrastructure).not.toHaveBeenCalled();
      expect(runChecks).not.toHaveBeenCalled();
      expect(resolveSafety).not.toHaveBeenCalled();
      expect(output.join("\n")).toContain(guidance);
      expect(output.join("\n")).toContain("then rerun kars up");
      exit.mockRestore();
    },
  );

  it("rejects explicit --skip-infra when retained deployment outputs are incomplete", async () => {
    const input = options(true);
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const consoleLog = vi
      .spyOn(console, "log")
      .mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    await expect(
      runPreflight(input, {
        detectExistingAksCluster: vi.fn().mockResolvedValue(healthyCluster),
        detectInfrastructureCompleteness: vi.fn().mockResolvedValue({
          complete: false,
          diagnostic: "Resource-group deployment 'main' is missing keyVaultName.",
        }),
        runPreflightChecks: runChecks,
        resolveAzureDeploymentSafety: resolveSafety,
      }),
    ).rejects.toThrow("process.exit(1)");

    expect(input.forceInfra).toBe(false);
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
    expect(consoleLog).toHaveBeenCalledWith(
      expect.stringContaining("Repair or recreate the missing ancillary resources"),
    );

    exit.mockRestore();
  });
});

describe("runPreflight immutable subscription and topology safety", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    azureMock.calls.length = 0;
    azureMock.subscriptions = [
      { id: "sub-1", name: "Test subscription", isDefault: true },
    ];
    azureMock.selectedSubscriptionId = "sub-1";
  });

  it("captures the interactive subscription and passes it to every later Azure phase", async () => {
    const originalArgv = process.argv;
    const originalIsTTY = Object.getOwnPropertyDescriptor(process.stdin, "isTTY");
    process.argv = [
      ...originalArgv,
      "--region",
      "westus3",
      "--name",
      "agent",
      "--isolation",
      "standard",
    ];
    Object.defineProperty(process.stdin, "isTTY", {
      configurable: true,
      value: true,
    });
    azureMock.subscriptions = [
      { id: "sub-1", name: "Test subscription", isDefault: true },
      { id: "sub-2", name: "Selected subscription", isDefault: false },
    ];
    azureMock.selectedSubscriptionId = "sub-2";
    const input = { ...options(false), yes: false };
    const detectAks = vi.fn().mockResolvedValue(healthyCluster);
    const detectInfrastructure = vi.fn().mockResolvedValue({
      complete: true,
      diagnostic: "complete",
    });
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    vi.spyOn(console, "error").mockImplementation(() => undefined);

    try {
      await expect(
        runPreflight(input, {
          detectExistingAksCluster: detectAks,
          detectInfrastructureCompleteness: detectInfrastructure,
          runPreflightChecks: runChecks,
          resolveAzureDeploymentSafety: resolveSafety,
        }),
      ).resolves.toEqual({
        rg: "kars-westus3",
        subscriptionId: "sub-2",
      });
    } finally {
      process.argv = originalArgv;
      if (originalIsTTY) {
        Object.defineProperty(process.stdin, "isTTY", originalIsTTY);
      } else {
        delete (process.stdin as unknown as { isTTY?: boolean }).isTTY;
      }
    }

    expect(detectAks).toHaveBeenCalledWith(
      "kars-westus3",
      "kars-aks",
      "sub-2",
    );
    expect(detectInfrastructure).toHaveBeenCalledWith("kars-westus3", {
      foundryEndpoint: undefined,
      openAiEndpoint: undefined,
      subscriptionId: "sub-2",
    });
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
    expect(
      azureMock.calls.some(
        (args) => args[0] === "account" && args[1] === "set",
      ),
    ).toBe(false);
  });

  it.each([
    ["multiple healthy system pools", (() => {
      const cluster = structuredClone(healthyCluster);
      cluster.agentPoolProfiles.push({
        ...cluster.agentPoolProfiles[0],
        name: "system2",
      });
      return cluster;
    })()],
    ["a healthy LTS cluster", {
      ...healthyCluster,
      supportPlan: "AKSLongTermSupport",
      sku: { name: "Base", tier: "Premium" },
    }],
  ])("preserves %s through complete reuse without Bicep", async (_label, cluster) => {
    const input = options(false);
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    vi.spyOn(console, "error").mockImplementation(() => undefined);

    await expect(
      runPreflight(input, {
        detectExistingAksCluster: vi.fn().mockResolvedValue(cluster),
        detectInfrastructureCompleteness: vi.fn().mockResolvedValue({
          complete: true,
          diagnostic: "complete",
        }),
        runPreflightChecks: runChecks,
        resolveAzureDeploymentSafety: resolveSafety,
      }),
    ).resolves.toEqual({
      rg: "kars-westus3",
      subscriptionId: "sub-1",
    });

    expect(input.skipInfra).toBe(true);
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
  });

  it.each([
    ["an unhealthy LTS cluster before infrastructure discovery", {
      cluster: {
        ...healthyCluster,
        provisioningState: "Failed",
        supportPlan: "AKSLongTermSupport",
        sku: { name: "Base", tier: "Premium" },
      },
      forceInfra: false,
      incomplete: false,
    }, /cluster provisioningState=Failed.*az aks show/s],
  ])("rejects %s before any deployment discovery", async (_label, scenario, diagnostic) => {
    const input = { ...options(false), forceInfra: scenario.forceInfra };
    const detectInfrastructure = vi.fn().mockResolvedValue({
      complete: !scenario.incomplete,
      diagnostic: scenario.incomplete ? "incomplete" : "complete",
    });
    const runChecks = vi.fn();
    const resolveSafety = vi.fn();
    const output: string[] = [];
    vi.spyOn(console, "log").mockImplementation((message) => {
      output.push(String(message));
    });
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });

    await expect(
      runPreflight(input, {
        detectExistingAksCluster: vi.fn().mockResolvedValue(scenario.cluster),
        detectInfrastructureCompleteness: detectInfrastructure,
        runPreflightChecks: runChecks,
        resolveAzureDeploymentSafety: resolveSafety,
      }),
    ).rejects.toThrow("process.exit(1)");

    expect(output.join("\n")).toMatch(diagnostic);
    expect(runChecks).not.toHaveBeenCalled();
    expect(resolveSafety).not.toHaveBeenCalled();
    exit.mockRestore();
  });
});
