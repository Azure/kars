// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { canonicalSchema, schemaDocuments, type ObjectMap, type SchemaExecute } from "./schema-documents.js";
import { assertNoCrdRemoval, assertRollbackCompatibility } from "./schema-compatibility.js";

export function enabledHelmFlag(args: readonly string[], flag: string): boolean {
  return args.includes(flag) || args.includes(`${flag}=true`);
}

export async function serverSchemaRenderFlags(execute: SchemaExecute): Promise<string[]> {
  const version = (await execute("helm", ["version", "--template", "{{.Version}}"], { stdio: "pipe" })).stdout.trim();
  const match = /^v([34])\.(\d+)\.\d+(?:[-+][0-9A-Za-z.+-]+)?$/.exec(version);
  if (!match || (match[1] === "3" && Number(match[2]) < 13)) {
    throw new Error("Schema rendering requires Helm 3.13+ or Helm 4 with server dry-run support");
  }
  // Helm 3's template ClientOnly flag otherwise replaces real capabilities.
  return ["--dry-run=server", ...(match[1] === "3" ? ["--validate"] : [])];
}

export async function prepareHelmFailureSafety(
  execute: SchemaExecute, args: readonly string[], documents: ObjectMap[], release: string, namespace: string, upgrading: boolean,
): Promise<{ rollbackDocuments?: ObjectMap[]; recheck?: () => Promise<void> }> {
  if (["--cleanup-on-fail", "--force", "--force-replace", "--force-conflicts", "--take-ownership"]
    .some(flag => enabledHelmFlag(args, flag))) {
    throw new Error("Forced replacement/adoption or cleanup-on-fail cannot preserve core schemas; explicit migration is required");
  }
  if (!upgrading) return {};
  const history = async () => {
    const value: unknown = JSON.parse((await execute("helm", ["history", release, "-n", namespace, "-o", "json"],
      { stdio: "pipe" })).stdout);
    if (!Array.isArray(value) || !value.length || value.length > 1024 || value.some(item =>
      !Number.isSafeInteger(item?.revision) || item.revision < 1 || typeof item.status !== "string")) {
      throw new Error("Helm history is incomplete; rollback compatibility cannot be proved");
    }
    return value as { revision: number; status: string }[];
  };
  const snapshot = await history();
  const current = Math.max(...snapshot.map(item => item.revision));
  const manifest = async (revision: number) => schemaDocuments((await execute("helm",
    ["get", "manifest", release, "-n", namespace, "--revision", String(revision)], { stdio: "pipe" })).stdout);
  assertNoCrdRemoval(await manifest(current), documents);
  const atomic = enabledHelmFlag(args, "--atomic") || enabledHelmFlag(args, "--rollback-on-failure");
  let rollbackDocuments: ObjectMap[] | undefined;
  if (atomic) {
    // Both Helm 3 atomic and Helm 4 rollback-on-failure select the latest
    // successful (deployed or superseded) release, not simply revision - 1.
    const successful = snapshot.filter(item => ["deployed", "superseded"].includes(item.status));
    if (!successful.length) throw new Error("Atomic core operation has no successful rollback target");
    rollbackDocuments = await manifest(Math.max(...successful.map(item => item.revision)));
    assertNoCrdRemoval(rollbackDocuments, documents);
    assertRollbackCompatibility(documents, rollbackDocuments);
  }
  return { rollbackDocuments, recheck: async () => {
    if (canonicalSchema(await history()) !== canonicalSchema(snapshot)) {
      throw new Error("Helm history changed during schema preparation; no operation was issued");
    }
  } };
}
