// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { filesystemPhase, missionPhaseDigest, validPhaseEvidence, type FilesystemPhase, type TaskPhaseEvidence } from "./mission-phase.js";

export const MISSION_PROTOCOL_VERSION = 1;
export const PHASE_MISSION_PROTOCOL_VERSION = 2;
export const MAX_MISSION_MESSAGE_BYTES = 192 * 1024;
export const MAX_MISSION_ARTIFACT_BYTES = 128 * 1024;
export const MAX_MISSION_ARTIFACTS = 16;
export type MissionArtifacts = Record<string, string>;

export function validMissionArtifacts(value: unknown): value is MissionArtifacts {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  try {
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) return false;
    const keys = Reflect.ownKeys(value);
    const descriptors = Object.getOwnPropertyDescriptors(value);
    return keys.length <= MAX_MISSION_ARTIFACTS && keys.every(name => {
      if (typeof name !== "string" || !/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}$/.test(name)
        || ["response.md", "__proto__", "constructor", "prototype"].includes(name)) return false;
      const descriptor = descriptors[name];
      return descriptor.enumerable && "value" in descriptor && typeof descriptor.value === "string" && !!descriptor.value.trim();
    }) && Buffer.byteLength(JSON.stringify(value), "utf8") <= MAX_MISSION_ARTIFACT_BYTES;
  } catch { return false; }
}

export interface MissionTarget {
  taskName: string;
  taskUid: string;
  sandboxUid: string;
  podUid: string;
  runNonce: string;
  agentDid: string;
  dispatcherDid: string;
}
export interface MissionContract {
  version: 1 | 2;
  reviewedPhase?: FilesystemPhase;
  phaseDigest?: string;
}
export interface MissionProbe extends MissionTarget, MissionContract {
  type: "mission:probe";
  challenge: string;
}
export interface MissionAssignment extends MissionTarget, MissionContract {
  type: "mission:assign";
  bootId: string;
  assignmentId: string;
  artifactFormat?: "text-v1";
  content: string;
}
export interface MissionReply extends MissionTarget, MissionContract {
  type: "mission:reply";
  bootId: string;
  challenge?: string;
  assignmentId?: string;
  status: "ready" | "accepted" | "running" | "succeeded" | "failed" | "rejected";
  output?: string;
  artifacts?: MissionArtifacts;
  artifactFormat?: "text-v1";
  error?: string;
  evidence?: {
    model: string;
    rounds: number;
    usage: { promptTokens: number; completionTokens: number; totalTokens: number } | null;
    phase?: TaskPhaseEvidence;
  };
}
export type MissionMessage = MissionProbe | MissionAssignment | MissionReply;
const id = (v: unknown): v is string => typeof v === "string" && /^[a-zA-Z0-9][a-zA-Z0-9._:-]{0,252}$/.test(v);
const did = (v: unknown): v is string => typeof v === "string" && /^did:mesh:[a-f0-9]{32}$/.test(v);
export function isMissionMessage(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  try {
    const type = Object.getOwnPropertyDescriptor(value, "type");
    // Malformed accessor envelopes must not fall through to the legacy task handler.
    return !!type && (!("value" in type) || (typeof type.value === "string" && type.value.startsWith("mission:")));
  } catch { return true; }
}

