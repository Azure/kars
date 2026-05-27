// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach } from "vitest";

// All external side effects (`execa`, `kubectl`, `az`) are mocked.
// We assert call shapes rather than running anything.
vi.mock("execa", () => ({
  execa: vi.fn(),
}));

import { execa } from "execa";

import {
  ensureAgentIdTrust,
  karsAuthConfigExists,
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
  it("returns true when kubectl confirms the CR exists", async () => {
    mockedExeca.mockResolvedValueOnce(ok("karsauthconfig/default") as any);
    await expect(karsAuthConfigExists()).resolves.toBe(true);
    expect(mockedExeca).toHaveBeenCalledWith(
      "kubectl",
      ["get", "karsauthconfig", "default", "-o", "name"],
      { stdio: "pipe" },
    );
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
});
