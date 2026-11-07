// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";

export interface TaskPhaseEvidence {
  name: string;
  attemptedToolCalls: number;
  successfulToolCalls: number;
  minToolCalls: number;
  maxToolCalls: number;
  completedRequiredToolCalls?: number;
}

export interface RequiredFetchCall {
  name: "http_fetch";
  arguments: { method: "GET"; url: string };
}

export interface FilesystemPhase {
  name: string;
  objective: string;
  capabilities: string[];
  requiredToolCalls: RequiredFetchCall[];
  minToolCalls: number;
  maxToolCalls: number;
  freshContext: boolean;
  executionMode?: "native";
}

export function nativeMissionLimits(phase: FilesystemPhase | undefined): { rounds: number; timeoutMs: number; events: number } | undefined {
  if (phase?.executionMode !== "native") return undefined;
  const rounds = Math.max(8, phase.maxToolCalls + 2);
  return {
    rounds,
    // Existing inference (630s) and peer-wait (300s) deadlines, capped only by the JS timer representation.
    timeoutMs: Math.min(2_147_483_647, rounds * 630_000 + phase.maxToolCalls * 300_000),
    events: Math.max(128, 2 * rounds + 3 * phase.maxToolCalls + 2),
  };
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

/** Conservative exact public URL syntax; the router still owns egress authorization. */
export function reviewedFetchUrl(value: unknown, allowOrigin = false): value is string {
  if (typeof value !== "string" || value.length > 2048 || !value.startsWith("https://")) return false;
  const slash = value.indexOf("/", 8);
  if (slash < 0 && !allowOrigin) return false;
  const host = slash < 0 ? value.slice(8) : value.slice(8, slash), path = slash < 0 ? "" : value.slice(slash);
  const labels = host.split(".");
  return host.length <= 253 && labels.length >= 2
    && labels.every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))
    && /^[a-z]{2,63}$/.test(labels.at(-1)!)
    && !["local", "internal", "localhost"].includes(labels.at(-1)!)
    && /^[A-Za-z0-9/_~.!$&'()*+,;=:@?%-]*$/.test(path)
    && !/%(?![0-9a-fA-F]{2})|%2e/i.test(path)
    && !path.split("?")[0].split("/").some(part => part === "." || part === "..");
}

export function reviewedFetchArguments(phase: FilesystemPhase, input: unknown): boolean {
  const args = record(input, ["method", "url"]);
  return !!args && args.method === "GET" && phase.capabilities.includes("network")
    && (phase.executionMode === "native"
      ? reviewedFetchUrl(args.url, true)
      : phase.requiredToolCalls.some(call => call.arguments.url === args.url));
}

