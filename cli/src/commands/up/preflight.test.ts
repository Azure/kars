// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
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

    expect(firstName).toMatch(/^kars-westus3-[0-9a-f]{12}$/);
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
    ["new infrastructure", false],
    ["dry run", true],
  ])(
    "rejects an overlong custom region on the %s path before deployment checks",
    async (_path, dryRun) => {
      const region = "customregion".repeat(6);
      const input = {
        ...options(false),
        dryRun,
        region,
        resourceGroup: undefined,
      };
      const detectAks = vi.fn().mockResolvedValue({ exists: false });
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
        if (dryRun) {
          expect(detectAks).not.toHaveBeenCalled();
        } else {
          expect(detectAks).toHaveBeenCalledOnce();
        }
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
