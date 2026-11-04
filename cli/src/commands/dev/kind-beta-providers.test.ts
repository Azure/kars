// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { createHash, X509Certificate } from "node:crypto";
import { chmodSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { rootCertificates } from "node:tls";
import { assertRetiredRoot } from "../../lib/private-activation-retirement.js";
import { type Execute, type PrivateActivation } from "../../lib/private-activation.js";
import { setupWorkspaceProviders } from "../../lib/workspace-provider-setup.js";
import { completeKindBetaProviderSetup, issueKindBudgetIdentity, rotateKindProviderBudget, setupInstalledKindBetaProviders } from "./kind-beta-providers.js";
import type { BetaExecute } from "./kind-beta.js";

vi.mock("../../lib/private-activation-retirement.js", async original => ({
  ...await original<object>(), assertRetiredRoot: vi.fn(async () => {}),
}));
vi.mock("../../lib/workspace-provider-setup.js", () => ({
  setupWorkspaceProviders: vi.fn(async () => {}), waitForWorkspaceProviders: vi.fn(async () => {}),
}));
const directories: string[] = [];
const directory = () => { const value = mkdtempSync(path.join(os.tmpdir(), "kars-provider-test-")); directories.push(value); return value; };
const local: BetaExecute = async (tool, args, options) => { const result = await execa(tool, args, options); return { stdout: result.stdout ?? "" }; };
const encoded = (text: string) => Buffer.from(text).toString("base64");
const key = (pem: string) => createHash("sha256").update(new X509Certificate(pem).publicKey.export({ type: "spki", format: "der" })).digest("hex");
const metadata = (name: string) => ({ name, uid: `${name}-uid`, resourceVersion: "1" });
const catalogName = "kars-inference-budget-contracts";
let oldIdentity: Awaited<ReturnType<typeof issueKindBudgetIdentity>>;
let nextIdentity: typeof oldIdentity;
beforeAll(async () => {
  oldIdentity = await issueKindBudgetIdentity(directory(), local);
  nextIdentity = await issueKindBudgetIdentity(directory(), local);
});
beforeEach(() => {
  vi.mocked(assertRetiredRoot).mockReset().mockResolvedValue();
  vi.mocked(setupWorkspaceProviders).mockReset().mockResolvedValue();
});
afterAll(() => { for (const value of directories) rmSync(value, { recursive: true, force: true }); });

function fixture() {
  const root = metadata("kars-system");
  const secret = { metadata: { ...metadata("budget-tls"), annotations: { "kars.azure.com/inference-budget-tls": "v1" } },
    type: "kubernetes.io/tls", data: { "tls.crt": encoded(oldIdentity.certificate), "tls.key": encoded(oldIdentity.privateKey), "ca.crt": encoded(oldIdentity.ca) } };
  const catalog = { metadata: { ...metadata(catalogName), annotations: {
    "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system", "kars.azure.com/inference-budget-contracts": "v1",
  }, labels: { "app.kubernetes.io/managed-by": "Helm" } }, data: { "ca.crt": oldIdentity.ca, untouched: "keep" } };
  const state = { version: 1, attempt: "a".repeat(64), binding: "b".repeat(64), replicaIntent: 1,
    originalVersion: "1", pauseRoot: true, phase: "retired", captured: {}, exposedKeys: [key(oldIdentity.certificate)],
    baseline: { secretUid: secret.metadata.uid, resourceVersion: "1", keyDigest: key(oldIdentity.certificate) } };
  const activation = { phase: "reviewed", root: { namespace: root, budgetTls: {
    namespace: root, secret: metadata("budget-tls"), keyDigest: key(oldIdentity.certificate),
  } } } as PrivateActivation;
  const objects: Record<string, any> = {
    namespace: { metadata: { ...root, annotations: { "kars.azure.com/private-root-retirement": JSON.stringify(state) } } },
    secret, configmap: catalog,
    karssreregistration: { metadata: metadata("canonical"), spec: { sandbox: { name: "sre" }, runtimeNamespace: { name: "kars-sre" } } },
    "karscredentialgrants.kars.azure.com": { metadata: metadata("workspace"), spec: { privateActivation: { ...activation, phase: "qualified" } } },
  };
  const execute = vi.fn<Execute>(async (args, input) => {
    if (args[0] === "get") return JSON.stringify(objects[args[1]!]);
    if (args[0] === "patch") {
      const patch = JSON.parse(input!); const object = objects[args[1]!];
      if (patch.metadata.uid !== object.metadata.uid || patch.metadata.resourceVersion !== object.metadata.resourceVersion) throw new Error("Conflict");
      Object.assign(object.data, patch.data); object.metadata.resourceVersion = String(Number(object.metadata.resourceVersion) + 1);
    }
    return "";
  });
  const issue = vi.fn(async () => nextIdentity);
  const dir = directory();
  return { activation, objects, state, execute, issue, dir,
    rotate: (run: Execute = execute) => rotateKindProviderBudget(run, activation, dir, issue),
    setState: () => { objects.namespace.metadata.annotations["kars.azure.com/private-root-retirement"] = JSON.stringify(state); },
  };
}

describe("Kind provider budget identity", () => {
  it("issues a real matching private leaf with a fresh public key and removes CA private material", async () => {
    const dir = directory(); const identity = await issueKindBudgetIdentity(dir, local);
    const leaf = new X509Certificate(identity.certificate), ca = new X509Certificate(identity.ca);
    expect(leaf.verify(ca.publicKey)).toBe(true);
    expect(leaf.checkHost("kars-inference-budget.kars-system.svc", { subject: "never" })).toBeTruthy();
    expect(key(identity.certificate)).not.toBe(key(oldIdentity.certificate));
    expect(leaf.keyUsage).toContain("1.3.6.1.5.5.7.3.1");
    expect(readdirSync(dir)).toEqual([".gitignore"]);
  });
  it("refuses an unsafe issuance directory and cleans up failed issuance without echoing secrets", async () => {
    const dir = directory(); chmodSync(dir, 0o755);
    const run = vi.fn<BetaExecute>();
    await expect(issueKindBudgetIdentity(dir, run)).rejects.toThrow("private directory"); expect(run).not.toHaveBeenCalled();
    chmodSync(dir, 0o700); run.mockRejectedValue(new Error("sensitive output"));
    await expect(issueKindBudgetIdentity(dir, run)).rejects.toThrow("private command output is not displayed");
    expect(readdirSync(dir)).toEqual([".gitignore"]);
  });
  it("never issues before genuine retirement and the persisted baseline", async () => {
    const f = fixture(); f.state.phase = "restoring"; f.setState();
    await expect(f.rotate()).rejects.toThrow("genuinely retired"); expect(f.issue).not.toHaveBeenCalled();
    f.state.phase = "retired"; f.setState(); vi.mocked(assertRetiredRoot).mockRejectedValueOnce(new Error("private Pod remains"));
    await expect(f.rotate()).rejects.toThrow("private Pod remains"); expect(f.issue).not.toHaveBeenCalled();
  });
  it("CAS rotates the same Secret and catalog, retaining unrelated data and private recovery state", async () => {
    const f = fixture(); await expect(f.rotate()).resolves.toBe(nextIdentity.ca);
    const patches = f.execute.mock.calls.filter(([args]) => args[0] === "patch");
    expect(patches.map(([args]) => args[1])).toEqual(["secret", "configmap"]);
    for (const [args, input] of patches) {
      expect(args).toContain("--patch-file=/dev/stdin"); expect(args.join(" ")).not.toContain("PRIVATE KEY");
      expect(JSON.parse(input!).metadata.resourceVersion).toBe("1");
    }
    expect(f.objects.secret.data["ca.crt"]).toBe(encoded(nextIdentity.ca));
    expect(f.objects.configmap.data.untouched).toBe("keep");
    expect(statSync(path.join(f.dir, "budget-rotation.json")).mode & 0o777).toBe(0o600);
    expect(assertRetiredRoot).toHaveBeenCalledTimes(3);
  });
  it("resumes a failed catalog write without issuing again or replacing the new Secret", async () => {
    const f = fixture(); const run: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "configmap") throw new Error("interrupted catalog write");
      return f.execute(args, input);
    };
    await expect(f.rotate(run)).rejects.toThrow("interrupted");
    f.execute.mockClear(); await expect(f.rotate()).resolves.toBe(nextIdentity.ca);
    expect(f.issue).toHaveBeenCalledTimes(1);
    expect(f.execute.mock.calls.filter(([args]) => args[0] === "patch").map(([args]) => args[1])).toEqual(["configmap"]);
  });
  it.each(["secret", "configmap"])("does not overwrite a concurrent %s edit", async kind => {
    const f = fixture(); const run: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === kind) f.objects[kind].metadata.resourceVersion = "99";
      return f.execute(args, input);
    };
    await expect(f.rotate(run)).rejects.toThrow("Conflict");
    await expect(f.rotate()).rejects.toThrow(/changed/); expect(f.issue).toHaveBeenCalledTimes(1);
  });
  it("rejects chart-managed TLS and previously exposed issuer keys", async () => {
    const f = fixture(); f.objects.secret.metadata.annotations["meta.helm.sh/release-name"] = "kars";
    await expect(f.rotate()).rejects.toThrow("chart-managed"); expect(f.issue).not.toHaveBeenCalled();
    delete f.objects.secret.metadata.annotations["meta.helm.sh/release-name"];
    f.issue.mockResolvedValue(oldIdentity); await expect(f.rotate()).rejects.toThrow("previously exposed");
    expect(f.execute.mock.calls.some(([args]) => args[0] === "patch")).toBe(false);
  });
  it.each(["symlink", "permissions", "different-attempt", "malformed"])("refuses %s recovery records", async fault => {
    const f = fixture(); await f.rotate(); const file = path.join(f.dir, "budget-rotation.json");
    if (fault === "symlink") { const other = path.join(f.dir, "other"); writeFileSync(other, readFileSync(file), { mode: 0o600 }); rmSync(file); symlinkSync(other, file); }
    if (fault === "permissions") chmodSync(file, 0o644);
    if (fault === "different-attempt") { const record = JSON.parse(readFileSync(file, "utf8")); record.attempt = "c".repeat(64); writeFileSync(file, JSON.stringify(record)); }
    if (fault === "malformed") writeFileSync(file, "{}");
    f.execute.mockClear(); await expect(f.rotate()).rejects.toThrow(/state/);
    expect(f.execute.mock.calls.some(([args]) => args[0] === "patch")).toBe(false);
  });
  it("only resumes public CA publication for the matching qualified identity", async () => {
    const f = fixture(); await f.rotate(); f.state.phase = "restoring"; f.setState();
    const publish = vi.fn(async () => {}), run = vi.fn<BetaExecute>(async () => ({ stdout: "" }));
    await completeKindBetaProviderSetup(f.execute, f.dir, run, publish);
    expect(publish).toHaveBeenCalledWith(nextIdentity.ca);
    f.objects.configmap.data["ca.crt"] = oldIdentity.ca; publish.mockClear();
    await expect(completeKindBetaProviderSetup(f.execute, f.dir, run, publish)).rejects.toThrow("differs from the installed");
    expect(publish).not.toHaveBeenCalled();
  });
});

