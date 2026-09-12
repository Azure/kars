// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  canonicalSchema, normalizedCrd, readSchemaObject, SCHEMA_DIGEST, schemaDigest, schemaDocuments,
  schemaIdentity, verifySchemaOwner, type ObjectMap, type SchemaExecute, type SchemaOwner,
} from "./schema-documents.js";
import { waitForPublishedSchemas, type PublishedType, type SchemaWait } from "./schema-discovery.js";
import { assertRollbackCompatibility, assertSchemaCompatibility, requireCrdRetention } from "./schema-compatibility.js";
import {
  authorizesSreSchemaMigration, completeSreSchemaMigration, recheckSreSchemaMigration, recordSreSchemaWrite, type QualifiedSreMigration,
} from "./sre-schema-migration.js";
import { schemaStep } from "./sre-schema-diagnostics.js";
import { buildSchemaWriteRequest, verifySchemaWritePreview } from "./schema-write-request.js";

interface PlannedSchema { desired: ObjectMap; current?: ObjectMap; uid?: string; change: boolean }
export interface SchemaStageOptions extends SchemaOwner, SchemaWait {
  checkOnly?: boolean;
  rollbackDocuments?: ObjectMap[];
  beforeWrite?: () => Promise<void>;
  reviewedSreMigration?: QualifiedSreMigration;
}

function validateOwner(owner: SchemaOwner): void {
  if (!/^[a-z0-9](?:[-a-z0-9.]*[a-z0-9])?$/.test(owner.release) || owner.release.length > 53
    || !/^[a-z0-9](?:[-a-z0-9]*[a-z0-9])?$/.test(owner.namespace) || owner.namespace.length > 63
    || !["helm", "template"].includes(owner.ownership)) throw new Error("An exact schema release, namespace and ownership mode are required");
}

function servedTypes(crds: ObjectMap[]): PublishedType[] {
  return crds.flatMap(crd => crd.spec.versions.filter((version: ObjectMap) => version.served).map((version: ObjectMap) => ({
    group: crd.spec.group, version: version.name, kind: crd.spec.names.kind, plural: crd.spec.names.plural,
    namespaced: crd.spec.scope === "Namespaced", schema: version.schema.openAPIV3Schema,
  })));
}

async function policySafety(execute: SchemaExecute, documents: ObjectMap[]): Promise<boolean> {
  let observed = true;
  for (const desired of documents.filter(object => object.kind === "ValidatingAdmissionPolicy")) {
    const current = await readSchemaObject(execute, "validatingadmissionpolicy", desired.metadata.name);
    if (!current || canonicalSchema(current.spec) !== canonicalSchema(desired.spec)) continue;
    if (typeof current.metadata.generation !== "number") throw new Error("Existing policy generation is unavailable");
    if (current.status?.observedGeneration >= current.metadata.generation) {
      const warnings = current.status?.typeChecking?.expressionWarnings ?? [];
      if (current.status.observedGeneration !== current.metadata.generation || !current.status.typeChecking
        || !Array.isArray(warnings) || warnings.length) {
        throw new Error(`Policy ${desired.metadata.name} already has observed type-check warnings or invalid status for this exact spec; schema staging cannot repair it without a real policy upgrade or upstream/operator recovery`);
      }
    } else {
      observed = false;
    }
  }
  return observed;
}

async function existingPoliciesObserved(execute: SchemaExecute, documents: ObjectMap[], options: SchemaWait): Promise<void> {
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? (ms => new Promise(resolve => setTimeout(resolve, ms)));
  const deadline = now() + (options.timeoutMs ?? 120_000);
  while (!await policySafety(execute, documents)) {
    if (now() >= deadline) throw new Error("Existing unchanged admission policy is not observed; no policy generation/status was altered");
    await sleep(Math.min(500, Math.max(1, deadline - now())));
  }
}

/** For the existing narrowly-owned SRE staging path, after its CRD writes.
 * This does not create, adopt or modify any schema. */