/** Canonical supported subset, shared by negotiation and execution; not a full-plan authorization. */
export function filesystemPhase(input: unknown): FilesystemPhase {
  const keys = ["name", "objective", "capabilities", "requiredToolCalls", "minToolCalls", "maxToolCalls", "freshContext", "executionMode"];
  const value = record(input, keys);
  if (!value) throw new Error("Invalid reviewed phase object");
  if ("executionMode" in value && value.executionMode !== "native") throw new Error("Unsupported phase execution mode");
  const { name, objective, maxToolCalls } = value;
  const capabilities = array(value.capabilities === undefined ? [] : value.capabilities, value.executionMode === "native" ? 6 : 3);
  const required = array(value.requiredToolCalls === undefined ? [] : value.requiredToolCalls, 8);
  const minToolCalls = value.minToolCalls === undefined ? 0 : value.minToolCalls;
  const freshContext = value.freshContext === undefined ? false : value.freshContext;
  if (typeof name !== "string" || !/^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/.test(name)
    || typeof objective !== "string" || Buffer.byteLength(objective.trim()) < 20 || Buffer.byteLength(objective) > 1200
    || !Number.isSafeInteger(minToolCalls) || !Number.isSafeInteger(maxToolCalls)
    || (minToolCalls as number) < 0
    || (maxToolCalls as number) > (value.executionMode === "native" ? 2_147_483_647 : 32)
    || (maxToolCalls as number) < (minToolCalls as number)
    || typeof freshContext !== "boolean") {
    throw new Error("Invalid reviewed phase name, objective, or call bounds");
  }
  const supported = ["filesystem-read", "filesystem-write", "network", ...(value.executionMode === "native" ? ["memory", "github-pr", "python-execute"] : [])];
  if (!capabilities || capabilities.some(capability => !supported.includes(capability as string))
    || new Set(capabilities).size !== capabilities.length) {
    throw new Error("Unsupported reviewed phase capabilities: filesystem-read, filesystem-write and network are executable; governed memory, github-pr and python-execute additionally require native mode");
  }
  if (capabilities.includes("python-execute") && !["filesystem-read", "filesystem-write", "network"].every(cap => capabilities.includes(cap))) {
    throw new Error("Native python-execute uses the existing agent identity and requires explicit filesystem-read, filesystem-write and network capabilities");
  }
  if (!required || required.length > (maxToolCalls as number)) throw new Error("Invalid required tool-call contracts");
  const requiredToolCalls: RequiredFetchCall[] = required.map(input => {
    const call = record(input, ["name", "arguments"]);
    const args = call && record(call.arguments, ["method", "url"]);
    if (call?.name !== "http_fetch" || !args || args.method !== "GET" || !reviewedFetchUrl(args.url)) {
      throw new Error("Required tool-call contracts support only exact public HTTPS http_fetch GET; hosted web-search is outside the finite funding contract");
    }
    return { name: "http_fetch", arguments: { method: "GET", url: args.url } };
  });
  if ((requiredToolCalls.length > 0 && !capabilities.includes("network"))
    || (value.executionMode !== "native" && capabilities.includes("network") && requiredToolCalls.length === 0)
    || new Set(requiredToolCalls.map(call => call.arguments.url)).size !== requiredToolCalls.length) {
    throw new Error("Network requires unique reviewed http_fetch URLs and the network capability");
  }
  if ((minToolCalls as number) > 0 && capabilities.length === 0) {
    throw new Error("Reviewed phase requires successful calls but grants no supported tools");
  }
  return { name, objective, capabilities: (capabilities as string[]).sort(), requiredToolCalls, minToolCalls: minToolCalls as number, maxToolCalls: maxToolCalls as number, freshContext,
    ...(value.executionMode === "native" ? { executionMode: "native" as const } : {}) };
}

export function missionPhaseDigest(input: unknown): string {
  return `sha256:${createHash("sha256").update(JSON.stringify(filesystemPhase(input))).digest("hex")}`;
}

export function validPhaseEvidence(value: unknown, phase: FilesystemPhase, succeeded: boolean): value is TaskPhaseEvidence {
  try {
    const evidence = record(value, ["name", "attemptedToolCalls", "successfulToolCalls", "minToolCalls", "maxToolCalls", "completedRequiredToolCalls"]);
    if (!evidence) return false;
    const completed = evidence.completedRequiredToolCalls === undefined ? 0 : evidence.completedRequiredToolCalls;
    if (!Number.isSafeInteger(completed) || (completed as number) < 0
      || (completed as number) > phase.requiredToolCalls.length
      || (completed as number) > (evidence.successfulToolCalls as number)
      || (succeeded && completed !== phase.requiredToolCalls.length)) return false;
    return evidence.name === phase.name && evidence.minToolCalls === phase.minToolCalls && evidence.maxToolCalls === phase.maxToolCalls
      && [evidence.attemptedToolCalls, evidence.successfulToolCalls].every(n => Number.isSafeInteger(n) && (n as number) >= 0)
      && (evidence.successfulToolCalls as number) <= (evidence.attemptedToolCalls as number)
      && (evidence.attemptedToolCalls as number) <= phase.maxToolCalls
      && (!succeeded || (evidence.successfulToolCalls as number) >= phase.minToolCalls);
  } catch { return false; }
}
