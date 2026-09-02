// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { destroyCommand, selectKarsResourceGroupLocks } from "./destroy.js";

const execa = vi.fn();
const configMocks = vi.hoisted(() => ({ loadContext: vi.fn() }));

vi.mock("execa", () => ({ execa }));
vi.mock("../config.js", () => ({ loadContext: configMocks.loadContext }));

vi.mock("ora", () => ({
  default: vi.fn(() => ({
    start() {
      return this;
    },
    succeed: vi.fn(),
    fail: vi.fn(),
    stop: vi.fn(),
    text: "",
  })),
}));

const rg = "kars-westus3";
const lockId = (name: string, resourceGroup = rg) =>
  `/subscriptions/sub-1/resourceGroups/${resourceGroup}/providers/Microsoft.Authorization/locks/${name}`;

describe("selectKarsResourceGroupLocks", () => {
  it("selects only Kars lock names at resource-group scope in deterministic removal order", () => {
    expect(selectKarsResourceGroupLocks([
      { id: lockId("customer-do-not-delete"), name: "customer-do-not-delete" },
      { id: lockId("kars-up-adopted"), name: "kars-up-adopted" },
      { id: lockId("kars-up-lease-z"), name: "kars-up-lease-z" },
      { id: lockId("kars-up-lease-a"), name: "kars-up-lease-a" },
      {
        id: `${lockId("kars-up-lease-resource")}/nested`,
        name: "kars-up-lease-resource",
      },
      { id: lockId("kars-up-lease-other", "customer-rg"), name: "kars-up-lease-other" },
      { id: lockId("kars-up-lease-"), name: "kars-up-lease-" },
    ], rg)).toEqual([
      { id: lockId("kars-up-lease-a"), name: "kars-up-lease-a" },
      { id: lockId("kars-up-lease-z"), name: "kars-up-lease-z" },
      { id: lockId("kars-up-adopted"), name: "kars-up-adopted" },
    ]);
  });

  it("rejects a Kars-named lock that cannot be removed at resource-group scope", () => {
    expect(() => selectKarsResourceGroupLocks([
      { name: "kars-up-adopted" },
    ], rg)).toThrow("not a removable resource-group lock");
  });
});