export async function waitForInstalledCoreSchemas(
  execute: SchemaExecute, documents: ObjectMap[], options: SchemaWait = {},
): Promise<void> {
  const crds = documents.filter(object => object.kind === "CustomResourceDefinition");
  if (!crds.length || crds.length > 64) throw new Error("Missing or unbounded installed core schema inventory");
  const identities = new Map<string, string>();
  for (const desired of crds) {
    const current = await readSchemaObject(execute, "customresourcedefinition", desired.metadata.name);
    if (!current || canonicalSchema(normalizedCrd(current)) !== canonicalSchema(normalizedCrd(desired))) {
      throw new Error(`Installed CRD ${desired.metadata.name} differs from the chart; prepare core schemas before SRE authority`);
    }
    identities.set(desired.metadata.name, schemaIdentity(current).uid);
  }
  await waitForPublishedSchemas(execute, servedTypes(crds), async () => {
    let established = true;
    for (const desired of crds) {
      const current = await readSchemaObject(execute, "customresourcedefinition", desired.metadata.name);
      if (!current || schemaIdentity(current).uid !== identities.get(desired.metadata.name)
        || canonicalSchema(normalizedCrd(current)) !== canonicalSchema(normalizedCrd(desired))) throw new Error("Installed core schema changed during publication");
      established &&= current.status?.conditions?.some((condition: ObjectMap) => condition.type === "Established" && condition.status === "True") === true;
    }
    return established;
  }, options);
  await existingPoliciesObserved(execute, documents, options);
}

