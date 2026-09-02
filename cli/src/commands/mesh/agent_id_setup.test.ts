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
import { ensureAgentIdTrustViaBicep } from "./agent_id_setup_bicep.js";

const mockedExeca = vi.mocked(execa) as unknown as ReturnType<typeof vi.fn>;

function ok(stdout: string): { stdout: string; stderr: string; exitCode: number } {
  return { stdout, stderr: "", exitCode: 0 };
}

beforeEach(() => {
  mockedExeca.mockReset();
});

function installExistingTrustMock(
  tenantsBySubscription: Record<string, string> = {},
): void {
  mockedExeca.mockImplementation(async (file: string, args: string[]) => {
    if (file === "kubectl") return ok("");
    if (args[0] === "account") {
      const subscriptionIndex = args.indexOf("--subscription");
      const subscriptionId =
        subscriptionIndex >= 0 ? args[subscriptionIndex + 1] : "ambient-sub";
      return ok(
        JSON.stringify({
          id: subscriptionId,
          tenantId:
            tenantsBySubscription[subscriptionId] ?? "tenant-abc",
          user: { name: "user@example.com" },
        }),
      );
    }
    if (args[0] === "group") return ok(JSON.stringify({ name: "demo-rg" }));
    if (args[0] === "identity") {
      return ok(
        JSON.stringify({
          id: "/subscriptions/target-sub/resourceGroups/demo-rg/providers/Microsoft.ManagedIdentity/userAssignedIdentities/demo-controller-mi",
          clientId: "mi-client",
          principalId: "mi-principal",
          name: "demo-controller-mi",
          location: "eastus",
        }),
      );
    }
    if (args[0] === "aks") {
      return ok(
        JSON.stringify({
          name: "demo-aks",
          resourceGroup: "demo-aks-rg",
          oidcIssuerProfile: {
            enabled: true,
            issuerUrl: "https://oidc.example/",
          },
        }),
      );
    }
    if (args[0] === "rest") {
      const url = args[args.indexOf("--url") + 1];
      if (url.includes("/applications?$filter=")) {
        return ok(
          JSON.stringify({
            value: [
              {
                id: "blueprint-object",
                appId: "blueprint-client",
                displayName: "kars-blueprint",
              },
            ],
          }),
        );
      }
      if (url.includes("/servicePrincipals?$filter=")) {
        return ok(
          JSON.stringify({
            value: [
              {
                id: "blueprint-sp",
                appId: "blueprint-client",
                displayName: "kars-blueprint",
              },
            ],
          }),
        );
      }
      if (url.includes("/federatedIdentityCredentials")) {
        return ok(
          JSON.stringify({
            value: [
              {
                id: "fic",
                name: "existing",
                subject: "mi-principal",
                issuer: "https://oidc.example/",
              },
            ],
          }),
        );
      }
    }
    throw new Error(`Unexpected command: ${file} ${args.join(" ")}`);
  });
}

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

  it("does NOT attempt device-code re-login on AADSTS530084 when interactive=false (preflight)", async () => {
    // Single Graph call returns the CA block. With interactive=false the
    // helper must NOT shell out to `az login --use-device-code` (which would
    // hang `up` on tenants whose CA also blocks the device-code device).
    mockedExeca.mockRejectedValueOnce(
      new Error(
        "ERROR: AADSTS530084: Access has been blocked by conditional access token protection policy configured by this organization.",
      ),
    );
    const r = await checkAgentIdRole({ interactive: false });
    expect(r.hasRole).toBe(false);
    expect(r.inconclusive).toBe(true);
    expect(r.message).toContain("AADSTS530084");
    // Exactly one az call — the Graph GET. No device-code re-login.
    expect(mockedExeca).toHaveBeenCalledTimes(1);
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

describe("ensureAgentIdTrust subscription scoping", () => {
  it("adds the supplied subscription exactly once to every ARM command", async () => {
    installExistingTrustMock();

    await ensureAgentIdTrust({
      clusterName: "demo",
      resourceGroup: "demo-rg",
      subscriptionId: "target-sub",
      credentialMode: "ManagedIdentityImds",
    });

    expect(mockedExeca).toHaveBeenCalledWith(
      "az",
      [
        "account",
        "show",
        "--subscription",
        "target-sub",
        "-o",
        "json",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    expect(mockedExeca).toHaveBeenCalledWith(
      "az",
      [
        "group",
        "show",
        "--name",
        "demo-rg",
        "--subscription",
        "target-sub",
        "-o",
        "json",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    expect(mockedExeca).toHaveBeenCalledWith(
      "az",
      [
        "identity",
        "show",
        "--resource-group",
        "demo-rg",
        "--name",
        "demo-controller-mi",
        "--subscription",
        "target-sub",
        "-o",
        "json",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );

    const armCalls = mockedExeca.mock.calls.filter(
      ([file, args]) =>
        file === "az" &&
        ["group", "identity", "aks"].includes((args as string[])[0]),
    );
    expect(armCalls).toHaveLength(2);
    for (const [, args] of armCalls) {
      const argv = args as string[];
      expect(argv.filter((arg) => arg === "--subscription")).toHaveLength(1);
      expect(argv[argv.indexOf("--subscription") + 1]).toBe("target-sub");
    }

    const graphCalls = mockedExeca.mock.calls.filter(
      ([file, args]) =>
        file === "az" && (args as string[])[0] === "rest",
    );
    expect(graphCalls.length).toBeGreaterThan(0);
    for (const [, args] of graphCalls) {
      const argv = args as string[];
      expect(argv.filter((arg) => arg === "--subscription")).toHaveLength(1);
      expect(argv[argv.indexOf("--subscription") + 1]).toBe("target-sub");
    }
  });

  it("scopes explicit AKS discovery to the supplied subscription", async () => {
    installExistingTrustMock();

    await ensureAgentIdTrust({
      subscriptionId: "aks-sub",
      credentialMode: "WorkloadIdentity",
      aksClusterName: "demo-aks",
      aksClusterResourceGroup: "demo-aks-rg",
    });

    expect(mockedExeca).toHaveBeenCalledWith(
      "az",
      [
        "aks",
        "show",
        "--name",
        "demo-aks",
        "--resource-group",
        "demo-aks-rg",
        "--subscription",
        "aks-sub",
        "-o",
        "json",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
  });

  it("keeps concurrent invocations isolated by subscription", async () => {
    installExistingTrustMock({
      "sub-alpha": "tenant-alpha",
      "sub-beta": "tenant-beta",
    });

    await Promise.all([
      ensureAgentIdTrust({
        clusterName: "alpha",
        resourceGroup: "alpha-rg",
        subscriptionId: "sub-alpha",
        credentialMode: "ManagedIdentityImds",
      }),
      ensureAgentIdTrust({
        clusterName: "beta",
        resourceGroup: "beta-rg",
        subscriptionId: "sub-beta",
        credentialMode: "ManagedIdentityImds",
      }),
    ]);

    const armCalls = mockedExeca.mock.calls.filter(
      ([file, args]) =>
        file === "az" &&
        ["group", "identity"].includes((args as string[])[0]),
    );
    expect(armCalls).toHaveLength(4);
    for (const [, args] of armCalls) {
      const argv = args as string[];
      const rgFlag = argv.includes("--resource-group")
        ? "--resource-group"
        : "--name";
      const rg = argv[argv.indexOf(rgFlag) + 1];
      const expectedSubscription = rg.startsWith("alpha")
        ? "sub-alpha"
        : "sub-beta";
      expect(argv.filter((arg) => arg === "--subscription")).toHaveLength(1);
      expect(argv[argv.indexOf("--subscription") + 1]).toBe(
        expectedSubscription,
      );
    }

    const accountCalls = mockedExeca.mock.calls.filter(
      ([file, args]) =>
        file === "az" && (args as string[])[0] === "account",
    );
    expect(accountCalls.map(([, args]) => {
      const argv = args as string[];
      return argv[argv.indexOf("--subscription") + 1];
    }).sort()).toEqual(["sub-alpha", "sub-beta"]);

    const graphCalls = mockedExeca.mock.calls.filter(
      ([file, args]) =>
        file === "az" && (args as string[])[0] === "rest",
    );
    expect(graphCalls).toHaveLength(6);
    const graphSubscriptions: string[] = [];
    for (const [, args] of graphCalls) {
      const argv = args as string[];
      const selectedSubscription =
        argv[argv.indexOf("--subscription") + 1];
      expect(["sub-alpha", "sub-beta"]).toContain(selectedSubscription);
      expect(argv.filter((arg) => arg === "--subscription")).toHaveLength(1);
      graphSubscriptions.push(selectedSubscription);
    }
    expect(graphSubscriptions.sort()).toEqual([
      "sub-alpha",
      "sub-alpha",
      "sub-alpha",
      "sub-beta",
      "sub-beta",
      "sub-beta",
    ]);

    const appliedConfigs = mockedExeca.mock.calls
      .filter(([file]) => file === "kubectl")
      .map(([, , options]) =>
        JSON.parse((options as { input: string }).input),
      );
    expect(
      appliedConfigs
        .map((config) => config.spec.tenant.tenantId)
        .sort(),
    ).toEqual(["tenant-alpha", "tenant-beta"]);
  });

  it("uses the selected subscription tenant for Graph and MI trust", async () => {
    mockedExeca.mockImplementation(async (file: string, args: string[], options?: unknown) => {
      if (file === "kubectl") return ok("");
      if (args[0] === "account") {
        expect(args).toEqual([
          "account",
          "show",
          "--subscription",
          "selected-sub",
          "-o",
          "json",
        ]);
        return ok(
          JSON.stringify({
            id: "selected-sub",
            tenantId: "tenant-b",
            user: { name: "user-in-b@example.com" },
          }),
        );
      }
      if (args[0] === "group") {
        return ok(JSON.stringify({ name: "demo-rg" }));
      }
      if (args[0] === "identity") {
        return ok(
          JSON.stringify({
            id: "/subscriptions/selected-sub/resourceGroups/demo-rg/providers/Microsoft.ManagedIdentity/userAssignedIdentities/demo-controller-mi",
            clientId: "mi-client-b",
            principalId: "mi-principal-b",
            name: "demo-controller-mi",
            location: "eastus",
          }),
        );
      }
      if (args[0] === "rest") {
        expect(args.filter((arg) => arg === "--subscription")).toHaveLength(1);
        expect(args[args.indexOf("--subscription") + 1]).toBe("selected-sub");
        const url = args[args.indexOf("--url") + 1];
        if (url.includes("/applications?$filter=")) {
          return ok(
            JSON.stringify({
              value: [
                {
                  id: "blueprint-object-b",
                  appId: "blueprint-client-b",
                  displayName: "kars-blueprint",
                },
              ],
            }),
          );
        }
        if (url.includes("/servicePrincipals?$filter=")) {
          return ok(
            JSON.stringify({
              value: [
                {
                  id: "blueprint-sp-b",
                  appId: "blueprint-client-b",
                  displayName: "kars-blueprint",
                },
              ],
            }),
          );
        }
        if (
          url.includes("/federatedIdentityCredentials") &&
          args[args.indexOf("--method") + 1] === "GET"
        ) {
          return ok(JSON.stringify({ value: [] }));
        }
        if (
          url.includes("/federatedIdentityCredentials") &&
          args[args.indexOf("--method") + 1] === "POST"
        ) {
          const body = JSON.parse(args[args.indexOf("--body") + 1]);
          expect(body.issuer).toBe(
            "https://login.microsoftonline.com/tenant-b/v2.0",
          );
          expect(body.subject).toBe("mi-principal-b");
          return ok(JSON.stringify({ id: "fic-b" }));
        }
      }
      throw new Error(
        `Unexpected command: ${file} ${args.join(" ")} ${JSON.stringify(options)}`,
      );
    });

    const result = await ensureAgentIdTrust({
      clusterName: "demo",
      resourceGroup: "demo-rg",
      subscriptionId: "selected-sub",
      credentialMode: "ManagedIdentityImds",
    });

    expect(result.tenantId).toBe("tenant-b");
    expect(result.controllerMiResourceId).toContain(
      "/subscriptions/selected-sub/",
    );
    const armCalls = mockedExeca.mock.calls.filter(
      ([file, args]) =>
        file === "az" &&
        ["group", "identity"].includes((args as string[])[0]),
    );
    expect(armCalls).toHaveLength(2);
    for (const [, args] of armCalls) {
      const argv = args as string[];
      expect(argv.filter((arg) => arg === "--subscription")).toHaveLength(1);
      expect(argv[argv.indexOf("--subscription") + 1]).toBe("selected-sub");
    }
  });

  it("preserves ambient standalone setup when no subscription is supplied", async () => {
    installExistingTrustMock({ "ambient-sub": "ambient-tenant" });

    const result = await ensureAgentIdTrust({
      clusterName: "demo",
      resourceGroup: "demo-rg",
      credentialMode: "ManagedIdentityImds",
    });

    expect(result.tenantId).toBe("ambient-tenant");
    expect(mockedExeca).toHaveBeenCalledWith(
      "az",
      ["account", "show", "-o", "json"],
      { stdio: ["ignore", "pipe", "pipe"] },
    );
    const graphCalls = mockedExeca.mock.calls.filter(
      ([file, args]) =>
        file === "az" && (args as string[])[0] === "rest",
    );
    expect(graphCalls.length).toBeGreaterThan(0);
    for (const [, args] of graphCalls) {
      const argv = args as string[];
      expect(argv[argv.indexOf("--subscription") + 1]).toBe("ambient-sub");
    }
  });
});

describe("ensureAgentIdTrustViaBicep subscription scoping", () => {
  it("targets the supplied subscription exactly once", async () => {
    mockedExeca.mockImplementation(async (file: string, args: string[]) => {
      if (file === "kubectl") return ok("");
      if (args[0] === "account") {
        const subscriptionIndex = args.indexOf("--subscription");
        expect(args[subscriptionIndex + 1]).toBe("target-sub");
        return ok(
          JSON.stringify({
            id: "target-sub",
            tenantId: "tenant-b",
            user: { name: "user@example.com" },
          }),
        );
      }
      if (args[0] === "deployment") {
        return ok(
          JSON.stringify({
            properties: {
              provisioningState: "Succeeded",
              outputs: {
                tenantId: { type: "String", value: "tenant-b" },
                blueprintClientId: {
                  type: "String",
                  value: "blueprint-client",
                },
                blueprintObjectId: {
                  type: "String",
                  value: "blueprint-object",
                },
                blueprintSpObjectId: {
                  type: "String",
                  value: "blueprint-sp",
                },
                credentialMode: {
                  type: "String",
                  value: "ManagedIdentityImds",
                },
                controllerMiClientId: {
                  type: "String",
                  value: "mi-client",
                },
                controllerMiResourceId: {
                  type: "String",
                  value: "mi-resource",
                },
                controllerMiPrincipalId: {
                  type: "String",
                  value: "mi-principal",
                },
              },
            },
          }),
        );
      }
      if (args[0] === "rest") {
        expect(args.filter((arg) => arg === "--subscription")).toHaveLength(1);
        expect(args[args.indexOf("--subscription") + 1]).toBe("target-sub");
        return ok("");
      }
      throw new Error(`Unexpected command: ${file} ${args.join(" ")}`);
    });

    await ensureAgentIdTrustViaBicep({
      clusterName: "demo",
      resourceGroup: "demo-rg",
      region: "westus2",
      subscriptionId: "target-sub",
    });

    expect(mockedExeca).toHaveBeenCalledWith(
      "az",
      [
        "account",
        "show",
        "--subscription",
        "target-sub",
        "-o",
        "json",
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );

    const deploymentCall = mockedExeca.mock.calls.find(
      ([file, args]) =>
        file === "az" && (args as string[])[0] === "deployment",
    );
    expect(deploymentCall).toBeDefined();
    const argv = deploymentCall?.[1] as string[];
    const deploymentName = argv[argv.indexOf("--name") + 1];
    const templatePath = argv[argv.indexOf("--template-file") + 1];
    expect(argv).toEqual([
      "deployment",
      "sub",
      "create",
      "--name",
      deploymentName,
      "--location",
      "westus2",
      "--template-file",
      templatePath,
      "--parameters",
      "clusterName=demo",
      "--parameters",
      "resourceGroupName=demo-rg",
      "--parameters",
      "region=westus2",
      "--parameters",
      "credentialMode=ManagedIdentityImds",
      "--subscription",
      "target-sub",
      "-o",
      "json",
    ]);
    expect(argv.filter((arg) => arg === "--subscription")).toHaveLength(1);
  });
});
