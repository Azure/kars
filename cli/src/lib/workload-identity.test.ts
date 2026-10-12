// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { execa } from "execa";
import { createSubscriptionPinnedExeca } from "./azure-subscription.js";
import { ensureWorkloadIdentity, sandboxNamesForFederation, validateWorkloadIdentityConfig, type WorkloadIdentityConfig } from "./workload-identity.js";

const config = {
  subscriptionId: "11111111-1111-4111-8111-111111111111", resourceGroup: "rg", identityName: "sandbox-wi",
  issuer: "https://eastus2.oic.prod-aks.azure.com/tenant/cluster/",
};
const principalId = "22222222-2222-4222-8222-222222222222";
const scope = `/subscriptions/${config.subscriptionId}/resourceGroups/rg/providers/Microsoft.ManagedIdentity/userAssignedIdentities/sandbox-wi`;
const role = "e40ec5ca-96e0-45a2-b4ff-59039f2c2b59";
const roleDefinitionId = `/subscriptions/${config.subscriptionId}/providers/Microsoft.Authorization/roleDefinitions/${role}`;
const audience = "api://AzureADTokenExchange";
function credential(name = "kars-controller-sa", subject = "system:serviceaccount:kars-system:kars-controller") {
  return { name, subject, issuer: config.issuer, audiences: [audience] };
}
function fixture() {
  const state = {
    identity: { id: scope, principalId } as unknown,
    credentials: [] as unknown,
    roles: [] as unknown,
    persist: true,
    hook: undefined as undefined | ((args: string[]) => void),
  };
  const execute = vi.fn(async (bin: string, args: string[]) => {
    expect(bin).toBe("az");
    expect(args.filter(arg => arg === "--subscription")).toHaveLength(1);
    expect(args[args.indexOf("--subscription") + 1]).toBe(config.subscriptionId);
    state.hook?.(args);
    const value = (key: string) => args[args.indexOf(key) + 1]!;
    let result: unknown;
    const command = args.slice(0, 3).join(" ");
    if (args[0] === "identity" && args[1] === "show") result = state.identity;
    else if (command === "identity federated-credential list") result = state.credentials;
    else if (command === "role assignment list") {
      expect(value("--assignee-object-id")).toBe(principalId);
      expect(value("--scope")).toBe(scope);
      expect(value("--fill-principal-name")).toBe("false");
      expect(value("--fill-role-definition-name")).toBe("false");
      result = state.roles;
    } else if (command === "identity federated-credential create") {
      if (state.persist) (state.credentials as object[]).push({ name: value("--name"), subject: value("--subject"), issuer: value("--issuer"), audiences: [value("--audiences")] });
    } else if (command === "role assignment create") {
      expect(value("--role")).toBe(role);
      expect(value("--assignee-principal-type")).toBe("ServicePrincipal");
      expect(value("--scope")).toBe(scope);
      if (state.persist) (state.roles as object[]).push({ scope, principalId, roleDefinitionId });
    } else throw new Error(`Unexpected command: ${args.join(" ")}`);
    return { stdout: JSON.stringify(result) ?? "" };
  });
  const run = (names = ["agent"]) => ensureWorkloadIdentity(createSubscriptionPinnedExeca(execute as unknown as typeof execa, config.subscriptionId), config, names);
  const writes = () => execute.mock.calls.filter(([, args]) => args.includes("create"));
  return { state, execute, run, writes };
}

describe("Workload Identity target validation", () => {
  it("preserves the exact issuer, including its trailing slash", () => {
    expect(validateWorkloadIdentityConfig(config)).toEqual(config);
  });
  it.each([
    { subscriptionId: "alias" }, { subscriptionId: "" }, { resourceGroup: "../rg" },
    { resourceGroup: undefined }, { identityName: undefined }, { identityName: "a,b" },
    { issuer: "http://example.invalid/" }, { issuer: "https://user:password@example.invalid/" },
    { issuer: "https://example.invalid/?query=1" }, { issuer: "https://example.invalid/#fragment" },
    { issuer: "https://example.invalid/a,b" }, { issuer: " https://example.invalid/" }, { issuer: "" },
  ])("refuses invalid target %j", patch => {
    expect(() => validateWorkloadIdentityConfig({ ...config, ...patch } as WorkloadIdentityConfig)).toThrow();
  });
  it("accepts an empty collection, not a missing one", () => {
    expect(sandboxNamesForFederation('{"items":[]}')).toEqual([]);
    expect(sandboxNamesForFederation('{"items":[{"metadata":{"name":"agent"}}]}')).toEqual(["agent"]);
  });
  it.each(["invalid", "null", "[]", "{}", '{"items":null}', '{"items":[{}]}',
    '{"items":[{"metadata":{"name":"UPPER"}}]}', '{"items":[{"metadata":{"name":"agent"}},{"metadata":{"name":"agent"}}]}',
    JSON.stringify({ items: [{ metadata: { name: "a".repeat(59) } }] }),
  ])("refuses malformed or ambiguous sandbox inventory %s", json => {
    expect(() => sandboxNamesForFederation(json)).toThrow();
  });
  it("refuses the controller credential name collision before any command", async () => {
    const f = fixture();
    await expect(f.run(["controller-sa"])).rejects.toThrow("conflicts");
    expect(f.execute).not.toHaveBeenCalled();
  });
});

