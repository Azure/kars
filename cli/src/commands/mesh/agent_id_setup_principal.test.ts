// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("execa", () => ({ execa: vi.fn() }));
import { execa } from "execa";
import { ensureAgentIdTrust } from "./agent_id_setup.js";

type ProcessCall = (program: string, args: string[], options?: unknown) => Promise<{
  stdout: string; stderr: string; exitCode: number;
}>;
const mockedExeca = vi.mocked(execa) as unknown as ReturnType<typeof vi.fn<ProcessCall>>;
const appId = "00001111-aaaa-2222-bbbb-3333cccc4444";
const principalId = "59e617e5-e447-4adc-8b88-00af644d7c92";
const type = "#microsoft.graph.agentIdentityBlueprintPrincipal";
const principal = { id: principalId, appId, "@odata.type": type, servicePrincipalType: "Application" };
const principalPath = "/beta/servicePrincipals/microsoft.graph.agentIdentityBlueprintPrincipal";
const readPath = `/beta/servicePrincipals/${principalId}/microsoft.graph.agentIdentityBlueprintPrincipal`;
const options = { credentialMode: "ManagedIdentityImds" as const };

interface Responses {
  lookup?: unknown;
  created?: unknown;
  typedRead?: unknown;
  lookupError?: string;
  typedReadError?: string;
  createdError?: string;
  malformedLookup?: boolean;
}

function configure(responses: Responses = {}) {
  mockedExeca.mockImplementation(async (program, args) => {
    const argv = args as string[];
    const reply = (value: unknown) => ({
      stdout: value === undefined ? "" : JSON.stringify(value), stderr: "", exitCode: 0,
    });
    if (program === "az" && argv[0] === "account" && argv[1] === "show") {
      return reply({ id: "subscription", tenantId: "tenant", user: { name: "fixture" } });
    }
    if (program === "az" && argv[0] === "rest") {
      const method = argv[argv.indexOf("--method") + 1];
      const url = new URL(argv[argv.indexOf("--url") + 1]);
      if (method === "GET" && url.pathname === "/beta/me") return reply({ id: "sponsor" });
      if (method === "GET" && url.pathname === "/beta/applications") {
        return reply({ value: [{ id: "blueprint", appId, displayName: "kars-blueprint" }] });
      }
      if (method === "GET" && url.pathname === "/beta/servicePrincipals") {
        if (responses.lookupError) throw new Error(responses.lookupError);
        if (responses.malformedLookup) return { stdout: "{broken", stderr: "", exitCode: 0 };
        return reply("lookup" in responses ? responses.lookup : { value: [principal] });
      }
      if (method === "GET" && url.pathname === readPath) {
        if (responses.typedReadError) throw new Error(responses.typedReadError);
        return reply("typedRead" in responses ? responses.typedRead : principal);
      }
      if (method === "POST" && url.pathname === principalPath) {
        if (responses.createdError) throw new Error(responses.createdError);
        expect(JSON.parse(argv[argv.indexOf("--body") + 1])).toEqual({ appId });
        return reply("created" in responses ? responses.created : principal);
      }
      if (method === "GET" && url.pathname === "/beta/applications/blueprint/federatedIdentityCredentials") {
        return reply({ value: [{ id: "fic", name: "kars-controller-mi", subject: "mi-principal" }] });
      }
    }
    if (program === "az" && argv[0] === "group" && argv[1] === "show") return reply({ name: "rg" });
    if (program === "az" && argv[0] === "identity" && argv[1] === "show") {
      return reply({ id: "mi-resource", clientId: "mi-client", principalId: "mi-principal", name: "mi", location: "eastus" });
    }
    if (program === "kubectl" && argv.join(" ") === "apply -f -") return reply({});
    throw new Error(`Unexpected mocked process: ${program} ${argv.join(" ")}`);
  });
}

function graphCalls() {
  return mockedExeca.mock.calls.filter(([program, args]) => program === "az" && (args as string[])[0] === "rest")
    .map(([, args]) => {
      const argv = args as string[];
      return { method: argv[argv.indexOf("--method") + 1], url: new URL(argv[argv.indexOf("--url") + 1]), argv };
    });
}

