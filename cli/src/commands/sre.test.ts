// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sreCommand } from "./sre.js";
import { prepareCoreHelmSchemas } from "../lib/core-helm-schemas.js";
import { PrivateRootUpgradeBlocked } from "../lib/private-root-upgrade-guard.js";
import { sreInferenceFlags } from "../lib/sre-helm.js";

const { execute } = vi.hoisted(() => ({
  execute: vi.fn<(file: string, args: readonly string[], options?: unknown) => Promise<{ stdout: string }>>(),
}));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("../lib/repo-assets.js", () => ({ requireBundledAsset: () => "/test/chart" }));
vi.mock("../lib/core-helm-schemas.js", () => ({
  prepareCoreHelmSchemas: vi.fn(async () => {}),
  prepareCoreTemplateSchemas: vi.fn(async (_execute, rendered) => rendered),
}));

const releases = JSON.stringify([{ name: "kars", namespace: "kars-system" }]);
const controller = JSON.stringify({
  apiVersion: "apps/v1", kind: "Deployment",
  metadata: { name: "kars-controller", namespace: "kars-system", uid: "controller-uid", resourceVersion: "1" },
});

function authority(args: readonly string[]): { stdout: string } {
  const metadata = (name: string, uid = name) => ({ name, uid, resourceVersion: "1", generation: 1 });
  const getIndex = args.indexOf("get");
  const resource = getIndex < 0 ? undefined : args[getIndex + 1];
  const name = getIndex < 0 ? undefined : args[getIndex + 2];
  let value: unknown;
  if (args.includes("can-i")) return { stdout: "yes" };
  if (resource === "deployment" && name === "kars-controller") value = JSON.parse(controller);
  if (resource === "crd") value = { metadata: metadata("karssreregistrations.kars.azure.com") };
  if (resource === "karssreregistrations.kars.azure.com") value = {
    metadata: metadata("canonical"),
    spec: {
      enabled: true,
      controller: { namespace: { name: "kars-system", uid: "kars-system" },
        deployment: { name: "kars-controller", uid: "controller-uid" }, release: currentRelease },
      sandbox: { namespace: "kars-system", name: "sre", uid: "source" },
      runtimeNamespace: { name: "kars-sre", uid: "kars-sre" },
    },
    status: { phase: "Ready", observedGeneration: 1, privacyRevision: "kars.azure.com/sre-privacy/v2" },
  };
  if (resource === "namespace") value = name === "kars-sre"
    ? { metadata: { ...metadata("kars-sre"), annotations: {
      "kars.azure.com/namespace-claim-version": "v1",
      "kars.azure.com/sandbox-namespace": "kars-system",
      "kars.azure.com/sandbox-name": "sre",
      "kars.azure.com/sandbox-uid": "source",
    } } }
    : { metadata: metadata("kars-system") };
  if (resource === "karssandbox") value = {
    metadata: { ...metadata("sre", "source"), namespace: "kars-system",
      annotations: { "kars.azure.com/namespace-uid": "kars-sre" } },
  };
  if (resource === "clusterrolebindings" || resource === "rolebindings") value = { items: [] };
  return { stdout: value ? JSON.stringify(value) : "" };
}

let currentRelease = "kars";

beforeEach(() => {
  currentRelease = "kars";
  execute.mockReset();
  vi.mocked(prepareCoreHelmSchemas).mockReset().mockResolvedValue({ schemas: 0, published: true });
  vi.spyOn(console, "log").mockImplementation(() => {});
});
afterEach(() => vi.restoreAllMocks());

