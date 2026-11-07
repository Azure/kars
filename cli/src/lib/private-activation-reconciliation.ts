// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import {
  at, PRIVATE_PREFIX as P, read, record, reviewed, template, templateDigest,
  type Execute, type NamespaceReview,
} from "./private-activation.js";

const FIELDS = [
  "kars.azure.com/inference-budget-ca-version", "kars.azure.com/inference-providers-version",
  "kars.azure.com/services-credential-version", "kars.azure.com/sre-private-credential-uid",
  "kars.azure.com/sre-tls-expiry",
];
const RECEIPT = ["epoch", "baseline", "structure", "template", "sandbox_uid", "generation"];

function structure(value: unknown): string {
  const current = structuredClone(template(value));
  const fields = at(current, "metadata", "annotations");
  if (fields !== undefined) {
    for (const name of FIELDS) {
      const annotations = record(fields);
      if (annotations[name] !== undefined && typeof annotations[name] !== "string") throw new Error("Malformed credential rollout annotation");
      delete annotations[name];
    }
  }
  return templateDigest({ kind: "Deployment", spec: { template: current } });
}

function modelStructure(value: unknown): string {
  const current = structuredClone(record(value));
  const containers = at(template(current), "spec", "containers");
  if (!Array.isArray(containers)) throw new Error("Malformed model rollout containers");
  const seen = new Set<string>();
  for (const container of containers) {
    const name = at(container, "name");
    if (typeof name !== "string") throw new Error("Malformed model rollout container");
    const env = at(container, "env");
    if (!Array.isArray(env)) continue;
    for (const entry of env) {
      const variable = at(entry, "name");
      if (typeof variable !== "string") throw new Error("Malformed model rollout environment");
      if (!((name === "agent" || name === "openclaw") && variable === "KARS_MODEL")
        && !(name === "openclaw" && variable === "OPENCLAW_MODEL")
        && !(name === "inference-router" && variable === "AZURE_OPENAI_DEPLOYMENT")) continue;
      const key = `${name}/${variable}`;
      const literal = at(entry, "value");
      if (seen.has(key) || Object.hasOwn(record(entry), "valueFrom")
        || typeof literal !== "string" || !literal.trim()) throw new Error("Malformed model rollout value");
      seen.add(key);
      record(entry).value = "controller-policy-model";
    }
  }
  return structure(current);
}

/** History is allowed only for operator migration review; ordinary continuity needs a projector receipt. */
export async function reviewedControllerRollout(
  execute: Execute, scope: NamespaceReview, consumer: NamespaceReview["consumers"][number],
  current: unknown, allowHistory = false,
): Promise<boolean> {
  if (consumer.kind !== "Deployment" || at(current, "kind") !== "Deployment"
    || at(current, "apiVersion") !== "apps/v1" || reviewed(current).uid !== consumer.object.uid
    || reviewed(current).name !== consumer.object.name || at(current, "metadata", "namespace") !== scope.namespace.name
    || !scope.epoch || at(template(current), "metadata", "annotations", `${P}epoch`) !== scope.epoch) return false;
  const namespace = await read(execute, "namespace", scope.namespace.name);
  const fields = record(at(namespace, "metadata", "annotations"));
  const raw = fields[`${P}reconciled-${consumer.object.uid}`];
  if (raw === undefined && !allowHistory) return false;
  const workspace = fields["kars.azure.com/sandbox-namespace"];
  if (reviewed(namespace).uid !== scope.namespace.uid || fields[`${P}epoch`] !== scope.epoch
    || fields[`${P}parent-${consumer.object.uid}`] !== scope.epoch
    || fields["kars.azure.com/namespace-claim-version"] !== "v1"
    || fields["kars.azure.com/sandbox-name"] !== consumer.object.name
    || scope.namespace.name !== `kars-${consumer.object.name}` || typeof workspace !== "string" || !workspace
    || fields["kars.azure.com/namespace-prestage"] !== undefined
    || (at(namespace, "metadata", "ownerReferences") as unknown[] | undefined)?.length) return false;
  const sandbox = await read(execute, "karssandbox", consumer.object.name, workspace);
  const generation = at(sandbox, "metadata", "generation");
  const bound = at(sandbox, "metadata", "annotations", "kars.azure.com/namespace-uid");
  if (reviewed(sandbox).name !== consumer.object.name || at(sandbox, "metadata", "namespace") !== workspace
    || reviewed(sandbox).uid !== fields["kars.azure.com/sandbox-uid"]
    || (bound !== undefined && bound !== scope.namespace.uid)
    || typeof generation !== "number" || !Number.isSafeInteger(generation) || generation < 1) return false;
  const actual = structure(current);
  if (raw !== undefined) {
    if (typeof raw !== "string" || Buffer.byteLength(raw) > 8192) return false;
    const receipt = record(JSON.parse(raw));
    const hasModelStructure = Object.hasOwn(receipt, "model_structure");
    return Object.keys(receipt).length === RECEIPT.length + Number(hasModelStructure)
      && Object.keys(receipt).every(key => RECEIPT.includes(key) || key === "model_structure")
      && (!hasModelStructure || receipt.model_structure === modelStructure(current))
      && receipt.epoch === scope.epoch && receipt.baseline === consumer.templateDigest
      && receipt.structure === actual && receipt.template === templateDigest(current)
      && receipt.sandbox_uid === reviewed(sandbox).uid && receipt.generation === generation;
  }
  const history = record(JSON.parse(await execute(["get", "replicasets.apps", "-n", scope.namespace.name, "--chunk-size=0", "-o", "json"])));
  if (at(history, "metadata", "continue") || !Array.isArray(history.items)) return false;
  return history.items.some(replica => {
    const owners = at(replica, "metadata", "ownerReferences");
    if (at(replica, "kind") !== "ReplicaSet" || at(replica, "apiVersion") !== "apps/v1"
      || at(replica, "metadata", "namespace") !== scope.namespace.name || at(replica, "metadata", "deletionTimestamp")
      || !Array.isArray(owners) || owners.length !== 1) return false;
    const owner = owners[0];
    if (at(owner, "kind") !== "Deployment" || at(owner, "apiVersion") !== "apps/v1"
      || at(owner, "uid") !== consumer.object.uid || at(owner, "name") !== consumer.object.name
      || at(owner, "controller") !== true) return false;
    const baseline = structuredClone(record(replica));
    const labels = at(template(baseline), "metadata", "labels");
    if (labels !== undefined) delete record(labels)["pod-template-hash"];
    return templateDigest(baseline) === consumer.templateDigest && structure(baseline) === actual;
  });
}
