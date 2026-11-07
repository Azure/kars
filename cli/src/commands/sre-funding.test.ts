// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { sreCommand } from "./sre.js";
import { SreFundingError } from "../lib/sre-funding.js";

const f = vi.hoisted(() => ({
  order: [] as string[], enrolled: false, failPublication: false, failVerification: false, governed: true,
  execute: vi.fn(), flags: vi.fn(), verify: vi.fn(),
}));
vi.mock("execa", () => ({ execa: f.execute }));
vi.mock("../lib/repo-assets.js", () => ({ requireBundledAsset: () => "/test/chart" }));
vi.mock("../lib/namespace-ownership.js", () => ({ inspectNamespaceOwnership: async () => {} }));
vi.mock("../lib/core-helm-schemas.js", () => ({
  prepareCoreHelmSchemas: async () => { f.order.push("schema-preflight"); },
}));
vi.mock("../lib/sre-source.js", () => ({
  stageSource: async () => { f.order.push("source-created"); return { uid: "source", namespaceUid: "runtime" }; },
}));
vi.mock("../lib/sre-authority.js", () => ({
  requireRegistrar: async () => {}, get: async () => ({ metadata: { name: "crd" } }),
  registration: async () => f.enrolled ? { spec: { enabled: true, controller: { namespace: { name: "kars-system" }, release: "kars" } } } : undefined,
  preview: async () => ({ legacyBindings: [] }),
  enroll: async (_execute: unknown, _spec: unknown, identity: unknown) => {
    expect(identity).toEqual({ sandboxUid: "source", namespaceUid: "runtime", binding: [] });
    f.order.push("enrolled");
  },
  waitForAuthority: async () => { f.order.push("authority-ready"); },
}));
vi.mock("../lib/private-root-upgrade-guard.js", async importOriginal => ({
  ...await importOriginal<typeof import("../lib/private-root-upgrade-guard.js")>(),
  assertControllerMutationAllowed: async () => { f.order.push("root-reviewed"); },
}));
vi.mock("../lib/sre-funding.js", async importOriginal => ({
  ...await importOriginal<typeof import("../lib/sre-funding.js")>(),
  planSreFunding: async () => ({ flags: f.flags, verify: f.verify }),
}));

beforeEach(() => {
  vi.clearAllMocks();
  f.order.length = 0;
  f.enrolled = f.failPublication = f.failVerification = false;
  f.governed = true;
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(process, "exit").mockImplementation(() => { throw new Error("exit boundary"); });
  f.flags.mockImplementation(async identity => {
    expect(identity).toEqual({ uid: "source", namespaceUid: "runtime" });
    f.order.push("grant-reviewed");
    return ["--set-json", 'inferenceBudget.builtInFunding=[{"reviewed":"actual-source"}]'];
  });
  f.verify.mockImplementation(async () => {
    f.order.push("grant-verified");
    if (f.failVerification) throw new Error("Grant publication did not converge");
  });
  f.execute.mockImplementation(async (file: string, args: string[]) => {
    if (file !== "helm") throw new Error("Unexpected Kubernetes mutation");
    if (args[0] === "list") return { stdout: '[{"name":"kars","namespace":"kars-system"}]' };
    if (args[0] === "get") return { stdout: JSON.stringify({ inferenceBudget: { enabled: f.governed } }) };
    if (args[0] === "template") return { stdout: "source render" };
    if (args[0] === "upgrade") {
      f.order.push("grant-published");
      if (f.failPublication) throw new Error("Helm publication failed");
      return { stdout: "" };
    }
    throw new Error(`Unexpected Helm operation ${args[0]}`);
  });
});
afterEach(() => vi.restoreAllMocks());

const run = (...extra: string[]) => sreCommand().parseAsync(["node", "sre", "install", "--no-wait", ...extra]);
const funded = () => run("--lifetime-tokens", "300000", "--funding-expires-at", "2099-01-01T00:00:00Z");

describe("funded SRE installation ordering", () => {
  it("publishes and verifies the exact grant before enrollment, using one Core upgrade", async () => {
    await funded();
    expect(f.order).toEqual([
      "root-reviewed", "source-created", "grant-reviewed", "schema-preflight", "grant-reviewed",
      "grant-published", "grant-verified", "enrolled", "authority-ready",
    ]);
    const upgrades = f.execute.mock.calls.filter(([, args]) => args[0] === "upgrade");
    expect(upgrades).toHaveLength(1);
    expect(upgrades[0][1]).toEqual(expect.arrayContaining([
      "--reset-then-reuse-values", "sre.enabled=true", "--set-json",
      'inferenceBudget.builtInFunding=[{"reviewed":"actual-source"}]',
    ]));
  });
  it("does not enroll after a failed Core publication", async () => {
    f.failPublication = true;
    await expect(funded()).rejects.toThrow("exit boundary");
    expect(f.order).not.toContain("enrolled");
    expect(f.order).not.toContain("grant-verified");
  });
  it("does not enroll when read-back fails", async () => {
    f.failVerification = true;
    await expect(funded()).rejects.toThrow("did not converge");
    expect(f.order).not.toContain("enrolled");
  });
  it("rechecks for staging drift after schema preparation and before Helm writes", async () => {
    f.flags.mockImplementationOnce(async () => ["--set-json", "inferenceBudget.builtInFunding=[]"])
      .mockRejectedValueOnce(new SreFundingError("Funding changed during preparation"));
    await expect(funded()).rejects.toThrow("exit boundary");
    expect(f.order).not.toContain("grant-published");
    expect(f.order).not.toContain("enrolled");
    expect(console.error).toHaveBeenCalledWith(expect.stringContaining("Funding changed during preparation"));
  });
  it("refuses funding an existing registered actor before staging", async () => {
    f.enrolled = true;
    await expect(funded()).rejects.toThrow("cannot be re-funded");
    expect(f.order).toEqual([]);
  });
  it("validates explicit amounts and expiry before any external command", async () => {
    await expect(run("--lifetime-tokens", "300000")).rejects.toThrow("--funding-expires-at");
    expect(f.execute).not.toHaveBeenCalled();
  });
  it("keeps legacy installation behavior when no lifetime funding is requested", async () => {
    f.governed = false;
    await run();
    expect(f.order).toEqual(["source-created", "enrolled", "authority-ready", "schema-preflight", "grant-published"]);
    expect(f.flags).not.toHaveBeenCalled();
    expect(f.verify).not.toHaveBeenCalled();
  });
  it("does not start calendar-only SRE by omitting funding from a governed fresh install", async () => {
    await expect(run()).rejects.toThrow("no calendar-only fallback");
    expect(f.order).toEqual([]);
    expect(f.execute.mock.calls.some(([, args]) => ["template", "upgrade"].includes(args[0]))).toBe(false);
  });
  it.each([false, true])("refuses funding without a Helm release (template controller=%s)", async template => {
    f.execute.mockResolvedValueOnce({ stdout: "[]" }).mockResolvedValueOnce({ stdout: template
      ? JSON.stringify({ kind: "Deployment", metadata: { name: "kars-controller", namespace: "kars-system", uid: "controller" } })
      : "" });
    await expect(funded()).rejects.toThrow("existing Helm Core release");
    expect(f.order).toEqual([]);
    expect(f.execute).toHaveBeenCalledTimes(2);
  });
});
