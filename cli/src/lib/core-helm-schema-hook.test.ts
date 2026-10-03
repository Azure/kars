// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { preparePendingCoreHelmSchemas, type CoreSchemaHookOptions } from "./core-helm-schema-hook.js";
import { SCHEMA_DIGEST, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import { admission, crd, schemaFixture } from "./schema-stage.test-support.js";

const controller = { apiVersion: "apps/v1", kind: "Deployment",
  metadata: { name: "kars-controller", namespace: "kars-system" } };
const protectedNamespace = { apiVersion: "v1", kind: "Namespace", metadata: {
  name: "kars-system", uid: "namespace", resourceVersion: "1",
  annotations: { "kars.azure.com/private-root-retirement": "incomplete" },
} };
const render = (documents: ObjectMap[]) => documents.map(document => JSON.stringify(document)).join("\n---\n");

function fixture(operation: "install" | "upgrade" = "install", documents = [crd(), admission(), controller]) {
  const f = schemaFixture(documents);
  const revision = operation === "install" ? 1 : 3;
  f.history.splice(0, f.history.length, ...(operation === "install" ? [] : [
    { revision: 1, status: "deployed" }, { revision: 2, status: "failed" },
  ]), { revision, status: `pending-${operation}` });
  const manifests = new Map<number, string>(Array.from({ length: revision }, (_, index) => [index + 1, render(documents)]));
  const reads: { file: string; args: readonly string[]; timeout?: number }[] = [];
  let before = (_file: string, _args: readonly string[]) => {};
  const execute: SchemaExecute = async (file, args, options) => {
    reads.push({ file, args, timeout: options.timeout });
    before(file, args);
    if (file === "helm" && args[0] === "get" && args[1] === "manifest") {
      if (!args.includes("--revision")) throw new Error("Unversioned manifest escaped the pending-owner fence");
      const text = manifests.get(Number(args[args.indexOf("--revision") + 1]));
      if (text === undefined) throw new Error("Missing exact release manifest");
      return { stdout: text };
    }
    return f.execute(file, args, options);
  };
  const options: CoreSchemaHookOptions = { ...f.owner, ...f.wait, revision, operation, installerHelmVersion: "v4.1.3" };
  return { ...f, execute, options, schemaOptions: f.options, manifests, reads, before: (callback: typeof before) => { before = callback; },
    run: (overrides: Partial<CoreSchemaHookOptions> = {}) => preparePendingCoreHelmSchemas(execute, { ...options, ...overrides }) };
}

describe("pending Helm release schema staging", () => {
  it("publishes exact retained schemas from revision one without rendering, policies or lifecycle commands", async () => {
    const f = fixture();
    await expect(f.run()).resolves.toMatchObject({ revision: 1, schemas: 1, published: true });
    expect(f.writes).toHaveLength(1);
    expect(f.writes[0].metadata.annotations).toMatchObject({ "helm.sh/resource-policy": "keep",
      "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" });
    expect(f.reads.every(request => request.timeout! > 0 && request.timeout! <= 1500)).toBe(true);
    expect(f.reads.some(request => request.args.includes("/openapi/v3"))).toBe(true);
    expect(f.reads.filter(request => request.file === "helm").every(request => ["get", "history"].includes(request.args[0]))).toBe(true);
    expect(f.reads.some(request => ["patch", "delete", "template", "upgrade", "rollback"].includes(request.args[0]))).toBe(false);
  });

  it("pins the failed immediate predecessor for owner lookup and the latest successful revision for rollback", async () => {
    const f = fixture("upgrade");
    const old = crd();
    old.spec.versions[0].schema.openAPIV3Schema.description = "Previous chart description";
    f.manifests.set(2, render([old, admission(), controller]));
    const current = f.install(old);
    delete current.metadata.annotations[SCHEMA_DIGEST];
    await expect(f.run()).resolves.toMatchObject({ revision: 3, published: true });
    const revisions = new Set(f.reads.filter(request => request.file === "helm" && request.args[0] === "get")
      .map(request => request.args[request.args.indexOf("--revision") + 1]));
    expect(revisions).toEqual(new Set(["1", "2", "3"]));
    expect(f.writes).toHaveLength(1);
    const request = f.requests.find(item => item.args[0] === "apply")!;
    expect(request.args).toContain("--server-side");
    expect(JSON.parse(request.input!).metadata).toMatchObject({ uid: current.metadata.uid, resourceVersion: current.metadata.resourceVersion });
  });

  it("does not confuse an unrecorded pending target with the prior owner", async () => {
    const f = fixture("upgrade");
    const prior = crd();
    prior.spec.versions[0].schema.openAPIV3Schema.description = "Expected prior owner";
    f.manifests.set(2, render([prior, admission(), controller]));
    const current = f.install(crd());
    current.spec.versions[0].schema.openAPIV3Schema.description = "Customer customization";
    delete current.metadata.annotations[SCHEMA_DIGEST];
    await expect(f.run()).rejects.toThrow("conflicts with its Helm release");
    expect(f.writes).toEqual([]);
  });

  it.each(["v3.19.0", "v4.1.2", "v4.1.3-rc.1", "v4.2.0", "v5.0.0"])("refuses uninspected installer %s before any read/write", async version => {
    const f = fixture();
    await expect(f.run({ installerHelmVersion: version })).rejects.toThrow("Helm 4.1.3");
    expect(f.reads).toEqual([]);
  });

  it.each([
    { revision: 0 }, { revision: 1.5 }, { release: "--other" }, { namespace: "wrong.namespace" },
    { timeoutMs: 0 }, { timeoutMs: 600001 }, { timeoutMs: Number.NaN },
  ])("rejects invalid exact options %j without commands", async invalid => {
    const f = fixture();
    await expect(f.run(invalid)).rejects.toThrow();
    expect(f.reads).toEqual([]);
  });

  it.each(["duplicate", "other-pending", "unknown-state", "wrong-operation", "later-revision", "missing-prior", "no-success"])(
    "rejects %s history before schema writes", async fault => {
      const f = fixture("upgrade");
      if (fault === "duplicate") f.history.push({ revision: 2, status: "failed" });
      if (fault === "other-pending") f.history[0].status = "pending-rollback";
      if (fault === "unknown-state") f.history[0].status = "unknown";
      if (fault === "wrong-operation") f.history.at(-1)!.status = "pending-install";
      if (fault === "later-revision") f.history.push({ revision: 4, status: "failed" });
      if (fault === "missing-prior") f.history.splice(1, 1);
      if (fault === "no-success") f.history[0].status = "failed";
      await expect(f.run()).rejects.toThrow();
      expect(f.writes).toEqual([]);
    });

  it("refuses release replacement masquerading as a cold install", async () => {
    const f = fixture("upgrade");
    f.history.at(-1)!.status = "pending-install";
    await expect(f.run({ operation: "install" })).rejects.toThrow("historical release");
    expect(f.writes).toEqual([]);
  });

  it("asks for one more than its history bound so truncation cannot look complete", async () => {
    const f = fixture();
    const execute: SchemaExecute = async () => ({ stdout: JSON.stringify(Array.from({ length: 1025 }, (_, i) =>
      ({ revision: i + 1, status: i === 1024 ? "pending-upgrade" : "superseded" }))) });
    await expect(preparePendingCoreHelmSchemas(execute, { ...f.options, revision: 1025, operation: "upgrade" }))
      .rejects.toThrow("incomplete or ambiguous");
    await f.run();
    expect(f.reads.find(item => item.args[0] === "history")!.args).toEqual(expect.arrayContaining(["--max", "1025"]));
  });

  it.each([1, 2, 3])("refuses drift in pinned manifest revision %s before writes", async revision => {
    const f = fixture("upgrade");
    let reads = 0;
    f.before((file, args) => {
      if (file === "helm" && args[0] === "get" && Number(args.at(-1)) === revision && ++reads === 2) {
        f.manifests.set(revision, render([crd()]));
      }
    });
    await expect(f.run()).rejects.toThrow("manifest changed");
    expect(f.writes).toEqual([]);
  });

  it("refuses history drift between planning and the first schema write", async () => {
    const f = fixture();
    f.before((file, args) => {
      if (file === "kubectl" && args[1] === "customresourcedefinition") f.history[0].status = "failed";
    });
    await expect(f.run()).rejects.toThrow("pending revision");
    expect(f.writes).toEqual([]);
  });

  it("rechecks between schema writes and retains the already-created schema on interruption", async () => {
    const f = fixture("install", [crd(), crd("KarsSandbox", "karssandboxes"), controller]);
    f.beforeWrite(() => { f.history[0].status = "failed"; });
    await expect(f.run()).rejects.toThrow("pending revision");
    expect(f.writes).toHaveLength(1);
    expect(f.objects.has(crd().metadata.name)).toBe(true);
    expect(f.requests.some(request => request.args[0] === "delete")).toBe(false);
  });

  it.each(["foreign", "warning", "protected", "missing-retention", "crd-hook", "ordinary-custom-resource"])(
    "refuses %s before any schema writes", async fault => {
      const f = fixture();
      if (fault === "foreign") f.install(crd(), { ...f.owner, release: "other" });
      if (fault === "warning") {
        const policy = admission();
        policy.metadata = { ...policy.metadata, uid: "policy", resourceVersion: "1", generation: 1 };
        policy.status = { observedGeneration: 1, typeChecking: { expressionWarnings: [{ warning: "invalid params" }] } };
        f.objects.set(policy.metadata.name, policy);
      }
      if (fault === "protected") f.objects.set("namespace/kars-system", protectedNamespace);
      if (fault === "missing-retention" || fault === "crd-hook") {
        const object = crd();
        if (fault === "missing-retention") delete object.metadata.annotations["helm.sh/resource-policy"];
        else object.metadata.annotations["helm.sh/hook"] = "pre-install";
        f.manifests.set(1, render([object, admission(), controller]));
      }
      if (fault === "ordinary-custom-resource") f.manifests.set(1, render([crd(), admission(), controller,
        { apiVersion: "kars.azure.com/v1alpha1", kind: "ToolPolicy", metadata: { name: "sre-tools" } }]));
      await expect(f.run()).rejects.toThrow();
      expect(f.writes).toEqual([]);
    });

  it("protects a controller removed from the target but still present in the prior release", async () => {
    const f = fixture("upgrade");
    f.manifests.set(3, render([crd(), admission()]));
    f.objects.set("namespace/kars-system", protectedNamespace);
    await expect(f.run()).rejects.toThrow("KARS_PRIVATE_ROOT_UPGRADE_BLOCKED");
    expect(f.writes).toEqual([]);
  });

  it("refuses an upgrade whose latest successful rollback would lose new fields", async () => {
    const f = fixture("upgrade");
    const older = crd();
    delete older.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.enabled;
    f.manifests.set(1, render([older, admission(), controller]));
    await expect(f.run()).rejects.toThrow("migration");
    expect(f.writes).toEqual([]);
  });

  it("refuses a removed retained CRD even if the rollback target would otherwise be compatible", async () => {
    const f = fixture("upgrade");
    f.manifests.set(2, render([crd(), crd("KarsSandbox", "karssandboxes"), controller]));
    await expect(f.run()).rejects.toThrow("remove a core CRD");
    expect(f.writes).toEqual([]);
  });

  it.each([false, true])("refuses new CRDs outside Helm's original manifest even if already present=%s", async present => {
    const added = crd("KarsSandbox", "karssandboxes");
    const f = fixture("upgrade", [crd(), added, admission(), controller]);
    for (const revision of [1, 2]) f.manifests.set(revision, render([crd(), admission(), controller]));
    if (present) f.install(added);
    await expect(f.run()).rejects.toThrow("Helm 4.1.3 cannot stage CRDs absent from its original upgrade manifest");
    expect(f.writes).toEqual([]);
    expect(f.reads.every(request => request.file === "helm")).toBe(true);
  });

  it("does not mistake a failed predecessor for Helm's deployed original manifest", async () => {
    const added = crd("KarsSandbox", "karssandboxes");
    const f = fixture("upgrade", [crd(), added, admission(), controller]);
    f.manifests.set(1, render([crd(), admission(), controller]));
    await expect(f.run()).rejects.toThrow("Helm 4.1.3 cannot stage CRDs absent from its original upgrade manifest");
    expect(f.writes).toEqual([]);
  });

  it("uses the last failed manifest as Helm's original when no deployed revision remains", async () => {
    const added = crd("KarsSandbox", "karssandboxes");
    const f = fixture("upgrade", [crd(), added, admission(), controller]);
    f.history[0].status = "superseded";
    f.manifests.set(2, render([crd(), admission(), controller]));
    await expect(f.run()).rejects.toThrow("Helm 4.1.3 cannot stage CRDs absent from its original upgrade manifest");
    expect(f.writes).toEqual([]);
  });

  it("preserves upgrades of CRDs in the deployed original but absent from a failed predecessor", async () => {
    const added = crd("KarsSandbox", "karssandboxes");
    const f = fixture("upgrade", [crd(), added, admission(), controller]);
    f.manifests.set(2, render([crd(), admission(), controller]));
    await expect(f.run()).resolves.toMatchObject({ revision: 3, schemas: 2, published: true });
    expect(f.writes).toHaveLength(2);
  });

  it.each(["missing", "drift", "unchanged"])("pins a deployed original distinct from prior and rollback manifests: %s", async state => {
    const added = crd("KarsSandbox", "karssandboxes");
    const documents = [crd(), added, admission(), controller];
    const f = fixture("upgrade", documents);
    f.history.splice(0, f.history.length, { revision: 1, status: "deployed" },
      { revision: 2, status: "superseded" }, { revision: 3, status: "failed" }, { revision: 4, status: "pending-upgrade" });
    f.manifests.set(4, render(documents));
    if (state === "missing") f.manifests.set(1, render([crd(), admission(), controller]));
    let originalReads = 0;
    f.before((file, args) => {
      if (file === "helm" && args[0] === "get" && args.at(-1) === "1" && ++originalReads === 2 && state === "drift") {
        f.manifests.set(1, render([crd(), admission(), controller]));
      }
    });
    if (state === "unchanged") {
      await expect(f.run({ revision: 4 })).resolves.toMatchObject({ revision: 4, schemas: 2, published: true });
      expect(originalReads).toBeGreaterThan(1);
      expect(f.writes).toHaveLength(2);
    } else {
      await expect(f.run({ revision: 4 })).rejects.toThrow(state === "missing"
        ? "Helm 4.1.3 cannot stage CRDs absent from its original upgrade manifest" : "Pinned Helm manifest changed");
      expect(f.writes).toEqual([]);
    }
  });

  it("does not return success if the pending manifest changes during publication", async () => {
    const f = fixture();
    f.beforeRaw(path => { if (path === "/openapi/v3") f.manifests.set(1, render([crd(), controller])); });
    await expect(f.run()).rejects.toThrow("manifest changed");
    expect(f.writes).toHaveLength(1);
    expect(f.requests.some(request => ["delete", "patch"].includes(request.args[0]))).toBe(false);
  });

  it("keeps one total deadline across Helm reads, safety checks and publication", async () => {
    const f = fixture("install", [crd(), admission()]);
    let now = 0;
    f.before(() => { now += 200; });
    await expect(f.run({ now: () => now, timeoutMs: 1000 })).rejects.toThrow("deadline expired");
    expect(f.writes).toEqual([]);
    expect(f.reads.map(item => item.timeout)).toEqual([1000, 800, 600, 400, 200]);
  });

  it.each(["unpublished", "dangling", "changedType", "unhashed", "foreign-discovery-link"])(
    "cannot declare publication through %s discovery", async fault => {
      const f = fixture();
      if (fault === "unpublished") f.schemaOptions.published = false;
      if (fault === "dangling") f.schemaOptions.dangling = true;
      if (fault === "changedType") f.schemaOptions.changedType = true;
      if (fault === "unhashed") f.schemaOptions.link = "/openapi/v3/apis/kars.azure.com/v1alpha1";
      if (fault === "foreign-discovery-link") f.schemaOptions.link = "https://foreign.invalid/openapi?hash=x";
      await expect(f.run()).rejects.toThrow();
      expect(f.writes).toHaveLength(1);
      expect(f.requests.some(request => ["patch", "delete"].includes(request.args[0]))).toBe(false);
    });

  it("rechecking the same pending release does not write identical owned schemas again", async () => {
    const f = fixture();
    await f.run();
    const writes = f.writes.length;
    await expect(f.run()).resolves.toMatchObject({ published: true });
    expect(f.writes).toHaveLength(writes);
  });

  it("refuses an uninstalled historical release rather than silently recreating it", async () => {
    const f = fixture("upgrade");
    f.history[1].status = "uninstalled";
    await expect(f.run()).rejects.toThrow("sole exact latest pending revision");
    expect(f.writes).toEqual([]);
  });

  it("never treats a Helm authorization failure as an absent release", async () => {
    const f = fixture();
    f.before(file => { if (file === "helm") throw new Error("403 release history forbidden"); });
    await expect(f.run()).rejects.toThrow("403 release history forbidden");
    expect(f.writes).toEqual([]);
  });
});