describe("SRE controller upgrade namespace preflight", () => {
  it.each([
    ["install", "protected"], ["install", "unavailable"],
    ["uninstall", "protected"], ["uninstall", "unavailable"],
  ] as const)("reports the safe %s/%s root refusal without attempting Helm upgrade", async (operation, reason) => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: releases };
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      const result = authority(args);
      if (operation === "uninstall" && !args.includes("crd") && args.some(arg => arg === "karssreregistrations.kars.azure.com")) {
        const retired = JSON.parse(result.stdout);
        retired.spec.enabled = false;
        retired.status.phase = "Retired";
        return { stdout: JSON.stringify(retired) };
      }
      return result;
    });
    const failure = Object.assign(new PrivateRootUpgradeBlocked(reason), {
      stderr: "PRIVATE_VALUE_SENTINEL", cause: new Error("PRIVATE_VALUE_SENTINEL"),
    });
    vi.mocked(prepareCoreHelmSchemas).mockRejectedValueOnce(failure);
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(process, "exit").mockImplementation(() => { throw new Error("exit boundary"); });
    await expect(sreCommand().parseAsync(["node", "sre", operation, ...(operation === "install" ? ["--no-wait"] : [])]))
      .rejects.toThrow("exit boundary");
    expect(errors.mock.calls.flat().join("\n")).toContain(failure.message);
    expect(errors.mock.calls.flat().join("\n")).not.toContain("PRIVATE_VALUE_SENTINEL");
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "upgrade")).toBe(false);
    expect(process.exit).toHaveBeenCalledWith(1);
  });

  it.each([
    {}, { model: "reviewed-model" }, { provider: "reviewed-provider" },
    { provider: "reviewed-provider", model: "reviewed-model" },
  ])("stages existing release policy before enrollment (inference override %j)", async selection => {
    const values = { sre: {
      enabled: false, dailyTokens: 50000, tokenBudget: 12000,
      provider: "configured-provider", model: "configured-model", requirePromptShields: true,
    } };
    execute.mockImplementation(async (file, args, options) => {
      if (file === "helm" && args[0] === "list") return { stdout: releases };
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      if (!args.includes("crd") && args.some(arg => arg === "karssreregistrations.kars.azure.com")) return { stdout: "" };
      if (file === "helm" && args[0] === "get") return { stdout: JSON.stringify(values) };
      if (file === "helm" && args[0] === "template") {
        expect(args).toEqual(expect.arrayContaining(["--values", "-", "sre.enabled=true"]));
        expect(options).toEqual({ stdio: "pipe", input: JSON.stringify(values) });
        expect(args.filter(arg => arg.startsWith("sre.model=") || arg.startsWith("sre.provider=")))
          .toEqual(sreInferenceFlags(selection).filter(arg => arg !== "--set-json"));
        throw new Error("Checked render boundary before source creation");
      }
      return authority(args);
    });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait",
      ...(selection.model ? ["--model", selection.model] : []),
      ...(selection.provider ? ["--provider", selection.provider] : [])])).rejects.toThrow("Checked render boundary");
    expect(execute.mock.calls.some(([file, args]) => file === "kubectl" && args.includes("create"))).toBe(false);
  });

  it.each(["upgrade", "template"])("carries the provider/model pair into the final %s without funding flags", async mode => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: mode === "upgrade" ? releases : "[]" };
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait",
      "--provider", "copilot", "--model", "gpt-4.1"]);
    const publication = execute.mock.calls.find(([file, args]) => file === "helm" && args[0] === mode);
    expect(publication?.[1]).toEqual(expect.arrayContaining([
      "--set-json", 'sre.provider="copilot"', "--set-json", 'sre.model="gpt-4.1"',
    ]));
    expect(publication?.[1].some(arg => /inferenceBudget|dailyTokens|tokenBudget|requirePromptShields/.test(arg))).toBe(false);
  });

  it("retains the existing Helm model baseline for an adopted first-install source", async () => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: releases };
      if (file === "helm" && args[0] === "get" && args[1] === "values") {
        return { stdout: JSON.stringify({ sre: { provider: "azure-openai", model: "original" } }) };
      }
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      const result = authority(args);
      if (file === "kubectl" && !args.includes("crd") && args.some(arg => arg === "karssreregistrations.kars.azure.com")) {
        const registration = JSON.parse(result.stdout);
        registration.spec.firstInstallFunding = { intent: { uid: "original-intent" } };
        return { stdout: JSON.stringify(registration) };
      }
      return result;
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait",
      "--provider", "github-copilot", "--model", "claude-opus-5.5"]);
    const publication = execute.mock.calls.find(([file, args]) => file === "helm" && args[0] === "upgrade");
    expect(publication?.[1]).toEqual(expect.arrayContaining([
      '--set-json', 'sre.modelRouteBaseline={"provider":"azure-openai","model":"original"}',
      'sre.provider="github-copilot"', 'sre.model="claude-opus-5.5"',
    ]));
  });

  it("passes the same selection through explicit authority source staging", async () => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: "[]" };
      if (file === "helm" && args[0] === "template") {
        expect(args).toEqual(expect.arrayContaining([
          "--set-json", 'sre.provider="local"', "--set-json", 'sre.model="org/model:small"',
        ]));
        throw new Error("Checked authority render boundary");
      }
      return authority(args);
    });
    await expect(sreCommand().parseAsync(["node", "sre", "authority", "stage-source",
      "--provider", "local", "--model", "org/model:small"]))
      .rejects.toThrow("Checked authority render boundary");
    expect(execute.mock.calls.some(([file, args]) => file === "kubectl" && args.includes("create"))).toBe(false);
  });

  it.each([
    ["install", "--provider", ""], ["install", "--provider", "bad,tag"],
    ["install", "--model", " "], ["authority", "stage-source", "--model", " padded "],
  ])("rejects invalid selection before discovery or mutation: %j", async (...args) => {
    await expect(sreCommand().parseAsync(["node", "sre", ...args])).rejects.toThrow(/SRE --/);
    expect(execute).not.toHaveBeenCalled();
  });

  it.each(["[]", '"invalid"'])("rejects malformed stored values before staging: %s", async values => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: releases };
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      if (!args.includes("crd") && args.some(arg => arg === "karssreregistrations.kars.azure.com")) return { stdout: "" };
      if (file === "helm" && args[0] === "get") return { stdout: values };
      return authority(args);
    });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("invalid stored values");
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "template")).toBe(false);
  });

  it("retains the existing release when Helm 4 removes the all-status flag", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") {
        if (args.includes("--all")) {
          throw Object.assign(new Error("Unsupported flag"), {
            exitCode: 1, stderr: "Error: unknown flag: --all\n",
          });
        }
        expect(args).toContain("--kube-context");
        return { stdout: releases };
      }
      if (file === "helm" && args[0] === "version") return { stdout: "v4.2.4" };
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait", "--context", "test-context"]);
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "upgrade")).toBe(true);
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "install")).toBe(false);
  });

  it.each(["upgrade", "template"])("preflights %s mode in the selected context before mutations", async mode => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: mode === "upgrade" ? releases : "[]" };
      if (file === "kubectl" && args.includes("deployment")) {
        expect(args).toContain("--ignore-not-found");
        return { stdout: controller };
      }
      if (file === "kubectl" && args.includes("karssandboxes")) {
        expect(args.slice(0, 2)).toEqual(["--context", "test-context"]);
        return { stdout: '{"items":[]}' };
      }
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait", "--context", "test-context"]);
    const calls = execute.mock.calls;
    const inspected = calls.findIndex(([file, args]) => file === "kubectl" && args.includes("karssandboxes"));
    const mutation = calls.findIndex(([file, args]) => file === "helm" && args[0] === mode);
    expect(inspected).toBeGreaterThanOrEqual(0);
    expect(mutation).toBeGreaterThan(inspected);
  });

  it("stops an existing release upgrade when ownership cannot be inspected", async () => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: releases };
      throw new Error("Forbidden (403)");
    });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("403");
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "upgrade")).toBe(false);
  });

  it.each(["timeout", "Forbidden (403)"])("never treats controller discovery %s as a fresh install", async error => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: "[]" };
      throw new Error(error);
    });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow(error);
    expect(execute.mock.calls).toHaveLength(2);
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] !== "list")).toBe(false);
  });

  it("propagates Helm discovery errors without attempting another install path", async () => {
    execute.mockRejectedValue(new Error("Helm authorization failed"));
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("authorization");
    expect(execute.mock.calls).toHaveLength(1);
  });

  it.each(["{}", "null", '[{"name":"kars","namespace":"other"}]'])("rejects invalid release inventory %s", async stdout => {
    execute.mockResolvedValue({ stdout });
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("invalid release inventory");
    expect(execute.mock.calls).toHaveLength(1);
  });

  it.each(["null", "{}", '{"kind":"Deployment","metadata":{"name":"foreign"}}'])("rejects invalid controller identity %s", async stdout => {
    execute.mockImplementation(async (file, args) => ({
      stdout: file === "helm" && args[0] === "list" ? "[]" : stdout,
    }));
    await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
      .rejects.toThrow("invalid controller identity");
    expect(execute.mock.calls).toHaveLength(2);
  });

  it("permits fresh installation only after successful inventory and explicit controller absence", async () => {
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") return { stdout: "[]" };
      if (args[0] === "-n") return { stdout: "" };
      if (args.includes("deployment") && args.includes("kars-controller")) return { stdout: controller };
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]);
    expect(execute.mock.calls.slice(0, 2).map(([file, args]) => [file, args[0]])).toEqual([
      ["helm", "list"], ["kubectl", "-n"],
    ]);
    expect(execute.mock.calls[1][1]).toContain("--ignore-not-found");
    const installs = execute.mock.calls.filter(([file, args]) => file === "helm" && args[0] === "install");
    expect(installs).toHaveLength(1);
    expect(installs[0][1]).toContain("sre.enabled=false");
    expect(execute.mock.calls.some(([, args]) => args.includes("--take-ownership") || args.includes("--force-conflicts"))).toBe(false);
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === "upgrade" && args.includes("sre.enabled=true"))).toBe(true);
  });

  it.each([
    { target: "kars.prod", names: ["kars.prod"] },
    { target: "kars", names: ["other.release", "kars"] },
    { target: "kars", names: ["other.release"] },
    { target: "a".repeat(53), names: ["a".repeat(53)] },
  ])("accepts Helm-compatible release inventory for $target", async ({ target, names }) => {
    currentRelease = target;
    execute.mockImplementation(async (file, args) => {
      if (file === "helm" && args[0] === "list") {
        return { stdout: JSON.stringify(names.map(name => ({ name, namespace: "kars-system" }))) };
      }
      if (file === "kubectl" && args.includes("deployment") && args.includes("kars-controller")) return { stdout: controller };
      if (file === "kubectl" && args.includes("karssandboxes")) return { stdout: '{"items":[]}' };
      return authority(args);
    });
    await sreCommand().parseAsync(["node", "sre", "install", "--no-wait", "--release", target]);
    const mode = names.includes(target) ? "upgrade" : "template";
    expect(execute.mock.calls.some(([file, args]) => file === "helm" && args[0] === mode && args[1] === target))
      .toBe(true);
    expect(execute.mock.calls.some(([, args]) => args.includes("karssandboxes"))).toBe(true);
  });

  it.each(["", ".kars", "kars.", "kars..prod", "kars.-prod", "Kars", "a".repeat(54)])(
    "rejects the Helm-invalid release name %s without installation",
    async name => {
      execute.mockResolvedValue({ stdout: JSON.stringify([{ name, namespace: "kars-system" }]) });
      await expect(sreCommand().parseAsync(["node", "sre", "install", "--no-wait"]))
        .rejects.toThrow("invalid release inventory");
      expect(execute.mock.calls).toHaveLength(1);
    },
  );
});
