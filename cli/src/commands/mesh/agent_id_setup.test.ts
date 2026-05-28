// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("execa", () => ({
  execa: vi.fn(),
}));

import { execa } from "execa";

import {
  ensureAgentIdTrust,
  karsAuthConfigExists,
  checkAgentIdRole,
  detectExistingBlueprint,
} from "./agent_id_setup.js";

type Execa = typeof execa;
const mockedExeca = vi.mocked(execa) as unknown as ReturnType<typeof vi.fn>;

function ok(stdout: string): { stdout: string; stderr: string; exitCode: number } {
  return { stdout, stderr: "", exitCode: 0 };
}

beforeEach(() => {
  mockedExeca.mockReset();
});

describe("karsAuthConfigExists", () => {
  it("returns true when kubectl returns the short-form CR reference", async () => {
    mockedExeca.mockResolvedValueOnce(ok("karsauthconfig/default") as any);
    await expect(karsAuthConfigExists()).resolves.toBe(true);
    expect(mockedExeca).toHaveBeenCalledWith(
      "kubectl",
      ["get", "karsauthconfig", "default", "-o", "name"],
      { stdio: "pipe" },
    );
  });

  it("returns true when kubectl returns the fully-qualified CR reference", async () => {
    // Newer clusters return `karsauthconfig.kars.azure.com/default`.
    mockedExeca.mockResolvedValueOnce(
      ok("karsauthconfig.kars.azure.com/default") as any,
    );
    await expect(karsAuthConfigExists()).resolves.toBe(true);
  });

  it("returns false when kubectl errors (NotFound / no CRD / no cluster)", async () => {
    mockedExeca.mockRejectedValueOnce(new Error("NotFound"));
    await expect(karsAuthConfigExists()).resolves.toBe(false);
  });

  it("returns false when kubectl succeeds with empty output", async () => {
    mockedExeca.mockResolvedValueOnce(ok("") as any);
    await expect(karsAuthConfigExists()).resolves.toBe(false);
  });
});

describe("ensureAgentIdTrust dry-run", () => {
  it("returns placeholder IDs and makes no Graph/ARM calls", async () => {
    // az account show
    mockedExeca.mockResolvedValueOnce(
      ok(
        JSON.stringify({
          id: "sub-123",
          tenantId: "tenant-abc",
          user: { name: "user@example.com" },
        }),
      ) as any,
    );

    const result = await ensureAgentIdTrust({
      clusterName: "demo",
      dryRun: true,
    });

    expect(result.tenantId).toBe("tenant-abc");
    expect(result.freshlyCreated).toBe(false);
    expect(result.blueprintClientId).toBe("<dry-run>");
    // Only one call — `az account show`. No further side effects.
    expect(mockedExeca).toHaveBeenCalledTimes(1);
  });

  it("threads through KARS_SERVICE_TREE env var", async () => {
    const prev = process.env.KARS_SERVICE_TREE;
    process.env.KARS_SERVICE_TREE = "00000000-0000-0000-0000-000000000001";
    try {
      mockedExeca.mockResolvedValueOnce(
        ok(
          JSON.stringify({
            id: "sub-123",
            tenantId: "tenant-abc",
            user: { name: "user@example.com" },
          }),
        ) as any,
      );
      // The dry-run branch logs the service tree GUID and returns
      // without mutating anything. We don't assert on stdout here —
      // any side effects (mock calls) would be the smoking gun.
      const result = await ensureAgentIdTrust({ dryRun: true });
      expect(result.freshlyCreated).toBe(false);
    } finally {
      if (prev === undefined) {
        delete process.env.KARS_SERVICE_TREE;
      } else {
        process.env.KARS_SERVICE_TREE = prev;
      }
    }
  });

  it("propagates az login errors with a clear message", async () => {
    mockedExeca.mockRejectedValueOnce(new Error("Please run az login"));
    await expect(ensureAgentIdTrust({})).rejects.toThrow(
      /Azure CLI is not signed in/,
    );
  });

  it("dry-run defaults credentialMode to ManagedIdentityImds when caller omits it", async () => {
    mockedExeca.mockResolvedValueOnce(
      ok(
        JSON.stringify({
          id: "sub-123",
          tenantId: "tenant-abc",
          user: { name: "user@example.com" },
        }),
      ) as any,
    );
    const result = await ensureAgentIdTrust({ dryRun: true });
    // The dry-run path returns the requested mode (or default) verbatim
    // so the caller's expectations are clear before any side effects.
    expect(result.credentialMode).toBe("ManagedIdentityImds");
  });

  it("dry-run preserves explicit credentialMode=WorkloadIdentity", async () => {
    mockedExeca.mockResolvedValueOnce(
      ok(
        JSON.stringify({
          id: "sub-123",
          tenantId: "tenant-abc",
          user: { name: "user@example.com" },
        }),
      ) as any,
    );
    const result = await ensureAgentIdTrust({
      dryRun: true,
      credentialMode: "WorkloadIdentity",
    });
    expect(result.credentialMode).toBe("WorkloadIdentity");
  });
});

