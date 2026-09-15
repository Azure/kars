// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import fs from "fs";

const state = vi.hoisted(() => ({
  execute: vi.fn(),
  effects: [] as string[],
  context: undefined as undefined | { acrName: string; acrLoginServer: string; aksCluster: string; resourceGroup: string; subscription: string },
  namespace: "",
  unavailable: false,
  stopAtEffect: false,
  preflight: vi.fn(),
  preparePushTarget: vi.fn(),
  prepareSchemas: vi.fn(),
  rollbackSchemas: vi.fn(),
  buildPreparation: vi.fn(),
}));

vi.mock("execa", () => ({ execa: state.execute }));
vi.mock("../config.js", async original => ({ ...await original<typeof import("../config.js")>(), loadContext: () => state.context }));
vi.mock("../lib/deployment-target.js", async original => ({
  ...await original<typeof import("../lib/deployment-target.js")>(),
  connectDeploymentTarget: async () => ({ execute: state.execute, subscription: "sub", kubeContext: "target" }),
  preparePushTarget: state.preparePushTarget,
}));
vi.mock("../lib/agt-bootstrap.js", () => ({ ensureAgtRepo: state.buildPreparation, ensureAgtWheels: vi.fn() }));
vi.mock("../lib/stage-rust-bin.js", () => ({ stageRustBinaries: async () => {} }));
vi.mock("../lib/core-helm-schemas.js", () => ({
  prepareCoreHelmSchemas: state.prepareSchemas, prepareCoreRollbackSchemas: state.rollbackSchemas,
}));
vi.mock("../lib/sre-authority.js", async original => ({
  ...await original<typeof import("../lib/sre-authority.js")>(),
  assertSafeMutation: async () => {}, assertRollbackSafe: async () => {},
}));
vi.mock("../lib/namespace-ownership.js", async original => ({
  ...await original<typeof import("../lib/namespace-ownership.js")>(), inspectNamespaceOwnership: async () => [],
}));
vi.mock("../lib/mesh-release.js", async original => ({
  ...await original<typeof import("../lib/mesh-release.js")>(),
  inspectMeshInstallation: async () => ({ kind: "absent", namespace: "agentmesh", deployments: [] }),
  readReleaseValues: async () => ({ agentMesh: { enabled: false } }),
}));
vi.mock("../lib/release.js", async original => ({
  ...await original<typeof import("../lib/release.js")>(),
  fetchRecentReleases: async () => [], fetchTagMessage: async () => null,
}));
vi.mock("./up/preflight.js", () => ({ runPreflight: state.preflight }));
vi.mock("../stepper.js", () => ({
  Stepper: class {
    step() {} detail() {} update() {} done() {} stop() {} summary() {} warn() {} fail() {}
  }, banner: vi.fn(), section: vi.fn(), kvLine: vi.fn(),
}));
vi.mock("ora", () => ({ default: () => ({
  start() { return this; }, succeed() {}, fail() {}, warn() {}, stop() {}, text: "",
}) }));

import { upgradeCommand, buildHelmUpgradeArgs } from "./upgrade.js";
import { pushCommand } from "./push.js";
import { upCommand } from "./up.js";
import { runFastUpgrade } from "./up/fast_upgrade.js";
import { acquireImages } from "./up/images.js";
import { Stepper } from "../stepper.js";

