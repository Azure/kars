// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";
import { completePendingCoreSre } from "./core-helm-sre-hook.js";
import { bridgeReactivationDigest } from "../commands/budget-topup.js";
import { runSchemaHook } from "../schema-hook.js";
import { claimAnnotations } from "./namespace-ownership.js";
import type { ObjectMap, SchemaExecute } from "./schema-documents.js";

function fixture(unbounded = false) {
  const ns = "kars-system";
  const owned = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": ns };
  const make = (kind: string, name: string, namespace?: string): ObjectMap => ({ apiVersion: "v1", kind,
    metadata: { name, ...(namespace ? { namespace } : {}), uid: `${name}-uid`, resourceVersion: "1", generation: 1,
      annotations: { ...owned } } });
  const resources = [["InferencePolicy", "sre-inference"], ["KarsSandbox", "sre"], ["ToolPolicy", "sre-tools"]]
    .map(([kind, name]) => ({ apiVersion: "kars.azure.com/v1alpha1", kind, metadata: { name, namespace: ns, annotations: kind === "KarsSandbox" ? { "kars.azure.com/sre-first-install-intent": "kars-sre-first-install" } : {} },
      spec: kind === "KarsSandbox" ? { runtime: { kind: "Hermes" } } : { appliesTo: { sandboxName: "sre" } } }));
  const intent = { version: 1, release: "kars", namespace: ns, valuesDigest: "a".repeat(64), resources,
    funding: unbounded ? { unboundedTokens: true } : { tokens: 1000, expiresAt: "2999-01-01T00:00:00Z" }, budget: { enabled: true, contracts: [{ id: "approved" }] } };
  const map: ObjectMap = { ...make("ConfigMap", "kars-sre-first-install", ns), immutable: true, data: { "intent.json": JSON.stringify(intent) } };
  const catalog: ObjectMap = { ...make("ConfigMap", "kars-inference-budget-contracts", ns), data: { "built-in-funding.json": "[]" } };
  catalog.metadata.annotations["kars.azure.com/inference-budget-contracts"] = "v1";
  const controller: ObjectMap = { ...make("Deployment", "kars-controller", ns), apiVersion: "apps/v1",
    spec: { template: { metadata: {}, spec: { containers: [{ name: "controller", image: `example.test/controller:latest@sha256:${"a".repeat(64)}` }] } } },
    status: { observedGeneration: 1, availableReplicas: 1 } };
  const documents: ObjectMap[] = [structuredClone(map), structuredClone(catalog), structuredClone(controller)];
  const objects = new Map<string, ObjectMap>();
  const key = (kind: string, name: string, namespace = "") => `${kind.toLowerCase()}/${namespace}/${name}`;
  const save = (object: ObjectMap) => objects.set(key(object.kind, object.metadata.name, object.metadata.namespace), object);
  [map, catalog, controller, make("Namespace", ns), make("Namespace", "kube-system"),
    make("CustomResourceDefinition", "karssreregistrations.kars.azure.com")].forEach(save);
  const state = { history: [{ revision: 1, status: "pending-install" }], manifest: () => documents.map(o => JSON.stringify(o)).join("\n---\n") };
  const writes: ObjectMap[] = [], calls: Array<{ file: string; args: readonly string[]; timeout?: number }> = [];
  let before = (_file: string, _args: readonly string[]) => {};
  const execute: SchemaExecute = async (file, args, options) => {
    calls.push({ file, args, timeout: options.timeout });
    before(file, args);
    if (file === "helm" && args[0] === "history") return { stdout: JSON.stringify(state.history) };
    if (file === "helm" && args[0] === "get" && args[1] === "manifest") return { stdout: state.manifest() };
    if (file !== "kubectl") throw new Error("Unapproved executable");
    if (args[0] === "auth") return { stdout: "yes" };
    if (args[0] === "get") {
      if (args.includes("-A")) return { stdout: '{"items":[]}' };
      const kind = ({ crd: "customresourcedefinition", "karssreregistrations.kars.azure.com": "karssreregistration" } as Record<string, string>)[args[1]] ?? args[1];
      const object = objects.get(key(kind, args[2], args.includes("-n") ? args[args.indexOf("-n") + 1] : ""));
      return { stdout: object ? JSON.stringify(object) : "" };
    }
    if (args[0] === "patch") {
      expect(args.slice(0, 6)).toEqual(["patch", "configmap", catalog.metadata.name, "-n", ns, "--type=json"]);
      const operations = JSON.parse(args[args.indexOf("-p") + 1]);
      expect(operations.slice(0, 2)).toEqual([
        { op: "test", path: "/metadata/uid", value: catalog.metadata.uid },
        { op: "test", path: "/metadata/resourceVersion", value: catalog.metadata.resourceVersion },
      ]);
      const change = operations[2];
      expect(change.path).toBe("/data/unbounded-token-allowances.json");
      catalog.data["unbounded-token-allowances.json"] = change.value;
      catalog.metadata.resourceVersion = "2";
      const account = objects.get(key("KarsBudgetAccount", "inference-budget-sre-created", ns))!;
      account.status.ledger.unboundedTokens = JSON.parse(change.value)[0];
      return { stdout: "patched" };
    }
    if (args[0] === "create") {
      const object = JSON.parse(options.input!);
      if (objects.has(key(object.kind, object.metadata.name, object.metadata.namespace))) throw new Error("409 CREATE conflict");
      writes.push(structuredClone(object));
      Object.assign(object.metadata, { uid: `${object.metadata.name}-created`, resourceVersion: "1", generation: 1 });
      save(object);
      if (object.kind === "KarsSandbox") {
        const runtime = make("Namespace", "kars-sre");
        runtime.metadata.annotations = { ...claimAnnotations(object), "kars.azure.com/calendar-initialization-uid": object.metadata.uid };
        object.metadata.annotations["kars.azure.com/namespace-uid"] = runtime.metadata.uid;
        save(runtime);
      }
      if (object.kind === "KarsSRERegistration") {
        object.status = { phase: "Ready", observedGeneration: 1, privacyRevision: "kars.azure.com/sre-privacy/v2" };
        const account: ObjectMap = { ...make("KarsBudgetAccount", "finite", ns), status: { phase: "Active", observedGeneration: 1, ledger: {
          phase: "Active", accountUid: "finite-uid", root: object.spec.firstInstallFunding.grant.root,
          limits: object.spec.firstInstallFunding.grant.limits, nodes: { [object.spec.sandbox.uid]: {
            active: true, authority: { effectiveAuthorization: object.spec.firstInstallFunding.grant } } },
        } } };
        const binding: Record<string, any> = {
          root: object.spec.firstInstallFunding.grant.root,
          account: { name: "finite", namespace: ns, uid: account.metadata.uid },
        };
        if (unbounded) {
          const grant = object.spec.firstInstallFunding.grant;
          account.metadata.name = "inference-budget-sre-created";
          account.metadata.annotations["kars.azure.com/inference-budget-bootstrap"] = "sealed";
          account.spec = { scope: "GovernedInference", root: grant.root, limits: grant.limits };
          Object.assign(account.status.ledger, { version: "governed-inference/v1", scope: "GovernedInference", tokenTopUps: {},
            meters: { settled: { tokens: 0, usdMicros: 0 }, reserved: { tokens: 0, usdMicros: 0 } } });
          const authority = account.status.ledger.nodes[object.spec.sandbox.uid].authority;
          Object.assign(authority, { task: grant.root.resource, limits: grant.limits, rootTaskUid: object.spec.sandbox.uid,
            authorizationDigest: bridgeReactivationDigest(grant) });
          binding.account.name = account.metadata.name;
          Object.assign(binding, { scope: "GovernedInference", taskUid: object.spec.sandbox.uid,
            rootTaskUid: object.spec.sandbox.uid, authorizationDigest: authority.authorizationDigest });
        }
        save(account);
        objects.get(key("KarsSandbox", "sre", ns))!.spec.inferenceBudgetRef = binding;
      }
      return { stdout: JSON.stringify(object) };
    }
    throw new Error(`Unapproved command ${args[0]}`);
  };
  const options = { release: "kars", namespace: ns, revision: 1, operation: "install" as const, installerHelmVersion: "v4.1.3", timeoutMs: 1500 };
  return { execute, options, objects, save, make, map, catalog, controller, documents, intent, writes, calls, state,
    before: (callback: typeof before) => { before = callback; },
    run: () => completePendingCoreSre(execute, options) };
}

