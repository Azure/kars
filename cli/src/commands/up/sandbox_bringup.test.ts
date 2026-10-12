// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { bringUpSandbox, type SandboxBringUpContext } from "./sandbox_bringup.js";

const state = vi.hoisted(() => ({ execute: vi.fn(), save: vi.fn() }));
vi.mock("execa", () => ({ execa: state.execute }));
vi.mock("../../config.js", () => ({ saveContext: state.save }));
vi.mock("../../stepper.js", () => ({ section: vi.fn(), kvLine: vi.fn(), checkLine: vi.fn() }));
vi.mock("node:net", () => ({ createServer: () => { throw new Error("WebUI unavailable in this test"); } }));

const federation = {
  subscriptionId: "11111111-1111-4111-8111-111111111111", resourceGroup: "identity-rg", identityName: "sandbox-wi",
  issuer: "https://eastus2.oic.prod-aks.azure.com/tenant/cluster/",
};
const principalId = "22222222-2222-4222-8222-222222222222";
const scope = `/subscriptions/${federation.subscriptionId}/resourceGroups/identity-rg/providers/Microsoft.ManagedIdentity/userAssignedIdentities/sandbox-wi`;
function fixture() {
  const stepper = { step: vi.fn(), update: vi.fn(), detail: vi.fn(), done: vi.fn(), warn: vi.fn(), summary: vi.fn() };
  const ctx = {
    options: { name: "agent", model: "model", region: "eastus2", isolation: "standard" },
    baseName: "deployment", rg: "cluster-rg", acrLoginServer: "mirror.azurecr.io", foundryEndpoint: "",
    openAiEndpoint: "https://account.openai.azure.com", kvName: "vault", wiClientId: "client", imdsClientId: "",
    repoRoot: process.cwd(), stepper, federation, registryMode: "local",
  } as unknown as SandboxBringUpContext;
  return { ctx, stepper };
}
beforeEach(() => {
  vi.clearAllMocks();
  state.save.mockReset();
  vi.spyOn(console, "log").mockImplementation(() => {});
  state.execute.mockImplementation(async (bin: string, args: string[]) => {
    if (bin === "az") {
      expect(args.filter(arg => arg === "--subscription")).toHaveLength(1);
      expect(args[args.indexOf("--subscription") + 1]).toBe(federation.subscriptionId);
      let result: unknown;
      if (args[0] === "identity" && args[1] === "show") result = { id: scope, principalId };
      else if (args.slice(0, 3).join(" ") === "identity federated-credential list") result = [
        { name: "kars-controller-sa", subject: "system:serviceaccount:kars-system:kars-controller", issuer: federation.issuer, audiences: ["api://AzureADTokenExchange"] },
        { name: "kars-agent", subject: "system:serviceaccount:kars-agent:sandbox", issuer: federation.issuer, audiences: ["api://AzureADTokenExchange"] },
      ];
      else if (args.slice(0, 3).join(" ") === "role assignment list") result = [{ principalId, scope, roleDefinitionId: `/subscriptions/${federation.subscriptionId}/providers/Microsoft.Authorization/roleDefinitions/e40ec5ca-96e0-45a2-b4ff-59039f2c2b59` }];
      else throw new Error(`Unexpected Azure mutation: ${args.join(" ")}`);
      return { stdout: JSON.stringify(result) };
    }
    if (bin === "kubectl" && ["apply", "get", "wait"].includes(args[0]!)) return { stdout: "Running" };
    if (bin === "kubectl" && args[0] === "exec") return { stdout: 'OPENCLAW_GATEWAY_TOKEN="synthetic-test-token"' };
    throw new Error(`Unexpected command: ${bin} ${args.join(" ")}`);
  });
});
afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });

describe("ordinary sandbox Workload Identity verification", () => {
  it.each(["identity read", "credential inventory", "role inventory", "final readback"])("stops before creating a sandbox or claiming success after %s fails", async stage => {
    const { ctx, stepper } = fixture();
    const original = state.execute.getMockImplementation()!;
    let reads = 0;
    state.execute.mockImplementation(async (bin: string, args: string[]) => {
      if (args[1] === "show") reads++;
      if ((stage === "identity read" && args[1] === "show")
        || (stage === "credential inventory" && args[1] === "federated-credential")
        || (stage === "role inventory" && args[1] === "assignment")
        || (stage === "final readback" && reads === 2)) throw new Error("ARM denied");
      return original(bin, args);
    });
    await expect(bringUpSandbox(ctx)).rejects.toThrow();
    expect(state.execute.mock.calls.some(([bin]) => bin === "kubectl")).toBe(false);
    expect(state.save).not.toHaveBeenCalled();
    expect(stepper.done).not.toHaveBeenCalled();
    expect(stepper.detail).not.toHaveBeenCalledWith("ok", expect.stringContaining("verified"));
  });
  it("persists the selected subscription and exact federation only after ARM readback", async () => {
    vi.useFakeTimers();
    const { ctx, stepper } = fixture();
    const run = bringUpSandbox(ctx);
    await Promise.all([run, vi.runAllTimersAsync()]);
    expect(stepper.detail).toHaveBeenCalledWith("ok", "Workload Identity configuration verified; token exchange not tested");
    expect(state.save).toHaveBeenCalledWith(expect.objectContaining({
      subscription: federation.subscriptionId, identityName: federation.identityName,
      identityResourceGroup: federation.resourceGroup, oidcIssuerUrl: federation.issuer,
      resourceGroup: "cluster-rg", aksCluster: "deployment-aks", phase: "complete",
    }));
    const finalIdentityRead = state.execute.mock.calls.reduce((last, [bin, args], index) => bin === "az" && args[1] === "show" ? index : last, -1);
    const apply = state.execute.mock.calls.findIndex(([bin, args]) => bin === "kubectl" && args[0] === "apply");
    expect(finalIdentityRead).toBeGreaterThan(0);
    expect(finalIdentityRead).toBeLessThan(apply);
    const input = JSON.parse(state.execute.mock.calls[apply]![2].input);
    expect(input.items.find((item: { kind: string }) => item.kind === "KarsSandbox").metadata.name).toBe("agent");
  });
});