beforeEach(() => {
  vi.clearAllMocks();
  state.effects.length = 0;
  state.context = { acrName: "mirror", acrLoginServer: "mirror.azurecr.io", aksCluster: "target-aks", resourceGroup: "rg", subscription: "sub" };
  state.namespace = JSON.stringify({ kind: "Namespace", metadata: {
    name: "kars-system", uid: "root-ns", resourceVersion: "1",
    annotations: { "kars.azure.com/private-root-retirement": '{"version":2}' },
  } });
  state.unavailable = false;
  state.stopAtEffect = false;
  state.preflight.mockImplementation(async options => options.dryRun ? null : { rg: "rg" });
  state.preparePushTarget.mockResolvedValue(state.execute);
  state.prepareSchemas.mockImplementation(async () => { state.effects.push("schemas"); });
  state.rollbackSchemas.mockImplementation(async () => { state.effects.push("rollback-schemas"); return 1; });
  state.buildPreparation.mockImplementation(async () => { state.effects.push("build-preparation"); throw new Error("no local AGT fixture"); });
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(process, "exit").mockImplementation(code => { throw new Error(`test exit:${code}`); });
  state.execute.mockImplementation(async (file: string, args: string[]) => {
    if (file === "kubectl" && args[0] === "get") {
      if (args[1] === "namespace") {
        if (state.unavailable) throw { stderr: "Error from server (Forbidden): PRIVATE_SENTINEL", exitCode: 1 };
        return { stdout: state.namespace };
      }
      if (args[1] === "deployment") {
        if (args.some(arg => arg.includes("jsonpath="))) return { stdout: args.some(arg => arg.includes("conditions")) ? "True" : "mirror.azurecr.io/kars-controller:latest" };
        return { stdout: JSON.stringify({ kind: "Deployment", metadata: {
          name: "kars-controller", namespace: "kars-system", uid: "controller-uid", resourceVersion: "1",
        }, spec: { replicas: 1, template: { spec: { containers: [{ name: "controller", image: "mirror.azurecr.io/kars-controller:latest" }] } } } }) };
      }
      if (args[1] === "nodes") return { stdout: "worker|True\n" };
      if (["deployments", "karssandbox", "deployment,service"].includes(args[1]!)) return { stdout: '{"items":[]}' };
      return { stdout: "" };
    }
    if (file === "helm") {
      if (args[0] === "list") return { stdout: '[{"name":"kars","revision":"2","app_version":"0.1.0"}]' };
      if (args[0] === "get") return { stdout: '{"karsRelease":"v1.2.2"}' };
      if (args.includes("--dry-run=server")) return { stdout: "" };
    }
    if (file === "az" && args[0] === "aks") return { stdout: args[1] === "show" ? "Succeeded" : "" };
    if (file === "az" && args[0] === "group" && args[1] === "show") return { stdout: "Succeeded" };
    state.effects.push([file, ...args.slice(0, 2)].join(" "));
    if (state.stopAtEffect || (file === "az" && args[1] === "login")) throw new Error("test effect boundary");
    return { stdout: "" };
  });
});

afterEach(() => vi.restoreAllMocks());