describe("Core original-install SRE enrollment", () => {
  it("atomically binds original Helm intent, real UIDs and a finite grant without Helm mutation", async () => {
    const f = fixture();
    await expect(f.run()).resolves.toMatchObject({ enrolled: true, sandboxUid: "sre-created", accountUid: "finite-uid" });
    expect(f.writes.map(o => o.kind)).toEqual(["InferencePolicy", "ToolPolicy", "KarsSandbox", "KarsSRERegistration"]);
    expect(f.writes.at(-1)!.spec.firstInstallFunding).toEqual({
      intent: { name: "kars-sre-first-install", uid: f.map.metadata.uid,
        digest: createHash("sha256").update(f.map.data["intent.json"]).digest("hex") },
      grant: { root: { kind: "KarsSandbox", resource: { namespace: "kars-system", name: "sre", uid: "sre-created" },
        workspaceUid: "kars-system-uid", clusterUid: "kube-system-uid" }, limits: { tokens: 1000 }, expiresAt: "2999-01-01T00:00:00Z" },
    });
    expect(f.calls.every(call => call.timeout! > 0 && call.timeout! <= 1500)).toBe(true);
    expect(f.calls.filter(call => call.file === "helm").every(call => ["history", "get"].includes(call.args[0]))).toBe(true);
    expect(f.calls.some(call => ["patch", "apply", "delete", "upgrade"].includes(call.args[0]))).toBe(false);
    await expect(f.run()).rejects.toThrow("no adoption");
    expect(f.writes).toHaveLength(4);
  });

  it("cold-enrolls unbounded funding, publishes the existing UID-bound allowance and waits for its receipt", async () => {
    const f = fixture(true);
    const result = await f.run();
    expect(result.unboundedTokenAllowance).toMatchObject({ accountUid: "finite-uid", originalLimits: {}, previousTokenTopUpIds: [] });
    expect(f.writes.at(-1)!.spec.firstInstallFunding.grant).toMatchObject({ unboundedTokens: true, limits: {} });
    expect(f.writes.at(-1)!.spec.firstInstallFunding.grant).not.toHaveProperty("expiresAt");
    expect(f.calls.filter(call => call.args[0] === "patch")).toHaveLength(1);
    expect(f.calls.some(call => call.args[0] === "auth" && call.args[2] === "fund")).toBe(true);
    const ledger = f.objects.get("karsbudgetaccount/kars-system/inference-budget-sre-created")!.status.ledger;
    expect(ledger.unboundedTokens).toEqual(result.unboundedTokenAllowance);
    expect(ledger.meters.settled.tokens).toBe(0);
  });

  it.each(["unconsumed", "changed", "revoked"])("does not complete with an %s allowance", async fault => {
    const f = fixture(true);
    let clock = 0;
    const execute: SchemaExecute = async (file, args, options) => {
      const result = await f.execute(file, args, options);
      if (args[0] === "patch") {
        const ledger = f.objects.get("karsbudgetaccount/kars-system/inference-budget-sre-created")!.status.ledger;
        if (fault === "unconsumed") delete ledger.unboundedTokens;
        if (fault === "changed") ledger.unboundedTokens.id = "other";
        if (fault === "revoked") f.catalog.data["unbounded-token-allowances.json"] = "[]";
      }
      return result;
    };
    await expect(completePendingCoreSre(execute, { ...f.options, now: () => clock, sleep: async () => { clock += 1000; } }))
      .rejects.toThrow(fault === "unconsumed" ? /deadline/ : fault === "changed" ? /allowance changed/ : /catalog changed/);
    expect(f.calls.filter(call => call.args[0] === "patch")).toHaveLength(1);
  });

  it.each(["tokens", "expiresAt", "false"])("rejects mixed or disabled unbounded policy: %s", async fault => {
    const f = fixture(true);
    Object.assign(f.intent.funding, fault === "false" ? { unboundedTokens: false }
      : fault === "tokens" ? { tokens: 1000 } : { expiresAt: "2999-01-01T00:00:00Z" });
    f.map.data["intent.json"] = JSON.stringify(f.intent); f.documents[0].data = { ...f.map.data };
    await expect(f.run()).rejects.toThrow("First-install funding requires exactly");
    expect(f.writes).toEqual([]);
  });

  it.each(["containers", "initContainers"])("accepts API omission of empty EnvVar values in %s", async field => {
    const f = fixture();
    const spec = f.controller.spec.template.spec;
    spec[field] ??= [{ name: "init", image: spec.containers[0].image }];
    spec[field][0].env = [{ name: "EMPTY", value: "" }, { name: "SET", value: "retained" }];
    f.documents[2].spec = structuredClone(f.controller.spec);
    delete spec[field][0].env[0].value;
    await expect(f.run()).resolves.toMatchObject({ enrolled: true });
  });

  it.each(["literal", "reference", "missing-env"])("rejects changed empty controller environment: %s", async fault => {
    for (const explicit of [true, false]) {
      const f = fixture();
      const container = f.controller.spec.template.spec.containers[0];
      container.env = [{ name: "EMPTY", ...(explicit ? { value: "" } : {}) }];
      f.documents[2].spec = structuredClone(f.controller.spec);
      if (fault === "literal") container.env[0].value = "unexpected";
      if (fault === "reference") container.env[0] = { name: "EMPTY", valueFrom: { secretKeyRef: { name: "unreviewed", key: "key" } } };
      if (fault === "missing-env") delete container.env;
      await expect(f.run()).rejects.toThrow("Core controller intent changed");
      expect(f.writes).toEqual([]);
    }
  });

  it("is reached by the actual packaged helper phase", async () => {
    const f = fixture();
    await expect(runSchemaHook(["--release", "kars", "--namespace", "kars-system", "--revision", "1",
      "--operation", "install", "--installer-helm-version", "v4.1.3", "--phase", "sre"], f.execute)).resolves.toMatchObject({ enrolled: true });
  });

  it.each(["history", "intent", "catalog", "controller", "namespace", "protected", "expired", "existing", "registration"])("fails closed before writing for %s", async fault => {
    const f = fixture();
    if (fault === "history") f.state.history[0].status = "failed";
    if (fault === "intent") f.map.metadata.uid = "replaced";
    if (fault === "catalog") f.catalog.data["built-in-funding.json"] = "[{}]";
    if (fault === "controller") f.controller.status.availableReplicas = 0;
    if (fault === "namespace") f.save({ ...f.make("Namespace", "kars-sre") });
    if (fault === "protected") f.controller.metadata.annotations["kars.azure.com/private-root-retirement"] = "incomplete";
    if (fault === "expired") {
      f.intent.funding.expiresAt = "2000-01-01T00:00:00Z";
      f.map.data["intent.json"] = JSON.stringify(f.intent); f.documents[0].data = { ...f.map.data };
    }
    if (fault === "existing") f.save(f.make("InferencePolicy", "sre-inference", "kars-system"));
    if (fault === "registration") f.save(f.make("KarsSRERegistration", "canonical"));
    // UID pinning begins at the first live read, not at a render's unassigned metadata.
    if (fault === "intent") {
      let reads = 0;
      f.before((_file, args) => { if (args[0] === "get" && args[2] === "kars-sre-first-install" && ++reads === 2) f.map.metadata.uid = "replaced-again"; });
    }
    await expect(f.run()).rejects.toThrow();
    expect(f.writes).toEqual([]);
  });

  it.each(["409", "timeout", "bad-response"])("does not adopt or retry an ambiguous registration CREATE: %s", async fault => {
    const f = fixture();
    const execute: SchemaExecute = async (file, args, settings) => {
      if (args[0] === "create" && JSON.parse(settings.input!).kind === "KarsSRERegistration") {
        if (fault === "409") throw new Error("409 CREATE conflict");
        const result = await f.execute(file, args, settings);
        if (fault === "timeout") throw new Error("Unknown committed outcome");
        return { stdout: JSON.stringify({ ...JSON.parse(result.stdout), metadata: { name: "canonical" } }) };
      }
      return f.execute(file, args, settings);
    };
    await expect(completePendingCoreSre(execute, f.options)).rejects.toThrow();
    expect(f.writes.filter(o => o.kind === "KarsSRERegistration")).toHaveLength(fault === "409" ? 0 : 1);
    await expect(f.run()).rejects.toThrow("no adoption");
  });

  it("pins manifest and history again before each CREATE", async () => {
    const f = fixture();
    let reads = 0;
    f.before((file, args) => { if (file === "helm" && args[0] === "history" && ++reads === 3) f.documents[0].data["intent.json"] += " "; });
    await expect(f.run()).rejects.toThrow("manifest changed");
    expect(f.writes).toEqual([]);
  });
  it("rejects a matching support resource that races the initial absence check", async () => {
    const f = fixture(); let reads = 0;
    f.before((_file, args) => {
      if (args[0] === "get" && args[1] === "inferencepolicy" && ++reads === 2) {
        f.save({ ...f.intent.resources[0], metadata: { ...f.intent.resources[0].metadata, uid: "racing", resourceVersion: "1" } });
      }
    });
    await expect(f.run()).rejects.toThrow("no adoption");
    expect(f.writes).toEqual([]);
  });

  it.each(["KarsSandbox", "InferencePolicy"])("does not recover an ambiguous %s CREATE", async kind => {
    const f = fixture();
    const execute: SchemaExecute = async (file, args, settings) => {
      const result = await f.execute(file, args, settings);
      if (args[0] === "create" && JSON.parse(settings.input!).kind === kind) throw new Error("Unknown outcome");
      return result;
    };
    await expect(completePendingCoreSre(execute, f.options)).rejects.toThrow("Unknown outcome");
    expect(f.writes.some(object => object.kind === "KarsSRERegistration")).toBe(false);
    await expect(f.run()).rejects.toThrow("no adoption");
  });

  it.each(["source-spec", "support-spec", "support-owner", "support-uid", "runtime-uid"])("fences %s drift after enrollment without replenishment", async fault => {
    const f = fixture();
    f.before((_file, args) => {
      if (args[0] !== "get" || !f.writes.some(object => object.kind === "KarsSRERegistration")) return;
      const source = f.objects.get("karssandbox/kars-system/sre")!;
      const support = f.objects.get("inferencepolicy/kars-system/sre-inference")!;
      if (fault === "source-spec") source.spec.runtime.kind = "OpenClaw";
      if (fault === "support-spec") support.spec.appliesTo.sandboxName = "other";
      if (fault === "support-owner") support.metadata.annotations!["meta.helm.sh/release-name"] = "other";
      if (fault === "support-uid") support.metadata.uid = "replacement";
      if (fault === "runtime-uid") f.objects.get("namespace//kars-sre")!.metadata.uid = "replacement";
    });
    await expect(f.run()).rejects.toThrow("Created SRE");
    expect(f.writes).toHaveLength(4);
  });

  it("does not accept a replaced source, controller configuration or a spent/blocked account", async () => {
    for (const fault of ["source", "controller", "account"]) {
      const f = fixture(); let clock = 0;
      f.before((_file, args) => {
        if (!f.writes.length) return;
        if (fault === "controller") f.controller.spec.template.spec.containers[0].env = [{ name: "DRIFT", value: "true" }];
        if (fault === "source" && args[0] === "get" && args[1] === "karssandbox") {
          const object = f.objects.get("karssandbox/kars-system/sre"); if (object) object.metadata.uid = "replacement";
        }
        if (fault === "account" && args[0] === "get" && args[1] === "karsbudgetaccount") {
          f.objects.get("karsbudgetaccount/kars-system/finite")!.status.phase = "Blocked";
        }
      });
      await expect(completePendingCoreSre(f.execute, { ...f.options, now: () => clock,
        sleep: async () => { clock += 2000; } })).rejects.toThrow();
    }
  });

});
