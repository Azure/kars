// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { prepareWorkspaceProviderGrant, setupWorkspaceProviders, waitForWorkspaceProviders } from "./workspace-provider-setup.js";
import { applyReviewedGrant, validateGrantDocument } from "../commands/credential-grants.js";
import { previewPrivateActivation, validateQualifiedActivation } from "./private-activation.js";
import { PrivateBudgetRotationRequiredError } from "./private-activation-retirement.js";

vi.mock("../commands/credential-grants.js", () => ({ applyReviewedGrant: vi.fn(), validateGrantDocument: vi.fn() }));
vi.mock("./private-activation.js", async importOriginal => ({
  ...await importOriginal<typeof import("./private-activation.js")>(), previewPrivateActivation: vi.fn(), validateQualifiedActivation: vi.fn(),
}));

const options = { namespace: "kars-system", bridgeNamespace: "kars-system", controllerProfile: "service-accounts" as const };
const activation = { root: {}, namespaces: [], phase: "reviewed" };
function object(name: string, spec = {}) {
  return { metadata: { name, uid: `${name}-uid`, resourceVersion: "1", generation: 1 }, spec };
}
function fixture() {
  const objects: Record<string, any> = {
    "namespace/kars-system": object("kars-system"),
    "deployment/kars-bridge-bff": object("kars-bridge-bff", { template: { spec: { serviceAccountName: "actual-bridge-account" } } }),
    "serviceaccount/actual-bridge-account": object("actual-bridge-account"),
    "deployment/kars-controller": object("kars-controller"),
    "karscredentialgrants.kars.azure.com/workspace": object("workspace", {
      workspaceUid: "kars-system-uid", enabled: true, writers: [], integrationStores: [], agentKeys: ["CUSTOM_TOKEN"],
    }),
  };
  const execute = vi.fn(async (args: string[], input?: string) => {
    if (args[0] === "auth") return "yes";
    if (args[0] === "get") return JSON.stringify(objects[`${args[1]}/${args[2]}`]) ?? "";
    if (args[0] === "wait") return "condition met";
    if (args[0] === "create") {
      const created = JSON.parse(input!);
      created.metadata = { ...created.metadata, uid: `${created.metadata.name}-uid`, resourceVersion: "1" };
      objects[`secret/${created.metadata.name}`] = created;
      return JSON.stringify(created);
    }
    throw new Error(`Unexpected operation ${args[0]}`);
  });
  return { objects, execute, grant: objects["karscredentialgrants.kars.azure.com/workspace"] };
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(previewPrivateActivation).mockResolvedValue(activation as any);
});

