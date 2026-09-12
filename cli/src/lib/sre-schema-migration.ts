// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { CANONICAL_SCHEMAS, EVALUATOR_V2, MIGRATION } from "./sre-migration-catalog.js";
import {
  canonicalSchema, normalizedCrd, readSchemaObject, schemaDigest, schemaIdentity, verifySchemaOwner,
  type ObjectMap, type SchemaExecute, type SchemaOwner,
} from "./schema-documents.js";
import { requireCrdRetention } from "./schema-compatibility.js";
import { get, requireRegistrar } from "./sre-authority.js";
import { qualifyMigrationData, recheckMigrationData, requireNoNewAuthorities, type MigrationDataSnapshot } from "./sre-migration-data.js";
import { schemaStep } from "./sre-schema-diagnostics.js";

export interface QualifiedSreMigration { readonly id: typeof MIGRATION }
interface Review {
  execute: SchemaExecute;
  owner: SchemaOwner;
  schemas: Map<string, { current?: ObjectMap; desired: ObjectMap; written: boolean }>;
  data: MigrationDataSnapshot[];
  profile: "core-470" | "evaluator-v2";
  controller: ObjectMap;
}
const reviews = new WeakMap<QualifiedSreMigration, Review>();
const requiresReviewedMigration = new Set([
  "karssreactions.kars.azure.com", "karstasks.kars.azure.com", "karsteams.kars.azure.com",
  "mcpservers.kars.azure.com", "karssandboxes.kars.azure.com",
]);

async function quiescentController(execute: SchemaExecute, owner: SchemaOwner, expected?: ObjectMap): Promise<ObjectMap> {
  schemaStep("controller-quiescence", "Deployment");
  const controller = await get(execute, "deployment", "kars-controller", owner.namespace);
  schemaStep("controller-quiescence", "Deployment", controller);
  if (!controller || controller.metadata.name !== "kars-controller" || controller.metadata.namespace !== owner.namespace
    || controller.spec?.replicas !== 0
    || controller.spec?.template?.spec?.serviceAccountName !== "kars-controller"
    || ["replicas", "availableReplicas", "readyReplicas", "updatedReplicas"].some(key => (controller.status?.[key] ?? 0) !== 0)) {
    throw new Error("Canonical BASE365 schema migration requires the reviewed controller already paused; no authority is grandfathered");
  }
  verifySchemaOwner(controller, owner);
  if (expected && canonicalSchema(schemaIdentity(controller)) !== canonicalSchema(schemaIdentity(expected))) {
    throw new Error("Controller UID/resourceVersion changed during schema migration review");
  }
  const pods: ObjectMap = JSON.parse((await execute("kubectl", ["get", "pods", "-n", owner.namespace,
    "--chunk-size=0", "-o", "json"], { stdio: "pipe" })).stdout);
  if (!Array.isArray(pods.items) || pods.items.length > 512 || pods.metadata?.continue
    || pods.items.some((pod: ObjectMap) => !pod.metadata?.uid || !pod.spec || pod.spec.serviceAccountName === "kars-controller")) {
    throw new Error("Controller authority has not quiesced for the canonical schema migration");
  }
  return controller;
}

export async function qualifySreSchemaMigration(
  execute: SchemaExecute, documents: ObjectMap[], owner: SchemaOwner,
): Promise<QualifiedSreMigration | undefined> {
  if (owner.ownership !== "helm") throw new Error("The canonical BASE365 migration requires its exact Helm owner");
  schemaStep("registrar");
  await requireRegistrar(execute);
  const crds = documents.filter(object => object.kind === "CustomResourceDefinition");
  const schemas: Review["schemas"] = new Map();
  let needed = false;
  for (const desired of crds) {
    schemaStep("schema-inventory", desired.spec?.names?.kind);
    const current = await readSchemaObject(execute, "customresourcedefinition", desired.metadata.name);
    if (current && requiresReviewedMigration.has(desired.metadata.name)
      && schemaDigest(normalizedCrd(current)) !== schemaDigest(normalizedCrd(desired))) needed = true;
    schemas.set(desired.metadata.name, { current, desired: structuredClone(desired), written: false });
  }
  if (!needed) return undefined;
  schemaStep("schema-inventory");
  if (canonicalSchema([...schemas.keys()].sort()) !== canonicalSchema(Object.keys(CANONICAL_SCHEMAS).sort())) {
    throw new Error("BASE365 migration requires the complete canonical core CRD inventory");
  }
  const controller = await quiescentController(execute, owner);
  schemaStep("schema-retention");
  requireCrdRetention(crds);
  const data: MigrationDataSnapshot[] = [];
  for (const [name, entry] of schemas) {
    schemaStep("canonical-target", entry.desired.spec.names.kind);
    const allowed = CANONICAL_SCHEMAS[name];
    const after = schemaDigest(normalizedCrd(entry.desired));
    if (!allowed.after.includes(after)) throw new Error(`Unreviewed target schema in BASE365 migration: ${name}`);
    if (!entry.current) {
      if (allowed.before) throw new Error(`Historical BASE365 CRD is missing: ${name}`);
      continue;
    }
    schemaStep("schema-owner", entry.desired.spec.names.kind, entry.current);
    verifySchemaOwner(entry.current, owner);
    schemaStep("canonical-before", entry.desired.spec.names.kind);
    const before = schemaDigest(normalizedCrd(entry.current));
    if (before !== after && before !== allowed.before) throw new Error(`Live schema is not the exact BASE365 or qualified target: ${name}`);
    schemaStep("stored-versions", entry.desired.spec.names.kind);
    if ((entry.current.status?.storedVersions ?? []).some((version: string) => version !== "v1alpha1")) {
      throw new Error("Canonical SRE migration cannot migrate another stored API version");
    }
    if (!allowed.before) await requireNoNewAuthorities(execute, entry.desired);
    if (before !== after) data.push(await qualifyMigrationData(execute, entry.current, entry.desired));
  }
  schemaStep("schema-qualification");
  if (data.reduce((count, item) => count + item.count, 0) > 512
    || data.reduce((bytes, item) => bytes + item.bytes, 0) > 8 * 1024 * 1024) throw new Error("Complete migration data inventory exceeds its bound");
  const evalSchema = schemas.get("karsevals.kars.azure.com")!.desired;
  const plan: QualifiedSreMigration = Object.freeze({ id: MIGRATION });
  reviews.set(plan, { execute, owner, schemas, data, controller,
    profile: schemaDigest(normalizedCrd(evalSchema)) === EVALUATOR_V2 ? "evaluator-v2" : "core-470" });
  await recheckSreSchemaMigration(plan);
  return plan;
}

