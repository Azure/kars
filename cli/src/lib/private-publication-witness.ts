// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { setTimeout as delay } from "node:timers/promises";
import { at, canonical, read, record, reviewed, type Execute } from "./private-activation.js";
import type { RootMigrationJournal, RootMigrationReceipt } from "./private-root-migration.js";
import type { ObjectMap } from "./schema-documents.js";

const failure = "Datapath publication writer changed or has not stopped; preserve its receipt and paused replica intent";

function selector(deployment: ObjectMap): string {
  const value = record(at(deployment, "spec", "selector"));
  const labels = record(value.matchLabels);
  if (Object.keys(value).some(key => key !== "matchLabels") || !Object.keys(labels).length
    || Object.entries(labels).some(([key, value]) => !/^[a-zA-Z0-9./_-]+$/.test(key)
      || typeof value !== "string" || !/^[a-zA-Z0-9._-]+$/.test(value))) throw new Error(failure);
  return Object.entries(labels).sort().map(([key, value]) => `${key}=${value}`).join(",");
}

async function replicas(run: Execute, current: ObjectMap, count: number): Promise<void> {
  const id = reviewed(current);
  const result = record(JSON.parse(await run(["patch", "Deployment", id.name, "-n", current.metadata.namespace,
    "--type=json", "--patch-file=/dev/stdin", "-o", "json"], JSON.stringify([
    { op: "test", path: "/metadata/uid", value: id.uid },
    { op: "test", path: "/metadata/resourceVersion", value: id.resourceVersion },
    { op: "test", path: "/spec", value: current.spec },
    { op: "replace", path: "/spec/replicas", value: count },
  ]))));
  if (reviewed(result).uid !== id.uid || reviewed(result).resourceVersion === id.resourceVersion
    || canonical(result.spec) !== canonical({ ...current.spec, replicas: count })) throw new Error(failure);
}

async function list(run: Execute, kind: string, deployment: ObjectMap, selected = true): Promise<ObjectMap[]> {
  const result = record(JSON.parse(await run(["get", kind, "-n", deployment.metadata.namespace,
    ...(selected ? ["-l", selector(deployment)] : []), "--chunk-size=0", "-o", "json"])));
  if (!Array.isArray(result.items) || result.items.length > 512 || at(result, "metadata", "continue")) throw new Error(failure);
  return result.items.map(record);
}
async function stopped(run: Execute, live: ObjectMap): Promise<boolean> {
  const autoscalers = await list(run, "horizontalpodautoscalers", live, false);
  if (autoscalers.some(value => value.spec?.scaleTargetRef?.kind === "Deployment"
    && value.spec.scaleTargetRef.name === live.metadata.name)) throw new Error(failure);
  const sets = await list(run, "replicasets", live);
  if (sets.some(value => !Array.isArray(value.metadata?.ownerReferences)
    || !value.metadata.ownerReferences.some((owner: ObjectMap) => owner.kind === "Deployment"
      && owner.uid === live.metadata.uid && owner.controller === true))) throw new Error(failure);
  const idle = (value: ObjectMap) => Number(value.status?.observedGeneration ?? 0) >= Number(value.metadata.generation ?? 0)
    && Number(value.spec?.replicas) === 0 && Number(value.status?.replicas ?? 0) === 0;
  return idle(live) && sets.every(idle) && !(await list(run, "pods", live)).length;
}

export async function assertPublicationWitnessStopped(run: Execute, receipt: RootMigrationReceipt): Promise<void> {
  const writer = receipt.publication?.witnessWriter;
  if (!writer) return;
  const before: ObjectMap = writer.deployment, id = reviewed(before);
  const live: ObjectMap = await read(run, "Deployment", id.name, before.metadata.namespace);
  const expected = ["publishing", "succeeded", "verified"].includes(receipt.publication!.phase)
    ? writer.publishedSpec : { ...before.spec, replicas: 0 };
  if (writer.restored || reviewed(live).uid !== id.uid || canonical(live.spec) !== canonical(expected)
    || live.spec.replicas !== 0 || !await stopped(run, live)) throw new Error(failure);
}