function expectNoDownstream() {
  expect(mockedExeca.mock.calls.every(([program, args]) =>
    program === "az" && ["account", "rest"].includes((args as string[])[0]),
  )).toBe(true);
  expect(graphCalls().some(({ url }) => url.pathname.includes("federatedIdentityCredentials"))).toBe(false);
}

beforeEach(() => {
  mockedExeca.mockReset();
});

const invalidPrincipals: [string, unknown][] = [
  ["null", null],
  ["empty body", undefined],
  ["array", []],
  ["string", "principal"],
  ["missing ID", { appId, "@odata.type": type }],
  ["empty ID", { ...principal, id: "" }],
  ["blank ID", { ...principal, id: "  " }],
  ["padded ID", { ...principal, id: ` ${principalId}` }],
  ["numeric ID", { ...principal, id: 1 }],
  ["missing appId", { id: principalId, "@odata.type": type }],
  ["numeric appId", { ...principal, appId: 1 }],
  ["foreign appId", { ...principal, appId: "foreign" }],
  ["ordinary service principal", { ...principal, "@odata.type": "#microsoft.graph.servicePrincipal" }],
  ["agent identity", { ...principal, "@odata.type": "#microsoft.graph.agentIdentity" }],
  ["null type", { ...principal, "@odata.type": null }],
  ["empty type", { ...principal, "@odata.type": "" }],
  ["numeric type", { ...principal, "@odata.type": 1 }],
  ["doubled type prefix", { ...principal, "@odata.type": `#${type}` }],
  ["padded type", { ...principal, "@odata.type": ` ${type}` }],
];

