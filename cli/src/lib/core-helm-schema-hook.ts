// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  canonicalSchema, schemaDigest, schemaDocuments, type ObjectMap, type SchemaExecute,
} from "./schema-documents.js";
import { assertNoCrdRemoval, assertRollbackCompatibility } from "./schema-compatibility.js";
import { planCoreSchemaDocuments, stageCoreSchemaDocuments } from "./schema-stage.js";
import { assertRenderedControllersMutable } from "./private-root-upgrade-guard.js";
import type { SchemaWait } from "./schema-discovery.js";

export interface CoreSchemaHookOptions extends SchemaWait {
  release: string;
  namespace: string;
  revision: number;
  operation: "install" | "upgrade";
  /** The chart must supply .Capabilities.HelmVersion.Version, not the helper binary's version. */
  installerHelmVersion: string;
}

interface HistoryItem extends ObjectMap { revision: number; status: string }
const terminalStates = new Set(["deployed", "superseded", "failed"]);
const manifestLimit = 8 * 1024 * 1024;

function validateOptions(options: CoreSchemaHookOptions): void {
  if (typeof options.release !== "string" || !/^[a-z0-9](?:[-a-z0-9.]*[a-z0-9])?$/.test(options.release) || options.release.length > 53
    || typeof options.namespace !== "string" || !/^[a-z0-9](?:[-a-z0-9]*[a-z0-9])?$/.test(options.namespace) || options.namespace.length > 63
    || !Number.isSafeInteger(options.revision) || options.revision < 1
    || !["install", "upgrade"].includes(options.operation)) {
    throw new Error("An exact pending Helm release, namespace, revision and operation are required");
  }
  // Only this installer version has been source-inspected for normal-resource SSA after pre-install hooks.
  if (!/^v4\.1\.3(?:\+[0-9A-Za-z.-]+)?$/.test(options.installerHelmVersion)) {
    throw new Error("Chart-managed schema staging requires the inspected Helm 4.1.3 installer");
  }
  const timeout = options.timeoutMs ?? 120_000;
  if (!Number.isSafeInteger(timeout) || timeout < 1 || timeout > 600_000) {
    throw new Error("Schema hook deadline must be between 1 and 600000 ms");
  }
}

/** Stages only CRDs from the already-stored pending release. It never re-renders,
 * installs policies, reconstructs private authority, retries writes or rolls Helm back. */
