// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { parse } from "yaml";
import { describe, expect, it, vi } from "vitest";
import { ACTION_CRD, planActionCrd } from "./sre-action-crd.js";
import { stageAuthority } from "./sre-stage.js";
import type { Execute } from "./sre-authority.js";
import { crd, schemaFixture } from "./schema-stage.test-support.js";
import { normalizedCrd, schemaDigest, schemaOwnerFields, SCHEMA_DIGEST } from "./schema-documents.js";
import { migrationFixture } from "./sre-migration.test-support.js";

const action = parse(readFileSync(new URL("../../../deploy/helm/kars/templates/crd-karssreaction.yaml", import.meta.url), "utf8"));
const registration = parse(readFileSync(new URL("../../../deploy/helm/kars/templates/crd-karssreregistration.yaml", import.meta.url), "utf8"));
const policy = { apiVersion: "admissionregistration.k8s.io/v1", kind: "ValidatingAdmissionPolicy",
  metadata: { name: "kars-sre-test" }, spec: { failurePolicy: "Fail" } };
const params = (object: any) => object.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.action.properties.params;

function fixture(helm = false) {
  const existing = structuredClone(action);
  existing.metadata.uid = "action-uid";
  existing.metadata.resourceVersion = "17";
  existing.metadata.annotations = { "operator.example/keep": "custom metadata" };
  if (helm) {
    existing.metadata.labels["app.kubernetes.io/managed-by"] = "Helm";
    Object.assign(existing.metadata.annotations, { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" });
  } else {
    Object.assign(existing.metadata.annotations, { "kars.azure.com/sre-authority-staged": "kars-system", "kars.azure.com/sre-authority-release": "kars" });
  }
  delete params(existing)["x-kubernetes-preserve-unknown-fields"];
  params(existing).additionalProperties = true;
  if (helm) existing.metadata.annotations[SCHEMA_DIGEST] = schemaDigest(normalizedCrd(existing));
  const schemas = helm ? migrationFixture() : schemaFixture([action, registration, policy]);
  const documents = helm ? [...("after" in schemas ? schemas.after : []), policy] : [action, registration, policy];
  schemas.objects.set(ACTION_CRD, existing);
  const controller = { metadata: { name: "kars-controller", namespace: "kars-system", uid: "controller-uid", resourceVersion: "2" },
    spec: { replicas: 0, template: { spec: { serviceAccountName: "kars-controller", containers: [{ name: "controller", image: "old:latest" }] } } } };
  if(helm) Object.assign(controller.metadata,schemaOwnerFields({ownership:"helm",namespace:"kars-system",release:"kars"}));
  const execute = vi.fn<Execute>(async (file, args, options) => {
    if (file === "helm") {
      if (args[0] === "list") return { stdout: helm ? '[{"name":"kars","namespace":"kars-system"}]' : "[]" };
      if (args[0] === "version") return { stdout: "v4.2.4" };
      if (args[0] === "template") return { stdout: documents.map(obj => JSON.stringify(obj)).join("\n---\n") };
      if (args[0] === "upgrade") return { stdout: "" };
      return schemas.execute(file,args,options);
    }
    if (args[0] === "auth") return { stdout: "yes" };
    if (args[0] === "get") {
      if(args[1] === "deployment")return {stdout:JSON.stringify(controller)};
      return schemas.execute(file,args,options);
    }
    if (args[0] === "patch" && args[2] === ACTION_CRD) {
      const operations = JSON.parse(args[args.indexOf("-p") + 1]);
      for (const operation of operations) {
        const segments = operation.path.slice(1).split("/").map((value: string) => value.replaceAll("~1", "/").replaceAll("~0", "~"));
        const target = segments.slice(0, -1).reduce((obj: any, key: string) => obj[key], existing);
        const key = segments.at(-1);
        if (operation.op === "test" && JSON.stringify(target[key]) !== JSON.stringify(operation.value)) throw new Error("409 UID/RV conflict");
        if (operation.op === "remove") delete target[key];
        if (operation.op === "add") target[key] = operation.value;
      }
      if(operations.some((operation:any)=>operation.op!=="test"))existing.metadata.resourceVersion=String(Number(existing.metadata.resourceVersion)+1);
      return {stdout:JSON.stringify(existing)};
    }
    if(args[0]==="create"||args[0]==="apply") {
      const result=await schemas.execute(file,args,options);
      if(args[0]==="apply"&&!args.includes("--dry-run=server")&&JSON.parse(options.input!).metadata.name===ACTION_CRD) {
        Object.assign(existing,JSON.parse(result.stdout));
        schemas.objects.set(ACTION_CRD,existing);
      }
      return result;
    }
    return { stdout: "" };
  });
  const run = (dry = false, exec: Execute = execute) => stageAuthority(exec, "chart", "kars-system", "kars", "controller:latest", "router:latest", dry);
  return { existing, controller, execute, run, schemas };
}

describe("existing action API prerequisite compatibility", () => {
  it("feeds the actual offline Helm render through template staging with the repaired action API first", async () => {
    const f = fixture();
    const execute: Execute = (file, args, options) => file === "helm" && args[0] === "template"
      ? execa(file, [...args, "--dry-run=client"], { ...options, timeout: 20_000 }) : f.execute(file, args, options);
    const root = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
    const directory = mkdtempSync(join(tmpdir(), "kars-sre-stage-"));
    const chart = join(directory, "chart");
    try {
      mkdirSync(join(chart, "templates"), { recursive: true });
      for (const file of [
        "Chart.yaml", "values.yaml",
        "templates/crd-karssreaction.yaml", "templates/crd-karssreregistration.yaml",
        "templates/sre-authority-rbac.yaml", "templates/sre-authority-admission.yaml",
        "templates/sre-authority-consumers.yaml",
      ]) copyFileSync(join(root, file), join(chart, file));
      await stageAuthority(execute, chart, "kars-system", "kars", "controller:latest", "router:latest", false);
      expect(params(f.existing)["x-kubernetes-preserve-unknown-fields"]).toBe(true);
      const created = f.execute.mock.calls.filter(([, args]) => args[0] === "create").map(([, , options]) => JSON.parse(options.input!));
      expect(created.some(obj => obj.metadata.name === registration.metadata.name)).toBe(true);
      expect(created.filter(obj => obj.kind === "ValidatingAdmissionPolicy")).toHaveLength(14);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  }, 30_000);

  it.each([false, true])("repairs ONLY recognized legacy params before dependent policies (Helm: %s)", async helm => {
    const f = fixture(helm);
    const before = structuredClone(f.existing);
    await f.run();
    const repaired = structuredClone(before);
    delete params(repaired).additionalProperties;
    params(repaired)["x-kubernetes-preserve-unknown-fields"] = true;
    expect(normalizedCrd(f.existing)).toEqual(normalizedCrd(repaired));
    expect(f.existing.metadata.uid).toBe(before.metadata.uid);
    expect(f.existing.metadata.annotations["operator.example/keep"]).toBe("custom metadata");
    const calls = f.execute.mock.calls;
    const patch = calls.findIndex(([, args, settings]) => helm
      ? args[0]==="apply"&&!args.includes("--dry-run=server")&&JSON.parse(settings.input!).metadata.name===ACTION_CRD
      : args[0] === "patch" && args[2] === ACTION_CRD);
    const wait = calls.findIndex(([, args]) => args.includes("/openapi/v3"));
    const dependent = calls.findIndex(([file, args, options]) => helm ? file === "helm" && args[0] === "upgrade" && !args.includes("--dry-run=server")
      : args[0] === "create" && JSON.parse(options.input!).kind === "ValidatingAdmissionPolicy");
    expect(patch).toBeGreaterThan(0);
    expect(wait).toBeGreaterThan(patch);
    expect(dependent).toBeGreaterThan(wait);
    if(helm) expect(JSON.parse(calls[patch][2].input!).metadata).toMatchObject({uid:"action-uid",resourceVersion:"17"});
    else {
      const operations = JSON.parse(calls[patch][1].at(-1)!);
      expect(operations.slice(0, 2)).toEqual([
        { op: "test", path: "/metadata/uid", value: "action-uid" },
        { op: "test", path: "/metadata/resourceVersion", value: "17" },
      ]);
    }
    expect(f.existing.metadata.annotations["kars.azure.com/sre-authority-staged"]).toBe(helm ? undefined : "kars-system");
    if (helm) expect(calls[dependent][1]).toEqual(expect.arrayContaining(["--wait=legacy", "--timeout", "8m"]));
  });

  it("recognizes only Kubernetes API defaults while preserving their serialized fields", async () => {
    const f = fixture();
    f.existing.spec.names.listKind = "KarsSREActionList";
    delete f.existing.spec.names.categories;
    f.existing.spec.conversion = { strategy: "None" };
    f.existing.spec.preserveUnknownFields = false;
    f.existing.spec.versions[0].deprecated = false;
    f.existing.spec.versions[0].additionalPrinterColumns[0].priority = 0;
    await f.run();
    expect(f.existing.spec.conversion).toEqual({ strategy: "None" });
    expect(f.existing.spec.names.listKind).toBe("KarsSREActionList");
    expect(f.existing.spec.names.categories).toBeUndefined();
  });

  it("accepts the exact template schema owner created by core preparation", async () => {
    const f = fixture();
    const fields = schemaOwnerFields({ ownership: "template", release: "kars", namespace: "kars-system" });
    f.existing.metadata.labels = { ...f.existing.metadata.labels, ...fields.labels };
    Object.assign(f.existing.metadata.annotations, fields.annotations);
    await f.run();
    expect(params(f.existing)["x-kubernetes-preserve-unknown-fields"]).toBe(true);
  });

  it("does not repair an unmarked action schema merely because its spec is recognizable", async () => {
    const f = fixture();
    delete f.existing.metadata.annotations["kars.azure.com/sre-authority-staged"];
    delete f.existing.metadata.annotations["kars.azure.com/sre-authority-release"];
    await expect(f.run()).rejects.toThrow("unmarked action CRD");
    expect(f.execute.mock.calls.some(([, args]) => ["create", "patch", "apply"].includes(args[0]))).toBe(false);
  });

  it.each([
    (obj: any) => { obj.metadata.annotations["meta.helm.sh/release-name"] = "foreign"; },
    (obj: any) => { obj.metadata.annotations["meta.helm.sh/release-namespace"] = "foreign"; },
    (obj: any) => { obj.metadata.annotations["kars.azure.com/sre-authority-staged"] = "foreign"; },
    (obj: any) => { obj.metadata.annotations["kars.azure.com/sre-authority-release"] = "foreign"; },
    (obj: any) => { obj.metadata.labels["app.kubernetes.io/managed-by"] = "other-operator"; },
    (obj: any) => { obj.metadata.ownerReferences = [{ uid: "foreign-owner" }]; },
    (obj: any) => { obj.metadata.deletionTimestamp = "2026-09-10T00:00:00Z"; },
    (obj: any) => { params(obj).properties = { custom: { type: "string" } }; },
    (obj: any) => { obj.spec.versions[0].schema.openAPIV3Schema.properties.spec.required.push("diagnosis"); },
    (obj: any) => { obj.spec.versions.push(structuredClone(obj.spec.versions[0])); },
    (obj: any) => { obj.spec.conversion = { strategy: "Webhook" }; },
  ])("refuses foreign/customized schemas without writing any authority resource: %#", async modify => {
    const f = fixture();
    modify(f.existing);
    await expect(f.run()).rejects.toThrow(/Foreign|Customized/);
    expect(f.execute.mock.calls.some(([, args]) => ["patch", "create", "upgrade"].includes(args[0]))).toBe(false);
  });

  it.each(["uid", "resourceVersion"])("rejects a racing %s replacement before policy creation", async field => {
    const f = fixture();
    const stage = await planActionCrd(f.execute, action, "kars-system", "kars", false);
    f.existing.metadata[field] = "replaced";
    await expect(stage()).rejects.toThrow("409");
    expect(params(f.existing).additionalProperties).toBe(true);
  });

  it.each([false, true])("propagates prerequisite failure before policies or Helm upgrade (Helm: %s)", async helm => {
    const f = fixture(helm);
    const execute = vi.fn<Execute>(async (file, args, options) => {
      if (args[0] === "wait" && args.includes(`crd/${ACTION_CRD}`)
        || (helm&&args.includes("/openapi/v3"))) throw new Error("Established timeout");
      return f.execute(file, args, options);
    });

    await expect(f.run(false, execute)).rejects.toThrow("Established timeout");
    expect(execute.mock.calls.some(([, args, options]) => (args[0]==="upgrade"&&!args.includes("--dry-run=server"))
      || (args[0]==="create"&&JSON.parse(options.input!).kind!=="CustomResourceDefinition"))).toBe(false);
  });

  it.each([false, true])("never continues after a forbidden prerequisite PATCH (Helm: %s)", async helm => {
    const f = fixture(helm);
    const execute: Execute = (file, args, options) => (args[0] === "patch" && args[2] === ACTION_CRD)
      || (args[0] === "apply" && JSON.parse(options.input!).metadata.name === ACTION_CRD)
      ? Promise.reject(new Error("Forbidden action API update")) : f.execute(file, args, options);
    await expect(f.run(false, execute)).rejects.toThrow("Forbidden action API update");
    expect(f.execute.mock.calls.some(([, args]) => ["create", "upgrade"].includes(args[0]) && !args.includes("--dry-run=server"))).toBe(false);
  });

  it("requires API establishment even when an existing registration schema is unchanged", async () => {
    const f = fixture();
    const execute = vi.fn<Execute>(async (file, args, options) => {
      if (args[0] === "get" && args[2] === registration.metadata.name) return { stdout: JSON.stringify({
        ...registration, metadata: { ...registration.metadata, uid: "registration-api", resourceVersion: "1" },
      }) };
      if (args[0] === "wait" && args.includes(`crd/${registration.metadata.name}`)) throw new Error("Registration API not established");
      return f.execute(file, args, options);
    });
    await expect(f.run(false, execute)).rejects.toThrow("Registration API not established");
    expect(execute.mock.calls.some(([, args]) => args[0] === "create")).toBe(false);
  });

  it("performs all template ownership preflights before repairing the prerequisite", async () => {
    const f = fixture();
    const execute: Execute = (file, args, options) => args[0] === "get" && args[2] === policy.metadata.name
      ? Promise.resolve({ stdout: JSON.stringify({ ...policy, metadata: { ...policy.metadata, uid: "foreign", resourceVersion: "1" },
        spec: { failurePolicy: "Ignore" } }) }) : f.execute(file, args, options);
    await expect(f.run(false, execute)).rejects.toThrow("Unowned");
    expect(f.execute.mock.calls.some(([, args]) => args[0] === "patch")).toBe(false);
  });

  it("rejects a late template core-schema mismatch before the action conversion", async () => {
    const f = fixture();
    const desired = crd("KarsSandbox", "karssandboxes");
    const current = f.schemas.install(desired);
    current.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.external = { type: "string" };
    const execute: Execute = (file,args,settings) => file==="helm"&&args[0]==="template"
      ? Promise.resolve({stdout:[action,registration,policy,desired].map(object=>JSON.stringify(object)).join("\n---\n")})
      : f.execute(file,args,settings);
    await expect(f.run(false,execute)).rejects.toThrow("separately reviewed core schema migration");
    expect(params(f.existing).additionalProperties).toBe(true);
    expect(f.execute.mock.calls.some(([,args])=>["create","apply","patch"].includes(args[0]))).toBe(false);
  });

  it.each([false, true])("dry-run never repairs APIs or mutates the controller (Helm: %s)", async helm => {
    const f = fixture(helm);
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    try {
      await f.run(true);
      expect(params(f.existing).additionalProperties).toBe(true);
      expect(f.execute.mock.calls.some(([,args])=>["patch", "create", "wait", "rollout"].includes(args[0])&&!args.includes("--dry-run=server"))).toBe(false);
    } finally { log.mockRestore(); }
  });

  it("keeps seeded registration, historical action schema, source UID, subjects and data unchanged during real CLI-shaped dry-run", async () => {
    const f = fixture(true);
    f.existing.status = { conditions: [{ type: "Established", status: "True" }] };
    f.schemas.install(registration);
    const source = { kind: "KarsSandbox", metadata: { name: "sre", uid: "historical-source", resourceVersion: "5" },
      spec: { retained: "source" } };
    const binding = { kind: "ClusterRoleBinding", metadata: { name: "kars-sre-reader", uid: "binding", resourceVersion: "7" },
      subjects: [{ kind: "ServiceAccount", name: "sandbox", namespace: "kars-sre" },
        { kind: "ServiceAccount", name: "unrelated", namespace: "operators" }] };
    const data = { kind: "Secret", metadata: { name: "fixture-data", uid: "data", resourceVersion: "9" },
      data: { opaque: "disposable-fixture-value" } };
    for (const object of [source,binding,data]) f.schemas.objects.set(object.metadata.name,object);
    const before = structuredClone([...f.schemas.objects]);
    await f.run(true);
    expect([...f.schemas.objects]).toEqual(before);
    expect(f.execute.mock.calls.some(([,args])=>["create","apply","patch","delete","wait","rollout"].includes(args[0])&&!args.includes("--dry-run=server"))).toBe(false);
    expect(f.execute.mock.calls.find(([file,args])=>file==="helm"&&args[0]==="upgrade")?.[1])
      .toContain("--dry-run=server");
    expect(params(f.existing).additionalProperties).toBe(true);
  });

  it("identifies an unclassified Helm dry-run failure without changing its error or applying the repair", async () => {
    const f = fixture(true);
    const failure = new Error("opaque disposable transport failure");
    const report = vi.spyOn(console,"error").mockImplementation(()=>{});
    const execute:Execute = (file,args,options) => file==="helm"&&args[0]==="upgrade"
      ? Promise.reject(failure) : f.execute(file,args,options);
    try {
      await expect(f.run(true,execute)).rejects.toBe(failure);
      expect(report).toHaveBeenCalledWith("SRE-STAGE-FAILURE helm-server-dry-run");
      expect(report.mock.calls.flat().join(" ")).not.toContain(failure.message);
      expect(params(f.existing).additionalProperties).toBe(true);
      expect(f.execute.mock.calls.some(([,args])=>["create","apply","patch","delete"].includes(args[0])&&!args.includes("--dry-run=server"))).toBe(false);
    } finally { report.mockRestore(); }
  });

  it("previews the real Helm apply before any schema write, even when --dry-run was not requested", async () => {
    const f = fixture(true);
    const failure = new Error("server-side Helm preflight rejected");
    const report = vi.spyOn(console,"error").mockImplementation(()=>{});
    const execute:Execute = (file,args,settings) => file==="helm"&&args[0]==="upgrade"&&args.includes("--dry-run=server")
      ? Promise.reject(failure) : f.execute(file,args,settings);
    try {
      await expect(f.run(false,execute)).rejects.toBe(failure);
      expect(report).toHaveBeenCalledWith("SRE-STAGE-FAILURE helm-server-dry-run");
      expect(f.schemas.writes).toEqual([]);
      expect(params(f.existing).additionalProperties).toBe(true);
      expect(f.execute.mock.calls.some(([file,args])=>file==="helm"&&args[0]==="upgrade"&&!args.includes("--dry-run=server"))).toBe(false);
    } finally { report.mockRestore(); }
  });

  it.each(["v5.0.0", "invalid"])("rejects unsupported Helm %s before any prerequisite write", async version => {
    const f = fixture(true);
    const execute: Execute = (file, args, options) => args[0] === "version"
      ? Promise.resolve({ stdout: version }) : f.execute(file, args, options);
    await expect(f.run(false, execute)).rejects.toThrow("Unsupported Helm");
    expect(f.execute.mock.calls.some(([, args]) => ["patch", "create", "upgrade"].includes(args[0]))).toBe(false);
  });

  it("retains fatal rollout failures instead of accepting an unready controller", async () => {
    const f = fixture();
    const execute: Execute = (file, args, options) => args[0] === "rollout"
      ? Promise.reject(new Error("controller rollout failed")) : f.execute(file, args, options);
    await expect(f.run(false, execute)).rejects.toThrow("controller rollout failed");
  });
});