describe("BlueprintPrincipal setup contract", () => {
  it("reuses a matching typed principal and completes setup without creating identities or credentials", async () => {
    configure();
    const result = await ensureAgentIdTrust(options);
    expect(result).toMatchObject({ blueprintClientId: appId, controllerMiClientId: "mi-client", freshlyCreated: false });
    const calls = graphCalls();
    expect(calls.every(({ method }) => method === "GET")).toBe(true);
    const lookup = calls.find(({ url }) => url.pathname === "/beta/servicePrincipals")!;
    expect(lookup.url.searchParams.get("$top")).toBe("2");
    expect(lookup.url.searchParams.get("$filter")).toBe(`appId eq '${appId}'`);
    expect(calls.every(({ argv }) => argv.includes("OData-Version=4.0"))).toBe(true);
    expect(mockedExeca.mock.calls.filter(([program]) => program === "kubectl")).toHaveLength(1);
  });

  it("creates a missing principal through the typed beta endpoint only", async () => {
    configure({ lookup: { value: [] } });
    await expect(ensureAgentIdTrust(options)).resolves.toMatchObject({ blueprintClientId: appId });
    expect(graphCalls().filter(({ method }) => method === "POST").map(({ url }) => url.pathname)).toEqual([principalPath]);
  });

  it("remains idempotent on the second setup after typed creation", async () => {
    configure({ lookup: { value: [] } });
    await ensureAgentIdTrust(options);
    mockedExeca.mockReset();
    configure();
    await ensureAgentIdTrust(options);
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it("checks the exact existing object through a typed read when metadata omits its type", async () => {
    configure({ lookup: { value: [{ id: principalId, appId }] }, typedRead: { id: principalId, appId } });
    await ensureAgentIdTrust(options);
    expect(graphCalls().filter(({ url }) => url.pathname === readPath)).toHaveLength(1);
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it("accepts omitted type metadata only from the typed creation endpoint", async () => {
    configure({ lookup: { value: [] }, created: { id: principalId, appId } });
    await expect(ensureAgentIdTrust(options)).resolves.toMatchObject({ blueprintClientId: appId });
  });

  it.each(["lookup", "creation", "typed read"])("accepts the documented unprefixed type on %s", async (route) => {
    const unprefixed = { ...principal, "@odata.type": "microsoft.graph.agentIdentityBlueprintPrincipal" };
    configure(route === "lookup" ? { lookup: { value: [unprefixed] } }
      : route === "creation" ? { lookup: { value: [] }, created: unprefixed }
      : { lookup: { value: [{ id: principalId, appId }] }, typedRead: unprefixed });
    await expect(ensureAgentIdTrust(options)).resolves.toMatchObject({ blueprintClientId: appId });
    expect(graphCalls().filter(({ url }) => url.pathname === readPath)).toHaveLength(route === "typed read" ? 1 : 0);
    expect(graphCalls().filter(({ method }) => method === "POST")).toHaveLength(route === "creation" ? 1 : 0);
  });

  it("compares Graph UUID identifiers case-insensitively", async () => {
    configure({ lookup: { value: [{ id: principalId, appId: appId.toUpperCase() }] }, typedRead: { ...principal, id: principalId.toUpperCase() } });
    await expect(ensureAgentIdTrust(options)).resolves.toMatchObject({ blueprintClientId: appId });
  });

  it.each([
    ["empty body", undefined], ["null", null], ["array", []], ["scalar", 3],
    ["missing values", {}], ["null values", { value: null }], ["nonarray values", { value: principal }],
    ["duplicates", { value: [principal, principal] }],
    ["partial page", { value: [principal], "@odata.nextLink": "https://graph.microsoft.com/beta/servicePrincipals?$skiptoken=next" }],
    ["empty partial page", { value: [], "@odata.nextLink": "https://graph.microsoft.com/beta/servicePrincipals?$skiptoken=next" }],
  ])("refuses %s lookup without creating or provisioning", async (_name, lookup) => {
    configure({ lookup });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow(/BlueprintPrincipal/);
    expectNoDownstream();
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it.each(invalidPrincipals)("refuses existing %s principal before downstream writes", async (_name, value) => {
    configure({ lookup: { value: [value] } });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow(/BlueprintPrincipal|requested blueprint/);
    expectNoDownstream();
    expect(graphCalls().filter(({ url }) => url.pathname === readPath)).toHaveLength(0);
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it.each(invalidPrincipals)("refuses typed creation returning %s without provisioning", async (_name, created) => {
    configure({ lookup: { value: [] }, created });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow(/BlueprintPrincipal|requested blueprint/);
    expectNoDownstream();
    expect(graphCalls().filter(({ method }) => method === "POST")).toHaveLength(1);
  });

  it.each([...invalidPrincipals, ["foreign object ID", { ...principal, id: "another-object" }]] as [string, unknown][])("refuses typed read returning %s without fallback creation", async (_name, typedRead) => {
    configure({ lookup: { value: [{ id: principalId, appId }] }, typedRead });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow(/BlueprintPrincipal|requested blueprint/);
    expectNoDownstream();
    expect(graphCalls().filter(({ url }) => url.pathname === readPath)).toHaveLength(1);
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it.each(["Request_ResourceNotFound", "Authorization_RequestDenied", "AADSTS530084"])("propagates typed-read %s without retry or credential changes", async (typedReadError) => {
    configure({ lookup: { value: [{ id: principalId, appId }] }, typedReadError });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow();
    expectNoDownstream();
    expect(graphCalls().filter(({ url }) => url.pathname === readPath)).toHaveLength(1);
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it.each(["Authorization_RequestDenied", "AADSTS530084"])("propagates lookup %s without creation or retry", async (lookupError) => {
    configure({ lookupError });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow();
    expectNoDownstream();
    expect(graphCalls().filter(({ url }) => url.pathname === "/beta/servicePrincipals")).toHaveLength(1);
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it("refuses malformed lookup JSON without creation", async () => {
    configure({ malformedLookup: true });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow();
    expectNoDownstream();
    expect(graphCalls().every(({ method }) => method === "GET")).toBe(true);
  });

  it("does not retry a failed typed creation or provision downstream", async () => {
    configure({ lookup: { value: [] }, createdError: "Conflict" });
    await expect(ensureAgentIdTrust(options)).rejects.toThrow("Conflict");
    expectNoDownstream();
    expect(graphCalls().filter(({ method }) => method === "POST")).toHaveLength(1);
  });
});
