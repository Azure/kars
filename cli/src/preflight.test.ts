// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, it, expect, vi } from "vitest";
import {
  matchAction,
  hasEffectiveAction,
  requiredActionsFor,
  runPreflightChecks,
} from "./preflight.js";

const azureState = vi.hoisted(() => ({
  calls: [] as string[][],
  permissions: [{ actions: ["*"], notActions: [] }] as Array<{
    actions: string[];
    notActions: string[];
  }>,
}));

vi.mock("execa", () => ({
  execa: vi.fn(async (_command: string, args: string[]) => {
    azureState.calls.push(args);
    if (args[0] === "account") {
      return {
        stdout: JSON.stringify({
          id: "ambient-subscription",
          tenantId: "tenant",
          user: { name: "operator@example.com" },
        }),
      };
    }
    if (args[0] === "rest") {
      return {
        stdout: JSON.stringify({
          value: azureState.permissions,
        }),
      };
    }
    return { stdout: "Registered" };
  }),
}));

afterEach(() => {
  azureState.calls.length = 0;
  azureState.permissions = [{ actions: ["*"], notActions: [] }];
  vi.restoreAllMocks();
});

describe("matchAction", () => {
  it("matches exact action", () => {
    expect(matchAction("Microsoft.ContainerService/managedClusters/write",
                       "Microsoft.ContainerService/managedClusters/write")).toBe(true);
  });

  it("matches wildcard across segments", () => {
    expect(matchAction("Microsoft.ContainerService/*",
                       "Microsoft.ContainerService/managedClusters/write")).toBe(true);
  });

  it("matches '*' catch-all", () => {
    expect(matchAction("*", "Microsoft.KeyVault/vaults/write")).toBe(true);
  });

  it("is case-insensitive (Azure action matching)", () => {
    expect(matchAction("microsoft.containerservice/managedClusters/WRITE",
                       "Microsoft.ContainerService/managedClusters/write")).toBe(true);
  });

  it("rejects different resource provider", () => {
    expect(matchAction("Microsoft.ContainerRegistry/*",
                       "Microsoft.ContainerService/managedClusters/write")).toBe(false);
  });

  it("escapes regex metacharacters in the pattern", () => {
    // A literal '.' in a pattern must match only '.', not any char.
    expect(matchAction("Microsoft.KeyVault/vaults/write",
                       "Microsoft-KeyVault/vaults/write")).toBe(false);
  });
});

describe("hasEffectiveAction", () => {
  it("grants when a permission set allows the action", () => {
    expect(hasEffectiveAction(
      [{ actions: ["Microsoft.ContainerService/*"], notActions: [] }],
      "Microsoft.ContainerService/managedClusters/write"
    )).toBe(true);
  });

  it("denies when notActions covers the action", () => {
    expect(hasEffectiveAction(
      [{ actions: ["*"], notActions: ["Microsoft.Authorization/*/write"] }],
      "Microsoft.Authorization/roleAssignments/write"
    )).toBe(false);
  });

  it("grants if ANY permission set allows (multiple roles merge)", () => {
    expect(hasEffectiveAction(
      [
        { actions: ["Microsoft.ContainerService/*"], notActions: [] },
        { actions: ["Microsoft.Authorization/roleAssignments/*"], notActions: [] },
      ],
      "Microsoft.Authorization/roleAssignments/write"
    )).toBe(true);
  });

  it("Contributor-shaped role (star + notActions) denies roleAssignments/write", () => {
    // This mirrors the real Contributor built-in role shape, which is the
    // classic pitfall: Contributor CANNOT grant/revoke role assignments.
    const contributorShape = [{
      actions: ["*"],
      notActions: [
        "Microsoft.Authorization/*/Delete",
        "Microsoft.Authorization/*/Write",
        "Microsoft.Authorization/elevateAccess/Action",
      ],
    }];
    expect(hasEffectiveAction(contributorShape,
      "Microsoft.Authorization/roleAssignments/write")).toBe(false);
    // But it still grants cluster creation
    expect(hasEffectiveAction(contributorShape,
      "Microsoft.ContainerService/managedClusters/write")).toBe(true);
  });

  it("denies when no permission set grants the action", () => {
    expect(hasEffectiveAction(
      [{ actions: ["Microsoft.ContainerRegistry/*"], notActions: [] }],
      "Microsoft.KeyVault/vaults/write"
    )).toBe(false);
  });
});

