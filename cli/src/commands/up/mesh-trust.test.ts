// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DeploymentContext } from "../../config.js";
import type { Stepper } from "../../stepper.js";
import {
  helmUsesEntra, inspectUpMeshTrust, upMeshHelmArgs, validateMeshTrustOptions, verifyHelmMeshTrust,
} from "./mesh-trust.js";
import { deployAgentMesh } from "./agentmesh_deploy.js";
import { runFastUpgrade } from "./fast_upgrade.js";

interface Env { name: string; value?: string; valueFrom?: object }
const audience = "11111111-1111-4111-8111-111111111111";
const tenant = "22222222-2222-4222-8222-222222222222";
function deployment(component: string) {
  return {
    kind: "Deployment", metadata: metadata(component),
    spec: { replicas: 1, template: {
      metadata: { labels: { app: `agentmesh-${component}` } },
      spec: { containers: [{ name: component, image: `example.invalid/${component}:latest`,
        env: [{ name: "AGENTMESH_ENTRA_AUDIENCE", value: audience },
          { name: "AGENTMESH_ENTRA_TENANT_ID", value: tenant }] as Env[], envFrom: [] as object[] }] },
    } },
    status: { observedGeneration: 1, readyReplicas: 1, updatedReplicas: 1,
      conditions: [{ type: "Available", status: "True" }] },
  };
}
function metadata(name: string) {
  return { name, namespace: "agentmesh", generation: 1,
    labels: { "app.kubernetes.io/managed-by": "Helm" } as Record<string, string>,
    annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" } as Record<string, string> };
}
function service(component: string) {
  return { kind: "Service", metadata: metadata(`agentmesh-${component}`), spec: { selector: { app: `agentmesh-${component}` } } };
}
const state = vi.hoisted(() => ({
  execute: vi.fn(), prepare: vi.fn(), context: null as DeploymentContext | null,
  inventory: null as unknown, values: {} as Record<string, unknown>, manifest: "",
  deployments: [] as ReturnType<typeof deployment>[], services: [] as ReturnType<typeof service>[],
  fault: undefined as undefined | ((bin: string, args: string[]) => void),
  afterHelm: undefined as undefined | (() => void), controllerHealthy: true,
}));
vi.mock("execa", () => ({ execa: state.execute }));
vi.mock("../../config.js", async original => ({ ...await original<typeof import("../../config.js")>(), loadContext: () => state.context }));
vi.mock("../../lib/core-helm-schemas.js", () => ({ prepareCoreHelmSchemas: state.prepare }));
vi.mock("../../lib/private-root-upgrade-guard.js", () => ({ assertControllerMutationAllowed: vi.fn() }));
vi.mock("../../lib/namespace-ownership.js", () => ({ inspectNamespaceOwnership: async () => [] }));
vi.mock("../../lib/sre-authority.js", async original => ({ ...await original<typeof import("../../lib/sre-authority.js")>(), assertSafeMutation: vi.fn() }));
vi.mock("ora", () => ({ default: () => ({ start() { return this; }, succeed() {}, fail() {}, warn() {} }) }));

function freezeManifest() { state.manifest = state.deployments.map(item => JSON.stringify(item)).join("\n---\n"); }
function unmanaged(legacy = false) {
  state.values = { agentMesh: { enabled: false } };
  for (const resource of [...state.deployments, ...state.services]) {
    resource.metadata.annotations = {};
    resource.metadata.labels = legacy ? { "kars.azure.com/mesh-provider": "agt" } : {};
  }
}
function noMesh() { state.deployments = []; state.services = []; state.values = { agentMesh: { enabled: false } }; }
function readOnlyCalls() {
  expect(state.execute.mock.calls.every(([bin, args]) =>
    (bin === "helm" && ["list", "get"].includes(args[0])) || (bin === "kubectl" && args[0] === "get"))).toBe(true);
}
function helmUpgrades() { return state.execute.mock.calls.filter(([bin, args]) => bin === "helm" && args[0] === "upgrade"); }
function stepper() {
  return { step: vi.fn(), update: vi.fn(), done: vi.fn() } as unknown as Stepper;
}
async function verify() {
  const trust = await inspectUpMeshTrust(state.execute, "entra");
  await verifyHelmMeshTrust(state.execute, trust.mesh, true);
}

beforeEach(() => {
  vi.clearAllMocks();
  state.context = { acrLoginServer: "mirror.azurecr.io", aksCluster: "target-aks", resourceGroup: "rg", subscription: "11111111-1111-4111-8111-111111111111" } as DeploymentContext;
  state.inventory = [{ name: "kars", chart: "kars-0.1.0", status: "deployed" }];
  state.values = { agentMesh: { enabled: true }, entraSidecar: { enabled: true, authConfig: { meshAuthBackend: "EntraAgentIdentity" } } };
  state.deployments = [deployment("registry"), deployment("relay")];
  state.services = [service("registry"), service("relay")];
  state.fault = undefined;
  state.afterHelm = undefined;
  state.controllerHealthy = true;
  freezeManifest();
  state.prepare.mockResolvedValue(undefined);
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  state.execute.mockImplementation(async (bin: string, args: string[]) => {
    state.fault?.(bin, args);
    if (bin === "helm" && args[0] === "list") return { stdout: JSON.stringify(state.inventory) };
    if (bin === "helm" && args[0] === "get") return { stdout: args[1] === "manifest" ? state.manifest : JSON.stringify(state.values) };
    if (bin === "helm" && args[0] === "upgrade") { state.afterHelm?.(); return { stdout: "" }; }
    if (bin === "kubectl" && args[0] === "get") {
      if (args[1] === "deployment,service") return { stdout: JSON.stringify({ items: [...state.deployments, ...state.services] }) };
      if (args[1] === "deployment" && args[2] === "kars-controller") return { stdout: state.controllerHealthy ? "True" : "False" };
      if (args[1] === "deployment") return { stdout: JSON.stringify(state.deployments.find(item => item.metadata.name === args[2])) };
      if (args[1] === "pods") return { stdout: "" };
      if (["deployments", "karssandbox"].includes(args[1]!)) return { stdout: '{"items":[]}' };
    }
    if (bin === "kubectl" && args[0] === "rollout") return { stdout: "" };
    if (bin === "az" && args[0] === "aks" && args[1] === "get-credentials") return { stdout: "" };
    throw new Error(`Unexpected command: ${bin} ${args.join(" ")}`);
  });
});
afterEach(() => vi.restoreAllMocks());

describe("up mesh options and activation", () => {
  it("defaults to anonymous without rewriting an installed Entra backend", async () => {
    expect(validateMeshTrustOptions({})).toBe("anonymous");
    const trust = await inspectUpMeshTrust(state.execute, "anonymous");
    expect(trust.mesh.kind).toBe("helm");
    expect(helmUsesEntra(state.values)).toBe(true);
    expect(upMeshHelmArgs(trust)).toEqual(["--reuse-values", "--set", "agentMesh.enabled=true"]);
    readOnlyCalls();
  });
  it.each([
    { meshTrust: "invalid" }, { meshProvider: "legacy" }, { globalRegistry: "" },
    { globalRegistry: "invalid" }, { globalRegistry: "file:///tmp/registry" },
    { globalRegistry: "https://user:secret@example.invalid" }, { globalRegistry: "https://example.invalid/#fragment" },
    { meshTrust: "entra", globalRegistry: "https://example.invalid" }, { exposeRegistry: true }, { serviceTree: "guid" },
  ])("rejects unsupported options before commands: %j", async options => {
    expect(() => validateMeshTrustOptions(options)).toThrow();
    await expect(runFastUpgrade(options)).rejects.toThrow();
    expect(state.execute).not.toHaveBeenCalled();
    expect(state.prepare).not.toHaveBeenCalled();
  });
  it.each([
    { entraSidecar: [] }, { entraSidecar: { enabled: "true" } }, { entraSidecar: { authConfig: [] } },
    { entraSidecar: { enabled: true, authConfig: { meshAuthBackend: "unknown" } } },
    { entraSidecar: { enabled: false, authConfig: { meshAuthBackend: "EntraAgentIdentity" } }, agentMesh: { enabled: true } },
    { entraSidecar: { enabled: true, authConfig: { meshAuthBackend: "EntraAgentIdentity" } }, agentMesh: { enabled: false } },
  ])("rejects malformed or incomplete activation: %j", values => {
    expect(() => helmUsesEntra(values)).toThrow();
  });
});

describe("up installed mesh inventory", () => {
  it.each([{}, [null], [{ name: "other", chart: "kars-0.1.0", status: "deployed" }],
    [{ name: "kars", chart: "foreign-1", status: "deployed" }],
    ["deployed", "deployed"], ...["failed", "pending-install", "pending-upgrade", "uninstalled"].map(status => [{ name: "kars", chart: "kars-0.1.0", status }]),
  ])("rejects unverified release inventory %j", async inventory => {
    state.inventory = inventory;
    await expect(inspectUpMeshTrust(state.execute, "anonymous")).rejects.toThrow();
    readOnlyCalls();
  });
  it.each(["helm", "kubectl"])("propagates %s permission failures", async command => {
    state.fault = bin => { if (bin === command) throw new Error("Forbidden"); };
    await expect(inspectUpMeshTrust(state.execute, "anonymous")).rejects.toThrow("Forbidden");
    readOnlyCalls();
  });
  it("checks real mesh even when the Core release inventory is empty", async () => {
    state.inventory = [];
    unmanaged();
    await expect(inspectUpMeshTrust(state.execute, "anonymous")).rejects.toThrow("external ownership");
    expect(state.execute.mock.calls.some(([, args]) => args[1] === "deployment,service")).toBe(true);
  });
  it("creates an absent mesh only through ordinary Core Helm arguments", async () => {
    state.inventory = [];
    noMesh();
    const trust = await inspectUpMeshTrust(state.execute, "anonymous");
    expect(upMeshHelmArgs(trust)).toEqual(["--set", "agentMesh.enabled=true"]);
    await expect(inspectUpMeshTrust(state.execute, "entra")).rejects.toThrow("reviewed same-release");
    readOnlyCalls();
  });
  it("keeps positively identified legacy infrastructure without Helm adoption", async () => {
    state.inventory = [];
    unmanaged(true);
    const trust = await inspectUpMeshTrust(state.execute, "anonymous");
    expect(trust.mesh.kind).toBe("legacy");
    expect(upMeshHelmArgs(trust)).toEqual(["--set", "agentMesh.enabled=false"]);
  });
  it("rejects enabled values without corresponding owned resources", async () => {
    state.deployments = []; state.services = [];
    await expect(inspectUpMeshTrust(state.execute, "anonymous")).rejects.toThrow();
  });
  it("does not silently switch managed mesh to an external registry", async () => {
    await expect(inspectUpMeshTrust(state.execute, "anonymous", "https://external.invalid")).rejects.toThrow("reviewed migration");
  });
  it("permits explicit external selection without adoption", async () => {
    unmanaged();
    const trust = await inspectUpMeshTrust(state.execute, "anonymous", "https://external.invalid");
    expect(trust.mesh.kind).toBe("external");
    expect(upMeshHelmArgs(trust, "https://external.invalid")).toEqual(["--reuse-values", "--set", "agentMesh.enabled=false"]);
  });
});

describe("installed Entra verifier configuration", () => {
  it("checks both deployed components against the installed manifest and readiness", async () => {
    await verify();
    for (const name of ["registry", "relay"]) {
      expect(state.execute.mock.calls.filter(([bin, args]) => bin === "kubectl" && args[1] === "deployment" && args[2] === name)).toHaveLength(2);
    }
    readOnlyCalls();
  });
  it.each(["missing", "duplicate", "indirect", "invalid", "envFrom", "override", "duplicate-container", "missing-container"])("rejects %s verifier input in the installed declaration", async fault => {
    const trust = await inspectUpMeshTrust(state.execute, "entra");
    const pod = state.deployments[0]!.spec.template.spec;
    const container = pod.containers[0]!;
    if (fault === "missing") container.env.pop();
    if (fault === "duplicate") container.env.push({ ...container.env[0]! });
    if (fault === "indirect") container.env[0] = { name: "AGENTMESH_ENTRA_AUDIENCE", valueFrom: { secretKeyRef: { name: "config", key: "audience" } } };
    if (fault === "invalid") container.env[0]!.value = "api://resource/.default";
    if (fault === "envFrom") container.envFrom.push({ configMapRef: { name: "config" } });
    if (fault === "override") container.env.push({ name: "AGENTMESH_RELAY_ALLOW_UNAUTHED_DID", value: "false" });
    if (fault === "duplicate-container") pod.containers.push(structuredClone(container));
    if (fault === "missing-container") container.name = "other";
    freezeManifest();
    await expect(verifyHelmMeshTrust(state.execute, trust.mesh, true)).rejects.toThrow();
    readOnlyCalls();
  });
  it("accepts a consistently named single container discovered by the maintained mesh lifecycle", async () => {
    state.deployments[0]!.spec.template.spec.containers[0]!.name = "custom-registry";
    freezeManifest();
    await expect(verify()).resolves.toBeUndefined();
    readOnlyCalls();
  });
  it.each(["missing", "duplicate", "invalid-yaml"])("rejects %s manifest", async fault => {
    if (fault === "missing") state.manifest = JSON.stringify(state.deployments[0]);
    if (fault === "duplicate") state.manifest += `\n---\n${JSON.stringify(state.deployments[0])}`;
    if (fault === "invalid-yaml") state.manifest = "items: [";
    await expect(verify()).rejects.toThrow();
  });
  it("rejects a different pair on the second component", async () => {
    state.deployments[1]!.spec.template.spec.containers[0]!.env[0]!.value = tenant;
    freezeManifest();
    await expect(verify()).rejects.toThrow("components disagree");
  });
  it("rejects live template drift without accepting the new pair", async () => {
    state.deployments[1]!.spec.template.spec.containers[0]!.env[0]!.value = tenant;
    await expect(verify()).rejects.toThrow("differs from the installed");
  });
  it.each(["unready", "old-generation", "old-replicas"])("rejects %s workloads", async fault => {
    const workload = state.deployments[1]!;
    if (fault === "unready") workload.status.readyReplicas = 0;
    if (fault === "old-generation") workload.status.observedGeneration = 0;
    if (fault === "old-replicas") workload.status.updatedReplicas = 0;
    await expect(verify()).rejects.toThrow();
  });
});

describe("mesh deployment verification phase", () => {
  it.each(["helm", "legacy"])("verifies %s mesh using only reads", async kind => {
    if (kind === "legacy") unmanaged(true);
    await expect(deployAgentMesh({ stepper: stepper() }, {})).resolves.toEqual({ registryMode: "local" });
    readOnlyCalls();
  });
  it("reports external selection without claiming trust or fabricating relay URLs", async () => {
    noMesh();
    const progress = stepper();
    await expect(deployAgentMesh({ stepper: progress }, { globalRegistry: "https://external.invalid" }))
      .resolves.toEqual({ registryMode: "global", globalRegistryUrl: "https://external.invalid" });
    expect(progress.done).toHaveBeenCalledWith(expect.stringContaining("remote trust and availability not verified"));
    readOnlyCalls();
  });
  it("stops sandbox creation if local mesh is absent", async () => {
    noMesh();
    await expect(deployAgentMesh({ stepper: stepper() }, {})).rejects.toThrow("sandbox creation stopped");
    readOnlyCalls();
  });
  it("refuses automatic ingress publication before any reads or writes", async () => {
    await expect(deployAgentMesh({ stepper: stepper() }, { exposeRegistry: true })).rejects.toThrow("temporarily unavailable");
    expect(state.execute).not.toHaveBeenCalled();
  });
});

describe("fast upgrade Workload Identity", () => {
  const issuer = "https://eastus2.oic.prod-aks.azure.com/tenant/cluster/";
  function identityFixture() {
    Object.assign(state.context!, { identityName: "sandbox-wi", identityResourceGroup: "identity-rg", oidcIssuerUrl: issuer });
    const fallback = state.execute.getMockImplementation()!;
    const scope = `/subscriptions/${audience}/resourceGroups/identity-rg/providers/Microsoft.ManagedIdentity/userAssignedIdentities/sandbox-wi`;
    const credentials: object[] = [];
    const roles: object[] = [];
    state.execute.mockImplementation(async (bin: string, args: string[]) => {
      state.fault?.(bin, args);
      if (bin === "kubectl" && args[0] === "get" && args[1] === "karssandbox") return { stdout: '{"items":[{"metadata":{"name":"agent"}}]}' };
      if (bin !== "az" || args[0] === "aks") return fallback(bin, args);
      expect(args.filter(arg => arg === "--subscription")).toHaveLength(1);
      expect(args[args.indexOf("--subscription") + 1]).toBe(audience);
      const value = (key: string) => args[args.indexOf(key) + 1]!;
      let result: unknown;
      const command = args.slice(0, 3).join(" ");
      if (args[0] === "identity" && args[1] === "show") result = { id: scope, principalId: tenant };
      else if (command === "identity federated-credential list") result = credentials;
      else if (command === "role assignment list") result = roles;
      else if (command === "identity federated-credential create") credentials.push({ name: value("--name"), subject: value("--subject"), issuer: value("--issuer"), audiences: [value("--audiences")] });
      else if (command === "role assignment create") roles.push({ principalId: tenant, scope, roleDefinitionId: `/subscriptions/${audience}/providers/Microsoft.Authorization/roleDefinitions/${value("--role")}` });
      else throw new Error(`Unexpected identity command: ${args.join(" ")}`);
      return { stdout: JSON.stringify(result) ?? "" };
    });
    return { credentials, roles };
  }
  it.each([undefined, "", "   "])("rejects missing cached subscription %j before effects", async subscription => {
    state.context!.subscription = subscription;
    await expect(runFastUpgrade({})).rejects.toThrow("will not use the ambient account");
    expect(state.execute).not.toHaveBeenCalled();
    expect(state.prepare).not.toHaveBeenCalled();
  });
  it("keeps legacy-context dry-run command-free", async () => {
    delete state.context!.subscription;
    await runFastUpgrade({ dryRun: true });
    expect(state.execute).not.toHaveBeenCalled();
  });
  it.each([{ identityName: "sandbox-wi" }, { oidcIssuerUrl: issuer }, { identityResourceGroup: "identity-rg" }])("refuses incomplete cached federation %j before effects", async patch => {
    Object.assign(state.context!, patch);
    await expect(runFastUpgrade({})).rejects.toThrow("Workload Identity");
    expect(state.execute).not.toHaveBeenCalled();
    expect(state.prepare).not.toHaveBeenCalled();
  });
  it("uses the same selected identity in Helm and verified ARM provisioning", async () => {
    const fixture = identityFixture();
    await runFastUpgrade({});
    expect(helmUpgrades()[0]![1]).toEqual(expect.arrayContaining([
      `fedcred.subscriptionId=${audience}`, "fedcred.identityName=sandbox-wi",
      "fedcred.identityResourceGroup=identity-rg", `fedcred.oidcIssuerUrl=${issuer}`,
    ]));
    expect(fixture.credentials).toHaveLength(2);
    expect(fixture.roles).toHaveLength(1);
    expect(vi.mocked(console.log).mock.calls.flat().join(" ")).toContain("Fast upgrade complete");
    const reads = state.execute.mock.calls.filter(([bin, args]) => bin === "az" && args[1] === "show");
    expect(reads).toHaveLength(2);
  });
  it.each(["sandbox inventory", "identity read", "credential create", "role create", "readback"])("does not report completion after %s fails", async fault => {
    identityFixture();
    let inventories = 0;
    state.fault = (bin, args) => {
      if (bin === "az" && args.slice(0, 3).join(" ") === "identity federated-credential list") inventories++;
      if ((fault === "sandbox inventory" && bin === "kubectl" && args[1] === "karssandbox")
        || (fault === "identity read" && bin === "az" && args[1] === "show")
        || (fault === "credential create" && args.slice(0, 3).join(" ") === "identity federated-credential create")
        || (fault === "role create" && args.slice(0, 3).join(" ") === "role assignment create")
        || (fault === "readback" && inventories === 2)) throw new Error("identity failure");
    };
    await expect(runFastUpgrade({})).rejects.toThrow();
    expect(vi.mocked(console.log).mock.calls.flat().join(" ")).not.toContain("Fast upgrade complete");
  });
});

describe("fast upgrade trust preservation", () => {
  it("does not acquire credentials, read the cluster or change schemas during dry-run", async () => {
    await runFastUpgrade({ dryRun: true });
    expect(state.execute).not.toHaveBeenCalled();
    expect(state.prepare).not.toHaveBeenCalled();
  });
  it("refuses an external registry change before acquiring credentials", async () => {
    await expect(runFastUpgrade({ globalRegistry: "https://different.invalid" })).rejects.toThrow("cannot change");
    expect(state.execute).not.toHaveBeenCalled();
  });
  it("preserves installed Entra without resetting mesh settings", async () => {
    await runFastUpgrade({});
    expect(helmUpgrades()).toHaveLength(1);
    const args = helmUpgrades()[0]![1];
    expect(args).toContain("--reuse-values");
    expect(args.some((arg: string) => /^(agentMesh|entraSidecar)\./.test(arg))).toBe(false);
    const preflight = state.execute.mock.calls.findIndex(([, args]) => args[0] === "get" && args[1] === "manifest");
    const apply = state.execute.mock.calls.findIndex(([bin, args]) => bin === "helm" && args[0] === "upgrade");
    expect(preflight).toBeLessThan(apply);
    expect(state.prepare.mock.invocationCallOrder[0]).toBeLessThan(state.execute.mock.invocationCallOrder[apply]!);
  });
  it.each(["disabled", "external"])("preserves %s Core mesh without enabling or restarting it", async mode => {
    noMesh();
    if (mode === "external") Object.assign(state.context!, { registryMode: "global", globalRegistryUrl: "https://external.invalid" });
    await runFastUpgrade({});
    expect(helmUpgrades()[0]![1]).not.toContain("agentMesh.enabled=true");
    expect(state.execute.mock.calls.some(([bin, args]) => bin === "kubectl" && args[0] === "rollout" && args.includes("agentmesh"))).toBe(false);
  });
  it("refuses drifted preflight trust before schemas or Helm", async () => {
    state.deployments[0]!.spec.template.spec.containers[0]!.env.pop();
    await expect(runFastUpgrade({})).rejects.toThrow();
    expect(state.prepare).not.toHaveBeenCalled();
    expect(helmUpgrades()).toHaveLength(0);
  });
  it("rechecks mesh ownership after schema preparation before applying Helm", async () => {
    state.prepare.mockImplementation(async () => { state.deployments[0]!.metadata.annotations["meta.helm.sh/release-name"] = "foreign"; });
    await expect(runFastUpgrade({})).rejects.toThrow("ownership");
    expect(helmUpgrades()).toHaveLength(0);
  });
  it("refuses a post-upgrade Entra downgrade even when trust was omitted", async () => {
    state.afterHelm = () => { state.values.entraSidecar = { enabled: false, authConfig: { meshAuthBackend: "Anonymous" } }; };
    await expect(runFastUpgrade({})).rejects.toThrow("reviewed same-release");
    expect(state.execute.mock.calls.some(([bin, args]) => bin === "kubectl" && args[0] === "rollout")).toBe(false);
  });
  it.each(["rollout", "controller", "mesh"])("does not report success after a %s failure", async fault => {
    if (fault === "rollout") state.fault = (bin, args) => { if (bin === "kubectl" && args[0] === "rollout" && args[1] === "status") throw new Error("rollout timeout"); };
    if (fault === "controller") state.afterHelm = () => { state.controllerHealthy = false; };
    if (fault === "mesh") state.afterHelm = () => { state.deployments[1]!.status.readyReplicas = 0; };
    await expect(runFastUpgrade({})).rejects.toThrow();
    expect(vi.mocked(console.log).mock.calls.flat().join(" ")).not.toContain("Fast upgrade complete");
  });
});
