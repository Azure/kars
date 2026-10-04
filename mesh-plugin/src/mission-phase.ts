// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";

export interface TaskPhaseEvidence {
  name: string;
  attemptedToolCalls: number;
  successfulToolCalls: number;
  minToolCalls: number;
  maxToolCalls: number;
}

export interface FilesystemPhase {
  name: string;
  objective: string;
  capabilities: string[];
  requiredToolCalls: never[];
  minToolCalls: number;
  maxToolCalls: number;
  freshContext: boolean;
}

function record(value: unknown, keys: string[]): Record<string, unknown> | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)
    || ![Object.prototype, null].includes(Object.getPrototypeOf(value))) return null;
  const descriptors = Object.getOwnPropertyDescriptors(value);
  const snapshot: Record<string, unknown> = Object.create(null);
  for (const key of Reflect.ownKeys(descriptors)) {
    if (typeof key !== "string" || !keys.includes(key)) return null;
    const descriptor = descriptors[key];
    if (!descriptor.enumerable || !("value" in descriptor)) return null;
    snapshot[key] = descriptor.value;
  }
  return snapshot;
}

function array(value: unknown, maximum: number): unknown[] | null {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return null;
  const descriptors = Object.getOwnPropertyDescriptors(value as object);
  const length = descriptors.length.value;
  if (!Number.isSafeInteger(length) || length < 0 || length > maximum
    || Reflect.ownKeys(descriptors).length !== length + 1) return null;
  const snapshot: unknown[] = [];
  for (let index = 0; index < length; index++) {
    const descriptor = descriptors[String(index)];
    if (!descriptor?.enumerable || !("value" in descriptor)) return null;
    snapshot.push(descriptor.value);
  }
  return snapshot;
}

/** Canonical supported subset, shared by negotiation and execution; not a full-plan authorization. */
export function filesystemPhase(input: unknown): FilesystemPhase {
  const keys = ["name", "objective", "capabilities", "requiredToolCalls", "minToolCalls", "maxToolCalls", "freshContext"];
  const value = record(input, keys);
  if (!value) throw new Error("Invalid reviewed phase object");
  const { name, objective, maxToolCalls } = value;
  const capabilities = array(value.capabilities === undefined ? [] : value.capabilities, 2);
  const requiredToolCalls = array(value.requiredToolCalls === undefined ? [] : value.requiredToolCalls, 0);
  const minToolCalls = value.minToolCalls === undefined ? 0 : value.minToolCalls;
  const freshContext = value.freshContext === undefined ? false : value.freshContext;
  if (typeof name !== "string" || !/^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/.test(name)
    || typeof objective !== "string" || Buffer.byteLength(objective.trim()) < 20 || Buffer.byteLength(objective) > 1200
    || !Number.isSafeInteger(minToolCalls) || !Number.isSafeInteger(maxToolCalls)
    || (minToolCalls as number) < 0 || (maxToolCalls as number) > 32 || (maxToolCalls as number) < (minToolCalls as number)
    || typeof freshContext !== "boolean") {
    throw new Error("Invalid reviewed phase name, objective, or call bounds");
  }
  if (!capabilities || capabilities.some(capability => capability !== "filesystem-read" && capability !== "filesystem-write")
    || new Set(capabilities).size !== capabilities.length) {
    throw new Error("Unsupported reviewed phase capabilities: only filesystem-read and filesystem-write are executable");
  }
  if (!Array.isArray(requiredToolCalls) || requiredToolCalls.length !== 0) {
    throw new Error("Required tool-call contracts are not supported by the filesystem phase executor");
  }
  if ((minToolCalls as number) > 0 && capabilities.length === 0) {
    throw new Error("Reviewed phase requires successful calls but grants no supported tools");
  }
  return { name, objective, capabilities: (capabilities as string[]).sort(), requiredToolCalls: [], minToolCalls: minToolCalls as number, maxToolCalls: maxToolCalls as number, freshContext };
}

export function missionPhaseDigest(input: unknown): string {
  return `sha256:${createHash("sha256").update(JSON.stringify(filesystemPhase(input))).digest("hex")}`;
}

export function validPhaseEvidence(value: unknown, phase: FilesystemPhase, succeeded: boolean): value is TaskPhaseEvidence {
  try {
    const evidence = record(value, ["name", "attemptedToolCalls", "successfulToolCalls", "minToolCalls", "maxToolCalls"]);
    if (!evidence) return false;
    return evidence.name === phase.name && evidence.minToolCalls === phase.minToolCalls && evidence.maxToolCalls === phase.maxToolCalls
      && [evidence.attemptedToolCalls, evidence.successfulToolCalls].every(n => Number.isSafeInteger(n) && (n as number) >= 0)
      && (evidence.successfulToolCalls as number) <= (evidence.attemptedToolCalls as number)
      && (evidence.attemptedToolCalls as number) <= phase.maxToolCalls
      && (!succeeded || (evidence.successfulToolCalls as number) >= phase.minToolCalls);
  } catch { return false; }
}