describe("checkAgentIdRole", () => {
  it("detects Agent ID Developer by display name", async () => {
    // az rest /me/transitiveMemberOf — returns one Agent ID Developer
    mockedExeca.mockResolvedValueOnce(
      ok(
        JSON.stringify({
          value: [
            { id: "x1", displayName: "Agent ID Developer", roleTemplateId: "8424c6f0-a189-499e-bbd0-26c1753c96d4" },
          ],
        }),
      ) as any,
    );
    const r = await checkAgentIdRole();
    expect(r.hasRole).toBe(true);
    expect(r.inconclusive).toBe(false);
    expect(r.detectedRoles).toHaveLength(1);
    expect(r.detectedRoles[0].displayName).toBe("Agent ID Developer");
  });

  it("detects Global Administrator by template id even with custom display name", async () => {
    mockedExeca.mockResolvedValueOnce(
      ok(
        JSON.stringify({
          value: [
            { id: "x1", displayName: "Custom Label", roleTemplateId: "62e90394-69f5-4237-9190-012177145e10" },
          ],
        }),
      ) as any,
    );
    const r = await checkAgentIdRole();
    expect(r.hasRole).toBe(true);
    expect(r.detectedRoles[0].id).toBe("x1");
  });

  it("returns hasRole=false when no matching role is found", async () => {
    mockedExeca.mockResolvedValueOnce(
      ok(
        JSON.stringify({
          value: [
            { id: "x1", displayName: "Reader", roleTemplateId: "acdd72a7-3385-48ef-bd42-f606fba81ae7" },
          ],
        }),
      ) as any,
    );
    const r = await checkAgentIdRole();
    expect(r.hasRole).toBe(false);
    expect(r.inconclusive).toBe(false);
    expect(r.message).toContain("Agent ID Developer");
  });

  it("returns inconclusive on Graph errors so preflight only warns", async () => {
    mockedExeca.mockRejectedValueOnce(new Error("Forbidden: missing User.Read"));
    const r = await checkAgentIdRole();
    expect(r.hasRole).toBe(false);
    expect(r.inconclusive).toBe(true);
    expect(r.message).toContain("Could not enumerate");
  });

  it("emits the az-login workaround when AADSTS530084 (CA block) is detected", async () => {
    // First call (graph /me/transitiveMemberOf) returns AADSTS530084
    mockedExeca.mockRejectedValueOnce(
      new Error(
        "ERROR: AADSTS530084: Access has been blocked by conditional access token protection policy configured by this organization.",
      ),
    );
    // Second call (device-code re-login attempt) also fails — exercises
    // the fallback path. We don't want to actually prompt the user in
    // tests, so the rejection here keeps things hermetic.
    mockedExeca.mockRejectedValueOnce(new Error("device-code login cancelled"));
    // Third call (retry of /me/transitiveMemberOf) — if the device-code
    // succeeded we'd retry. Mock it to return AADSTS530084 again so the
    // final message preserves the original error code for the user.
    mockedExeca.mockRejectedValueOnce(
      new Error(
        "ERROR: AADSTS530084: Access has been blocked by conditional access token protection policy configured by this organization.",
      ),
    );
    const r = await checkAgentIdRole();
    expect(r.hasRole).toBe(false);
    expect(r.inconclusive).toBe(true);
    expect(r.message).toContain("AADSTS530084");
    expect(r.message).toContain("az login --scope https://graph.microsoft.com//.default");
  });

  it("emits the az-login workaround when AADSTS65001/65002 (missing consent) is detected", async () => {
    mockedExeca.mockRejectedValueOnce(
      new Error("ERROR: AADSTS65001: The user or administrator has not consented..."),
    );
    const r = await checkAgentIdRole();
    expect(r.hasRole).toBe(false);
    expect(r.inconclusive).toBe(true);
    expect(r.message).toContain("az login --scope https://graph.microsoft.com//.default");
  });
});

describe("detectExistingBlueprint", () => {
  it("returns present=false when no blueprint matches", async () => {
    mockedExeca.mockResolvedValueOnce(ok(JSON.stringify({ value: [] })) as any);
    const r = await detectExistingBlueprint("kars-blueprint");
    expect(r.present).toBe(false);
    expect(r.appId).toBeUndefined();
    expect(r.message).toContain("will be created");
  });

  it("returns present=true with appId when blueprint exists", async () => {
    mockedExeca.mockResolvedValueOnce(
      ok(
        JSON.stringify({
          value: [
            { id: "obj-1", appId: "app-1", displayName: "kars-blueprint" },
          ],
        }),
      ) as any,
    );
    const r = await detectExistingBlueprint("kars-blueprint");
    expect(r.present).toBe(true);
    expect(r.appId).toBe("app-1");
  });
});