describe("Workload Identity ARM provisioning and readback", () => {
  it("creates only missing credentials and the exact self-scoped role, then reads back", async () => {
    const f = fixture();
    await f.run(["agent", "child"]);
    expect(f.writes()).toHaveLength(4);
    expect(f.state.credentials).toEqual([credential(), credential("kars-agent", "system:serviceaccount:kars-agent:sandbox"), credential("kars-child", "system:serviceaccount:kars-child:sandbox")]);
    expect(f.execute.mock.calls.filter(([, args]) => args[1] === "show")).toHaveLength(2);
    for (const noun of ["federated-credential", "assignment"]) {
      expect(f.execute.mock.calls.filter(([, args]) => args[1] === noun && args[2] === "list")).toHaveLength(2);
    }
  });
  it("reuses correct existing ARM state without writes", async () => {
    const f = fixture();
    await f.run();
    f.execute.mockClear();
    await f.run();
    expect(f.writes()).toHaveLength(0);
  });
  it.each([
    { issuer: config.issuer.slice(0, -1) }, { subject: "system:serviceaccount:other:sandbox" },
    { audiences: [] }, { audiences: [audience, "extra"] }, { audiences: ["wrong"] },
  ])("refuses conflicting credentials without repairing them: %j", patch => {
    const f = fixture();
    f.state.credentials = [{ ...credential(), ...patch }];
    return expect(f.run()).rejects.toThrow("Conflicting").then(() => expect(f.writes()).toHaveLength(0));
  });
  it.each([null, {}, [null], [{ name: "missing-fields" }], [credential(), credential()]])("refuses malformed credential inventory %j", async value => {
    const f = fixture(); f.state.credentials = value;
    await expect(f.run()).rejects.toThrow("credential inventory");
    expect(f.writes()).toHaveLength(0);
  });
  it.each([null, {}, [null], [{ principalId, scope }]])("refuses malformed role inventory before credential writes %j", async value => {
    const f = fixture(); f.state.roles = value;
    await expect(f.run()).rejects.toThrow("role inventory");
    expect(f.writes()).toHaveLength(0);
  });
  it("does not broaden a conditional role", async () => {
    const f = fixture(); f.state.roles = [{ principalId, scope, roleDefinitionId, condition: "review-required" }];
    await expect(f.run()).rejects.toThrow("Conditional");
    expect(f.writes()).toHaveLength(0);
  });
  it.each([{ principalId: config.subscriptionId }, { scope: "/subscriptions/other" }, { roleDefinitionId: "wrong" }])("does not accept mismatched role binding %j", async patch => {
    const f = fixture(); f.state.roles = [{ principalId, scope, roleDefinitionId, ...patch }];
    await f.run();
    expect(f.writes().filter(([, args]) => args[0] === "role")).toHaveLength(1);
  });
  it.each([null, {}, { id: scope, principalId: "invalid" }, { id: `${scope}-other`, principalId }])("rejects mismatched managed identity %j", async identity => {
    const f = fixture(); f.state.identity = identity;
    await expect(f.run()).rejects.toThrow("selected resource or principal");
    expect(f.writes()).toHaveLength(0);
  });
  it.each(["identity show", "identity federated-credential list", "role assignment list", "identity federated-credential create", "role assignment create"])("propagates Azure failure at %s", async command => {
    const f = fixture(); f.state.hook = args => { if (args.join(" ").startsWith(command)) throw new Error("AuthorizationFailed"); };
    await expect(f.run()).rejects.toThrow("configuration is incomplete");
  });
  it("rejects success exit without persisted credentials", async () => {
    const f = fixture(); f.state.persist = false;
    await expect(f.run()).rejects.toThrow("readback verification");
  });
  it("rejects success exit without persisted role", async () => {
    const f = fixture(); f.state.credentials = [credential()]; f.state.persist = false;
    await expect(f.run([])).rejects.toThrow("assignment failed readback");
  });
  it.each(["identity federated-credential list", "role assignment list"])("propagates second-read failure at %s", async command => {
    const f = fixture(); let count = 0;
    f.state.hook = args => { if (args.join(" ").startsWith(command) && ++count === 2) throw new Error("readback denied"); };
    await expect(f.run()).rejects.toThrow("configuration is incomplete");
  });
  it("does not count earlier successful sandbox credentials after a later failure", async () => {
    const f = fixture(); f.state.hook = args => {
      if (args.includes("create") && args.includes("kars-child")) throw new Error("AuthorizationFailed");
    };
    await expect(f.run(["agent", "child"])).rejects.toThrow("'kars-child' creation failed");
    expect((f.state.credentials as object[])).toHaveLength(2);
    expect(f.state.roles).toEqual([]);
  });
  it("rejects principal replacement during configuration", async () => {
    const f = fixture(); let count = 0;
    f.state.hook = args => { if (args[1] === "show" && ++count === 2) f.state.identity = { id: scope, principalId: config.subscriptionId }; };
    await expect(f.run()).rejects.toThrow("principal changed");
  });
  it("rejects invalid JSON instead of accepting an empty inventory", async () => {
    const execute = vi.fn().mockResolvedValue({ stdout: "not json" });
    await expect(ensureWorkloadIdentity(execute as unknown as typeof execa, config, [])).rejects.toThrow("Invalid Workload Identity");
  });
});
