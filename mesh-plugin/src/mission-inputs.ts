// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";

export const MAX_MISSION_INPUT_BYTES = 64 * 1024;
export const MAX_MISSION_INPUTS = 16;
export interface MissionInputSource {
  namespace: string;
  taskName: string;
  taskUid: string;
  sandboxUid: string;
  podUid: string;
  runNonce: string;
  assignmentId: string;
  agentDid: string;
  artifactName: string;
}
export interface MissionInputReference {
  name: string;
  source: MissionInputSource;
  sha256: string;
}
export interface MissionInput extends MissionInputReference {
  content: string;
}
const digest = (text: string): string => `sha256:${createHash("sha256").update(text, "utf8").digest("hex")}`;
export const missionInputContentDigest = digest;
export function validMissionInputName(value: unknown): value is string {
  return typeof value === "string" && /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}$/.test(value)
    && !["__proto__", "constructor", "prototype"].includes(value);
}
function fields(value: unknown, expected: string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) throw new Error("Mission input requires plain records");
  const keys = Reflect.ownKeys(value);
  if (keys.length !== expected.length || !keys.every(key => typeof key === "string" && expected.includes(key))) {
    throw new Error("Mission input fields do not match the contract");
  }
  const result: Record<string, unknown> = Object.create(null);
  for (const key of expected) {
    const field = Object.getOwnPropertyDescriptor(value, key);
    if (!field?.enumerable || !("value" in field)) throw new Error("Mission input requires enumerable data fields");
    result[key] = field.value;
  }
  return result;
}

function canonicalInputs(value: unknown, withContent: true): readonly MissionInput[];
function canonicalInputs(value: unknown, withContent: false): readonly MissionInputReference[];
function canonicalInputs(value: unknown, withContent: boolean): readonly MissionInputReference[] {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype
    || value.length < 1 || value.length > MAX_MISSION_INPUTS || Reflect.ownKeys(value).length !== value.length + 1) {
    throw new Error("Mission inputs require a bounded, nonempty dense array");
  }
  const result: MissionInputReference[] = [];
  const names = new Set<string>();
  let bytes = 0;
  for (let index = 0; index < value.length; index++) {
    const field = Object.getOwnPropertyDescriptor(value, String(index));
    if (!field?.enumerable || !("value" in field)) throw new Error("Mission inputs require data array entries");
    const item = fields(field.value, withContent ? ["name", "source", "sha256", "content"] : ["name", "source", "sha256"]);
    if (!validMissionInputName(item.name) || names.has(item.name)) throw new Error("Mission input names must be unique safe filenames");
    names.add(item.name);
    if (typeof item.sha256 !== "string" || !/^sha256:[a-f0-9]{64}$/.test(item.sha256)) throw new Error("Invalid mission input digest");
    if (withContent && (typeof item.content !== "string" || !item.content.trim()
      || Buffer.byteLength(item.content) > MAX_MISSION_INPUT_BYTES || Buffer.from(item.content).toString("utf8") !== item.content
      || item.sha256 !== digest(item.content))) throw new Error("Mission input content must match its exact UTF8 digest");
    const source = fields(item.source, ["namespace", "taskName", "taskUid", "sandboxUid", "podUid", "runNonce", "assignmentId", "agentDid", "artifactName"]);
    if (typeof source.namespace !== "string" || !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(source.namespace)
      || ![source.taskName, source.taskUid, source.sandboxUid, source.podUid, source.runNonce, source.assignmentId].every(
        id => typeof id === "string" && /^[a-zA-Z0-9][a-zA-Z0-9._:-]{0,252}$/.test(id))
      || typeof source.agentDid !== "string" || !/^did:mesh:[a-f0-9]{32}$/.test(source.agentDid)
      || !validMissionInputName(source.artifactName)) throw new Error("Invalid mission input producer identity");
    const copied = Object.freeze({ name: item.name, source: Object.freeze(source) as unknown as MissionInputSource, sha256: item.sha256,
      ...(withContent ? { content: item.content as string } : {}) });
    bytes += Buffer.byteLength(JSON.stringify(copied));
    if (bytes + result.length + 2 > MAX_MISSION_INPUT_BYTES) throw new Error("Mission inputs exceed serialized byte limit");
    result.push(copied);
  }
  return Object.freeze(result.sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

/** Content and provenance are immutable data, not proof of source authority. The installer must verify custody. */
export function missionInputs(value: unknown): readonly MissionInput[] {
  return canonicalInputs(value, true);
}

/** Content-free pins for a trusted reader; caller-provided bytes are never accepted. */
export function missionInputReferences(value: unknown): readonly MissionInputReference[] {
  return canonicalInputs(value, false);
}

export function missionInputsDigest(value: unknown): string {
  return digest(JSON.stringify({ schema: "kars.mission-inputs/v1", inputs: missionInputs(value) }));
}