describe("destroy --all --yes lock removal", () => {
  beforeEach(() => {
    configMocks.loadContext.mockReturnValue({
      subscription: "cached-sub",
      resourceGroup: rg,
      aksCluster: "kars-aks",
    });
  });

  afterEach(() => {
    execa.mockReset();
    configMocks.loadContext.mockReset();
    vi.restoreAllMocks();
  });

  it("uses the cached subscription only for its cached resource group", async () => {
    execa.mockImplementation(async (_command: string, args: string[]) => {
      if (args[0] === "lock" && args[1] === "list") {
        return {
          stdout: JSON.stringify([
            { id: lockId("customer-lock"), name: "customer-lock" },
            { id: lockId("kars-up-adopted"), name: "kars-up-adopted" },
            { id: lockId("kars-up-lease-run-b"), name: "kars-up-lease-run-b" },
            { id: lockId("kars-up-lease-run-a"), name: "kars-up-lease-run-a" },
          ]),
        };
      }
      return { stdout: "" };
    });

    await destroyCommand().parseAsync([
      "--all", "--yes", "--resource-group", rg,
    ], { from: "user" });

    const azureActions = execa.mock.calls.map((call) => call[1] as string[]);
    expect(azureActions.slice(0, 4)).toEqual([
      ["lock", "list", "--resource-group", rg, "--output", "json", "--subscription", "cached-sub"],
      ["lock", "delete", "--ids", lockId("kars-up-lease-run-a"), "--output", "none", "--subscription", "cached-sub"],
      ["lock", "delete", "--ids", lockId("kars-up-lease-run-b"), "--output", "none", "--subscription", "cached-sub"],
      ["lock", "delete", "--ids", lockId("kars-up-adopted"), "--output", "none", "--subscription", "cached-sub"],
    ]);
    expect(azureActions[4]?.slice(0, 2)).toEqual(["group", "delete"]);
    for (const args of azureActions) {
      expect(args.filter((arg) => arg === "--subscription")).toHaveLength(1);
      expect(args).toContain("cached-sub");
    }
    expect(JSON.stringify(azureActions)).not.toContain("customer-lock");
    expect(azureActions.some((args) => args[0] === "account")).toBe(false);
  });

  it("uses an explicit subscription instead of cached or ambient state", async () => {
    configMocks.loadContext.mockReturnValue({
      subscription: "cached-sub",
      resourceGroup: rg,
      aksCluster: "kars-aks",
    });
    execa.mockImplementation(async (_command: string, args: string[]) => ({
      stdout: args[0] === "lock" ? "[]" : "",
    }));

    await destroyCommand().parseAsync([
      "--all", "--yes", "--resource-group", rg,
      "--subscription", "explicit-sub",
    ], { from: "user" });

    const azureActions = execa.mock.calls.map((call) => call[1] as string[]);
    expect(azureActions.some((args) => args[0] === "account")).toBe(false);
    expect(azureActions.find((args) =>
      args[0] === "group" && args[1] === "delete"
    )).toContain("explicit-sub");
    for (const args of azureActions) {
      expect(args[args.indexOf("--subscription") + 1]).toBe("explicit-sub");
    }
  });

  it("uniquely discovers a legacy deployment and pins every mutation", async () => {
    configMocks.loadContext.mockReturnValue({
      subscription: undefined,
      resourceGroup: rg,
      aksCluster: "custom-aks",
    });
    execa.mockImplementation(async (_command: string, args: string[]) => {
      if (args.slice(0, 2).join(" ") === "account list") {
        return { stdout: JSON.stringify([{ id: "sub-1" }, { id: "sub-2" }]) };
      }
      if (args.slice(0, 2).join(" ") === "aks list") {
        return {
          stdout: JSON.stringify(args.includes("sub-2")
            ? [{ name: "custom-aks", resourceGroup: rg }]
            : [{ name: "custom-aks", resourceGroup: "other-rg" }]),
        };
      }
      return { stdout: args[0] === "lock" ? "[]" : "" };
    });

    await destroyCommand().parseAsync([
      "--all", "--yes", "--resource-group", rg,
    ], { from: "user" });

    const azureActions = execa.mock.calls.map((call) => call[1] as string[]);
    const lookups = azureActions.filter((args) =>
      args[0] === "aks" && args[1] === "list"
    );
    expect(lookups).toHaveLength(2);
    expect(lookups.map((args) =>
      args[args.indexOf("--subscription") + 1]
    )).toEqual(["sub-1", "sub-2"]);
    const mutations = azureActions.filter((args) =>
      (args[0] === "lock" && args[1] === "delete") ||
      args[0] === "group" ||
      (args[0] === "cognitiveservices" && args.includes("purge")) ||
      (args[0] === "keyvault" && args.includes("purge"))
    );
    expect(mutations).not.toHaveLength(0);
    for (const args of mutations) {
      expect(args[args.indexOf("--subscription") + 1]).toBe("sub-2");
    }
  });

  it("does not invoke Azure before the existing destructive confirmation", async () => {
    vi.spyOn(console, "log").mockImplementation(() => undefined);

    await destroyCommand().parseAsync([
      "--all", "--resource-group", rg,
    ], { from: "user" });

    expect(execa).not.toHaveBeenCalled();
  });

  it("deletes the resource group normally when the deployment has no locks", async () => {
    execa.mockImplementation(async (_command: string, args: string[]) => ({
      stdout: args[0] === "lock" ? "[]" : "",
    }));

    await destroyCommand().parseAsync([
      "--all", "--yes", "--resource-group", rg,
    ], { from: "user" });

    const azureActions = execa.mock.calls.map((call) => call[1] as string[]);
    expect(azureActions[0]?.slice(0, 2)).toEqual(["lock", "list"]);
    expect(azureActions[1]?.slice(0, 2)).toEqual(["group", "delete"]);
    expect(azureActions.some((args) =>
      args[0] === "lock" && args[1] === "delete"
    )).toBe(false);
  });

  it.each([
    {
      name: "zero matching deployments",
      clusters: (_subscription: string) => [],
      message: "No enabled Azure subscription contains",
    },
    {
      name: "same-name deployments in multiple subscriptions",
      clusters: (_subscription: string) => [{
        name: "kars-aks",
        resourceGroup: rg,
      }],
      message: "exists in multiple enabled Azure subscriptions",
    },
  ])("fails closed for $name", async ({ clusters, message }) => {
    configMocks.loadContext.mockReturnValue({
      subscription: "wrong-cached-sub",
      resourceGroup: "different-rg",
      aksCluster: "different-aks",
    });
    execa.mockImplementation(async (_command: string, args: string[]) => {
      if (args.slice(0, 2).join(" ") === "account list") {
        return { stdout: JSON.stringify([{ id: "sub-1" }, { id: "sub-2" }]) };
      }
      if (args.slice(0, 2).join(" ") === "aks list") {
        return { stdout: JSON.stringify(clusters(
          args[args.indexOf("--subscription") + 1],
        )) };
      }
      return { stdout: "" };
    });
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const exit = vi.spyOn(process, "exit").mockImplementation((code): never => {
      throw new Error(`process.exit(${String(code)})`);
    });

    await expect(destroyCommand().parseAsync([
      "--all", "--yes", "--resource-group", rg,
    ], { from: "user" })).rejects.toThrow("process.exit(1)");

    expect(console.error).toHaveBeenCalledWith(expect.stringContaining(message));
    const actions = execa.mock.calls.map((call) => call[1] as string[]);
    expect(actions.some((args) =>
      args[0] === "lock" || args[0] === "group" ||
      args[0] === "keyvault" || args[0] === "cognitiveservices"
    )).toBe(false);
    exit.mockRestore();
  });

  it("fails before resource-group deletion when a Kars lock cannot be removed", async () => {
    execa.mockImplementation(async (_command: string, args: string[]) => {
      if (args[0] === "account" && args[1] === "show") {
        return { stdout: "sub-1" };
      }
      if (args[0] === "lock" && args[1] === "list") {
        return {
          stdout: JSON.stringify([
            { id: lockId("kars-up-lease-run-a"), name: "kars-up-lease-run-a" },
          ]),
        };
      }
      if (args[0] === "lock" && args[1] === "delete") {
        throw new Error("lock delete denied");
      }
      return { stdout: "" };
    });
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const exit = vi.spyOn(process, "exit").mockImplementation((code): never => {
      throw new Error(`process.exit(${String(code)})`);
    });

    await expect(destroyCommand().parseAsync([
      "--all", "--yes", "--resource-group", rg,
    ], { from: "user" })).rejects.toThrow("process.exit(1)");

    expect(execa.mock.calls.some((call) => {
      const args = call[1] as string[];
      return args[0] === "group" && args[1] === "delete";
    })).toBe(false);
    exit.mockRestore();
  });
});

