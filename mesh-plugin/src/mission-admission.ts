// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { snapshotMissionData } from "./mission-protocol.js";

interface AdmissionAuthority {
  readonly version: 1;
  readonly taskGeneration: number;
  readonly authorizationDigest: string;
}
export type MissionAdmission = AdmissionAuthority & (
  | { readonly state: "idle" }
  | { readonly state: "run"; readonly runNonce: string; readonly objectiveDigest: string }
);

export function missionObjectiveDigest(content: string): string {
  return `sha256:${createHash("sha256").update(content, "utf8").digest("hex")}`;
}

/** Installed controller authority, not authority supplied by an incoming message. */
export function parseMissionAdmission(value: unknown): MissionAdmission | null {
  try {
    if (!value || typeof value !== "object" || Array.isArray(value)) return null;
    const keys = Reflect.ownKeys(value);
    const v = snapshotMissionData(value) as Record<string, unknown>;
    const expected = v.state === "idle"
      ? ["authorizationDigest", "state", "taskGeneration", "version"]
      : ["authorizationDigest", "objectiveDigest", "runNonce", "state", "taskGeneration", "version"];
    const digest = (d: unknown): d is string => typeof d === "string" && /^sha256:[a-f0-9]{64}$/.test(d);
    if (keys.length !== expected.length || Object.keys(v).sort().join(",") !== expected.join(",")
      || v.version !== 1 || !Number.isSafeInteger(v.taskGeneration) || (v.taskGeneration as number) < 1
      || !digest(v.authorizationDigest)) return null;
    if (v.state !== "idle" && (v.state !== "run" || typeof v.runNonce !== "string"
      || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,252}$/.test(v.runNonce) || !digest(v.objectiveDigest))) return null;
    return v as unknown as MissionAdmission;
  } catch { return null; }
}

export function missionAdmissionAllows(admission: MissionAdmission, runNonce: string, content?: string): boolean {
  return admission.state === "run" && admission.runNonce === runNonce
    && (content === undefined || admission.objectiveDigest === missionObjectiveDigest(content));
}
