// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { parseAllDocuments } from "yaml";
import { parseSreFunding, planSreFunding } from "./sre-funding.js";
import type { Execute } from "./sre-authority.js";

const expiry = "2099-01-01T00:00:00Z";
const funding = { tokens: 300000, expiresAt: expiry };
const created = { uid: "source-uid", namespaceUid: "runtime-uid" };

function fixture() {
  const otherGrant = {
    root: { kind: "KarsSandbox", resource: { namespace: "kars-system", name: "bridge-orchestrator", uid: "orchestrator-uid" },
      workspaceUid: "workspace-uid", clusterUid: "cluster-uid" },
    limits: { tokens: 100000, usdMicros: 50 }, expiresAt: "2020-01-01T00:00:00Z",
  };
  const values = { inferenceBudget: { enabled: true, builtInFunding: [otherGrant] }, sre: { dailyTokens: 50000 } };
  let saved: unknown = structuredClone(values);
  const map = {
    kind: "ConfigMap", metadata: { name: "kars-inference-budget-contracts", namespace: "kars-system",
      uid: "catalog-uid", resourceVersion: "10", annotations: {
        "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system",
        "kars.azure.com/inference-budget-contracts": "v1",
      } },
    data: { "built-in-funding.json": JSON.stringify(values.inferenceBudget.builtInFunding), "contracts.json": "retained" },
  };
  const source = {
    kind: "KarsSandbox", metadata: {
      name: "sre", namespace: "kars-system", uid: created.uid, resourceVersion: "2",
      annotations: { "kars.azure.com/namespace-uid": created.namespaceUid },
    }, spec: { runtime: { kind: "Hermes" }, inferenceBudgetRef: undefined as unknown },
  };
  const runtime = {
    metadata: { name: "kars-sre", uid: created.namespaceUid, resourceVersion: "3", annotations: {
      "kars.azure.com/namespace-claim-version": "v1",
      "kars.azure.com/sandbox-namespace": "kars-system", "kars.azure.com/sandbox-name": "sre",
      "kars.azure.com/sandbox-uid": created.uid, "kars.azure.com/calendar-initialization-uid": created.uid,
      "kars.azure.com/calendar-account-uid": undefined as string | undefined,
    } },
  };
  const workspace = { metadata: { name: "kars-system", uid: "workspace-uid", resourceVersion: "1" } };
  const cluster = { metadata: { name: "kube-system", uid: "cluster-uid", resourceVersion: "1" } };
  let deployment = false;
  const execute = vi.fn<Execute>(async (file, args) => {
    if (file === "helm" && args[0] === "get" && args[1] === "values") return { stdout: JSON.stringify(saved) };
    if (file !== "kubectl" || args[0] !== "get") throw new Error(`Unexpected command ${file} ${args.join(" ")}`);
    let result: unknown;
    if (args[1] === "configmap") result = map;
    else if (args[1] === "karssandbox") result = source;
    else if (args[1] === "namespace") {
      result = args[2] === "kars-system" ? workspace : args[2] === "kube-system" ? cluster : runtime;
    } else if (args[1] === "deployment") {
      result = deployment ? { metadata: { name: "sre", uid: "prior-consumer", resourceVersion: "1" } } : undefined;
    } else throw new Error(`Unexpected resource ${args[1]}`);
    return { stdout: result ? JSON.stringify(result) : "" };
  });
  const plan = () => planSreFunding(execute, "kars-system", "kars", values, funding);
  const publish = (flags: string[]) => {
    const declarations: unknown = JSON.parse(flags[1].slice(flags[1].indexOf("=") + 1));
    map.data["built-in-funding.json"] = JSON.stringify(declarations);
    map.metadata.resourceVersion = "11";
    saved = { ...values, inferenceBudget: { ...values.inferenceBudget, builtInFunding: declarations } };
    return declarations;
  };
  return { values, map, source, runtime, workspace, cluster, execute, plan, publish,
    setSaved: (value: unknown) => { saved = value; }, addConsumer: () => { deployment = true; } };
}

