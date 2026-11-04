// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { parseDocument } from "yaml";
import { canonicalSchema, type SchemaExecute } from "./schema-documents.js";
import { get } from "./sre-authority.js";

/** Carry the ORIGINAL witness baseline into both preflight and the real upgrade.
 * The chart proves it against the immutable adoption receipt before accepting it. */
export async function coreWitnessUpgradeFlags(
  execute: SchemaExecute, release: string, namespace: string, overlays: readonly string[], assignments: readonly string[],
): Promise<string[]> {
  const adoption = await get(execute, "configmap", "kars-sre-helm-adoption", namespace);
  if (!adoption) return [];
  if (adoption.immutable !== true || adoption.metadata.deletionTimestamp
    || adoption.metadata.name !== "kars-sre-helm-adoption" || adoption.metadata.namespace !== namespace
    || adoption.metadata.annotations?.["meta.helm.sh/release-name"] !== release
    || adoption.metadata.annotations?.["meta.helm.sh/release-namespace"] !== namespace
    || typeof adoption.data?.intentUID !== "string" || !adoption.data.intentUID
    || !/^[a-f0-9]{64}$/.test(adoption.data?.valuesDigest ?? "")) {
    throw new Error("Witness upgrade requires the original immutable Core adoption receipt");
  }
  const saved = JSON.parse((await execute("helm", ["get", "values", release, "-n", namespace, "--all", "-o", "json"],
    { stdio: "pipe", timeout: 25_000 })).stdout);
  const witness = saved?.datapathWitness;
  if (witness?.enabled !== true || witness.managed !== true) return [];
  const baseline = structuredClone(saved.sre?.datapathWitnessBaseline ?? witness);
  if (!baseline || typeof baseline !== "object" || Array.isArray(baseline)) {
    throw new Error("Invalid original witness baseline in Core values");
  }
  // Helm get-values omits the parent's profile propagated during rendering.
  // Capture what the original receipt sealed, never a new overlay's profile.
  if (saved.sre?.datapathWitnessBaseline === undefined && saved.global?.installationProfile !== undefined) {
    baseline.global = { ...baseline.global, installationProfile: saved.global.installationProfile };
  }
  let explicitScope = assignments.some(value => /(?:^|,)datapathWitness\.scope=/.test(value));
  // Do not overwrite a caller-supplied baseline silently, even though the chart
  // would reject one that cannot reproduce the original sealed digest.
  if (assignments.some(value => /(?:^|,)sre\.datapathWitnessBaseline(?:[.[]|=)/.test(value))) {
    throw new Error("Core captures the original witness baseline; do not override it with --set");
  }
  for (const file of overlays) {
    const document = parseDocument(readFileSync(file, "utf8"));
    if (document.errors.length) throw new Error("Invalid Core values overlay");
    const values = document.toJSON();
    if (values?.datapathWitness && Object.hasOwn(values.datapathWitness, "scope")) explicitScope = true;
    const supplied = values?.sre?.datapathWitnessBaseline;
    if (supplied !== undefined && canonicalSchema(supplied) !== canonicalSchema(baseline)) {
      throw new Error("Core values must retain the original witness baseline, not the last upgrade's values");
    }
  }
  return ["--set-json", `sre.datapathWitnessBaseline=${JSON.stringify(baseline)}`,
    // Old releases omitted scope. Never let a new dependency default silently
    // turn an unchanged selected witness into fleet-wide discovery.
    ...(!explicitScope ? ["--set-json", `datapathWitness.scope=${JSON.stringify(witness.scope ?? "selected")}`] : [])];
}
