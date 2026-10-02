// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export const MISSION_PROTOCOL_VERSION = 1;
export const MAX_MISSION_MESSAGE_BYTES = 192 * 1024;

export interface MissionTarget {
  taskName: string;
  taskUid: string;
  sandboxUid: string;
  podUid: string;
  runNonce: string;
  agentDid: string;
  dispatcherDid: string;
}
export interface MissionProbe extends MissionTarget {
  type: "mission:probe";
  version: 1;
  challenge: string;
}
export interface MissionAssignment extends MissionTarget {
  type: "mission:assign";
  version: 1;
  bootId: string;
  assignmentId: string;
  content: string;
}
export interface MissionReply extends MissionTarget {
  type: "mission:reply";
  version: 1;
  bootId: string;
  challenge?: string;
  assignmentId?: string;
  status: "ready" | "accepted" | "running" | "succeeded" | "failed" | "rejected";
  output?: string;
  error?: string;
  evidence?: {
    model: string;
    rounds: number;
    usage: { promptTokens: number; completionTokens: number; totalTokens: number } | null;
  };
}
export type MissionMessage = MissionProbe | MissionAssignment | MissionReply;
const id = (v: unknown): v is string => typeof v === "string" && /^[a-zA-Z0-9][a-zA-Z0-9._:-]{0,252}$/.test(v);
const did = (v: unknown): v is string => typeof v === "string" && /^did:mesh:[a-f0-9]{32}$/.test(v);
export const isMissionMessage = (v: unknown): boolean => !!v && typeof v === "object"
  && typeof (v as { type?: unknown }).type === "string" && (v as { type: string }).type.startsWith("mission:");

/** Reserved mission messages are never interpreted by the legacy task handler. */
export function parseMissionMessage(value: unknown): MissionMessage | null {
  if (!isMissionMessage(value)) return null;
  let size: number;
  try { size = Buffer.byteLength(JSON.stringify(value)); } catch { return null; }
  if (size > MAX_MISSION_MESSAGE_BYTES) return null;
  const v = value as Record<string, unknown>;
  if (v.version !== 1 || ![v.taskName, v.taskUid, v.sandboxUid, v.podUid, v.runNonce].every(id)
    || !did(v.agentDid) || !did(v.dispatcherDid)) return null;
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
    if (!e || typeof e.model !== "string" || !e.model || !Number.isSafeInteger(e.rounds) || e.rounds < 0) return null;
    if (e.usage !== null) {
      const u = e.usage;
      if (!u || ![u.promptTokens, u.completionTokens, u.totalTokens].every(n => Number.isSafeInteger(n) && n >= 0)
        || u.promptTokens + u.completionTokens !== u.totalTokens) return null;
    }
  }
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