/** Bounded, immutable plain-data snapshot before async authorization or transport. */
export function snapshotMissionData(value: unknown): unknown {
  let nodes = 0;
  let stringBytes = 0;
  const copy = (input: unknown, depth: number): unknown => {
    if (++nodes > 16384 || depth > 16) throw new Error("Mission data exceeds structural bounds");
    if (input === null || typeof input === "boolean") return input;
    if (typeof input === "string") {
      stringBytes += Buffer.byteLength(input);
      if (stringBytes > MAX_MISSION_MESSAGE_BYTES) throw new Error("Mission data exceeds wire bounds");
      return input;
    }
    if (typeof input === "number" && Number.isFinite(input)) return input;
    if (!input || typeof input !== "object") throw new Error("Mission requires plain data");
    const isArray = Array.isArray(input);
    const prototype = Object.getPrototypeOf(input);
    if (isArray ? prototype !== Array.prototype : prototype !== Object.prototype && prototype !== null) {
      throw new Error("Mission requires plain objects and arrays");
    }
    const descriptors = Object.getOwnPropertyDescriptors(input as object);
    const keys = Reflect.ownKeys(descriptors);
    if (keys.length > 16384) throw new Error("Mission data exceeds structural bounds");
    if (isArray) {
      const length = descriptors.length.value;
      if (!Number.isSafeInteger(length) || length < 0 || length > 16384 || keys.length !== length + 1) {
        throw new Error("Mission requires dense arrays");
      }
      const result: unknown[] = [];
      for (let index = 0; index < length; index++) {
        const entry = descriptors[String(index)];
        if (!entry?.enumerable || !("value" in entry)) throw new Error("Mission requires data array entries");
        result.push(copy(entry.value, depth + 1));
      }
      return Object.freeze(result);
    }
    const result: Record<string, unknown> = Object.create(null);
    for (const key of keys) {
      if (typeof key !== "string") throw new Error("Mission requires string keys");
      const entry = descriptors[key];
      if (!entry.enumerable || !("value" in entry)) throw new Error("Mission requires enumerable data fields");
      stringBytes += Buffer.byteLength(key);
      if (stringBytes > MAX_MISSION_MESSAGE_BYTES) throw new Error("Mission data exceeds wire bounds");
      if (entry.value !== undefined) result[key] = copy(entry.value, depth + 1);
    }
    return Object.freeze(result);
  };
  const result = copy(value, 0);
  if (Buffer.byteLength(JSON.stringify(result)) > MAX_MISSION_MESSAGE_BYTES) throw new Error("Mission data exceeds wire bounds");
  return result;
}

export function missionContract(reviewedPhase?: unknown): MissionContract {
  if (reviewedPhase === undefined) return Object.freeze({ version: MISSION_PROTOCOL_VERSION });
  const phase = filesystemPhase(reviewedPhase);
  Object.freeze(phase.capabilities);
  Object.freeze(phase.requiredToolCalls);
  return Object.freeze({ version: PHASE_MISSION_PROTOCOL_VERSION, reviewedPhase: Object.freeze(phase), phaseDigest: missionPhaseDigest(phase) });
}

/** Validate the installed contract independently of a peer's message. No envelope fields are allowed. */
export function parseMissionContract(value: unknown): MissionContract | null {
  try {
    if (!value || typeof value !== "object" || Array.isArray(value)) return null;
    const keys = Reflect.ownKeys(value);
    const v = snapshotMissionData(value) as Record<string, unknown>;
    if (v.version === MISSION_PROTOCOL_VERSION) {
      return keys.length === 1 && keys[0] === "version" ? missionContract() : null;
    }
    if (v.version !== PHASE_MISSION_PROTOCOL_VERSION || keys.length !== 3
      || !keys.every(key => ["version", "reviewedPhase", "phaseDigest"].includes(String(key)))) return null;
    const contract = missionContract(v.reviewedPhase);
    return contract.version === PHASE_MISSION_PROTOCOL_VERSION && v.phaseDigest === contract.phaseDigest ? contract : null;
  } catch { return null; }
}

export function sameMissionContract(a: MissionContract, b: MissionContract): boolean {
  return a.version === b.version && a.phaseDigest === b.phaseDigest;
}

/** A constrained execution cannot erase prior counts or make unknown consumed usage known. */
export function missionEvidenceAdvances(previous: MissionReply["evidence"], next: MissionReply["evidence"]): boolean {
  if (!previous) return true;
  if (!next || next.model !== previous.model || next.rounds < previous.rounds) return false;
  if (previous.phase && (!next.phase || next.phase.attemptedToolCalls < previous.phase.attemptedToolCalls
    || next.phase.successfulToolCalls < previous.phase.successfulToolCalls)) return false;
  if (previous.rounds > 0 && previous.usage === null && next.usage !== null) return false;
  return !previous.usage || !next.usage || (next.usage.promptTokens >= previous.usage.promptTokens
    && next.usage.completionTokens >= previous.usage.completionTokens && next.usage.totalTokens >= previous.usage.totalTokens);
}

