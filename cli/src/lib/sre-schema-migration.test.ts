// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { canonicalSchema, normalizedCrd, schemaDigest } from "./schema-documents.js";
import { assertSchemaCompatibility } from "./schema-compatibility.js";
import { planCoreSchemaDocuments, stageCoreSchemaDocuments } from "./schema-stage.js";
import { qualifySreSchemaMigration, sreMigrationSummary } from "./sre-schema-migration.js";
import { canonicalMigrationSchemas, migrationFixture } from "./sre-migration.test-support.js";
import { CANONICAL_SCHEMAS, EVALUATOR_V2, MIGRATION } from "./sre-migration-catalog.js";
import { planCoreHelmSchemas } from "./core-helm-schemas.js";

describe("closed BASE365 SRE schema migration", () => {
  it("accepts a new-CRD server CREATE preview without inventing a persisted resourceVersion", async () => {
    const f = migrationFixture(true);
    const execute: typeof f.execute = async (file, args, options) => {
      const result = await f.execute(file, args, options);
      if (file === "kubectl" && args[0] === "create" && args.includes("--dry-run=server")) {
        const preview = JSON.parse(result.stdout);
        delete preview.metadata.resourceVersion;
        return { stdout: JSON.stringify(preview) };
      }
      return result;
    };
    const permit = await qualifySreSchemaMigration(execute, f.after, f.owner);
    const apply = await planCoreSchemaDocuments(execute, f.after, { ...f.owner, ...f.wait, reviewedSreMigration: permit });
    expect(f.writes).toEqual([]);
    await apply();
    for (const object of f.objects.values()) {
      expect(object.metadata.resourceVersion).toBeTruthy();
      expect(object.metadata.uid).not.toBe("dry-run-uid");
    }
  });

  it("qualifies only the exact optional Sandbox condition generation addition", async () => {
    const f = migrationFixture();
    const target = f.after.find(object => object.spec.names.kind === "KarsSandbox")!;
    const previous = structuredClone(target);
    const condition = previous.spec.versions[0].schema.openAPIV3Schema.properties.status.properties.conditions.items;
    expect(condition.properties.observedGeneration).toEqual({ type: "integer", format: "int64" });
    expect(condition.required ?? []).not.toContain("observedGeneration");
    delete condition.properties.observedGeneration;
    expect(schemaDigest(normalizedCrd(previous))).toBe("da674a84c19c8ac64a1d96d04f79435c6899601426e25931feaca483f139b920");
    expect(schemaDigest(normalizedCrd(target))).toBe("5d495b8cfe5e4526741a673161cbae0492812e2650c2f2d08c5e522a3bda946f");
    expect(CANONICAL_SCHEMAS[target.metadata.name].after).toContain(schemaDigest(normalizedCrd(previous)));
    expect(() => assertSchemaCompatibility(previous, target)).not.toThrow();
    expect(await qualifySreSchemaMigration(f.execute, f.after, f.owner)).toBeDefined();
    condition.properties.observedGeneration = { type: "string" };
    expect(() => assertSchemaCompatibility(target, previous)).toThrow();
    f.after[f.after.indexOf(target)] = previous;
    await expect(qualifySreSchemaMigration(f.execute, f.after, f.owner)).rejects.toThrow();
    expect(f.writes).toEqual([]);
  });

  it("upgrades a previously qualified Sandbox target on the strict ordinary path", async () => {
    const f = migrationFixture();
    for (const target of f.after) f.install(target);
    const sandbox = f.objects.get("karssandboxes.kars.azure.com")!;
    delete sandbox.spec.versions[0].schema.openAPIV3Schema.properties.status.properties.conditions.items.properties.observedGeneration;
    f.objects.get("kars-controller")!.spec.replicas = 1;
    const manifest = f.after.map(object => object.metadata.name === sandbox.metadata.name ? sandbox : object)
      .map(object => JSON.stringify(object)).join("\n---\n");
    const execute: typeof f.execute = async (file, args, options) => {
      if (file === "helm" && args[0] === "get" && args[1] === "manifest") return { stdout: manifest };
      return f.execute(file, args, options);
    };
    await stageCoreSchemaDocuments(execute, f.after, { ...f.owner, ...f.wait });
    expect(f.writes).toHaveLength(1);
    expect(f.writes[0].metadata.name).toBe(sandbox.metadata.name);
  });

  it.each([false, true])("pins complete before/after schemas including evaluator-v2=%s", evalV2 => {
    const { before, after } = canonicalMigrationSchemas(evalV2);
    expect(before).toHaveLength(18);
    expect(after).toHaveLength(21);
    for (const object of before) expect(schemaDigest(normalizedCrd(object))).toBe(CANONICAL_SCHEMAS[object.metadata.name].before);
    for (const object of after) expect(CANONICAL_SCHEMAS[object.metadata.name].after).toContain(schemaDigest(normalizedCrd(object)));
    const evalCrd = after.find(object => object.spec.names.kind === "KarsEval")!;
    const legacy = CANONICAL_SCHEMAS[evalCrd.metadata.name].after.filter(value => value !== EVALUATOR_V2);
    expect(legacy).toHaveLength(1);
    expect(schemaDigest(normalizedCrd(evalCrd))).toBe(evalV2 ? EVALUATOR_V2 : legacy[0]);
  });

  it.each([false, true])("preserves object data/UID/RV while applying only the qualified target (v2=%s)", async evalV2 => {
    const f = migrationFixture(evalV2);
    f.addData("KarsTask", { envelope: { budget: { tokens: 30 } }, blueprint: {}, execution: { launch: false } });
    f.addData("KarsTeam", { envelope: {}, blueprint: {}, roster: [{ envelope: {}, blueprint: {} }] });
    f.addData("McpServer", { url: "https://existing.example.test", productionMode: false });
    f.addData("KarsSandbox", { credentialsRef: { name: "kars-credential-source-sre", uid: "input-uid" } });
    f.addData("KarsSREAction", { action: { params: { anything: { nested: [1, "two", true] } } } });
    f.addData("KarsEval", {}, { phase: "Ready", history: [] });
    const original = f.snapshot();
    const identities = new Map([...f.objects].map(([name, object]) => [name, object.metadata.uid]));
    const permit = await qualifySreSchemaMigration(f.execute, f.after, f.owner);
    expect(permit).toBeDefined();
    const apply = await planCoreSchemaDocuments(f.execute, f.after, { ...f.owner, ...f.wait, reviewedSreMigration: permit });
    expect(f.writes).toEqual([]);
    expect(f.snapshot()).toBe(original);
    expect(f.validations.length).toBeGreaterThan(3);
    const result = await apply();
    expect(result).toEqual({ schemas: 21, published: true });
    expect(f.snapshot()).toBe(original);
    for (const [name, uid] of identities) expect(f.objects.get(name)!.metadata.uid).toBe(uid);
    for (const target of f.after) expect(normalizedCrd(f.objects.get(target.metadata.name)!)).toEqual(normalizedCrd(target));
    expect(sreMigrationSummary(permit!)).toMatchObject({ profile: evalV2 ? "evaluator-v2" : "core-470" });
    expect(f.requests.filter(request => request.args[0] === "replace").every(request =>
      request.args.some(arg => arg.endsWith("?dryRun=All")))).toBe(true);
  });

  it("leaves the generic comparator strict and rejects a forged migration permit", async () => {
    const f = migrationFixture();
    const task = f.before.find(object => object.spec.names.kind === "KarsTask")!;
    const desired = f.after.find(object => object.spec.names.kind === "KarsTask")!;
    expect(() => assertSchemaCompatibility(task, desired)).toThrow("migration");
    await expect(stageCoreSchemaDocuments(f.execute, f.after, {
      ...f.owner, ...f.wait, reviewedSreMigration: { id: MIGRATION },
    })).rejects.toThrow();
    expect(f.writes).toEqual([]);
  });

  it("does not extend a qualified exception when a caller changes a target after qualification", async () => {
    const f = migrationFixture();
    const permit = await qualifySreSchemaMigration(f.execute, f.after, f.owner);
    f.after.find(object => object.spec.names.kind === "KarsTask")!.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.unreviewed = { type: "string" };
    await expect(planCoreSchemaDocuments(f.execute, f.after, { ...f.owner, ...f.wait, reviewedSreMigration: permit }))
      .rejects.toThrow("qualified SRE migration snapshot");
    expect(f.writes).toEqual([]);
  });

  it.each(["before", "after", "missing", "foreign", "stored-version"])("preflights late %s conflict before action/schema writes", async fault => {
    const f = migrationFixture(true);
    const name = "karstasks.kars.azure.com";
    if (fault === "before") f.objects.get(name)!.spec.versions[0].schema.openAPIV3Schema.description = "external schema";
    if (fault === "after") f.after.find(object => object.metadata.name === name)!.spec.versions[0].schema.openAPIV3Schema.description = "unreviewed target";
    if (fault === "missing") f.objects.delete(name);
    if (fault === "foreign") f.objects.get(name)!.metadata.annotations["meta.helm.sh/release-name"] = "other";
    if (fault === "stored-version") f.objects.get(name)!.status.storedVersions.push("v2");
    await expect(qualifySreSchemaMigration(f.execute, f.after, f.owner)).rejects.toThrow();
    expect(f.writes).toEqual([]);
    expect(f.objects.get("karssreactions.kars.azure.com")!.spec.versions[0].schema.openAPIV3Schema
      .properties.spec.properties.action.properties.params.additionalProperties).toBe(true);
  });

  it.each([
    ["KarsTask", { envelope: { budget: { scope: "GovernedInference" } } }, undefined],
    ["KarsTask", { blueprint: { credentialBindings: {} } }, undefined],
    ["KarsTeam", { roster: [{ blueprint: { githubBinding: {} } }] }, undefined],
    ["KarsProfile", { defaultEnvelope: { budget: { scope: null } } }, undefined],
    ["McpServer", { managed: { preset: "everything" } }, undefined],
    ["KarsSandbox", { credentialsRef: { name: "kars-credential-bundle-sre", uid: "forged" } }, undefined],
    ["KarsSandbox", { inferenceBudgetRef: {} }, undefined],
    ["KarsEval", {}, { reportConfigMapUid: "unverified" }],
  ])("does not grandfather post-baseline authority/evidence in %s", async (kind, spec, status) => {
    const f = migrationFixture(true);
    f.addData(kind as string, spec as Record<string, unknown>, status as Record<string, unknown> | undefined);
    await expect(qualifySreSchemaMigration(f.execute, f.after, f.owner)).rejects.toThrow();
    expect(f.writes).toEqual([]);
  });

  it("rejects server-invalid or mutating data validation before any schema write", async () => {
    const f = migrationFixture();
    f.addData("KarsTask", { blueprint: {} });
    const execute: typeof f.execute = async (file, args, options) => {
      const result = await f.execute(file, args, options);
      if (args[0] === "replace") {
        const mutated = JSON.parse(result.stdout);
        mutated.spec.injected = true;
        return { stdout: JSON.stringify(mutated) };
      }
      return result;
    };
    await expect(qualifySreSchemaMigration(execute, f.after, f.owner)).rejects.toThrow("round-trip");
    expect(f.writes).toEqual([]);
  });

  it("rejects a schema dry-run failure before even the approved action migration", async () => {
    const f = migrationFixture();
    const permit = await qualifySreSchemaMigration(f.execute, f.after, f.owner);
    f.onDryRun(object => {
      if (object.metadata.name === "karstasks.kars.azure.com") throw new Error("Forbidden schema dry-run");
    });
    await expect(planCoreSchemaDocuments(f.execute, f.after, { ...f.owner, ...f.wait, reviewedSreMigration: permit })).rejects.toThrow("Forbidden");
    expect(f.writes).toEqual([]);
  });

  it.each(["uid", "resourceVersion", "data", "create"])("fences %s drift after complete preflight and before the first write", async fault => {
    const f = migrationFixture();
    const object = f.addData("McpServer", { url: "https://existing.example.test" });
    const permit = await qualifySreSchemaMigration(f.execute, f.after, f.owner);
    const apply = await planCoreSchemaDocuments(f.execute, f.after, { ...f.owner, ...f.wait, reviewedSreMigration: permit });
    if (fault === "uid") object.metadata.uid = "replacement";
    if (fault === "resourceVersion") object.metadata.resourceVersion = "2";
    if (fault === "data") object.spec.url = "https://external-change.example.test";
    if (fault === "create") f.addData("KarsSandbox", {});
    await expect(apply()).rejects.toThrow("data/UID/resourceVersion");
    expect(f.writes).toEqual([]);
  });

  it("keeps exact CRD UID/RV CAS across the first migration write", async () => {
    const f = migrationFixture();
    for (const target of f.after.filter(object => !CANONICAL_SCHEMAS[object.metadata.name].before)) f.install(target);
    const permit = await qualifySreSchemaMigration(f.execute, f.after, f.owner);
    const apply = await planCoreSchemaDocuments(f.execute, f.after, { ...f.owner, ...f.wait, reviewedSreMigration: permit });
    f.beforeWrite(object => {
      const current = f.objects.get(object.metadata.name);
      if (current) current.metadata.resourceVersion = "concurrent";
    });
    await expect(apply()).rejects.toThrow("409");
    expect(f.writes).toEqual([]);
  });

  it("does not silently drop automatic rollback to run this explicit migration", async () => {
    const f = migrationFixture();
    await expect(planCoreHelmSchemas(f.execute, ["upgrade", "kars", "chart", "-n", "kars-system", "--atomic"],
      { base365SreMigration: true })).rejects.toThrow("explicitly non-atomic");
    expect(f.requests).toEqual([]);
  });

  it.each(["running", "uid", "version"])("requires controller quiescence and its exact %s identity throughout preflight", async fault => {
    const f = migrationFixture();
    const permit = await qualifySreSchemaMigration(f.execute, f.after, f.owner);
    const apply = await planCoreSchemaDocuments(f.execute, f.after, { ...f.owner, ...f.wait, reviewedSreMigration: permit });
    const controller = f.objects.get("kars-controller")!;
    if (fault === "running") controller.spec.replicas = 1;
    if (fault === "uid") controller.metadata.uid = "other-controller";
    if (fault === "version") controller.metadata.resourceVersion = "3";
    await expect(apply()).rejects.toThrow(/paused|Controller UID/);
    expect(f.writes).toEqual([]);
  });

  it("does not grandfather a post-baseline grant even when its seeded CRD is canonical", async () => {
    const f = migrationFixture();
    const grantCrd = f.after.find(object => object.spec.names.kind === "KarsCredentialGrant")!;
    f.install(grantCrd);
    f.data.set(grantCrd.spec.names.plural, [{ apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
      metadata: { name: "workspace", namespace: "kars-system", uid: "unreviewed-grant", resourceVersion: "1" }, spec: {} }]);
    await expect(qualifySreSchemaMigration(f.execute, f.after, f.owner)).rejects.toThrow("post-baseline authority");
    expect(f.writes).toEqual([]);
  });

  it("supports already-completed schemas without remigrating objects or minting authority", async () => {
    const f = migrationFixture(true);
    for (const target of f.after) f.install(target);
    f.addData("KarsEval", {}, { reportConfigMapUid: "current-controller-evidence" });
    const before = canonicalSchema([...f.objects]);
    await expect(qualifySreSchemaMigration(f.execute, f.after, f.owner)).resolves.toBeUndefined();
    expect(canonicalSchema([...f.objects])).toBe(before);
    expect(f.writes).toEqual([]);
  });

  it("leaves an isolated compatible evaluator-v2 addition on the strict ordinary path", async () => {
    const f = migrationFixture(true);
    for (const target of f.after) f.install(target);
    f.install(f.before.find(object => object.spec.names.kind === "KarsEval")!);
    f.objects.get("kars-controller")!.spec.replicas = 1;
    await expect(qualifySreSchemaMigration(f.execute, f.after, f.owner)).resolves.toBeUndefined();
    await stageCoreSchemaDocuments(f.execute, f.after, { ...f.owner, ...f.wait });
    expect(f.writes).toHaveLength(1);
    expect(f.writes[0].metadata.name).toBe("karsevals.kars.azure.com");
  });
});
