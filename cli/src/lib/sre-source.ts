// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { parseAllDocuments } from "yaml";
import { namespaceClaimed } from "./namespace-ownership.js";
import { get, legacyBindings, requireRegistrar, type ApiObject, type Execute } from "./sre-authority.js";

function same(left: unknown, right: unknown): boolean {
  const canonical = (value: any): any => Array.isArray(value) ? value.map(canonical)
    : value && typeof value === "object" ? Object.fromEntries(Object.keys(value).sort().map(key => [key,canonical(value[key])])) : value;
  return JSON.stringify(canonical(left)) === JSON.stringify(canonical(right));
}

/** Stage only unprivileged configuration and CREATE a genuinely new source.
 * A 409 never becomes a get/adopt of a racing tenant-created Sandbox. */
export async function stageSource(
  execute: Execute, rendered: string, namespace: string, release: string,
): Promise<{ uid: string; namespaceUid: string }> {
  await requireRegistrar(execute);
  if ((await legacyBindings(execute)).length) throw new Error("Existing SRE grants require explicit authority preview/enrollment before staging");
  if (await get(execute,"karssandbox","sre",namespace)) {
    throw new Error("SRE source already exists; preview and enroll its explicitly reviewed UID instead of adopting by name");
  }
  if (await get(execute,"namespace","kars-sre")) {
    throw new Error("Existing kars-sre namespace requires explicit ownership review; foreign or unfinished occupants are preserved");
  }
  const objects = parseAllDocuments(rendered).map(document => document.toJSON() as ApiObject | null).filter((value): value is ApiObject => !!value);
  const source = objects.find(value => value.kind === "KarsSandbox" && value.metadata.name === "sre");
  if (!source || source.metadata.namespace !== namespace) throw new Error("Rendered canonical SRE source is absent or has the wrong workspace");
  const allowed = new Map([
    ["InferencePolicy","sre-inference"], ["ToolPolicy","sre-tools"],
  ]);
  const supports = objects.filter(value =>
    allowed.get(value.kind!) === value.metadata.name
    || (value.kind === "ClusterRole" && ["kars-sre-reader","kars-sre-action-author","kars-sre-approver"].includes(value.metadata.name!)));
  const missing: ApiObject[] = [];
  // Validate the entire support set before writing any of it.
  for (const object of supports) {
    const existing = await get(execute,object.kind!.toLowerCase(),object.metadata.name!,object.metadata.namespace);
    if (existing) {
      if (!same(existing.spec ?? existing.rules,object.spec ?? object.rules)) {
        throw new Error(`Existing ${object.kind}/${object.metadata.name} differs; no support resource was adopted or overwritten`);
      }
    } else missing.push(object);
  }
  const own = (object: ApiObject) => ({
    ...object, metadata: {
      ...object.metadata,
      labels: { ...object.metadata.labels, "app.kubernetes.io/managed-by":"Helm" },
      annotations: { ...object.metadata.annotations,
        "meta.helm.sh/release-name":release,"meta.helm.sh/release-namespace":namespace },
    },
  });
  for (const object of missing) {
    await execute("kubectl",["create","-f","-"],{stdio:"pipe",input:JSON.stringify(own(object))});
  }
  const {stdout}=await execute("kubectl",["create","-f","-","-o","json"],{stdio:"pipe",input:JSON.stringify(own(source))});
  const created=JSON.parse(stdout) as ApiObject;
  if (created.metadata?.namespace!==namespace || created.metadata.name!=="sre" || !created.metadata.uid) {
    throw new Error("SRE CREATE response omitted its actual source UID/workspace");
  }
  for (let attempt=0;attempt<60;attempt++) {
    const live=await get(execute,"karssandbox","sre",namespace);
    if (!live || live.metadata.uid!==created.metadata.uid || live.metadata.deletionTimestamp) {
      throw new Error("Created SRE source was replaced or deleted during namespace staging");
    }
    const runtime=await get(execute,"namespace","kars-sre");
    if (runtime) {
      if (runtime.metadata.deletionTimestamp || !namespaceClaimed(runtime,live)) {
        throw new Error("SRE runtime namespace belongs to a foreign/ambiguous occupant; preserved without any grant");
      }
      if (live.metadata.annotations?.["kars.azure.com/namespace-uid"]===runtime.metadata.uid) {
        return {uid:created.metadata.uid,namespaceUid:runtime.metadata.uid!};
      }
    }
    await new Promise(resolve=>setTimeout(resolve,1000));
  }
  throw new Error("SRE source was staged but its exact namespace claim did not converge");
}
