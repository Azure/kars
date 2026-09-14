// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { canonicalSchema, SCHEMA_DIGEST, schemaDocuments } from "./schema-documents.js";
import { stageCoreSchemaDocuments, waitForInstalledCoreSchemas } from "./schema-stage.js";
import { admission, crd, schemaFixture } from "./schema-stage.test-support.js";

describe("schema-before-admission lifecycle", () => {
  it("stages the actual chart CRDs before any policy and keeps them in the Helm-owned templates", async () => {
    const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
    const rendered = execFileSync("helm", ["template", "kars", chart, "--namespace", "kars-system", "--dry-run=client"], { encoding: "utf8" });
    const documents = schemaDocuments(rendered);
    const f = schemaFixture(documents);
    await expect(stageCoreSchemaDocuments(f.execute, documents, { ...f.owner, ...f.wait })).resolves.toEqual({
      schemas: documents.filter(object => object.kind === "CustomResourceDefinition").length, published: true,
    });
    expect(f.writes.length).toBeGreaterThan(15);
    expect(f.writes.every(object => object.kind === "CustomResourceDefinition")).toBe(true);
    expect(f.writes.every(object => object.metadata.labels["app.kubernetes.io/managed-by"] === "Helm"
      && object.metadata.annotations["meta.helm.sh/release-name"] === "kars"
      && object.metadata.annotations["meta.helm.sh/release-namespace"] === "kars-system")).toBe(true);
    expect(f.requests.some(request => request.args.includes("/openapi/v3"))).toBe(true);
    const before = canonicalSchema([...f.objects]);
    f.writes.length = 0;
    await stageCoreSchemaDocuments(f.execute, documents, { ...f.owner, ...f.wait });
    expect(f.writes).toEqual([]);
    expect(canonicalSchema([...f.objects])).toBe(before);
  });

  it("does not substitute Established for published and resolvable parameter schemas", async () => {
    const documents = [crd(), admission()];
    const f = schemaFixture(documents);
    f.options.established = false;
    f.options.published = false;
    let steps = 0;
    f.onSleep(() => {
      steps++;
      if (steps === 1) f.options.established = true;
      if (steps === 2) { f.options.published = true; f.options.dangling = true; }
      if (steps === 3) f.options.dangling = false;
    });
    await stageCoreSchemaDocuments(f.execute, documents, { ...f.owner, ...f.wait, timeoutMs: 2000 });
    expect(steps).toBe(3);
    expect(f.requests.filter(request => request.args.includes("/openapi/v3")).length).toBeGreaterThan(2);
    expect(f.writes).toHaveLength(1);
  });

  it.each(["published", "resourceVisible", "dangling", "changedType"])("never proceeds when %s prevents KCM-compatible resolution", async flag => {
    const f = schemaFixture();
    if (flag === "published" || flag === "resourceVisible") f.options[flag] = false;
    else if (flag === "dangling") f.options.dangling = true;
    else f.options.changedType = true;
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow("Timed out");
    expect(f.writes.every(object => object.kind === "CustomResourceDefinition")).toBe(true);
    expect(f.requests.some(request => ["patch", "delete"].includes(request.args[0]))).toBe(false);
  });

  it("rechecks the advertised hash instead of retaining a stale discovery document", async () => {
    const f = schemaFixture();
    let indexes = 0;
    f.beforeRaw(path => {
      if (path === "/openapi/v3" && ++indexes === 2) f.options.link = "/openapi/v3/apis/kars.azure.com/v1alpha1?hash=next";
    });
    await stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait });
    expect(f.requests.some(request => request.args.includes("/openapi/v3/apis/kars.azure.com/v1alpha1?hash=next"))).toBe(true);
    expect(indexes).toBeGreaterThanOrEqual(4);
  });

  it.each(["https://foreign.example/schema?hash=one", "/openapi/v3/apis/kars.azure.com/v1alpha1",
    "/openapi/v3/apis/kars.azure.com/v1alpha1?hash=one&token=other"])("rejects untrusted discovery link %s", async link => {
    const f = schemaFixture();
    f.options.link = link;
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow("untrusted or unhashed");
    expect(f.requests.some(request => request.args.includes(link))).toBe(false);
  });

  it("propagates discovery authorization/transport failures rather than treating them as absence", async () => {
    const f = schemaFixture();
    f.beforeRaw(() => { throw new Error("403 discovery forbidden"); });
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow("403 discovery forbidden");
  });

  it("waits for a not-yet-published group route, but never treats a missing CRD as discovery lag", async () => {
    const f = schemaFixture();
    let missing = true;
    f.beforeRaw(path => {
      if (path === "/apis/kars.azure.com/v1alpha1" && missing) {
        missing = false;
        throw Object.assign(new Error("404"), { stderr: "Error from server (NotFound): resource discovery is not published" });
      }
    });
    await stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait });
    expect(f.requests.filter(request => request.args.includes("/apis/kars.azure.com/v1alpha1"))).toHaveLength(2);
  });

  it.each(["uid", "schema", "owner"])("rejects a racing %s change during publication", async fault => {
    const f = schemaFixture();
    f.beforeRaw(path => {
      if (path !== "/openapi/v3") return;
      const current = f.objects.get(crd().metadata.name)!;
      if (fault === "uid") current.metadata.uid = "replacement";
      if (fault === "schema") current.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.extra = { type: "boolean" };
      if (fault === "owner") current.metadata.annotations["meta.helm.sh/release-name"] = "other";
    });
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow();
    expect(f.writes.every(object => object.kind === "CustomResourceDefinition")).toBe(true);
  });

  it("surfaces SSA field ownership conflicts without retrying with force", async () => {
    const f = schemaFixture();
    const old = crd();
    delete old.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.enabled;
    f.install(old);
    f.beforeWrite(() => { throw new Error("409 field manager conflict"); });
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow("field manager conflict");
    expect(f.writes).toEqual([]);
    expect(f.requests.some(request => request.args.some(arg => arg.startsWith("--force")))).toBe(false);
  });

  it.each(["release", "namespace", "manager", "unmarked", "owner-reference", "terminating"])("fails %s ownership before any schema writes", async fault => {
    const second = crd("KarsSandbox", "karssandboxes");
    const f = schemaFixture([crd(), second, admission()]);
    const existing = f.install(second);
    if (fault === "release") existing.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "namespace") existing.metadata.annotations["meta.helm.sh/release-namespace"] = "foreign";
    if (fault === "manager") existing.metadata.labels["app.kubernetes.io/managed-by"] = "terraform";
    if (fault === "unmarked") { existing.metadata.labels = {}; existing.metadata.annotations = {}; }
    if (fault === "owner-reference") existing.metadata.ownerReferences = [{ uid: "other" }];
    if (fault === "terminating") existing.metadata.deletionTimestamp = "2026-09-11T00:00:00Z";
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), second, admission()], { ...f.owner, ...f.wait })).rejects.toThrow();
    expect(f.writes).toEqual([]);
  });

  it("updates only a recorded owned schema using UID/RV and non-forced SSA, preserving custom resources", async () => {
    const desired = crd();
    const old = structuredClone(desired);
    delete old.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.enabled;
    const f = schemaFixture([desired, admission()]);
    const current = f.install(old);
    current.metadata.annotations["customer.example/keep"] = "retain";
    const uid = current.metadata.uid;
    const version = current.metadata.resourceVersion;
    const customer = { kind: "KarsCredentialGrant", metadata: { name: "workspace", uid: "customer" }, spec: { enabled: true } };
    f.objects.set("customer-resource", structuredClone(customer));
    await stageCoreSchemaDocuments(f.execute, [desired, admission()], { ...f.owner, ...f.wait });
    const request = f.requests.find(request => request.args[0] === "apply")!;
    expect(request.args).toContain("--server-side");
    expect(request.args.some(arg => arg.startsWith("--force"))).toBe(false);
    expect(JSON.parse(request.input!).metadata).toMatchObject({ uid, resourceVersion: version });
    expect(f.objects.get(desired.metadata.name)?.metadata.annotations["customer.example/keep"]).toBe("retain");
    expect(f.objects.get("customer-resource")).toEqual(customer);
    expect(f.writes).toHaveLength(1);
  });

  it("uses the owning Helm release manifest for pre-existing CRDs without a staging record", async () => {
    const desired = crd();
    const old = structuredClone(desired);
    delete old.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.enabled;
    const f = schemaFixture([old, admission()]);
    const current = f.install(old);
    delete current.metadata.annotations[SCHEMA_DIGEST];
    await stageCoreSchemaDocuments(f.execute, [desired, admission()], { ...f.owner, ...f.wait });
    expect(f.requests.some(request => request.file === "helm" && request.args[0] === "get" && request.args[1] === "manifest")).toBe(true);
    expect(f.objects.get(desired.metadata.name)?.spec).toEqual(desired.spec);
  });

  it("rejects customized schemas even if the release ownership labels match", async () => {
    const f = schemaFixture();
    const current = f.install(crd());
    current.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.customer = { type: "string" };
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow("conflicts with its Helm release");
    expect(f.writes).toEqual([]);
  });

  it.each(["uid", "resourceVersion"])("rejects a concurrent %s change at the owned update CAS", async field => {
    const desired = crd();
    const old = structuredClone(desired);
    delete old.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.enabled;
    const f = schemaFixture();
    const current = f.install(old);
    f.beforeWrite(() => { current.metadata[field] = "replaced"; });
    await expect(stageCoreSchemaDocuments(f.execute, [desired, admission()], { ...f.owner, ...f.wait })).rejects.toThrow("409");
    expect(f.writes).toEqual([]);
  });

  it("does not remove a stored custom-resource version during schema preparation", async () => {
    const f = schemaFixture();
    const old = crd();
    old.spec.versions.push({ ...structuredClone(old.spec.versions[0]), name: "v1beta1", storage: false });
    const current = f.install(old);
    current.status.storedVersions.push("v1beta1");
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow("storage migration");
    expect(f.writes).toEqual([]);
  });

  it("cannot heal an already-observed warning by waiting, deleting or changing policy text/status", async () => {
    const f = schemaFixture();
    const policy = admission();
    policy.metadata = { ...policy.metadata, uid: "policy", resourceVersion: "1", generation: 1 };
    policy.status = { observedGeneration: 1, typeChecking: { expressionWarnings: [{ warning: "undeclared reference params" }] } };
    f.objects.set(policy.metadata.name, policy);
    await expect(stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait })).rejects.toThrow("already has observed");
    expect(f.writes).toEqual([]);
    expect(f.requests.every(request => request.args[0] === "get")).toBe(true);
  });

  it.each([false, true])("waits for an existing pending policy and rejects poisoned observation=%s", async poisoned => {
    const f = schemaFixture();
    const policy = admission();
    policy.metadata = { ...policy.metadata, uid: "policy", resourceVersion: "1", generation: 1 };
    f.objects.set(policy.metadata.name, policy);
    f.onSleep(() => {
      policy.status = { observedGeneration: 1, typeChecking: {
        expressionWarnings: poisoned ? [{ warning: "undeclared reference params" }] : [],
      } };
    });
    const result = stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...f.owner, ...f.wait });
    if (poisoned) await expect(result).rejects.toThrow("already has observed");
    else await expect(result).resolves.toMatchObject({ published: true });
    expect(f.writes.every(object => object.kind === "CustomResourceDefinition")).toBe(true);
    expect(policy.metadata.generation).toBe(1);
  });

  it("checks already installed SRE schemas through the same resolver without adopting them", async () => {
    const f = schemaFixture();
    const existing = f.install(crd());
    existing.metadata.labels = {};
    existing.metadata.annotations = {};
    await waitForInstalledCoreSchemas(f.execute, [crd(), admission()], f.wait);
    expect(f.writes).toEqual([]);
    expect(f.requests.some(request => request.args.includes("/openapi/v3"))).toBe(true);
  });

  it("creates explicit template ownership and supports check-only without writes", async () => {
    const f = schemaFixture();
    const options = { ...f.owner, ...f.wait, ownership: "template" as const };
    await stageCoreSchemaDocuments(f.execute, [crd(), admission()], options);
    expect(f.writes[0].metadata.labels["app.kubernetes.io/managed-by"]).toBe("kars-schema-stage");
    expect(f.writes[0].metadata.annotations["meta.helm.sh/release-name"]).toBeUndefined();
    f.writes.length = 0;
    await stageCoreSchemaDocuments(f.execute, [crd(), admission()], { ...options, checkOnly: true });
    expect(f.writes).toEqual([]);
  });
});