describe("destroy Azure authentication compatibility", () => {
  afterEach(() => {
    execa.mockReset();
    configMocks.loadContext.mockReset();
    vi.restoreAllMocks();
  });

  function kubectlResult(args: string[]) {
    return { stdout: args[0] === "config" ? "aks-context" : "" };
  }

  function expectKubernetesSandboxDeleted() {
    const kubectlCalls = execa.mock.calls
      .filter(([command]) => command === "kubectl")
      .map(([, args]) => args as string[]);
    expect(kubectlCalls.some((args) =>
      args[0] === "delete" && args[1] === "karssandbox" &&
      args[2] === "demo"
    )).toBe(true);
    expect(kubectlCalls.some((args) =>
      args[0] === "delete" && args[1] === "ns" &&
      args[2] === "kars-demo"
    )).toBe(true);
  }

  it("uses the explicit subscription for sandbox credential cleanup", async () => {
    configMocks.loadContext.mockReturnValue({
      subscription: "cached-sub",
      resourceGroup: rg,
      aksCluster: "cached-aks",
    });
    execa.mockImplementation(async (command: string, args: string[]) => {
      if (command === "kubectl") return kubectlResult(args);
      return { stdout: "" };
    });
    vi.spyOn(console, "log").mockImplementation(() => undefined);

    await destroyCommand().parseAsync([
      "demo", "--cloud", "--yes", "--resource-group", rg,
      "--subscription", "explicit-sub",
    ], { from: "user" });

    expectKubernetesSandboxDeleted();
    const azureCalls = execa.mock.calls
      .filter(([command]) => command === "az")
      .map(([, args]) => args as string[]);
    expect(azureCalls).toHaveLength(1);
    expect(azureCalls[0]).toContain("explicit-sub");
    expect(azureCalls[0].filter((arg) => arg === "--subscription")).toHaveLength(1);
    expect(azureCalls[0].slice(0, 2)).toEqual([
      "identity", "federated-credential",
    ]);
  });

  it("uses the cached subscription only when its resource group matches", async () => {
    configMocks.loadContext.mockReturnValue({
      subscription: "cached-sub",
      resourceGroup: rg,
      aksCluster: "cached-aks",
    });
    execa.mockImplementation(async (command: string, args: string[]) => {
      if (command === "kubectl") return kubectlResult(args);
      return { stdout: "" };
    });
    vi.spyOn(console, "log").mockImplementation(() => undefined);

    await destroyCommand().parseAsync([
      "demo", "--cloud", "--yes", "--resource-group", rg,
    ], { from: "user" });

    expectKubernetesSandboxDeleted();
    const azureCalls = execa.mock.calls
      .filter(([command]) => command === "az")
      .map(([, args]) => args as string[]);
    expect(azureCalls).toHaveLength(1);
    expect(azureCalls[0][azureCalls[0].indexOf("--subscription") + 1])
      .toBe("cached-sub");
  });

  it("uniquely discovers a legacy sandbox subscription before cleanup", async () => {
    configMocks.loadContext.mockReturnValue({
      resourceGroup: rg,
      aksCluster: "legacy-aks",
    });
    execa.mockImplementation(async (command: string, args: string[]) => {
      if (command === "kubectl") return kubectlResult(args);
      if (args.slice(0, 2).join(" ") === "account list") {
        return { stdout: JSON.stringify([{ id: "sub-1" }, { id: "sub-2" }]) };
      }
      if (args.slice(0, 2).join(" ") === "aks list") {
        return {
          stdout: JSON.stringify(args.includes("sub-2")
            ? [{ name: "legacy-aks", resourceGroup: rg }]
            : [{ name: "other-aks", resourceGroup: rg }]),
        };
      }
      return { stdout: "" };
    });
    vi.spyOn(console, "log").mockImplementation(() => undefined);

    await destroyCommand().parseAsync([
      "demo", "--cloud", "--yes", "--resource-group", rg,
    ], { from: "user" });

    expectKubernetesSandboxDeleted();
    const azureCalls = execa.mock.calls
      .filter(([command]) => command === "az")
      .map(([, args]) => args as string[]);
    expect(azureCalls.filter((args) => args[0] === "aks")).toHaveLength(2);
    const cleanup = azureCalls.find((args) => args[0] === "identity");
    expect(cleanup?.[cleanup.indexOf("--subscription") + 1]).toBe("sub-2");
    expect(cleanup?.filter((arg) => arg === "--subscription")).toHaveLength(1);
  });

  it("skips ambiguous Azure cleanup after deleting the Kubernetes sandbox", async () => {
    configMocks.loadContext.mockReturnValue({
      resourceGroup: rg,
      aksCluster: "legacy-aks",
    });
    execa.mockImplementation(async (command: string, args: string[]) => {
      if (command === "kubectl") return kubectlResult(args);
      if (args.slice(0, 2).join(" ") === "account list") {
        return { stdout: JSON.stringify([{ id: "sub-1" }, { id: "sub-2" }]) };
      }
      if (args.slice(0, 2).join(" ") === "aks list") {
        return {
          stdout: JSON.stringify([{ name: "legacy-aks", resourceGroup: rg }]),
        };
      }
      return { stdout: "" };
    });
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);

    await destroyCommand().parseAsync([
      "demo", "--cloud", "--yes", "--resource-group", rg,
    ], { from: "user" });

    expectKubernetesSandboxDeleted();
    expect(execa.mock.calls.some(([, args]) =>
      (args as string[])[0] === "identity"
    )).toBe(false);
    expect(warning).toHaveBeenCalledWith(expect.stringContaining(
      "skipping federated credential cleanup",
    ));
    expect(warning).toHaveBeenCalledWith(expect.stringContaining(
      "multiple enabled Azure subscriptions",
    ));
  });

  it("skips unauthenticated Azure cleanup after deleting the Kubernetes sandbox", async () => {
    configMocks.loadContext.mockReturnValue({
      resourceGroup: rg,
      aksCluster: "legacy-aks",
    });
    execa.mockImplementation(async (command: string, args: string[]) => {
      if (command === "kubectl") {
        return kubectlResult(args);
      }
      if (command === "az" && args.slice(0, 2).join(" ") === "account list") {
        throw new Error("Please run 'az login' to setup account.");
      }
      return { stdout: "" };
    });
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);

    await destroyCommand().parseAsync([
      "demo", "--cloud", "--yes", "--resource-group", rg,
    ], { from: "user" });

    expectKubernetesSandboxDeleted();
    expect(execa.mock.calls.some(([, args]) =>
      (args as string[])[0] === "identity"
    )).toBe(false);
    expect(warning).toHaveBeenCalledWith(expect.stringContaining(
      "skipping federated credential cleanup",
    ));
  });

  it("blocks full resource-group deletion when Azure authentication fails", async () => {
    execa.mockRejectedValue(new Error("Azure authentication expired"));
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const exit = vi.spyOn(process, "exit").mockImplementation((code): never => {
      throw new Error(`process.exit(${String(code)})`);
    });

    await expect(destroyCommand().parseAsync([
      "--all", "--yes", "--resource-group", rg,
    ], { from: "user" })).rejects.toThrow("process.exit(1)");

    expect(execa).toHaveBeenCalledTimes(1);
    expect(execa.mock.calls[0]?.[1]).toEqual([
      "account", "list",
      "--query", "[?state=='Enabled'].{id:id}",
      "--output", "json",
    ]);
    exit.mockRestore();
  });
});
