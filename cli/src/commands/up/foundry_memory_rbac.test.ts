// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

type Execa = typeof import("execa").execa;

// A no-op stepper — the helper only calls update().
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fakeStepper = { update: () => {}, detail: () => {} } as any;

const FOUNDRY_EP = "https://acct.services.ai.azure.com/api/projects/proj";

// Controlled `setupFoundryForKars` result, swapped per test.
let setupResult: unknown = null;

vi.mock("./foundry_setup.js", async (importOriginal) => {
  const orig = await importOriginal<typeof import("./foundry_setup.js")>();
  return {
    ...orig, // keep the real parseFoundryEndpoint
    setupFoundryForKars: vi.fn(async () => setupResult),
  };
});

/** Fake execa for the helper's own `az` calls (group show + role create). */
function makeFakeExeca(opts: {
  rgId?: string;
  roleCreate?: "ok" | "exists" | "denied";
  calls: string[][];
}): Execa {
  const { rgId = "/subscriptions/s/resourceGroups/rg", roleCreate = "ok", calls } = opts;
  return (async (_bin: string, args: readonly string[]) => {
    calls.push([...args]);
    const a = args.join(" ");
    if (a.includes("group show")) return { stdout: rgId };
    if (a.includes("role assignment create")) {
      if (roleCreate === "ok") return { stdout: "" };
      if (roleCreate === "exists") throw new Error("RoleAssignmentExists: already there");
      throw new Error("AuthorizationFailed: caller lacks User Access Administrator");
    }
    return { stdout: "" };
  }) as unknown as Execa;
}

beforeEach(() => {
  setupResult = {
    accountName: "acct",
    accountResourceId: "/subscriptions/s/resourceGroups/rg/providers/Microsoft.CognitiveServices/accounts/acct",
    resourceGroup: "rg",
    projectName: "proj",
    embeddingModel: "text-embedding-3-large",
    projectMiPrincipalId: "mi-principal-123",
    miJustEnabled: false,
    notes: ["Enabled the Foundry project's system-assigned managed identity."],
  };
});

afterEach(() => vi.clearAllMocks());

describe("buildProjectMiRoleAssignmentArgs", () => {
  it("grants the Azure AI User role at the given scope by object id", async () => {
    const { buildProjectMiRoleAssignmentArgs, AZURE_AI_USER_ROLE_ID } = await import("./foundry_memory_rbac.js");
    expect(buildProjectMiRoleAssignmentArgs("pid", "/rgid")).toEqual([
      "role", "assignment", "create",
      "--assignee-object-id", "pid",
      "--assignee-principal-type", "ServicePrincipal",
      "--role", AZURE_AI_USER_ROLE_ID,
      "--scope", "/rgid",
      "--output", "none",
    ]);
  });
});

describe("ensureFoundryMemoryRbac", () => {
  it("skips a non-Foundry endpoint (plain Azure OpenAI)", async () => {
    const { ensureFoundryMemoryRbac } = await import("./foundry_memory_rbac.js");
    const execa = vi.fn() as unknown as Execa;
    const res = await ensureFoundryMemoryRbac({
      execa,
      stepper: fakeStepper,
      foundryEndpoint: "https://my-aoai.openai.azure.com",
    });
    expect(res.granted).toBe(false);
    expect(execa as unknown as ReturnType<typeof vi.fn>).not.toHaveBeenCalled();
    expect(res.notes.join(" ")).toMatch(/not applicable/i);
  });

  it("grants the project MI Azure AI User on the RG (happy path)", async () => {
    const { ensureFoundryMemoryRbac, buildProjectMiRoleAssignmentArgs } = await import("./foundry_memory_rbac.js");
    const calls: string[][] = [];
    const res = await ensureFoundryMemoryRbac({
      execa: makeFakeExeca({ calls }),
      stepper: fakeStepper,
      foundryEndpoint: FOUNDRY_EP,
    });
    expect(res.granted).toBe(true);
    expect(res.projectMiPrincipalId).toBe("mi-principal-123");
    const roleCall = calls.find((c) => c.join(" ").includes("role assignment create"));
    expect(roleCall).toEqual(
      buildProjectMiRoleAssignmentArgs("mi-principal-123", "/subscriptions/s/resourceGroups/rg"),
    );
  });

  it("treats RoleAssignmentExists as success (idempotent)", async () => {
    const { ensureFoundryMemoryRbac } = await import("./foundry_memory_rbac.js");
    const res = await ensureFoundryMemoryRbac({
      execa: makeFakeExeca({ calls: [], roleCreate: "exists" }),
      stepper: fakeStepper,
      foundryEndpoint: FOUNDRY_EP,
    });
    expect(res.granted).toBe(true);
  });

  it("returns granted=false with a remediation note when the caller lacks rights", async () => {
    const { ensureFoundryMemoryRbac } = await import("./foundry_memory_rbac.js");
    const res = await ensureFoundryMemoryRbac({
      execa: makeFakeExeca({ calls: [], roleCreate: "denied" }),
      stepper: fakeStepper,
      foundryEndpoint: FOUNDRY_EP,
    });
    expect(res.granted).toBe(false);
    expect(res.notes.join(" ")).toMatch(/User Access Administrator|grant it once/i);
  });

  it("does not grant when the project MI principalId is unavailable", async () => {
    const { ensureFoundryMemoryRbac } = await import("./foundry_memory_rbac.js");
    setupResult = {
      accountName: "acct", accountResourceId: "x", resourceGroup: "rg", projectName: "proj",
      projectMiPrincipalId: "", miJustEnabled: false, notes: [],
    };
    const calls: string[][] = [];
    const res = await ensureFoundryMemoryRbac({
      execa: makeFakeExeca({ calls }),
      stepper: fakeStepper,
      foundryEndpoint: FOUNDRY_EP,
    });
    expect(res.granted).toBe(false);
    expect(calls.find((c) => c.join(" ").includes("role assignment create"))).toBeUndefined();
  });
});
