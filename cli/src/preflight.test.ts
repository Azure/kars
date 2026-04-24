import { describe, it, expect } from "vitest";
import { matchAction, hasEffectiveAction } from "./preflight.js";

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