export async function planCoreSchemaDocuments(
  execute: SchemaExecute, documents: ObjectMap[], options: SchemaStageOptions,
): Promise<() => Promise<{ schemas: number; published: true }>> {
  schemaStep("schema-plan");
  validateOwner(options);
  documents = structuredClone(documents);
  if (options.reviewedSreMigration && options.rollbackDocuments) throw new Error("Reviewed SRE schema migration cannot use automatic rollback");
  const owner: SchemaOwner = { release: options.release, namespace: options.namespace, ownership: options.ownership };
  const crds = documents.filter(object => object.kind === "CustomResourceDefinition");
  if (!crds.length || crds.length > 64 || new Set(crds.map(object => object.metadata.name)).size !== crds.length) {
    throw new Error("Core chart must contain between 1 and 64 uniquely identified CRDs");
  }
  for (const crd of crds) {
    normalizedCrd(crd);
    if (crd.metadata.namespace || crd.metadata.uid || crd.metadata.resourceVersion || crd.metadata.ownerReferences?.length) {
      throw new Error("Chart CRDs must not carry live/foreign object identities");
    }
  }
  requireCrdRetention(crds);
  if (options.rollbackDocuments) assertRollbackCompatibility(crds, options.rollbackDocuments);
  const types = servedTypes(crds);
  for (const policy of documents.filter(object => object.kind === "ValidatingAdmissionPolicy" && object.spec?.paramKind)) {
    const param = policy.spec.paramKind;
    if (param.apiVersion !== "v1" && !types.some(type => `${type.group}/${type.version}` === param.apiVersion && type.kind === param.kind)) {
      throw new Error(`Policy ${policy.metadata.name} parameter schema is absent from the exact chart`);
    }
  }
  schemaStep("policy-review");
  await policySafety(execute, documents);
  const plans: PlannedSchema[] = [];
  let priorManifest: ObjectMap[] | undefined;
  for (const desired of crds) {
    schemaStep("schema-plan-identity", desired.spec.names.kind);
    const current = await readSchemaObject(execute, "customresourcedefinition", desired.metadata.name);
    schemaStep("schema-plan-identity", desired.spec.names.kind, current);
    if (options.reviewedSreMigration && !options.checkOnly
      && !authorizesSreSchemaMigration(options.reviewedSreMigration, current, desired)) {
      throw new Error("Schema plan differs from its qualified SRE migration snapshot");
    }
    if (!current) {
      if (options.checkOnly) throw new Error(`Schema ${desired.metadata.name} has not been staged`);
      plans.push({ desired, change: true });
      continue;
    }
    verifySchemaOwner(current, owner);
    const wanted = normalizedCrd(desired);
    const actual = normalizedCrd(current);
    const change = canonicalSchema(actual) !== canonicalSchema(wanted);
    if (change) {
      if (options.checkOnly) throw new Error(`Schema ${desired.metadata.name} differs from the chart`);
      const recorded = current.metadata.annotations?.[SCHEMA_DIGEST] === schemaDigest(actual);
      if (!recorded) {
        schemaStep("helm-schema-match", desired.spec.names.kind);
        if (owner.ownership !== "helm") throw new Error(`Customized or unrecorded schema ${desired.metadata.name}; no overwrite is permitted`);
        priorManifest ??= schemaDocuments((await execute("helm", ["get", "manifest", owner.release, "-n", owner.namespace],
          { stdio: "pipe" })).stdout);
        const previous = priorManifest.find(object => object.kind === "CustomResourceDefinition" && object.metadata.name === desired.metadata.name);
        if (!previous || canonicalSchema(normalizedCrd(previous)) !== canonicalSchema(actual)) {
          throw new Error(`Live schema ${desired.metadata.name} conflicts with its Helm release; no overwrite is permitted`);
        }
      }
      if (actual.scope !== wanted.scope || canonicalSchema(actual.names) !== canonicalSchema(wanted.names)
        || (current.status?.storedVersions ?? []).some((version: string) => !wanted.versions.some((item: ObjectMap) => item.name === version))) {
        throw new Error(`Schema ${desired.metadata.name} requires an explicit identity/storage migration`);
      }
      if (!options.reviewedSreMigration) assertSchemaCompatibility(current, desired);
    }
    if (options.rollbackDocuments) assertRollbackCompatibility([current], options.rollbackDocuments);
    plans.push({ desired, current, uid: schemaIdentity(current).uid, change });
  }
  const writeRequest = (plan: PlannedSchema) => buildSchemaWriteRequest(plan.desired, plan.current, owner);
  if (options.reviewedSreMigration && !options.checkOnly) {
    await recheckSreSchemaMigration(options.reviewedSreMigration);
    for (const plan of plans.filter(plan => plan.change)) {
      const { object, args } = writeRequest(plan);
      schemaStep("schema-server-preview", plan.desired.spec.names.kind);
      const checked: ObjectMap = JSON.parse((await execute("kubectl", [...args, "--dry-run=server", "--request-timeout=20s"],
        { stdio: "pipe", input: JSON.stringify(object), timeout: 25_000 })).stdout);
      schemaStep("schema-preview-identity", plan.desired.spec.names.kind, checked);
      verifySchemaWritePreview(checked, plan.desired, owner, plan.uid);
    }
    await recheckSreSchemaMigration(options.reviewedSreMigration);
  }
  // Every plan and server dry-run completes before any real schema/action write.
  return async () => {
    schemaStep("schema-plan");
    await options.beforeWrite?.();
    if (options.reviewedSreMigration) await recheckSreSchemaMigration(options.reviewedSreMigration);
    for (const plan of plans.filter(plan => plan.change)) {
      if (options.reviewedSreMigration) await recheckSreSchemaMigration(options.reviewedSreMigration, plan.desired.metadata.name);
      const { object, args } = writeRequest(plan);
      schemaStep("schema-write", plan.desired.spec.names.kind);
      const applied: ObjectMap = JSON.parse((await execute("kubectl", [...args, "--request-timeout=20s"],
        { stdio: "pipe", input: JSON.stringify(object), timeout: 25_000 })).stdout);
      schemaStep("schema-plan-identity", plan.desired.spec.names.kind, applied);
      const identity = schemaIdentity(applied);
      if ((plan.uid && plan.uid !== identity.uid) || applied.metadata.name !== plan.desired.metadata.name
        || canonicalSchema(normalizedCrd(applied)) !== canonicalSchema(normalizedCrd(plan.desired))) throw new Error("Schema write returned an unreviewed identity/spec");
      verifySchemaOwner(applied, owner);
      plan.uid = identity.uid;
      if (options.reviewedSreMigration) {
        recordSreSchemaWrite(options.reviewedSreMigration, applied);
        await recheckSreSchemaMigration(options.reviewedSreMigration, plan.desired.metadata.name);
      }
    }
    schemaStep("schema-publication");
    await waitForPublishedSchemas(execute, types, async () => {
      let established = true;
      for (const plan of plans) {
        const current = await readSchemaObject(execute, "customresourcedefinition", plan.desired.metadata.name);
        if (!current || schemaIdentity(current).uid !== plan.uid
          || canonicalSchema(normalizedCrd(current)) !== canonicalSchema(normalizedCrd(plan.desired))) throw new Error("CRD identity or schema changed before admission installation");
        verifySchemaOwner(current, owner);
        const conditions = current.status?.conditions ?? [];
        if (conditions.some((condition: ObjectMap) => (condition.type === "NamesAccepted" && condition.status === "False")
          || (condition.type === "NonStructuralSchema" && condition.status === "True"))) throw new Error("CRD names or structural schema are rejected");
        established &&= conditions.some((condition: ObjectMap) => condition.type === "Established" && condition.status === "True");
      }
      return established;
    }, options);
    schemaStep("policy-review");
    await existingPoliciesObserved(execute, documents, options);
    if (options.reviewedSreMigration) await completeSreSchemaMigration(options.reviewedSreMigration);
    return { schemas: crds.length, published: true };
  };
}

export async function stageCoreSchemaDocuments(
  execute: SchemaExecute, documents: ObjectMap[], options: SchemaStageOptions,
): Promise<{ schemas: number; published: true }> {
  return (await planCoreSchemaDocuments(execute, documents, options))();
}
