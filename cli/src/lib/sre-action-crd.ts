// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { get, type ApiObject, type Execute } from "./sre-authority.js";

export const ACTION_CRD = "karssreactions.kars.azure.com";
const PARAMS = "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/action/properties/params";
// Complete immutable baseline spec, not a name/label-based adoption rule.
// Azure/kars 8b206065608593667a40665b3f48225ef9ce278d (BASE365).
const BASELINE = "2e119f9dee47426bdf1332e346a5f75f8b6a2d0d6c2a2781903406e7e25b577e";

function canonical(value: any): string {
  const sort = (v: any): any => Array.isArray(v) ? v.map(sort)
    : v && typeof v === "object" ? Object.fromEntries(Object.keys(v).sort().map(k => [k, sort(v[k])])) : v;
  return JSON.stringify(sort(value));
}

function normalizedSpec(object: ApiObject): any {
  const spec = structuredClone(object.spec);
  if (!spec?.names || !Array.isArray(spec.versions)) throw new Error("Action CRD schema is missing");
  // Only API defaulting is ignored; custom fields, versions and validations
  // are never overwritten or treated as the recognized historical schema.
  spec.names.categories ??= [];
  if (spec.names.listKind === "KarsSREActionList") delete spec.names.listKind;
  if (canonical(spec.conversion) === '{"strategy":"None"}') delete spec.conversion;
  if (spec.preserveUnknownFields === false) delete spec.preserveUnknownFields;
  for (const version of spec.versions) {
    if (version.deprecated === false) delete version.deprecated;
    for (const column of version.additionalPrinterColumns ?? []) {
      if (column.priority === 0) delete column.priority;
    }
  }
  return spec;
}

export async function planActionCrd(
  execute: Execute, desired: ApiObject, namespace: string, release: string, helm: boolean,
): Promise<() => Promise<void>> {
  const spec = normalizedSpec(desired);
  const legacy = structuredClone(spec);
  const params = legacy.versions[0]?.schema?.openAPIV3Schema?.properties?.spec?.properties?.action?.properties?.params;
  if (desired.kind !== "CustomResourceDefinition" || desired.metadata.name !== ACTION_CRD
    || params?.["x-kubernetes-preserve-unknown-fields"] !== true || "additionalProperties" in params) {
    throw new Error("Staged chart lacks the compatible action CRD params repair");
  }
  delete params["x-kubernetes-preserve-unknown-fields"];
  params.additionalProperties = true;
  if (createHash("sha256").update(canonical(legacy)).digest("hex") !== BASELINE) {
    throw new Error("Unrecognized action CRD chart schema; explicitly review compatibility before staging policies");
  }
  const existing = await get(execute, "customresourcedefinition", ACTION_CRD);
  if (!existing) {
    return async () => {
      await execute("kubectl", ["create", "-f", "-"], { stdio: "pipe", input: JSON.stringify({
        ...desired, metadata: { ...desired.metadata, ...(helm ? {
          labels: { ...desired.metadata.labels, "app.kubernetes.io/managed-by": "Helm" },
          annotations: { "meta.helm.sh/release-name": release, "meta.helm.sh/release-namespace": namespace },
        } : {}) },
      }) });
      await established(execute);
    };
  }
  const annotations = existing.metadata.annotations ?? {};
  const manager = existing.metadata.labels?.["app.kubernetes.io/managed-by"];
  if (existing.metadata.deletionTimestamp || existing.metadata.ownerReferences?.length
    || (manager && manager !== "Helm")
    || (annotations["meta.helm.sh/release-name"] && (!helm || annotations["meta.helm.sh/release-name"] !== release))
    || (annotations["meta.helm.sh/release-namespace"] && (!helm || annotations["meta.helm.sh/release-namespace"] !== namespace))
    || (annotations["kars.azure.com/sre-authority-staged"] && annotations["kars.azure.com/sre-authority-staged"] !== namespace)
    || (annotations["kars.azure.com/sre-authority-release"] && annotations["kars.azure.com/sre-authority-release"] !== release)
    || (manager === "Helm" && (!helm || annotations["meta.helm.sh/release-name"] !== release
      || annotations["meta.helm.sh/release-namespace"] !== namespace))) {
    throw new Error("Foreign or terminating action CRD; review its installation ownership before staging");
  }
  const current = normalizedSpec(existing);
  const repair = canonical(current) === canonical(legacy);
  if (!repair && canonical(current) !== canonical(spec)) {
    throw new Error("Customized action CRD schema; explicitly review the params compatibility repair before staging policies");
  }
  return async () => {
    // Even the already-repaired case tests the complete reviewed snapshot before
    // installing dependent policies. No legacy ownership annotation is adopted.
    await execute("kubectl", ["patch", "customresourcedefinition", ACTION_CRD, "--type=json", "-p", JSON.stringify([
      { op: "test", path: "/metadata/uid", value: existing.metadata.uid },
      { op: "test", path: "/metadata/resourceVersion", value: existing.metadata.resourceVersion },
      ...(repair ? [
        { op: "test", path: `${PARAMS}/additionalProperties`, value: true },
        { op: "remove", path: `${PARAMS}/additionalProperties` },
        { op: "add", path: `${PARAMS}/x-kubernetes-preserve-unknown-fields`, value: true },
      ] : []),
    ])], { stdio: "pipe" });
    await established(execute);
  };
}

async function established(execute: Execute): Promise<void> {
  await execute("kubectl", ["wait", "--for=condition=Established", `crd/${ACTION_CRD}`, "--timeout=60s"], { stdio: "pipe" });
}