export function authorizesSreSchemaMigration(plan: QualifiedSreMigration, current: ObjectMap | undefined, desired: ObjectMap): boolean {
  const entry = reviews.get(plan)?.schemas.get(desired.metadata.name);
  if (!entry || entry.written || canonicalSchema(entry.desired) !== canonicalSchema(desired)) return false;
  if (!entry.current || !current) return !entry.current && !current;
  return canonicalSchema(schemaIdentity(entry.current)) === canonicalSchema(schemaIdentity(current))
    && schemaDigest(normalizedCrd(entry.current)) === schemaDigest(normalizedCrd(current));
}

export async function recheckSreSchemaMigration(plan: QualifiedSreMigration, name?: string): Promise<void> {
  const review = reviews.get(plan);
  if (!review) throw new Error("Unqualified SRE schema migration permit");
  await quiescentController(review.execute, review.owner, review.controller);
  for (const [key, entry] of review.schemas) {
    if (name && key !== name) continue;
    schemaStep("migration-recheck", entry.desired.spec.names.kind);
    const current = await readSchemaObject(review.execute, "customresourcedefinition", key);
    if (!entry.current) {
      if (current) throw new Error("A new CRD appeared after migration review");
      continue;
    }
    if (!current || schemaIdentity(current).uid !== entry.current.metadata.uid
      || (!entry.written && schemaIdentity(current).resourceVersion !== entry.current.metadata.resourceVersion)
      || schemaDigest(normalizedCrd(current)) !== schemaDigest(normalizedCrd(entry.written ? entry.desired : entry.current))) {
      throw new Error("CRD UID/resourceVersion/schema changed after migration review");
    }
    verifySchemaOwner(current, review.owner);
    if (!CANONICAL_SCHEMAS[key].before && !entry.written) await requireNoNewAuthorities(review.execute, entry.desired);
  }
  for (const snapshot of review.data) if (!name || snapshot.crd.metadata.name === name) await recheckMigrationData(review.execute, snapshot);
}

export function recordSreSchemaWrite(plan: QualifiedSreMigration, applied: ObjectMap): void {
  const entry = reviews.get(plan)?.schemas.get(applied.metadata.name);
  if (!entry || (entry.current && schemaIdentity(applied).uid !== entry.current.metadata.uid)
    || schemaDigest(normalizedCrd(applied)) !== schemaDigest(normalizedCrd(entry.desired))) throw new Error("Migration write returned another schema identity");
  entry.current = structuredClone(applied);
  entry.written = true;
}

export async function completeSreSchemaMigration(plan: QualifiedSreMigration): Promise<void> {
  await recheckSreSchemaMigration(plan);
  const review = reviews.get(plan)!;
  for (const [name, entry] of review.schemas) {
    if (!CANONICAL_SCHEMAS[name].before) await requireNoNewAuthorities(review.execute, entry.desired);
  }
}

export function sreMigrationSummary(plan: QualifiedSreMigration): ObjectMap {
  const review = reviews.get(plan);
  if (!review) throw new Error("Unqualified SRE schema migration permit");
  return { id: MIGRATION, profile: review.profile, schemas: review.schemas.size,
    objects: review.data.reduce((count, item) => count + item.count, 0) };
}