describe("explicit fresh SRE funding", () => {
  it("leaves existing non-funded installation unchanged", () => {
    expect(parseSreFunding()).toBeUndefined();
  });
  it.each([
    [undefined, expiry], ["0", expiry], ["-1", expiry], ["1.5", expiry], ["1e6", expiry],
    [" 100", expiry], ["9007199254740992", expiry], ["100", undefined], ["100", "2000-01-01T00:00:00Z"],
    ["100", "2099-02-31T00:00:00Z"], ["100", "2099-01-01"], ["100", "2099-01-01T25:00:00Z"],
  ])("rejects incomplete/invalid funding %s / %s", (tokens, at) => {
    expect(() => parseSreFunding(tokens, at)).toThrow("SRE --");
  });
  it.each(["2099-01-01T00:00:00Z", "2099-01-01T00:00:00.000Z", "2099-01-01T00:00:00.123Z"])("accepts exact explicit funding %s", at => {
    expect(parseSreFunding("300000", at)).toEqual({ tokens: 300000, expiresAt: at });
  });
  it("pins real identities and preserves an unrelated expired grant and policy", async () => {
    const f = fixture();
    const plan = await f.plan();
    const flags = await plan.flags(created);
    expect(flags[0]).toBe("--set-json");
    expect(f.publish(flags)).toEqual([
      f.values.inferenceBudget.builtInFunding[0],
      { root: { kind: "KarsSandbox", resource: { namespace: "kars-system", name: "sre", uid: created.uid },
        workspaceUid: "workspace-uid", clusterUid: "cluster-uid" }, limits: { tokens: 300000 }, expiresAt: expiry },
    ]);
    await plan.verify(created);
    expect(f.map.data["contracts.json"]).toBe("retained");
    expect(f.values.sre.dailyTokens).toBe(50000);
    expect(f.execute.mock.calls.every(([file, args]) => args[0] === "get" && ["helm", "kubectl"].includes(file))).toBe(true);
  });
  it("renders the exact funding flag through the maintained Core chart", async () => {
    const f = fixture();
    const plan = await f.plan();
    const flags = await plan.flags(created);
    const values = {
      ...f.values, azure: { workloadIdentity: { clientId: "dummy" } }, sre: { enabled: true },
      inferenceBudget: { ...f.values.inferenceBudget, catalogVersion: "test-v1",
        contracts: [{ id: "offline-shape-test" }], caBundle: "public-test-ca", tlsSecretName: "test-budget-tls",
        routerImageDigest: `sha256:${"1".repeat(64)}` },
    };
    const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
    const { stdout } = await execa("helm", ["template", "kars", chart, "--namespace", "kars-system",
      "--dry-run=client", "--values", "-", "--show-only", "templates/inference-budget.yaml",
      "--show-only", "templates/sre.yaml", ...flags], { input: JSON.stringify(values), timeout: 20000 });
    const documents = parseAllDocuments(stdout).map(document => {
      expect(document.errors).toEqual([]);
      return document.toJSON();
    });
    const catalog = documents.find(document => document?.kind === "ConfigMap"
      && document.metadata.name === "kars-inference-budget-contracts");
    expect(JSON.parse(catalog.data["built-in-funding.json"])).toEqual(f.publish(flags));
    expect(documents.find(document => document?.kind === "KarsSandbox"
      && document.metadata.name === "sre").spec.runtime.kind).toBe("Hermes");
  });
  it.each(["live", "saved", "neither"])("refuses enrollment if grant publication reached %s only", async where => {
    const f = fixture();
    const plan = await f.plan();
    const flags = await plan.flags(created);
    const original = structuredClone(f.map);
    if (where !== "neither") f.publish(flags);
    if (where === "live") f.setSaved(f.values);
    if (where === "saved") Object.assign(f.map, original);
    await expect(plan.verify(created)).rejects.toThrow("not persisted");
  });
  it.each(["uid", "resourceVersion", "data", "saved"])("rejects %s drift after staging", async field => {
    const f = fixture();
    const plan = await f.plan();
    if (field === "uid") f.map.metadata.uid = "replacement";
    if (field === "resourceVersion") f.map.metadata.resourceVersion = "11";
    if (field === "data") f.map.data["contracts.json"] = "changed";
    if (field === "saved") f.setSaved({ ...f.values, sre: { dailyTokens: 100000 } });
    await expect(plan.flags(created)).rejects.toThrow("changed during SRE staging");
  });
  it.each(["source", "runtime", "workspace", "cluster", "consumer", "calendar", "initialization", "binding", "harness"])(
    "refuses replacement or legacy %s before publication", async field => {
      const f = fixture();
      const plan = await f.plan();
      if (field === "source") f.source.metadata.uid = "replacement";
      if (field === "runtime") f.runtime.metadata.uid = "replacement";
      if (field === "workspace") f.workspace.metadata.uid = "replacement";
      if (field === "cluster") f.cluster.metadata.uid = "replacement";
      if (field === "consumer") f.addConsumer();
      if (field === "calendar") f.runtime.metadata.annotations["kars.azure.com/calendar-account-uid"] = "existing-account";
      if (field === "initialization") f.runtime.metadata.annotations["kars.azure.com/calendar-initialization-uid"] = "older-source";
      if (field === "binding") f.source.spec.inferenceBudgetRef = { account: "existing-account" };
      if (field === "harness") f.source.spec.runtime.kind = "OpenClaw";
      await expect(plan.flags(created)).rejects.toThrow(/identity changed|fresh, unused namespace/);
    },
  );
  it("rechecks fresh history after Helm publication", async () => {
    const f = fixture();
    const plan = await f.plan();
    f.publish(await plan.flags(created));
    f.addConsumer();
    await expect(plan.verify(created)).rejects.toThrow("legacy migration is not supported");
  });
  it("rejects existing SRE funding instead of replacing its identity or cap", async () => {
    const f = fixture();
    f.values.inferenceBudget.builtInFunding[0].root.resource.name = "sre";
    await expect(f.plan()).rejects.toThrow("funding already exists");
    expect(f.execute).not.toHaveBeenCalled();
  });
  it("rejects a disabled broker before reads", async () => {
    const f = fixture();
    f.values.inferenceBudget.enabled = false;
    await expect(f.plan()).rejects.toThrow("Configure the Core");
    expect(f.execute).not.toHaveBeenCalled();
  });
  it("rejects foreign catalog ownership", async () => {
    const f = fixture();
    f.map.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    await expect(f.plan()).rejects.toThrow("Core-owned");
  });
  it("does not overwrite live-only funding", async () => {
    const f = fixture();
    f.map.data["built-in-funding.json"] = "[]";
    await expect(f.plan()).rejects.toThrow("differs from saved");
  });
  it.each(["{}", "null", "[{}]", "x".repeat(32769)])("rejects malformed live grant %s", async encoded => {
    const f = fixture();
    f.map.data["built-in-funding.json"] = encoded;
    await expect(f.plan()).rejects.toThrow();
  });
  it("propagates discovery errors without default grants or writes", async () => {
    const f = fixture();
    f.execute.mockRejectedValueOnce(new Error("Forbidden"));
    await expect(f.plan()).rejects.toThrow("Forbidden");
    expect(f.execute).toHaveBeenCalledTimes(1);
  });
});