describe("requiredActionsFor", () => {
  it("requires deployment lock permissions only for rollback-on-failure", () => {
    const normalActions = requiredActionsFor({
      region: "westus3",
      resourceGroup: "kars-westus3",
      isolation: "standard",
    }).map((required) => required.action);
    const rollbackActions = requiredActionsFor({
      region: "westus3",
      resourceGroup: "kars-westus3",
      isolation: "standard",
      rollbackOnFailure: true,
    }).map((required) => required.action);

    expect(normalActions).not.toEqual(expect.arrayContaining([
      "Microsoft.Resources/subscriptions/resourceGroups/delete",
      "Microsoft.Authorization/locks/read",
      "Microsoft.Authorization/locks/write",
      "Microsoft.Authorization/locks/delete",
    ]));
    expect(rollbackActions).toEqual(expect.arrayContaining([
      "Microsoft.Resources/subscriptions/resourceGroups/delete",
      "Microsoft.Authorization/locks/read",
      "Microsoft.Authorization/locks/write",
      "Microsoft.Authorization/locks/delete",
    ]));
  });
});

describe("runPreflightChecks subscription scope", () => {
  it("pins every Azure CLI command to the selected subscription", async () => {
    vi.spyOn(console, "log").mockImplementation(() => undefined);

    await expect(
      runPreflightChecks({
        region: "westus3",
        resourceGroup: "kars-westus3",
        isolation: "standard",
        foundryEndpoint:
          "https://shared.services.ai.azure.com/api/projects/project",
        subscriptionId: "selected-subscription",
      }),
    ).resolves.toMatchObject({
      ok: true,
      subscription: "selected-subscription",
    });

    expect(azureState.calls.length).toBeGreaterThan(3);
    for (const args of azureState.calls) {
      expect(args).toEqual(
        expect.arrayContaining([
          "--subscription",
          "selected-subscription",
        ]),
      );
    }
    expect(azureState.calls).toContainEqual([
      "account",
      "show",
      "-o",
      "json",
      "--subscription",
      "selected-subscription",
    ]);
    expect(azureState.calls).toContainEqual([
      "rest",
      "--method",
      "GET",
      "--url",
      "/subscriptions/selected-subscription/providers/Microsoft.Authorization/permissions?api-version=2022-04-01",
      "--subscription",
      "selected-subscription",
    ]);
  });

  it("passes a normal custom role without lock permissions", async () => {
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    const options = {
      region: "westus3",
      resourceGroup: "kars-westus3",
      isolation: "standard",
      foundryEndpoint:
        "https://shared.services.ai.azure.com/api/projects/project",
    } as const;
    azureState.permissions = [{
      actions: requiredActionsFor(options)
        .map((required) => required.action)
        .filter((action) => !action.startsWith("Microsoft.Authorization/locks/")),
      notActions: [],
    }];

    await expect(runPreflightChecks(options)).resolves.toMatchObject({
      ok: true,
      blocking: [],
    });
  });

  it("fails a rollback run when the custom role lacks lock permissions", async () => {
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    const options = {
      region: "westus3",
      resourceGroup: "kars-westus3",
      isolation: "standard",
      foundryEndpoint:
        "https://shared.services.ai.azure.com/api/projects/project",
      rollbackOnFailure: true,
    } as const;
    azureState.permissions = [{
      actions: requiredActionsFor(options)
        .map((required) => required.action)
        .filter((action) => !action.startsWith("Microsoft.Authorization/locks/")),
      notActions: [],
    }];

    await expect(runPreflightChecks(options)).resolves.toMatchObject({
      ok: false,
      blocking: [expect.stringContaining("Grant the current user")],
    });
  });

  it("passes a rollback run when the custom role grants lock permissions", async () => {
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    const options = {
      region: "westus3",
      resourceGroup: "kars-westus3",
      isolation: "standard",
      foundryEndpoint:
        "https://shared.services.ai.azure.com/api/projects/project",
      rollbackOnFailure: true,
    } as const;
    azureState.permissions = [{
      actions: requiredActionsFor(options).map((required) => required.action),
      notActions: [],
    }];

    await expect(runPreflightChecks(options)).resolves.toMatchObject({
      ok: true,
      blocking: [],
    });
  });
});
