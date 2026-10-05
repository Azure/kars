// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual } from "node:util";
import { missionAttemptName } from "./mission-dispatcher.js";
import { missionInputReferences, missionInputs, type MissionInput, type MissionInputSource } from "./mission-inputs.js";
import { missionResultMaps, unpackMissionAttempt, type MissionConfigMap } from "./mission-record.js";
import type { MissionMetadata } from "./mission-workload.js";

export interface MissionInputCustodyReader {
  task(source: Readonly<MissionInputSource>): Promise<unknown>;
  configMap(namespace: string, name: string): Promise<unknown>;
}
interface Resource { apiVersion: string; kind: string; metadata: MissionMetadata }
function resource(value: unknown, apiVersion: string, kind: string, namespace: string, name: string): Resource {
  if (!value || typeof value !== "object") throw new Error("Missing upstream custody resource");
  const r = value as Resource;
  if (r.apiVersion !== apiVersion || r.kind !== kind || r.metadata?.namespace !== namespace || r.metadata.name !== name
    || typeof r.metadata.uid !== "string" || !r.metadata.uid
    || typeof r.metadata.resourceVersion !== "string" || !r.metadata.resourceVersion || r.metadata.deletionTimestamp) {
    throw new Error("Upstream custody resource identity mismatch");
  }
  return r;
}
function configMap(value: unknown, namespace: string, name: string): MissionConfigMap {
  const cm = resource(value, "v1", "ConfigMap", namespace, name) as MissionConfigMap;
  if (!cm.data || typeof cm.data !== "object" || Array.isArray(cm.data)
    || !Object.values(cm.data).every(text => typeof text === "string")
    || Buffer.byteLength(JSON.stringify(cm.data)) > 1024 * 1024) throw new Error("Invalid upstream custody data");
  return cm;
}
function canonical(actual: MissionConfigMap, expected: MissionConfigMap): void {
  if (actual.immutable !== true || !isDeepStrictEqual(actual.data, expected.data)
    || !isDeepStrictEqual(actual.metadata.ownerReferences, expected.metadata.ownerReferences)
    || !isDeepStrictEqual(actual.metadata.annotations, expected.metadata.annotations)
    || !isDeepStrictEqual(actual.metadata.labels, expected.metadata.labels)) throw new Error("Upstream canonical evidence mismatch");
}

/** Retained result custody only. This does not authorize a Team dependency, execution, or review. */
export async function readRetainedMissionInputs(value: unknown, reader: MissionInputCustodyReader): Promise<readonly MissionInput[]> {
  const references = missionInputReferences(value);
  for (const { source } of references) {
    if (!/^[a-z0-9][a-z0-9.-]{0,252}$/.test(source.taskName)
      || ["response.md", "kars-router-observations.json"].includes(source.artifactName)) {
      throw new Error("Upstream input must select a named useful document");
    }
  }
  const reads = new Map<string, { snapshot: unknown; read: () => Promise<unknown> }>();
  const capture = async (key: string[], read: () => Promise<unknown>): Promise<unknown> => {
    const id = JSON.stringify(key);
    const prior = reads.get(id);
    if (prior) return prior.snapshot;
    const snapshot: unknown = structuredClone(await read());
    reads.set(id, { snapshot, read });
    return snapshot;
  };
  const map = async (namespace: string, name: string): Promise<MissionConfigMap> => configMap(
    await capture(["ConfigMap", namespace, name], () => reader.configMap(namespace, name)), namespace, name);
  const inputs: MissionInput[] = [];
  for (const reference of references) {
    const { source } = reference;
    const task = resource(await capture(["KarsTask", source.namespace, source.taskName], () => reader.task(source)),
      "kars.azure.com/v1alpha1", "KarsTask", source.namespace, source.taskName);
    if (task.metadata.uid !== source.taskUid) throw new Error("Upstream Task was replaced");
    const record = await map(source.namespace, missionAttemptName(source));
    const { attempt } = unpackMissionAttempt(record, source);
    const { candidate, assignment, reply } = attempt;
    if (attempt.phase !== "succeeded" || reply?.status !== "succeeded" || assignment.artifactFormat !== "text-v1"
      || reply.artifactFormat !== "text-v1" || assignment.assignmentId !== source.assignmentId
      || candidate.sandboxUid !== source.sandboxUid || candidate.podUid !== source.podUid || candidate.agentDid !== source.agentDid
      || !reply.artifacts || !Object.hasOwn(reply.artifacts, source.artifactName)) {
      throw new Error("Upstream document does not belong to the pinned successful assignment");
    }
    const expected = missionResultMaps(attempt);
    const output = await map(source.namespace, expected.output.metadata.name);
    const artifacts = await map(source.namespace, expected.artifacts.metadata.name);
    canonical(output, expected.output);
    canonical(artifacts, expected.artifacts);
    inputs.push({ ...reference, content: artifacts.data[source.artifactName]! });
  }
  const result = missionInputs(inputs);
  // Fence every captured incarnation after all inputs are read; this is not a cross-resource transaction.
  for (const { snapshot, read } of [...reads.values()].reverse()) {
    if (!isDeepStrictEqual(snapshot, await read())) throw new Error("Upstream custody changed during input selection");
  }
  return result;
}