/** The witness owns mutable evidence, not credential authority. Its declared
 * replica intent must survive maintenance without racing Helm's frozen RVs. */
export async function stopPublicationWitness(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal, documents: ObjectMap[],
): Promise<void> {
  const publication = receipt.publication!;
  const evidence = documents.filter(value => value.kind === "ConfigMap" && value.metadata.name === "kars-datapath-witness"
    && at(value, "metadata", "labels", "kars.azure.com/witness-addon") === "true");
  if (!evidence.length && !publication.witnessWriter) return;
  const writers = documents.filter(value => value.kind === "Deployment" && value.metadata.name === "aggregator"
    && at(value, "metadata", "labels", "kars.azure.com/witness-addon") === "true"
    && at(value, "spec", "template", "metadata", "labels", "app.kubernetes.io/component") === "aggregator");
  if (evidence.length !== 1 || writers.length !== 1 || !writers[0]!.metadata.namespace
    || writers[0]!.spec.replicas !== 1 || publication.phase !== "preparing") throw new Error(failure);
  const desired = writers[0]!;
  let live: ObjectMap = await read(run, "Deployment", desired.metadata.name, desired.metadata.namespace);
  if (at(live, "metadata", "annotations", "meta.helm.sh/release-name") !== publication.release
    || at(live, "metadata", "annotations", "meta.helm.sh/release-namespace") !== publication.namespace
    || at(live, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm"
    || canonical(live.spec.selector) !== canonical(desired.spec.selector)
    || ![0, 1].includes(live.spec.replicas)) throw new Error(failure);
  if (!publication.witnessWriter) {
    publication.witnessWriter = { deployment: structuredClone(live) };
    await journal.save(receipt);
  }
  const captured: ObjectMap = publication.witnessWriter.deployment;
  if (publication.witnessWriter.restored || reviewed(live).uid !== reviewed(captured).uid
    || canonical({ ...live.spec, replicas: 0 }) !== canonical({ ...captured.spec, replicas: 0 })) throw new Error(failure);
  if (live.spec.replicas !== 0) await replicas(run, live, 0);
  const deadline = Date.now() + 120_000;
  while (true) {
    live = await read(run, "Deployment", desired.metadata.name, desired.metadata.namespace);
    if (reviewed(live).uid !== reviewed(captured).uid || live.spec.replicas !== 0
      || canonical(live.spec) !== canonical({ ...captured.spec, replicas: 0 })) throw new Error(failure);
    if (await stopped(run, live)) break;
    if (Date.now() >= deadline) throw new Error(failure);
    await delay(1000);
  }
  desired.spec.replicas = 0;
  await assertPublicationWitnessStopped(run, receipt);
}

export async function restorePublicationWitness(
  run: Execute, receipt: RootMigrationReceipt, journal: RootMigrationJournal, documents: ObjectMap[],
): Promise<void> {
  const writer = receipt.publication?.witnessWriter;
  if (!writer || writer.restored) return;
  if (receipt.publication!.phase !== "verified") throw new Error(failure);
  const before: ObjectMap = writer.deployment, id = reviewed(before);
  const desired = documents.find(value => value.kind === "Deployment" && value.metadata.name === id.name
    && value.metadata.namespace === before.metadata.namespace);
  if (!desired || desired.spec.replicas !== 0 || !writer.publishedSpec || writer.publishedSpec.replicas !== 0) throw new Error(failure);
  const expectedSpec = writer.publishedSpec;
  let live: ObjectMap = await read(run, "Deployment", id.name, before.metadata.namespace);
  if (reviewed(live).uid !== id.uid || ![0, before.spec.replicas].includes(live.spec.replicas)
    || canonical({ ...live.spec, replicas: 0 }) !== canonical(expectedSpec)) throw new Error(failure);
  if (live.spec.replicas !== before.spec.replicas) await replicas(run, live, before.spec.replicas);
  live = await read(run, "Deployment", id.name, before.metadata.namespace);
  if (reviewed(live).uid !== id.uid || canonical(live.spec) !== canonical({ ...expectedSpec, replicas: before.spec.replicas })) throw new Error(failure);
  writer.restored = true;
  await journal.save(receipt);
}