/** Reserved mission messages are never interpreted by the legacy task handler. */
export function parseMissionMessage(value: unknown): MissionMessage | null {
  if (!isMissionMessage(value)) return null;
  try { value = snapshotMissionData(value); } catch { return null; }
  const v = value as Record<string, unknown>;
  if (v.version === MISSION_PROTOCOL_VERSION) {
    if (v.reviewedPhase !== undefined || v.phaseDigest !== undefined) return null;
  } else if (v.version === PHASE_MISSION_PROTOCOL_VERSION) {
    try {
      const contract = missionContract(v.reviewedPhase);
      if (contract.version !== PHASE_MISSION_PROTOCOL_VERSION || v.phaseDigest !== contract.phaseDigest) return null;
      value = Object.freeze({ ...v, ...contract });
      if (Buffer.byteLength(JSON.stringify(value)) > MAX_MISSION_MESSAGE_BYTES) return null;
    } catch { return null; }
  } else return null;
  if (v.artifactFormat !== undefined && (v.artifactFormat !== "text-v1" || !["mission:assign", "mission:reply"].includes(String(v.type)))) return null;
  if (v.artifacts !== undefined && (v.type !== "mission:reply" || v.status !== "succeeded" || v.artifactFormat !== "text-v1" || !validMissionArtifacts(v.artifacts))) return null;
  if (![v.taskName, v.taskUid, v.sandboxUid, v.podUid, v.runNonce].every(id)
    || !did(v.agentDid) || !did(v.dispatcherDid)) return null;
  if (v.evidence !== undefined && (v.type !== "mission:reply" || v.status === "ready")) return null;
  if (v.type === "mission:probe") return id(v.challenge) ? value as MissionProbe : null;
  if (!id(v.bootId)) return null;
  if (v.type === "mission:assign") return id(v.assignmentId) && typeof v.content === "string" && !!v.content.trim()
    ? value as MissionAssignment : null;
  if (v.type !== "mission:reply") return null;
  if (v.status === "ready") return id(v.challenge) ? value as MissionReply : null;
  if (!id(v.assignmentId) || !["accepted", "running", "succeeded", "failed", "rejected"].includes(String(v.status))) return null;
  if (v.error !== undefined && typeof v.error !== "string") return null;
  if (v.evidence !== undefined) {
    const e = v.evidence as MissionReply["evidence"];
    if (!e || typeof e.model !== "string" || !e.model || Buffer.byteLength(e.model) > 253
      || !Number.isSafeInteger(e.rounds) || e.rounds < 0
      || Object.keys(e).some(key => !["model", "rounds", "usage", "phase"].includes(key))) return null;
    if (v.version === MISSION_PROTOCOL_VERSION ? e.phase !== undefined
      : !validPhaseEvidence(e.phase, (value as MissionContract).reviewedPhase!, v.status === "succeeded")) return null;
    if (e.usage !== null) {
      const u = e.usage;
      if (!u || ![u.promptTokens, u.completionTokens, u.totalTokens].every(n => Number.isSafeInteger(n) && n >= 0)
        || Object.keys(u).some(key => !["promptTokens", "completionTokens", "totalTokens"].includes(key))
        || u.promptTokens + u.completionTokens !== u.totalTokens) return null;
    }
  }
  if (v.version === PHASE_MISSION_PROTOCOL_VERSION && v.status === "running" && !v.evidence) return null;
  if (v.status === "succeeded") {
    const e = v.evidence as MissionReply["evidence"];
    if (typeof v.output !== "string" || !v.output.trim() || !e?.usage || e.usage.totalTokens <= 0 || e.rounds <= 0) return null;
  }
  return value as MissionReply;
}

export function sameMissionTarget(a: MissionTarget, b: MissionTarget): boolean {
  return a.taskName === b.taskName && a.taskUid === b.taskUid && a.sandboxUid === b.sandboxUid
    && a.podUid === b.podUid && a.runNonce === b.runNonce && a.agentDid === b.agentDid && a.dispatcherDid === b.dispatcherDid;
}
