// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  execute: vi.fn(), prepare: vi.fn(), sandbox: vi.fn(), phase: vi.fn(),
  subscriptionId: "11111111-1111-4111-8111-111111111111",
  issuer: "https://eastus2.oic.prod-aks.azure.com/tenant/cluster/",
  discoveryFails: false,
  unexpected: [] as string[],
}));
vi.mock("execa", () => ({ execa: state.execute }));
vi.mock("./preflight.js", () => ({
  runPreflight: async () => ({ rg: "selected-rg", subscriptionId: state.subscriptionId }),
  isValidAzureHost: vi.fn(),
}));
vi.mock("./root-preflight.js", () => ({ preflightUpRoot: async () => true }));
vi.mock("./resume.js", () => ({
  loadResumeState: () => null, isPhaseSkippable: () => false,
  markPhaseDone: state.phase, formatAge: vi.fn(),
}));
vi.mock("./images.js", () => ({ acquireImages: vi.fn() }));
vi.mock("./agentmesh_deploy.js", () => ({ deployAgentMesh: async () => ({ registryMode: "local" }) }));
vi.mock("./sandbox_bringup.js", () => ({ bringUpSandbox: state.sandbox }));
vi.mock("../../lib/core-helm-schemas.js", () => ({ prepareCoreHelmSchemas: state.prepare }));
vi.mock("../../lib/private-root-upgrade-guard.js", () => ({ assertControllerMutationAllowed: vi.fn() }));
vi.mock("../../lib/sre-authority.js", () => ({ assertSafeMutation: vi.fn() }));
vi.mock("../../stepper.js", () => ({
  Stepper: class { step() {} detail() {} update() {} done() {} stop() {} }, banner: vi.fn(),
}));

import { upCommand } from "../up.js";

beforeEach(() => {
  vi.clearAllMocks();
  state.discoveryFails = false;
  state.issuer = "https://eastus2.oic.prod-aks.azure.com/tenant/cluster/";
  state.unexpected.length = 0;
  state.prepare.mockResolvedValue(undefined);
  state.sandbox.mockReset().mockResolvedValue(undefined);
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(process, "exit").mockImplementation(() => undefined as never);
  state.execute.mockImplementation(async (bin: string, args: string[]) => {
    if (bin === "az") {
      expect(args.filter(arg => arg === "--subscription")).toHaveLength(1);
      expect(args[args.indexOf("--subscription") + 1]).toBe(state.subscriptionId);
      if (args.slice(0, 3).join(" ") === "deployment group show") return { stdout: JSON.stringify({
        acrLoginServer: { value: "mirror.azurecr.io" }, sandboxIdentityClientId: { value: "client" },
        keyVaultName: { value: "vault" },
      }) };
      if (args[0] === "aks" && args[1] === "show") {
        if (args.includes("oidcIssuerProfile.issuerUrl")) {
          if (state.discoveryFails) throw new Error("OIDC discovery denied");
          return { stdout: state.issuer };
        }
        if (args.includes("networkProfile.loadBalancerProfile.effectiveOutboundIPs[0].id")) return { stdout: "" };
      }
      if (args[0] === "group" && args[1] === "show") return { stdout: "Succeeded" };
      if ((args[0] === "group" && args[1] === "create")
        || (args[0] === "feature" && args[1] === "register")
        || (args[0] === "provider" && args[1] === "register")
        || (args[0] === "aks" && args[1] === "update")) return { stdout: "" };
    }
    if (bin === "curl") return { stdout: "" };
    if (bin === "helm" && args[0] === "list") return { stdout: "[]" };
    if (bin === "helm" && args[0] === "upgrade") return { stdout: "" };
    if (bin === "kubectl" && args[0] === "get" && args[1] === "deployment,service") return { stdout: '{"items":[]}' };
    if (bin === "kubectl" && args[0] === "rollout") return { stdout: "" };
    const command = `${bin} ${args.join(" ")}`;
    state.unexpected.push(command);
    throw new Error(`Unexpected command: ${command}`);
  });
});
afterEach(() => vi.restoreAllMocks());

async function runUp() {
  await upCommand().parseAsync(["--cluster-name", "target-aks", "--name", "agent", "--skip-infra", "--yes"], { from: "user" });
  expect(state.unexpected).toEqual([]);
}
function helmUpgrades() {
  return state.execute.mock.calls.filter(([bin, args]) => bin === "helm" && args[0] === "upgrade");
}

describe("ordinary up Workload Identity wiring", () => {
  it("pins the selected subscription and passes identical discovered federation to Helm and sandbox setup", async () => {
    await runUp();
    expect(process.exit).toHaveBeenCalledExactlyOnceWith(0);
    expect(helmUpgrades()).toHaveLength(1);
    const helm = helmUpgrades()[0]![1] as string[];
    expect(state.prepare).toHaveBeenCalledExactlyOnceWith(expect.any(Function), helm);
    expect(state.sandbox).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      baseName: "target", rg: "selected-rg", options: expect.objectContaining({ name: "agent" }),
      federation: {
        subscriptionId: state.subscriptionId, resourceGroup: "selected-rg",
        identityName: "target-aks-sandbox-wi", issuer: state.issuer,
      },
    }));
    for (const value of [
      `fedcred.subscriptionId=${state.subscriptionId}`, "fedcred.identityResourceGroup=selected-rg",
      "fedcred.identityName=target-aks-sandbox-wi", `fedcred.oidcIssuerUrl=${state.issuer}`,
    ]) {
      expect(helm.filter(arg => arg === value)).toHaveLength(1);
      expect(helm[helm.indexOf(value) - 1]).toBe("--set-string");
    }
  });
  it.each(["discovery denied", "issuer missing", "issuer malformed"])("stops before schemas, Helm or sandbox setup when %s", async fault => {
    state.discoveryFails = fault === "discovery denied";
    if (fault === "issuer missing") state.issuer = "  ";
    if (fault === "issuer malformed") state.issuer = "http://invalid.example";
    await runUp();
    expect(process.exit).toHaveBeenCalledExactlyOnceWith(1);
    expect(state.prepare).not.toHaveBeenCalled();
    expect(helmUpgrades()).toHaveLength(0);
    expect(state.sandbox).not.toHaveBeenCalled();
    expect(state.phase.mock.calls.some(([phase]) => ["helm", "mesh"].includes(phase))).toBe(false);
    expect(console.error).toHaveBeenCalledWith(expect.stringContaining("Deployment failed"));
  });
  it("does not report command success when sandbox identity verification fails", async () => {
    state.sandbox.mockRejectedValue(new Error("ARM readback failed"));
    await runUp();
    expect(helmUpgrades()).toHaveLength(1);
    expect(state.sandbox).toHaveBeenCalledOnce();
    expect(process.exit).toHaveBeenCalledExactlyOnceWith(1);
    expect(console.error).toHaveBeenCalledWith(expect.stringContaining("ARM readback failed"));
  });
});