describe("real controller-changing command preflights", () => {
  it.each([
    ["upgrade", ["--yes", "--to", "v1.2.3"]],
    ["forced upgrade", ["--yes", "--force", "--force-conflicts", "--to", "v1.2.3"]],
    ["rollback", ["--rollback"]],
  ])("stops %s before image publication, schemas, Helm mutation or rollouts", async (_name, args) => {
    await expect(upgradeCommand().parseAsync(args, { from: "user" })).rejects.toThrow("test exit:1");
    expect(state.effects).toEqual([]);
    expect(state.prepareSchemas).not.toHaveBeenCalled();
    expect(state.rollbackSchemas).not.toHaveBeenCalled();
    const diagnostic = vi.mocked(console.error).mock.calls.flat().join(" ");
    expect(diagnostic).toContain("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(diagnostic).not.toContain("auto-rolls-back");
  });

  it.each(["missing permission", "malformed response"])("does not turn %s into an upgrade permission", async fault => {
    state.unavailable = fault === "missing permission";
    if (fault === "malformed response") state.namespace = "[]";
    await expect(upgradeCommand().parseAsync(["--yes", "--to", "v1.2.3"], { from: "user" })).rejects.toThrow();
    expect(state.effects).toEqual([]);
    expect(vi.mocked(console.error).mock.calls.flat().join(" ")).toContain("KARS_PRIVATE_ROOT_CHECK_UNAVAILABLE");
    expect(vi.mocked(console.error).mock.calls.flat().join(" ")).not.toContain("PRIVATE_SENTINEL");
  });

  it("preserves unqualified upgrade image-import and Helm arguments", async () => {
    state.namespace = JSON.stringify({ kind: "Namespace", metadata: { name: "kars-system", uid: "root-ns", resourceVersion: "1" } });
    await upgradeCommand().parseAsync(["--yes", "--to", "v1.2.3"], { from: "user" }).catch(() => {});
    expect(vi.mocked(process.exit).mock.calls[0]?.[0], vi.mocked(console.error).mock.calls.flat().join(" ")).toBe(0);
    expect(state.execute.mock.calls).toContainEqual(["az", [
      "acr", "import", "--name", "mirror", "--source", "ghcr.io/azure/kars-controller:v1.2.3",
      "--image", "kars-controller:latest", "--force",
    ], { stdio: "pipe" }]);
    const helm = state.execute.mock.calls.find(([file, args]) => file === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server"))!;
    expect(helm[1]).toEqual(buildHelmUpgradeArgs(state.context!, helm[1][3], "v1.2.3", {
      mesh: { kind: "absent", namespace: "agentmesh", deployments: [] },
    }));
    expect(state.effects.indexOf("schemas")).toBeLessThan(state.effects.indexOf("helm upgrade --install"));
  });

  it.each([{ args: ["--dry-run", "--to", "v1.2.3"] }, { args: ["--dry-run", "--rollback"] }])("retains readonly upgrade preview $args", async ({ args }) => {
    await upgradeCommand().parseAsync(args, { from: "user" }).catch(() => {});
    if (!args.includes("--rollback")) expect(vi.mocked(process.exit).mock.calls[0]?.[0]).toBe(0);
    expect(state.effects).toEqual([]);
    expect(state.execute.mock.calls.some(([file, args]) => file === "kubectl" && args[1] === "namespace")).toBe(false);
  });

  it("stops fast upgrade before schemas and every later mutation", async () => {
    await expect(runFastUpgrade({})).rejects.toThrow("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(state.effects).toEqual([]);
    expect(state.prepareSchemas).not.toHaveBeenCalled();
  });

  it("keeps fast-upgrade readonly preview non-mutating", async () => {
    await upCommand().parseAsync(["--upgrade", "--dry-run"], { from: "user" });
    expect(state.effects).toEqual([]);
    expect(state.prepareSchemas).not.toHaveBeenCalled();
  });

  it("preserves unqualified fast-upgrade Helm flags and rollout behavior", async () => {
    state.namespace = JSON.stringify({ kind: "Namespace", metadata: { name: "kars-system", uid: "root-ns", resourceVersion: "1" } });
    await runFastUpgrade({});
    const args = state.prepareSchemas.mock.calls[0]![1];
    expect(args.slice(0, 7)).toEqual(["upgrade", "--install", "kars", expect.any(String), "--namespace", "kars-system", "--create-namespace"]);
    expect(args).toEqual(expect.arrayContaining([
      "--reuse-values", "controller.image.repository=mirror.azurecr.io/kars-controller", "controller.image.tag=latest",
      "sandbox.image.repository=mirror.azurecr.io/openclaw-sandbox", "azure.workloadIdentity.clientId=",
      "--atomic", "--wait", "--timeout", "8m",
    ]));
    expect(state.execute.mock.calls).toContainEqual(["helm", args, { stdio: "pipe" }]);
    expect(state.execute.mock.calls).toContainEqual(["kubectl",
      ["rollout", "restart", "deployment/kars-controller", "-n", "kars-system"], { stdio: "pipe" }]);
  });

  it.each([[], ["--skip-infra"], ["--force-infra"], ["--from-scratch"]].map(args => ({ args })))("stops full up $args before resource provisioning and image acquisition", async ({ args }) => {
    await expect(upCommand().parseAsync(["--cluster-name", "target-aks", ...args], { from: "user" }))
      .rejects.toThrow("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(state.effects).toEqual([]);
    expect(state.prepareSchemas).not.toHaveBeenCalled();
  });

  it("keeps full up readonly preview independent of a protected root", async () => {
    await upCommand().parseAsync(["--dry-run"], { from: "user" });
    expect(state.execute).not.toHaveBeenCalled();
    expect(state.effects).toEqual([]);
  });

  it.each(["existing unqualified", "fresh absent"])("preserves %s up provisioning arguments after preflight", async kind => {
    state.namespace = JSON.stringify({ kind: "Namespace", metadata: { name: "kars-system", uid: "root-ns", resourceVersion: "1" } });
    state.stopAtEffect = true;
    if (kind === "fresh absent") {
      const run = state.execute.getMockImplementation()!;
      state.execute.mockImplementation((file, args) => {
        if (file === "az" && args[0] === "aks" && args[1] === "show") throw { stderr: "ERROR: (ResourceNotFound) absent" };
        return run(file, args);
      });
    }
    await upCommand().parseAsync(["--cluster-name", "target-aks", "--region", "eastus2"], { from: "user" }).catch(() => {});
    expect(state.effects).toEqual(["az group create"]);
    expect(state.execute.mock.calls).toContainEqual(["az", [
      "group", "create", "--name", "rg", "--location", "eastus2", "--output", "none",
    ], { stdio: "pipe" }]);
    if (kind === "fresh absent") expect(state.execute.mock.calls.some(([file]) => file === "kubectl")).toBe(false);
  });

  it.each(["controller", "router", "sandbox", "runtime-openai-agents", "mcp-everything"])("stops push --only %s --apply before build preparation and ACR login", async name => {
    await expect(pushCommand().parseAsync(["--only", name, "--apply"], { from: "user" }))
      .rejects.toThrow("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(state.effects).toEqual([]);
    expect(state.buildPreparation).not.toHaveBeenCalled();
  });

  it("also stops context-bound mutable controller publication without --apply", async () => {
    await expect(pushCommand().parseAsync(["--only", "controller"], { from: "user" }))
      .rejects.toThrow("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(state.effects).toEqual([]);
    expect(state.preparePushTarget.mock.calls[0]?.[2]).toMatchObject({ apply: true });
  });

  it("preserves root-unchanged sandbox publication and standalone controller publication", async () => {
    for (const standalone of [false, true]) {
      state.effects.length = 0;
      state.execute.mockClear();
      if (standalone) state.context = undefined;
      await pushCommand().parseAsync(["--only", standalone ? "controller" : "sandbox", "--acr", "mirror",
        "--subscription", "sub"], { from: "user" }).catch(() => {});
      expect(state.effects).toContain("build-preparation");
      expect(state.execute.mock.calls).toContainEqual(["az", ["acr", "login", "--name", "mirror"], { stdio: "pipe" }]);
      expect(state.effects.some(effect => effect.startsWith("helm ") || effect.startsWith("kubectl "))).toBe(false);
      if (standalone) expect(state.execute.mock.calls.some(([file]) => file === "kubectl")).toBe(false);
    }
  });

  it.each(["protected", "unavailable"])("rechecks a root that becomes %s during build and never retries the refusal", async fault => {
    const protectedNamespace = state.namespace;
    state.namespace = JSON.stringify({ kind: "Namespace", metadata: { name: "kars-system", uid: "root-ns", resourceVersion: "1" } });
    vi.spyOn(fs, "readdirSync").mockReturnValue([]);
    const run = state.execute.getMockImplementation()!;
    state.execute.mockImplementation((file, args) => {
      if (file === "az" && args[0] === "acr" && args[1] === "login") return { stdout: "" };
      if (file === "docker" && args[0] === "build") {
        state.effects.push("docker build");
        if (fault === "protected") state.namespace = protectedNamespace;
        else state.unavailable = true;
        return { stdout: "" };
      }
      return run(file, args);
    });
    await expect(pushCommand().parseAsync(["--only", "controller", "--apply"], { from: "user" }))
      .rejects.toThrow(fault === "protected" ? "KARS_PRIVATE_ROOT_UPGRADE_BLOCKED" : "KARS_PRIVATE_ROOT_CHECK_UNAVAILABLE");
    expect(state.effects).toContain("docker build");
    expect(state.execute.mock.calls.filter(([file, args]) => file === "az" && args[1] === "login")).toHaveLength(1);
    expect(state.execute.mock.calls.some(([file, args]) => file === "docker" && args[0] === "push")).toBe(false);
    expect(state.prepareSchemas).not.toHaveBeenCalled();
  });

  it.each([{}, { release: true }, { build: true }])("guards the image-acquisition helper itself for %j", async options => {
    await expect(acquireImages({
      stepper: new Stepper({ totalSteps: 1 }), options: { sourceAcr: "source", ...options },
      acrLoginServer: "mirror.azurecr.io", acr: "mirror", repoRoot: "/unused",
      resumeFromPhase: null, resumeTopology: { region: "region", resourceGroup: "rg", aksCluster: "target-aks", sandboxName: "agent", sourceAcr: "source" },
    })).rejects.toThrow("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(state.effects).toEqual([]);
  });

  it.each([{}, { release: true }, { build: true }])("rechecks protection before each acquisition publication for %j", async options => {
    const protectedNamespace = state.namespace;
    state.namespace = JSON.stringify({ kind: "Namespace", metadata: { name: "kars-system", uid: "root-ns", resourceVersion: "1" } });
    const run = state.execute.getMockImplementation()!;
    state.execute.mockImplementation((file, args) => {
      if (file === "az" && args[0] === "acr" && args[1] === "login") return { stdout: "" };
      if ((file === "docker" && args[0] === "build") || (file === "az" && args[0] === "acr" && args[1] === "import")) {
        state.effects.push(file === "docker" ? "docker build" : "acr import");
        state.namespace = protectedNamespace;
        return { stdout: "" };
      }
      return run(file, args);
    });
    await expect(acquireImages({
      stepper: new Stepper({ totalSteps: 1 }), options: { sourceAcr: "source", ...options },
      acrLoginServer: "mirror.azurecr.io", acr: "mirror", repoRoot: "/unused",
      resumeFromPhase: null, resumeTopology: { region: "region", resourceGroup: "rg", aksCluster: "target-aks", sandboxName: "agent", sourceAcr: "source" },
    })).rejects.toThrow("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(state.effects).toEqual(["build" in options ? "docker build" : "acr import"]);
    expect(state.execute.mock.calls.some(([file, args]) => file === "docker" && args[0] === "push")).toBe(false);
  });
});
