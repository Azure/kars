// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { assertRollbackCompatibility, assertSchemaCompatibility, CRD_RETENTION } from "./schema-compatibility.js";
import { stageCoreSchemaDocuments } from "./schema-stage.js";
import { prepareCoreHelmSchemas, prepareCoreRollbackSchemas } from "./core-helm-schemas.js";
import { schemaDocuments, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import { admission, crd, schemaFixture } from "./schema-stage.test-support.js";

const schema = (object: ObjectMap, index = 0) => object.spec.versions[index].schema.openAPIV3Schema;
const manifests = (objects: ObjectMap[]) => objects.map(object => JSON.stringify(object)).join("\n---\n");

describe("one compatibility policy for schema writes and rollback", () => {
  it.each(["field", "type", "required", "enum", "pattern", "default", "validation", "preserve", "list-map", "items"])(
    "refuses retained-version %s loss/change even with a valid ownership digest", async change => {
      const live = crd();
      const wanted = structuredClone(live);
      const spec = schema(wanted).properties.spec;
      if (change === "field") delete spec.properties.workspaceUid;
      if (change === "type") spec.properties.workspaceUid.type = "integer";
      if (change === "required") spec.required = ["workspaceUid"];
      if (change === "enum") spec.properties.workspaceUid.enum = ["only-one"];
      if (change === "pattern") spec.properties.workspaceUid.pattern = "^limited$";
      if (change === "default") spec.properties.enabled.default = false;
      if (change === "validation") spec["x-kubernetes-validations"] = [{ rule: "self.enabled == true" }];
      if (change === "preserve") schema(live).properties.spec["x-kubernetes-preserve-unknown-fields"] = true;
      if (change === "list-map") spec["x-kubernetes-map-type"] = "atomic";
      if (change === "items") {
        schema(live).properties.spec.properties.entries = { type: "array", items: { type: "object", properties: { value: { type: "string" } } } };
        spec.properties.entries = { type: "array", items: { type: "object", properties: {} } };
      }
      const f = schemaFixture([wanted, admission()]);
      const before = f.install(live);
      const retained = structuredClone(before);
      await expect(stageCoreSchemaDocuments(f.execute, [wanted, admission()], { ...f.owner, ...f.wait })).rejects.toThrow("migration");
      expect(f.writes).toEqual([]);
      expect(f.objects.get(live.metadata.name)).toEqual(retained);
    });

  it("stages a reduced role minimum but refuses restoring the old minimum", async () => {
    const live = crd();
    schema(live).properties.spec.properties.roles = {
      type: "array", minItems: 1, maxItems: 8, items: { type: "string" },
    };
    const wanted = structuredClone(live);
    schema(wanted).properties.spec.properties.roles.minItems = 0;
    expect(() => assertSchemaCompatibility(live, wanted)).not.toThrow();
    expect(() => assertSchemaCompatibility(wanted, live)).toThrow("migration");
    expect(() => assertRollbackCompatibility([wanted], [live])).toThrow("migration");
    const f = schemaFixture([wanted, admission()]);
    f.install(live);
    await expect(stageCoreSchemaDocuments(f.execute, [wanted, admission()], { ...f.owner, ...f.wait }))
      .resolves.toMatchObject({ published: true });
    expect(f.writes).toHaveLength(1);
    expect(schema(f.writes[0]).properties.spec.properties.roles).toEqual(schema(wanted).properties.spec.properties.roles);
  });

  it.each(["negative", "fractional", "string", "maxItems", "default", "items", "type"])(
    "does not hide a %s change when reducing an array minimum", change => {
      const live = crd();
      schema(live).properties.spec.properties.roles = {
        type: "array", minItems: 1, maxItems: 8, items: { type: "string" },
      };
      const wanted = structuredClone(live);
      const roles = schema(wanted).properties.spec.properties.roles;
      roles.minItems = 0;
      if (change === "negative") roles.minItems = -1;
      if (change === "fractional") roles.minItems = 0.5;
      if (change === "string") roles.minItems = "0";
      if (change === "maxItems") roles.maxItems = 7;
      if (change === "default") roles.default = [];
      if (change === "items") roles.items.type = "integer";
      if (change === "type") roles.type = "string";
      expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
    });

  it("stages capability enum and array widening without allowing narrowing rollback", async () => {
    const live = crd();
    schema(live).properties.spec.properties.capabilities = {
      type: "array", default: [], maxItems: 7,
      items: { type: "string", enum: ["filesystem-read", "filesystem-write", "shell", "network", "web-search", "mcp", "memory"] },
    };
    const wanted = structuredClone(live);
    const capabilities = schema(wanted).properties.spec.properties.capabilities;
    capabilities.maxItems = 8;
    capabilities.items.enum.push("github-pr");
    expect(() => assertSchemaCompatibility(live, wanted)).not.toThrow();
    expect(() => assertRollbackCompatibility([wanted], [live])).toThrow("migration");
    const f = schemaFixture([wanted, admission()]);
    f.install(live);
    await expect(stageCoreSchemaDocuments(f.execute, [wanted, admission()], { ...f.owner, ...f.wait }))
      .resolves.toMatchObject({ published: true });
    expect(f.writes).toHaveLength(1);
    expect(schema(f.writes[0]).properties.spec.properties.capabilities).toEqual(capabilities);
  });

  it.each([0, 7])("allows array maximum %s to widen independently of its item schema", maximum => {
    const live = crd();
    schema(live).properties.spec.properties.entries = { type: "array", maxItems: maximum, items: { type: "string" } };
    const wanted = structuredClone(live);
    schema(wanted).properties.spec.properties.entries.maxItems = 8;
    expect(() => assertSchemaCompatibility(live, wanted)).not.toThrow();
    expect(() => assertRollbackCompatibility([wanted], [live])).toThrow("migration");
  });

  it.each(["integer", "number"])("does not generalize enum widening to %s schemas", type => {
    const live = crd();
    schema(live).properties.spec.properties.value = { type, enum: [1] };
    const wanted = structuredClone(live);
    schema(wanted).properties.spec.properties.value.enum.push(2);
    expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
  });

  it.each([6, -1, 7.5, "8", undefined, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1])(
    "rejects narrowing, removed or invalid array maximum %s", maximum => {
      const live = crd();
      schema(live).properties.spec.properties.capabilities = { type: "array", maxItems: 7, items: { type: "string" } };
      const wanted = structuredClone(live);
      const capabilities = schema(wanted).properties.spec.properties.capabilities;
      if (maximum === undefined) delete capabilities.maxItems;
      else capabilities.maxItems = maximum;
      expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
    });

  it.each([undefined, [], ["memory"], ["memory", "github-pr"], ["memory", "mcp", 1], ["memory", "mcp", "mcp"]].map(values => ({ values })))(
    "rejects removed, narrowed or malformed string enum $values", ({ values }) => {
      const live = crd();
      schema(live).properties.spec.properties.capability = { type: "string", enum: ["memory", "mcp"] };
      const wanted = structuredClone(live);
      const capability = schema(wanted).properties.spec.properties.capability;
      if (values === undefined) delete capability.enum;
      else capability.enum = values;
      expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
    });

  it.each(["default", "minItems", "type", "validation", "pattern", "list-type"])(
    "does not hide %s changes behind capability widening", change => {
      const live = crd();
      schema(live).properties.spec.properties.capabilities = {
        type: "array", default: [], minItems: 0, maxItems: 1, items: { type: "string", enum: ["memory"] },
      };
      const wanted = structuredClone(live);
      const capabilities = schema(wanted).properties.spec.properties.capabilities;
      capabilities.maxItems = 2;
      capabilities.items.enum.push("github-pr");
      if (change === "default") capabilities.default = ["github-pr"];
      if (change === "minItems") capabilities.minItems = 1;
      if (change === "type") capabilities.items.type = "integer";
      if (change === "validation") capabilities["x-kubernetes-validations"] = [{ rule: "size(self) > 0" }];
      if (change === "pattern") capabilities.items.pattern = "^memory$";
      if (change === "list-type") capabilities["x-kubernetes-list-type"] = "set";
      expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
    });

  it.each([128, 2_147_483_647])("stages an integer maximum of %s but refuses narrowing rollback", async maximum => {
    const live = crd();
    schema(live).properties.spec.properties.maxToolCalls = { type: "integer", minimum: 1, maximum: 32 };
    const wanted = structuredClone(live);
    schema(wanted).properties.spec.properties.maxToolCalls.maximum = maximum;
    expect(() => assertSchemaCompatibility(live, wanted)).not.toThrow();
    expect(() => assertSchemaCompatibility(wanted, live)).toThrow("migration");
    expect(() => assertRollbackCompatibility([wanted], [live])).toThrow("migration");
    const f = schemaFixture([wanted, admission()]);
    f.install(live);
    await expect(stageCoreSchemaDocuments(f.execute, [wanted, admission()], { ...f.owner, ...f.wait }))
      .resolves.toMatchObject({ published: true });
    expect(f.writes).toHaveLength(1);
    expect(schema(f.writes[0]).properties.spec.properties.maxToolCalls.maximum).toBe(maximum);
  });

  it.each([31, 32.5, "128", undefined, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1])(
    "rejects narrowing, removed or non-safe-integer maximum %s", maximum => {
      const live = crd();
      schema(live).properties.spec.properties.maxToolCalls = { type: "integer", maximum: 32 };
      const wanted = structuredClone(live);
      const limit = schema(wanted).properties.spec.properties.maxToolCalls;
      if (maximum === undefined) delete limit.maximum;
      else limit.maximum = maximum;
      expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
    });

  it.each(["type", "minimum", "format", "default", "validation", "exclusiveMaximum"])(
    "does not hide a %s change when increasing an integer maximum", change => {
      const live = crd();
      schema(live).properties.spec.properties.maxToolCalls = { type: "integer", minimum: 1, maximum: 32 };
      const wanted = structuredClone(live);
      const limit = schema(wanted).properties.spec.properties.maxToolCalls;
      limit.maximum = 128;
      if (change === "type") limit.type = "number";
      if (change === "minimum") limit.minimum = 2;
      if (change === "format") limit.format = "int32";
      if (change === "default") limit.default = 128;
      if (change === "validation") limit["x-kubernetes-validations"] = [{ rule: "self < 100" }];
      if (change === "exclusiveMaximum") limit.exclusiveMaximum = true;
      expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
    });

  it("does not generalize integer widening to floating-point schemas", () => {
    const live = crd();
    schema(live).properties.spec.properties.limit = { type: "number", maximum: 32 };
    const wanted = structuredClone(live);
    schema(wanted).properties.spec.properties.limit.maximum = 128;
    expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
  });

  it("checks each retained version, not only the current storage version", () => {
    const live = crd();
    live.spec.versions.push({ ...structuredClone(live.spec.versions[0]), name: "v1beta1", storage: false });
    const wanted = structuredClone(live);
    delete schema(wanted, 1).properties.spec.properties.workspaceUid;
    expect(() => assertSchemaCompatibility(live, wanted)).toThrow("v1beta1");
  });

  it("allows optional non-defaulted additions but not new typing of preserved arbitrary values", () => {
    const live = crd();
    const wanted = structuredClone(live);
    schema(wanted).properties.spec.properties.newField = { type: "string" };
    expect(() => assertSchemaCompatibility(live, wanted)).not.toThrow();
    schema(live).properties.spec["x-kubernetes-preserve-unknown-fields"] = true;
    schema(wanted).properties.spec["x-kubernetes-preserve-unknown-fields"] = true;
    expect(() => assertSchemaCompatibility(live, wanted)).toThrow("migration");
  });

  it.each(["field", "type", "validation"])("refuses explicit rollback with same-version %s incompatibility before writes", async change => {
    const live = crd();
    const prior = structuredClone(live);
    if (change === "field") delete schema(prior).properties.spec.properties.workspaceUid;
    if (change === "type") schema(prior).properties.spec.properties.workspaceUid.type = "integer";
    if (change === "validation") schema(prior).properties.spec["x-kubernetes-validations"] = [{ rule: "self.enabled" }];
    const f = schemaFixture();
    f.install(live);
    const execute: SchemaExecute = (file, args, options) => {
      if (file === "helm" && args[0] === "history") return Promise.resolve({ stdout: '[{"revision":1,"status":"superseded"},{"revision":2,"status":"deployed"}]' });
      if (file === "helm" && args[0] === "get" && args[1] === "manifest") {
        return Promise.resolve({ stdout: manifests([args.at(-1) === "1" ? prior : live, admission()]) });
      }
      return f.execute(file, args, options);
    };
    await expect(prepareCoreRollbackSchemas(execute, "kars", "kars-system")).rejects.toThrow("migration");
    expect(f.writes).toEqual([]);
  });
});

describe("automatic Helm failure safety", () => {
  it("puts keep retention on every CRD in the actual chart, without changing its schemas", () => {
    const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
    const objects = schemaDocuments(execFileSync("helm", ["template", "kars", chart, "-n", "kars-system", "--dry-run=client"], { encoding: "utf8" }));
    const crds = objects.filter(object => object.kind === "CustomResourceDefinition");
    expect(crds).toHaveLength(21);
    expect(crds.every(object => object.metadata.annotations?.[CRD_RETENTION] === "keep")).toBe(true);
  });

  it("keeps receipt storage compatible with the preserved ledger and only adds an optional ceiling observation", () => {
    const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
    const objects = schemaDocuments(execFileSync("helm", ["template", "kars", chart, "-n", "kars-system", "--dry-run=client"], { encoding: "utf8" }));
    const account = objects.find(object => object.metadata.name === "karsbudgetaccounts.kars.azure.com")!;
    const status = schema(account).properties.status;
    expect(status.properties.ledger["x-kubernetes-preserve-unknown-fields"]).toBe(true);
    expect(status.properties.ledger.properties).not.toHaveProperty("tokenTopUps");
    expect(status.properties.effectiveLimits).toMatchObject({ type: "object", nullable: true });
    expect(status.required ?? []).not.toContain("effectiveLimits");
    const previous = structuredClone(account);
    delete schema(previous).properties.status.properties.effectiveLimits;
    expect(() => assertSchemaCompatibility(previous, account)).not.toThrow();
  });

  it.each(["--atomic", "--rollback-on-failure"])("refuses schema-changing %s before any schema write", async flag => {
    const previous = crd();
    const wanted = structuredClone(previous);
    schema(wanted).properties.spec.properties.added = { type: "string" };
    const f = schemaFixture([wanted, admission()]);
    f.install(previous);
    const execute: SchemaExecute = (file, args, options) => file === "helm" && args[0] === "get" && args[1] === "manifest"
      ? Promise.resolve({ stdout: manifests([previous, admission()]) }) : f.execute(file, args, options);
    await expect(prepareCoreHelmSchemas(execute, ["upgrade", "kars", "chart", "-n", "kars-system", flag])).rejects.toThrow("migration");
    expect(f.writes).toEqual([]);
  });

  it("protects richer live schemas even if the proposed and rollback manifests agree", async () => {
    const f = schemaFixture();
    const live = crd();
    schema(live).properties.spec.properties.newLiveData = { type: "string" };
    f.install(live);
    await expect(prepareCoreHelmSchemas(f.execute, ["upgrade", "kars", "chart", "-n", "kars-system", "--atomic"])).rejects.toThrow("migration");
    expect(f.writes).toEqual([]);
  });

  it.each([false, true])("allows retained unchanged-schema atomic operation with existing release=%s", async existing => {
    const f = schemaFixture();
    f.options.releaseExists = existing;
    if (existing) f.install(crd());
    const args = ["upgrade", "--install", "kars", "chart", "-n", "kars-system", "--atomic"];
    await prepareCoreHelmSchemas(f.execute, args);
    await f.execute("helm", args, { stdio: "pipe" });
    expect(f.requests.at(-1)!.args).toContain("--atomic");
    expect(f.writes.every(object => object.metadata.annotations[CRD_RETENTION] === "keep")).toBe(true);
  });

  it("allows a new retained CRD that will remain after automatic rollback", async () => {
    const added = crd("KarsSandbox", "karssandboxes");
    const f = schemaFixture([crd(), added, admission()]);
    f.install(crd());
    const execute: SchemaExecute = (file, args, options) => file === "helm" && args[0] === "get" && args[1] === "manifest"
      ? Promise.resolve({ stdout: manifests([crd(), admission()]) }) : f.execute(file, args, options);
    await prepareCoreHelmSchemas(execute, ["upgrade", "kars", "chart", "-n", "kars-system", "--atomic"]);
    expect(f.writes.map(object => object.metadata.name)).toEqual([added.metadata.name]);
    expect(f.writes[0].metadata.annotations[CRD_RETENTION]).toBe("keep");
  });

  it("refuses an older rollback target without retention instead of silently dropping atomic", async () => {
    const previous = crd();
    delete previous.metadata.annotations[CRD_RETENTION];
    const f = schemaFixture();
    const execute: SchemaExecute = (file, args, options) => file === "helm" && args[0] === "get" && args[1] === "manifest"
      ? Promise.resolve({ stdout: manifests([previous, admission()]) }) : f.execute(file, args, options);
    await expect(prepareCoreHelmSchemas(execute, ["upgrade", "kars", "chart", "-n", "kars-system", "--atomic"])).rejects.toThrow("retention migration");
    expect(f.writes).toEqual([]);
  });

  it("uses the latest successful Helm revision, not a failed immediately previous revision", async () => {
    const f = schemaFixture();
    f.history.splice(0, 1, { revision: 1, status: "superseded" }, { revision: 2, status: "failed" });
    const wanted = crd();
    const previous = crd();
    delete schema(previous).properties.spec.properties.workspaceUid;
    const execute: SchemaExecute = (file, args, options) => file === "helm" && args[0] === "get" && args[1] === "manifest"
      ? Promise.resolve({ stdout: manifests([args.at(-1) === "1" ? previous : wanted, admission()]) }) : f.execute(file, args, options);
    await expect(prepareCoreHelmSchemas(execute, ["upgrade", "kars", "chart", "-n", "kars-system", "--atomic"])).rejects.toThrow("migration");
    expect(f.writes).toEqual([]);
  });

  it("stops changed release history before the first schema write", async () => {
    const f = schemaFixture();
    let histories = 0;
    const execute: SchemaExecute = (file, args, options) => {
      if (file === "helm" && args[0] === "history" && ++histories === 2) f.history.push({ revision: 2, status: "deployed" });
      return f.execute(file, args, options);
    };
    await expect(prepareCoreHelmSchemas(execute, ["upgrade", "kars", "chart", "-n", "kars-system", "--atomic"])).rejects.toThrow("history changed");
    expect(f.writes).toEqual([]);
  });
});