describe("workspace provider installation", () => {
  it("creates reusable empty stores without configuring credentials or granting agent GitHub access", async () => {
    const f = fixture();
    const document = await prepareWorkspaceProviderGrant(f.execute, options);
    expect(document.spec.writers).toEqual([{ namespace: "kars-system", name: "actual-bridge-account", uid: "actual-bridge-account-uid" }]);
    expect(document.spec.integrationStores.map((entry: any) => entry.purpose)).toEqual(["providers", "controller-settings", "github-app"]);
    expect(document.spec.githubConnections).toEqual([]);
    expect(document.spec.integrationStores[2]).toEqual({ secret: { name: "kars-github-app", uid: "kars-github-app-uid" }, purpose: "github-app" });
    expect(document.spec.controller).toEqual({ name: "kars-controller", uid: "kars-controller-uid" });
    expect(document.spec.agentKeys).toEqual(["CUSTOM_TOKEN"]);
    expect(document.spec.bridgeConsumers).toBeUndefined();
    expect(f.execute.mock.calls.filter(([args]) => args[0] === "create")).toHaveLength(3);
    expect(f.objects["secret/kars-inference-providers"].data).toBeUndefined();
    expect(f.objects["secret/kars-github-app"].data).toBeUndefined();
    expect(validateGrantDocument).toHaveBeenCalledWith(f.execute, document);
    expect(applyReviewedGrant).not.toHaveBeenCalled();
  });

  it("preserves existing provider data and other enrolled capabilities", async () => {
    const f = fixture();
    f.objects["secret/kars-inference-providers"] = { ...object("kars-inference-providers"), type: "Opaque", data: { KARS_PROVIDER_OPENAI_API_KEY: "c2VjcmV0" } };
    f.grant.spec.integrationStores.push({ secret: { name: "teams-store", uid: "teams-uid" }, purpose: "teams" });
    const document = await prepareWorkspaceProviderGrant(f.execute, options);
    expect(document.spec.integrationStores).toHaveLength(4);
    expect(f.objects["secret/kars-inference-providers"].data).toEqual({ KARS_PROVIDER_OPENAI_API_KEY: "c2VjcmV0" });
    expect(f.execute.mock.calls.filter(([args]) => args[0] === "create")).toHaveLength(2);
  });

  it("preserves the existing App store and explicitly enrolled GitHub connections", async () => {
    const f = fixture();
    const store = { ...object("kars-github-app"), type: "Opaque", data: { GITHUB_APP_ID: "MTIz", GITHUB_APP_PRIVATE_KEY: "dGVzdC1rZXk=" } };
    const original = structuredClone(store);
    f.objects["secret/kars-github-app"] = store;
    f.grant.spec.integrationStores.push({ secret: { name: "kars-github-app", uid: store.metadata.uid }, purpose: "github-app" });
    const connections = [{ connection: { name: "existing-connection", uid: "existing-connection-uid" } }];
    f.grant.spec.githubConnections = structuredClone(connections);
    const document = await prepareWorkspaceProviderGrant(f.execute, options);
    expect(f.objects["secret/kars-github-app"]).toEqual(original);
    expect(document.spec.githubConnections).toEqual(connections);
    expect(document.spec.integrationStores.filter((entry: any) => entry.purpose === "github-app")).toHaveLength(1);
    expect(JSON.stringify(document)).not.toContain("GITHUB_APP_PRIVATE_KEY");
    expect(f.execute.mock.calls.filter(([args]) => args[0] === "create")).toHaveLength(2);
  });

  it.each(["missing", "uid", "purpose", "type"])("refuses %s App-store authority without replacement or publication", async fault => {
    const f = fixture();
    const store = { ...object("kars-github-app"), type: "Opaque", data: { GITHUB_APP_ID: "MTIz" } };
    if (fault !== "missing") f.objects["secret/kars-github-app"] = store;
    f.grant.spec.integrationStores.push({ secret: { name: "kars-github-app", uid: fault === "uid" ? "replaced" : store.metadata.uid }, purpose: fault === "purpose" ? "providers" : "github-app" });
    if (fault === "type") store.type = "kubernetes.io/tls";
    const original = structuredClone(store);
    await expect(setupWorkspaceProviders(f.execute, options)).rejects.toThrow(/identity|purpose|Opaque/);
    expect(applyReviewedGrant).not.toHaveBeenCalled();
    expect(f.objects["secret/kars-github-app"]).toEqual(fault === "missing" ? undefined : original);
    expect(f.execute.mock.calls.some(([args, input]) => args[0] === "create" && JSON.parse(input!).metadata.name === "kars-github-app")).toBe(false);
  });

  it.each(["workspace", "writer", "store", "controller"])("refuses replaced %s authority without publishing a grant", async selected => {
    const f = fixture();
    if (selected === "workspace") f.grant.spec.workspaceUid = "replaced";
    if (selected === "writer") f.grant.spec.writers.push({ namespace: "kars-system", name: "actual-bridge-account", uid: "replaced" });
    if (selected === "controller") f.grant.spec.controller = { name: "kars-controller", uid: "replaced" };
    if (selected === "store") f.grant.spec.integrationStores.push({ secret: { name: "kars-inference-providers", uid: "missing" }, purpose: "providers" });
    await expect(setupWorkspaceProviders(f.execute, options)).rejects.toThrow(/replaced|identity/);
    expect(applyReviewedGrant).not.toHaveBeenCalled();
  });

  it("does not grant an anonymous or default Bridge identity", async () => {
    const f = fixture();
    f.objects["deployment/kars-bridge-bff"].spec.template.spec.serviceAccountName = "default";
    await expect(setupWorkspaceProviders(f.execute, options)).rejects.toThrow(/dedicated/);
    expect(f.execute.mock.calls.some(([args]) => args[0] === "create")).toBe(false);
  });

  it("rotates only after the real retirement signal and takes fresh reviews before resuming", async () => {
    const f = fixture(), rotate = vi.fn(async () => {});
    vi.mocked(applyReviewedGrant).mockRejectedValueOnce(new PrivateBudgetRotationRequiredError()).mockResolvedValueOnce();
    await setupWorkspaceProviders(f.execute, options, rotate);
    expect(rotate).toHaveBeenCalledExactlyOnceWith(activation);
    expect(previewPrivateActivation).toHaveBeenCalledTimes(3);
    expect(applyReviewedGrant).toHaveBeenCalledTimes(2);
    expect(rotate.mock.invocationCallOrder[0]).toBeGreaterThan(vi.mocked(applyReviewedGrant).mock.invocationCallOrder[0]!);
  });

  it("validates existing qualified authority without retiring writers or rotating again", async () => {
    const f = fixture(), rotate = vi.fn();
    const document = await prepareWorkspaceProviderGrant(f.execute, options);
    f.grant.spec = { ...document.spec, privateActivation: { ...activation, phase: "qualified" } };
    await setupWorkspaceProviders(f.execute, options, rotate);
    expect(validateQualifiedActivation).toHaveBeenCalledExactlyOnceWith(f.execute, f.grant.spec.privateActivation);
    expect(applyReviewedGrant).not.toHaveBeenCalled();
    expect(rotate).not.toHaveBeenCalled();
    vi.mocked(validateQualifiedActivation).mockRejectedValueOnce(new Error("Private authority changed"));
    await expect(setupWorkspaceProviders(f.execute, options, rotate)).rejects.toThrow("Private authority changed");
    expect(applyReviewedGrant).not.toHaveBeenCalled();
  });

  it("never treats arbitrary activation errors as permission to rotate", async () => {
    const f = fixture(), rotate = vi.fn();
    vi.mocked(applyReviewedGrant).mockRejectedValueOnce(new Error("Unexplained private consumer preserved"));
    await expect(setupWorkspaceProviders(f.execute, options, rotate)).rejects.toThrow(/Unexplained/);
    expect(rotate).not.toHaveBeenCalled();
  });

  it("does not equate an empty Ready grant with usable provider storage", async () => {
    const f = fixture();
    f.grant.status = { observedGeneration: 1, conditions: [{ type: "Ready", status: "True" }, { type: "WriterReady", status: "False" }] };
    await expect(waitForWorkspaceProviders(f.execute, options.namespace)).rejects.toThrow(/installation is not complete/);
    expect(f.execute.mock.calls.some(([args]) => args.includes("--for=condition=PrivateConsumptionReady"))).toBe(true);
  });

  it("requires current-generation private writer and integration readiness", async () => {
    const f = fixture();
    f.grant.status = { observedGeneration: 1, conditions: ["Ready", "WriterReady", "IntegrationReady", "PrivateConsumptionReady"].map(type => ({ type, status: "True" })) };
    await expect(waitForWorkspaceProviders(f.execute, options.namespace)).resolves.toBeUndefined();
    f.grant.status.observedGeneration = 0;
    await expect(waitForWorkspaceProviders(f.execute, options.namespace)).rejects.toThrow(/changed/);
  });
});