export async function preparePendingCoreHelmSchemas(
  execute: SchemaExecute, options: CoreSchemaHookOptions,
): Promise<{ schemas: number; published: true; revision: number; manifestDigest: string }> {
  validateOptions(options);
  const { release, namespace, revision, operation } = options;
  const now = options.now ?? (() => performance.now());
  const deadline = now() + (options.timeoutMs ?? 120_000);
  const remaining = () => {
    const value = Math.floor(deadline - now());
    if (value <= 0) throw new Error("Schema hook deadline expired; no further operation is authorized");
    return value;
  };
  const invoke: SchemaExecute = async (file, args, settings) => {
    const result = await execute(file, args, { ...settings, timeout: Math.min(settings.timeout ?? 25_000, remaining()) });
    remaining();
    if (typeof result.stdout !== "string" || Buffer.byteLength(result.stdout) > manifestLimit) {
      throw new Error("Schema hook command returned an oversized or invalid response");
    }
    return result;
  };
  const history = async (): Promise<HistoryItem[]> => {
    const value: unknown = JSON.parse((await invoke("helm", ["history", release, "-n", namespace,
      "-o", "json", "--max", "1025"], { stdio: "pipe" })).stdout);
    if (!Array.isArray(value) || !value.length || value.length > 1024 || value.some(item =>
      !item || typeof item !== "object" || !Number.isSafeInteger(item.revision) || item.revision < 1
      || typeof item.status !== "string") || new Set(value.map(item => item.revision)).size !== value.length) {
      throw new Error("Pending Helm history is incomplete or ambiguous");
    }
    const rows = [...value].sort((a, b) => a.revision - b.revision) as HistoryItem[];
    if (rows.at(-1)!.revision !== revision || rows.at(-1)!.status !== `pending-${operation}`
      || rows.slice(0, -1).some(item => !terminalStates.has(item.status))) {
      throw new Error("Helm operation is not the sole exact latest pending revision");
    }
    if (operation === "install" && (revision !== 1 || rows.length !== 1)) {
      throw new Error("Fresh schema installation cannot replace a historical release");
    }
    if (operation === "upgrade" && (rows.length < 2 || rows.at(-2)!.revision !== revision - 1)) {
      throw new Error("Pending upgrade lacks its immediate prior release history");
    }
    return rows;
  };
  const manifest = async (target: number) => {
    const text = (await invoke("helm", ["get", "manifest", release, "-n", namespace,
      "--revision", String(target)], { stdio: "pipe" })).stdout;
    return { text, documents: schemaDocuments(text), digest: schemaDigest(text) };
  };
  const snapshot = await history();
  const snapshots = new Map<number, Awaited<ReturnType<typeof manifest>>>();
  const pending = await manifest(revision);
  snapshots.set(revision, pending);
  const documents = pending.documents;
  if (documents.some(object => object.kind === "CustomResourceDefinition" && object.metadata.annotations?.["helm.sh/hook"])) {
    throw new Error("Core CRDs must remain ordinary retained Helm resources, not hooks");
  }
  if (operation === "install" && documents.some(object => object.apiVersion?.startsWith("kars.azure.com/"))) {
    throw new Error("A cold schema hook cannot install ordinary custom resources before their REST mapping exists");
  }
  let prior: Awaited<ReturnType<typeof manifest>> | undefined;
  let rollbackDocuments: ObjectMap[] | undefined;
  if (operation === "upgrade") {
    prior = await manifest(revision - 1);
    snapshots.set(revision - 1, prior);
    assertNoCrdRemoval(prior.documents, documents);
    const successful = snapshot.filter(item => ["deployed", "superseded"].includes(item.status)).at(-1);
    if (!successful) throw new Error("Schema upgrade has no successful rollback target");
    const rollback = snapshots.get(successful.revision) ?? await manifest(successful.revision);
    snapshots.set(successful.revision, rollback);
    rollbackDocuments = rollback.documents;
    assertNoCrdRemoval(rollbackDocuments, documents);
    // Hooks cannot observe the caller's atomic flag; conservatively require rollback safety for every upgrade.
    assertRollbackCompatibility(documents, rollbackDocuments);
    // Helm 4.1.3 fixes its original resource list before pre-upgrade hooks run.
    // Creating a CRD missing from that list makes its subsequent Update fail.
    const originalRevision = snapshot.filter(item => item.status === "deployed").at(-1)?.revision ?? revision - 1;
    const original = snapshots.get(originalRevision) ?? await manifest(originalRevision);
    snapshots.set(originalRevision, original);
    const originalCrds = new Set(original.documents.filter(object => object.kind === "CustomResourceDefinition")
      .map(object => object.metadata.name));
    if (documents.some(object => object.kind === "CustomResourceDefinition" && !originalCrds.has(object.metadata.name))) {
      throw new Error("Helm 4.1.3 cannot stage CRDs absent from its original upgrade manifest; no schema writes were attempted");
    }
  }
  const controllerDocuments = [...snapshots.values()].flatMap(item => item.documents);
  const recheck = async () => {
    if (canonicalSchema(await history()) !== canonicalSchema(snapshot)) {
      throw new Error("Helm history changed during schema staging");
    }
    for (const [target, pinned] of snapshots) {
      if ((await manifest(target)).digest !== pinned.digest) {
        throw new Error("Pinned Helm manifest changed during schema staging");
      }
    }
    await assertRenderedControllersMutable(invoke, controllerDocuments, namespace);
    if (canonicalSchema(await history()) !== canonicalSchema(snapshot)) {
      throw new Error("Helm history changed during the controller safety check");
    }
  };
  const guarded: SchemaExecute = async (file, args, settings) => {
    if (file === "helm") {
      // The generic planner's unversioned owner lookup must never read the pending target as its prior owner.
      if (canonicalSchema(args) !== canonicalSchema(["get", "manifest", release, "-n", namespace]) || !prior) {
        throw new Error("Unreviewed Helm operation during schema staging");
      }
      await recheck();
      return { stdout: prior.text };
    }
    if (file !== "kubectl" || !["get", "create", "apply"].includes(args[0])) {
      throw new Error("Only bounded schema reads and original CRD writes are permitted");
    }
    if (args[0] !== "get") {
      const object = JSON.parse(settings.input ?? "null");
      if (object?.kind !== "CustomResourceDefinition" || !documents.some(desired =>
        desired.kind === object.kind && desired.metadata.name === object.metadata?.name
        && canonicalSchema(desired.spec) === canonicalSchema(object.spec))
        || args.some(arg => arg.startsWith("--force"))) {
        throw new Error("Schema hook refused a write outside its pinned CRD inventory");
      }
      await recheck();
    }
    return invoke(file, args, settings);
  };
  await recheck();
  const stageOptions = () => ({ release, namespace, ownership: "helm" as const,
    rollbackDocuments, beforeWrite: recheck, now, sleep: options.sleep, timeoutMs: remaining() });
  const apply = await planCoreSchemaDocuments(guarded, documents, stageOptions());
  const prepared = await apply();
  await recheck();
  await stageCoreSchemaDocuments(guarded, documents, { ...stageOptions(), checkOnly: true });
  await recheck();
  return { ...prepared, revision, manifestDigest: pending.digest };
}