describe("installed Kind provider setup", () => {
  const chart = fileURLToPath(new URL("../../../../deploy/helm/kars", import.meta.url));
  const policy = JSON.parse(readFileSync(path.join(chart, "files/inference-budget-admission.json"), "utf8")
    .replaceAll("__ACCOUNTING_NAMESPACE__", "kars-system")).items.find((p: any) => p.name === "kars-inference-budget-workloads");
  const tools = { kind: "/kind", kubectl: "/kubectl", helm: "/helm", runtime: "/docker", env: {} };
  function installed() {
    const f = fixture();
    const container: any = { Name: "/chosen-control-plane", State: { Running: true }, Config: { Labels: {
      "io.x-k8s.kind.cluster": "chosen", "io.x-k8s.kind.role": "control-plane",
    } } };
    let values: any = { global: { installationProfile: "kind-beta" }, inferenceBudget: { tlsSecretName: "budget-tls", caBundle: oldIdentity.ca }, schemaHook: { enabled: false }, unrelated: "preserve" };
    let adoption = ""; let trustedCA = oldIdentity.ca;
    let admission = structuredClone(policy.spec);
    const controller = { metadata: metadata("kars-controller"), spec: { replicas: 1 } };
    const run = vi.fn<BetaExecute>(async (tool, args, options) => {
      if (tool === tools.runtime) return { stdout: args[0] === "inspect" ? JSON.stringify([container]) : oldIdentity.ca };
      if (tool === "openssl") return { stdout: "OpenSSL" };
      expect(args.slice(0, 4)).toEqual(["--kubeconfig", "/private/config", tool === tools.helm ? "--kube-context" : "--context", "kind-chosen"]);
      const command = args.slice(4);
      if (tool === tools.helm) return { stdout: JSON.stringify(values) };
      if (command[0] === "config") return { stdout: encoded(trustedCA) };
      if (command.includes("kars-sre-helm-adoption")) return { stdout: adoption };
      if (command[1] === "deployment") return { stdout: JSON.stringify(controller) };
      if (command[1] === "validatingadmissionpolicy") return { stdout: JSON.stringify({ metadata: metadata(policy.name), spec: admission }) };
      return { stdout: await f.execute(command, options?.input) };
    });
    const installCore = vi.fn(async (...args: any[]) => {
      values = JSON.parse(readFileSync(args[5][0], "utf8")); adoption = "{}"; admission = structuredClone(policy.spec);
    });
    return { ...f, container, controller, run, installCore, setProfile: (value: string) => { values.global.installationProfile = value; },
      outdated: () => { adoption = "{}"; admission.variables = []; },
      wrongCA: () => { trustedCA = rootCertificates[0]!; }, saved: () => values,
      setup: () => setupInstalledKindBetaProviders("chosen", "/private/config", f.dir, chart, tools, run, installCore),
    };
  }
  it.each(["production", "foreign-container", "wrong-CA"])("refuses %s before enrollment, issuance or Helm mutation", async fault => {
    const f = installed(); if (fault === "production") f.setProfile("production");
    if (fault === "foreign-container") f.container.Config.Labels["io.x-k8s.kind.cluster"] = "other";
    if (fault === "wrong-CA") f.wrongCA();
    await expect(f.setup()).rejects.toThrow(); expect(f.installCore).not.toHaveBeenCalled(); expect(setupWorkspaceProviders).not.toHaveBeenCalled();
    expect(existsSync(path.join(f.dir, "budget-rotation.json"))).toBe(false);
  });
  it("adopts once, preserves installed values, then fences the CA-only publication and resumes without another upgrade", async () => {
    const f = installed(); await f.rotate(); f.state.phase = "restoring"; f.setState();
    await f.setup(); expect(f.installCore).toHaveBeenCalledTimes(2);
    expect(f.installCore.mock.calls[0]![9]).toEqual({ kubeconfig: "/private/config", fencedControllerNoop: true });
    expect(f.installCore.mock.calls[1]![9]).toEqual({ kubeconfig: "/private/config", fencedControllerNoop: true });
    expect(f.saved().controller.upgradePrecondition).toEqual({ uid: "kars-controller-uid", resourceVersion: "1" });
    expect(f.saved().inferenceBudget.caBundle).toBe(nextIdentity.ca); expect(f.saved().unrelated).toBe("preserve");
    expect(f.saved().schemaHook.enabled).toBe(false);
    await f.setup(); expect(f.installCore).toHaveBeenCalledTimes(2);
  });
  it("repairs installed policy while preserving the stopped root, then publishes CA with restored replicas", async () => {
    const f = installed(); await f.rotate(); f.state.phase = "restoring"; f.setState(); f.outdated();
    f.controller.spec.replicas = 0;
    vi.mocked(setupWorkspaceProviders).mockImplementationOnce(async () => { f.controller.spec.replicas = 1; });
    await f.setup();
    const overlays = f.installCore.mock.calls.map(args => JSON.parse(readFileSync(args[5][0], "utf8")));
    expect(overlays.map(value => value.controller.replicas)).toEqual([0, 1]);
    expect(overlays.every(value => value.controller.upgradePrecondition.uid === "kars-controller-uid")).toBe(true);
    expect(f.installCore.mock.calls.every(args => args[9]?.fencedControllerNoop)).toBe(true);
  });
  it("does not begin retirement when guarded policy publication fails", async () => {
    const f = installed(); f.outdated();
    f.installCore.mockRejectedValueOnce(new Error("root changed"));
    await expect(f.setup()).rejects.toThrow("root changed");
    expect(setupWorkspaceProviders).not.toHaveBeenCalled();
  });
  it("refuses a missing live replica intent before guarded publication", async () => {
    const f = installed(); f.outdated(); f.controller.spec.replicas = -1;
    await expect(f.setup()).rejects.toThrow("replica intent");
    expect(f.installCore).not.toHaveBeenCalled(); expect(setupWorkspaceProviders).not.toHaveBeenCalled();
  });
});
