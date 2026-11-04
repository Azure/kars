// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { filesystemPhase, missionPhaseDigest, validPhaseEvidence, type FilesystemPhase, type TaskPhaseEvidence } from "./mission-phase.js";
import { MAX_MISSION_INPUT_BYTES, missionInputs, missionInputsDigest, type MissionInput } from "./mission-inputs.js";

export const MISSION_PROTOCOL_VERSION = 1;
export const PHASE_MISSION_PROTOCOL_VERSION = 2;
export const INPUT_MISSION_PROTOCOL_VERSION = 3;
// Keep ordinary payload budgets unchanged; reserve separate space for bounded Activity.
export const MAX_MISSION_BASE_BYTES = 192 * 1024;
export const MAX_MISSION_MESSAGE_BYTES = MAX_MISSION_BASE_BYTES + 384 * 1024;
export const MAX_MISSION_ARTIFACT_BYTES = 128 * 1024;
export const MAX_ROUTER_OBSERVATION_ROUNDS = 130; // Native 128-tool allowance plus two response rounds.
export const MAX_ROUTER_OBSERVATION_EVENTS = 1024; // Existing router retention, not execution allowance.
export const MAX_ROUTER_OBSERVATION_PAGE_BYTES = 256 * 1024;
export const MAX_MISSION_ARTIFACTS = 16;
export const MISSION_ACTIVITY_FORMAT = "router-observations-v1";
export const MAX_MISSION_ACTIVITY_BYTES = 384 * 1024;
export const ROUTER_OBSERVATIONS_ARTIFACT = "kars-router-observations.json";
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
    }) && Buffer.byteLength(JSON.stringify(Object.fromEntries(Object.entries(value)
      .filter(([name]) => name !== ROUTER_OBSERVATIONS_ARTIFACT))), "utf8") <= MAX_MISSION_ARTIFACT_BYTES
      && (!keys.includes(ROUTER_OBSERVATIONS_ARTIFACT)
        || Buffer.byteLength(JSON.stringify({ [ROUTER_OBSERVATIONS_ARTIFACT]: (value as MissionArtifacts)[ROUTER_OBSERVATIONS_ARTIFACT] })) <= MAX_MISSION_ACTIVITY_BYTES);
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
export interface MissionPeerContext {
  rootUid: string;
  stateName: string;
  slot: string;
  peers: readonly { role: string; phase: string; slot: string; agentName: string }[];
}
export interface MissionContract {
  version: 1 | 2 | 3 | 4 | 5;
  peerContext?: MissionPeerContext;
  reviewedPhase?: FilesystemPhase;
  phaseDigest?: string;
  inputDigest?: string;
  requiredOutputs?: readonly string[];
}
export interface MissionProbe extends MissionTarget, MissionContract {
  type: "mission:probe";
  challenge: string;
  activityFormat?: typeof MISSION_ACTIVITY_FORMAT;
}
export interface MissionAssignment extends MissionTarget, MissionContract {
  type: "mission:assign";
  activityFormat?: typeof MISSION_ACTIVITY_FORMAT;
  bootId: string;
  assignmentId: string;
  artifactFormat?: "text-v1";
  content: string;
  inputArtifacts?: readonly MissionInput[];
}
export interface MissionReply extends MissionTarget, MissionContract {
  type: "mission:reply";
  activityFormat?: typeof MISSION_ACTIVITY_FORMAT;
  routerObservations?: string;
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

export function missionRequiredOutputs(value: unknown): readonly string[] {
  const names = snapshotMissionData(value);
  if (!Array.isArray(names) || names.length > MAX_MISSION_ARTIFACTS || new Set(names).size !== names.length
    || !names.every(name => typeof name === "string" && validMissionArtifacts({ [name]: "required" }))) {
    throw new Error("Invalid required mission output names");
  }
  return Object.freeze([...(names as string[])].sort());
}

export function missionPeerContext(value: unknown): MissionPeerContext {
  const v = snapshotMissionData(value) as MissionPeerContext;
  const exact = (item: unknown, fields: string[]): boolean => !!item && typeof item === "object" && !Array.isArray(item)
    && Object.keys(item).length === fields.length && fields.every(key => Object.hasOwn(item, key));
  const name = (text: unknown): text is string => typeof text === "string" && /^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/.test(text);
  if (!exact(v, ["rootUid", "stateName", "slot", "peers"]) || !id(v.rootUid)
    || typeof v.stateName !== "string" || !/^kars-reviewed-[a-f0-9]{40}$/.test(v.stateName)
    || typeof v.slot !== "string" || !/^(?:r[0-7]-p[0-7]|synthesis)$/.test(v.slot)
    || !Array.isArray(v.peers) || v.peers.length > 64) throw new Error("Invalid reviewed peer context");
  const slots = new Set<string>();
  const phases = new Set<string>();
  const roles = new Map<string, string>();
  const indices = new Map<string, string>();
  for (const peer of v.peers) {
    if (!exact(peer, ["role", "phase", "slot", "agentName"]) || !name(peer.role) || !name(peer.phase)
      || typeof peer.slot !== "string" || !/^r[0-7]-p[0-7]$/.test(peer.slot)
      || peer.agentName !== `rp-${v.stateName.slice(14, 34)}-${peer.slot}` || slots.has(peer.slot)
      || phases.has(`${peer.role}/${peer.phase}`)
      || (roles.has(peer.role) && roles.get(peer.role) !== peer.slot.slice(0, 2))
      || (indices.has(peer.slot.slice(0, 2)) && indices.get(peer.slot.slice(0, 2)) !== peer.role)) {
      throw new Error("Invalid reviewed peer locator");
    }
    slots.add(peer.slot);
    phases.add(`${peer.role}/${peer.phase}`);
    roles.set(peer.role, peer.slot.slice(0, 2));
    indices.set(peer.slot.slice(0, 2), peer.role);
  }
  if (v.slot !== "synthesis" && !slots.has(v.slot)) throw new Error("Reviewed peer context omits its own slot");
  return Object.freeze({ rootUid: v.rootUid, stateName: v.stateName, slot: v.slot,
    peers: Object.freeze([...v.peers].sort((a, b) => a.slot < b.slot ? -1 : a.slot > b.slot ? 1 : 0)
      .map(peer => Object.freeze({ role: peer.role, phase: peer.phase, slot: peer.slot, agentName: peer.agentName }))) });
}

export function missionContract(reviewedPhase?: unknown, inputArtifacts?: unknown, requiredOutputs?: unknown, peerContext?: unknown): MissionContract {
  if (reviewedPhase === undefined) {
    if (inputArtifacts !== undefined || requiredOutputs !== undefined || peerContext !== undefined) throw new Error("Mission inputs, outputs and peers require a reviewed phase");
    return Object.freeze({ version: MISSION_PROTOCOL_VERSION });
  }
  const phase = filesystemPhase(reviewedPhase);
  Object.freeze(phase.capabilities);
  Object.freeze(phase.requiredToolCalls);
  const contract = { reviewedPhase: Object.freeze(phase), phaseDigest: missionPhaseDigest(phase) };
  if (inputArtifacts !== undefined && (!phase.capabilities.includes("filesystem-read") || phase.maxToolCalls < 1)) {
    throw new Error("Mission inputs require a reviewed read-capable phase");
  }
  if (peerContext !== undefined && requiredOutputs === undefined) throw new Error("Mission peers require explicit reviewed outputs");
  if (requiredOutputs !== undefined) return Object.freeze({ version: peerContext === undefined ? 4 : 5, ...contract,
    ...(inputArtifacts !== undefined ? { inputDigest: missionInputsDigest(inputArtifacts) } : {}), requiredOutputs: missionRequiredOutputs(requiredOutputs),
    ...(peerContext !== undefined ? { peerContext: missionPeerContext(peerContext) } : {}) });
  if (inputArtifacts === undefined) return Object.freeze({ version: PHASE_MISSION_PROTOCOL_VERSION, ...contract });
  return Object.freeze({ version: INPUT_MISSION_PROTOCOL_VERSION, ...contract, inputDigest: missionInputsDigest(inputArtifacts) });
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
    const peers = v.version === 5;
    const outputs = v.version === 4 || peers;
    const hasInputs = v.version === INPUT_MISSION_PROTOCOL_VERSION || (outputs && v.inputDigest !== undefined);
    const expected = ["version", "reviewedPhase", "phaseDigest", ...(hasInputs ? ["inputDigest"] : []), ...(outputs ? ["requiredOutputs"] : []), ...(peers ? ["peerContext"] : [])];
    if ((!outputs && !hasInputs && v.version !== PHASE_MISSION_PROTOCOL_VERSION) || keys.length !== expected.length
      || !keys.every(key => typeof key === "string" && expected.includes(key))) return null;
    const contract = missionContract(v.reviewedPhase, undefined, outputs ? v.requiredOutputs : undefined, peers ? v.peerContext : undefined);
    if (contract.version !== (peers ? 5 : outputs ? 4 : PHASE_MISSION_PROTOCOL_VERSION) || v.phaseDigest !== contract.phaseDigest) return null;
    if (!hasInputs) return contract;
    if (typeof v.inputDigest !== "string" || !/^sha256:[a-f0-9]{64}$/.test(v.inputDigest)
      || !contract.reviewedPhase!.capabilities.includes("filesystem-read") || contract.reviewedPhase!.maxToolCalls < 1) return null;
    return Object.freeze({ ...contract, version: peers ? 5 : outputs ? 4 : INPUT_MISSION_PROTOCOL_VERSION, inputDigest: v.inputDigest });
  } catch { return null; }
}

/** v5 adds up to 64 locators; older installed contracts retain their 8-KiB ceiling. */
export function parseMissionContractConfiguration(value: unknown): MissionContract | null {
  try {
    if (typeof value !== "string" || Buffer.byteLength(value, "utf8") > 32768) return null;
    const contract = parseMissionContract(JSON.parse(value));
    return contract && (contract.version === 5 || Buffer.byteLength(value, "utf8") <= 8192) ? contract : null;
  } catch { return null; }
}

export function sameMissionContract(a: MissionContract, b: MissionContract): boolean {
  return a.version === b.version && a.phaseDigest === b.phaseDigest && a.inputDigest === b.inputDigest
    && JSON.stringify(a.requiredOutputs) === JSON.stringify(b.requiredOutputs)
    && JSON.stringify(a.peerContext) === JSON.stringify(b.peerContext);
}

/** A constrained execution cannot erase prior counts or make unknown consumed usage known. */
export function missionEvidenceAdvances(previous: MissionReply["evidence"], next: MissionReply["evidence"]): boolean {
  if (!previous) return true;
  if (!next || next.model !== previous.model || next.rounds < previous.rounds) return false;
  if (previous.phase && (!next.phase || next.phase.attemptedToolCalls < previous.phase.attemptedToolCalls
    || next.phase.successfulToolCalls < previous.phase.successfulToolCalls
    || (next.phase.completedRequiredToolCalls ?? 0) < (previous.phase.completedRequiredToolCalls ?? 0))) return false;
  if (previous.rounds > 0 && previous.usage === null && next.usage !== null) return false;
  return !previous.usage || !next.usage || (next.usage.promptTokens >= previous.usage.promptTokens
    && next.usage.completionTokens >= previous.usage.completionTokens && next.usage.totalTokens >= previous.usage.totalTokens);
}

function validNativeObservations(value: Record<string, unknown>): boolean {
  const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
  const count = (v: unknown): v is number => Number.isSafeInteger(v) && Number(v) >= 0;
  const positive = (v: unknown): v is number => count(v) && v > 0;
  const text = (v: unknown, limit = 256): v is string => typeof v === "string" && v.length > 0
    && Buffer.byteLength(v) <= limit && !/\p{Cc}/u.test(v);
  const nullableText = (v: unknown, limit = 256) => v === null || text(v, limit);
  const ordered = (v: unknown): v is number[] => Array.isArray(v) && v.length <= MAX_ROUTER_OBSERVATION_ROUNDS
    && v.every((n, i) => positive(n) && (i === 0 || n > v[i - 1]));
  const scope = (v: unknown): v is string => text(v, 128) && /^[A-Za-z0-9._:-]+$/.test(v);
  if (Object.hasOwn(value, "responses") || !positive(value.correlated_tools)
    || (value.state !== "unavailable" && value.correlated_tools > 256)
    || !ordered(value.rounds) || value.rounds.length > value.correlated_tools
    || !(value.scope_id === null || scope(value.scope_id))) return false;
  if (value.state === "unavailable") return value.reason
    === "Native tool-correlated observations unavailable (reset, expiry, truncation or read failure)";
  const trace = value.trace;
  if (value.state !== "observed" || !scope(value.scope_id) || !value.rounds.length || !record(trace)
    || trace.scope_id !== value.scope_id || trace.coverage !== value.coverage || trace.durable !== false
    || JSON.stringify(trace.rounds) !== JSON.stringify(value.rounds)
    || !Array.isArray(trace.missing_rounds) || trace.missing_rounds.length !== 0
    || trace.dropped_events !== 0 || trace.truncated !== false || !Array.isArray(trace.events) || trace.events.length > MAX_ROUTER_OBSERVATION_EVENTS) return false;
  const proposals = new Map<string, { name: string; round: number }>();
  const completed = new Set<string>();
  const proposing = new Set<number>();
  const accepted = new Set<number>();
  let sequence = 0;
  for (const event of trace.events) {
    if (!record(event) || event.scope_id !== value.scope_id || !positive(event.seq) || event.seq <= sequence
      || !positive(event.round) || !value.rounds.includes(event.round)
      || (event.observed_at !== undefined && (typeof event.observed_at !== "string"
        || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(event.observed_at)
        || !Number.isFinite(Date.parse(event.observed_at)) || new Date(event.observed_at).toISOString() !== event.observed_at))) return false;
    sequence = event.seq;
    if (event.kind === "round") {
      const usage = event.usage;
      if (event.source !== "router-upstream" || event.accepted !== true || accepted.has(event.round)
        || !text(event.provider) || !nullableText(event.model) || !text(event.outcome, 128)
        || !nullableText(event.finish_reason, 128) || typeof event.partial_observation !== "boolean"
        || !(event.http_status === null || count(event.http_status) && event.http_status >= 100 && event.http_status <= 599)
        || !count(event.ms) || !count(event.tool_calls_observed)
        || !["present", "partial", "missing"].includes(String(event.usage_state))
        || !(usage === null || record(usage)
          && ["prompt_tokens", "completion_tokens", "total_tokens"].every(key => usage[key] === null || count(usage[key])))) return false;
      accepted.add(event.round);
    } else if (event.kind === "router_operation") {
      if (!scope(event.call_id) || !scope(event.name) || !count(event.ms) || event.ms > 3_600_000) return false;
      const proposal = proposals.get(event.call_id);
      if (!proposal || proposal.name !== event.name || proposal.round !== event.round
        || !(event.destination_host === null && event.destination_port === null
          || typeof event.destination_host === "string" && /^[A-Za-z0-9.:[\]-]{1,253}$/.test(event.destination_host)
          && positive(event.destination_port) && event.destination_port <= 65535)) return false;
      const status = count(event.http_status) && event.http_status >= 100 && event.http_status <= 599;
      if (event.operation === "tool_policy") {
        if (event.source !== "router-policy" || typeof event.allowed !== "boolean"
          || event.http_status !== null || event.complete !== null || event.destination_host !== null
          || !(event.flag === "governance" || event.flag === "rate_limit" && event.allowed === false)) return false;
      } else if (event.operation === "egress_policy") {
        if (event.source !== "router-policy" || typeof event.allowed !== "boolean"
          || event.http_status !== null || event.complete !== null
          || !(event.flag === "egress_check" || ["private_target", "github_service", "dispatch_recheck"].includes(String(event.flag))
            && event.allowed === false)) return false;
      } else if (event.operation !== "egress_fetch" || event.source !== "router" || event.allowed !== null
        || !(event.flag === "response" && status && typeof event.complete === "boolean"
          || event.flag === "body_error" && status && event.complete === false
          || event.flag === "transport_error" && event.http_status === null && event.complete === false)) return false;
    } else if (event.kind === "tool_proposed" || event.kind === "tool_result") {
      if (!text(event.call_id) || !text(event.name)) return false;
      if (event.kind === "tool_proposed") {
        if (event.source !== "model-proposed" || event.ok !== null || proposals.has(event.call_id)) return false;
        if (event.argument_preview !== undefined) {
          const preview = event.argument_preview;
          if (!record(preview) || Object.keys(preview).some(key => !["text", "truncated", "redacted"].includes(key))
            || typeof preview.text !== "string" || !preview.text.length || Buffer.byteLength(preview.text) > 2048
            || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(preview.text)
            || typeof preview.truncated !== "boolean" || typeof preview.redacted !== "boolean") return false;
        }
        proposals.set(event.call_id, { name: event.name, round: event.round });
        proposing.add(event.round);
      } else {
        const proposal = proposals.get(event.call_id);
        if (event.source !== "harness-reported" || !(event.ok === null || typeof event.ok === "boolean")
          || !proposal || proposal.name !== event.name || proposal.round !== event.round || completed.has(event.call_id)
          || (Object.hasOwn(event, "reported_in_round") && (!positive(event.reported_in_round)
            || event.reported_in_round <= event.round || !value.rounds.includes(event.reported_in_round)))
          || (event.ms !== undefined && (!count(event.ms) || event.ms > 3_600_000))) return false;
        if (event.result_summary !== undefined) {
          const summary = event.result_summary;
          if (!record(summary)) return false;
          const only = (keys: string[]) => Object.keys(summary).every(key => keys.includes(key));
          if (summary.kind === "file_read") {
            if (event.ok !== true || !["file_read", "read_file", "kars_mission_read", "kars_mission_file_read"].includes(event.name)
              || !only(["kind", "bytes", "returned_bytes", "truncated"]) || !count(summary.bytes)
              || !count(summary.returned_bytes) || summary.returned_bytes > 16 * 1024 * 1024
              || summary.returned_bytes > summary.bytes || summary.truncated !== (summary.returned_bytes < summary.bytes)) return false;
          } else if (summary.kind === "file_write") {
            if (event.ok !== true || !["file_write", "write_file", "kars_mission_write", "kars_mission_file_write"].includes(event.name)
              || !only(["kind", "bytes", "artifact_name"]) || !count(summary.bytes) || summary.bytes > 16 * 1024 * 1024
              || (summary.artifact_name !== undefined && (typeof summary.artifact_name !== "string"
                || !/^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/.test(summary.artifact_name)))) return false;
          } else if (summary.kind === "python_execution") {
            if (!["python_execute", "kars_mission_python_execute"].includes(event.name)
              || !only(["kind", "bytes", "receipt_sha256", "exit_code", "reason"])
              || !count(summary.bytes) || summary.bytes === 0 || summary.bytes > 65536
              || typeof summary.receipt_sha256 !== "string" || !/^sha256:[a-f0-9]{64}$/.test(summary.receipt_sha256)
              || !(summary.exit_code === null || (Number.isInteger(summary.exit_code) && Math.abs(summary.exit_code as number) <= 2147483647))
              || !["exited", "timeout", "cancelled", "output_limit", "process_limit", "launch_failed", "cleanup_failed"].includes(String(summary.reason))
              || event.ok !== (summary.reason === "exited" && summary.exit_code === 0)) return false;
          } else if (summary.kind !== "failure" || event.ok !== false || !only(["kind", "reason"])
            || !["filesystem_operation_failed", "invalid_arguments", "policy_denied", "reviewed_phase_denied", "tool_execution_failed"].includes(String(summary.reason))) return false;
        }
        completed.add(event.call_id);
      }
    } else return false;
  }
  return proposals.size === value.correlated_tools && proposing.size === value.rounds.length
    && accepted.size === value.rounds.length && value.rounds.every(round => proposing.has(round) && accepted.has(round));
}

/** Reserved mission messages are never interpreted by the legacy task handler. */
export function parseMissionMessage(value: unknown): MissionMessage | null {
  if (!isMissionMessage(value)) return null;
  try { value = snapshotMissionData(value); } catch { return null; }
  const v = value as Record<string, unknown>;
  const base = { ...v };
  delete base.routerObservations;
  // Inputs have their own custody-checked budget; metadata must not consume it.
  if (v.type === "mission:assign") delete base.inputArtifacts;
  if (base.artifacts && typeof base.artifacts === "object" && !Array.isArray(base.artifacts)) {
    base.artifacts = Object.fromEntries(Object.entries(base.artifacts).filter(([name]) => name !== ROUTER_OBSERVATIONS_ARTIFACT));
  }
  if (Buffer.byteLength(JSON.stringify(base)) > MAX_MISSION_BASE_BYTES) return null;
  if (v.version !== 5 && v.peerContext !== undefined) return null;
  if (v.version === MISSION_PROTOCOL_VERSION) {
    if (v.reviewedPhase !== undefined || v.phaseDigest !== undefined || v.inputDigest !== undefined || v.inputArtifacts !== undefined || v.requiredOutputs !== undefined) return null;
  } else if (v.version === PHASE_MISSION_PROTOCOL_VERSION || v.version === INPUT_MISSION_PROTOCOL_VERSION || v.version === 4 || v.version === 5) {
    try {
      const outputs = v.version === 4 || v.version === 5;
      if (!outputs && v.requiredOutputs !== undefined) return null;
      const hasInputs = v.version === INPUT_MISSION_PROTOCOL_VERSION || (outputs && v.inputDigest !== undefined);
      if (!hasInputs && (v.inputDigest !== undefined || v.inputArtifacts !== undefined)) return null;
      if (v.type !== "mission:assign" && v.inputArtifacts !== undefined) return null;
      const contract = parseMissionContract({ version: v.version, reviewedPhase: v.reviewedPhase, phaseDigest: v.phaseDigest,
        ...(hasInputs ? { inputDigest: v.inputDigest } : {}), ...(outputs ? { requiredOutputs: v.requiredOutputs } : {}),
        ...(v.version === 5 ? { peerContext: v.peerContext } : {}) });
      if (!contract) return null;
      const inputs = hasInputs && v.type === "mission:assign" ? missionInputs(v.inputArtifacts) : undefined;
      if (inputs && missionInputsDigest(inputs) !== contract.inputDigest) return null;
      value = Object.freeze({ ...v, ...contract, ...(inputs ? { inputArtifacts: inputs } : {}) });
      if (Buffer.byteLength(JSON.stringify(value)) > MAX_MISSION_MESSAGE_BYTES) return null;
    } catch { return null; }
  } else return null;
  if (v.activityFormat !== undefined && v.activityFormat !== MISSION_ACTIVITY_FORMAT) return null;
  if (v.routerObservations !== undefined) {
    if (v.type !== "mission:reply" || !["running", "failed"].includes(String(v.status)) || v.activityFormat !== MISSION_ACTIVITY_FORMAT
      || typeof v.routerObservations !== "string" || Buffer.byteLength(v.routerObservations) > MAX_MISSION_ACTIVITY_BYTES) return null;
    try {
      const observations = JSON.parse(v.routerObservations);
      if (!observations || observations.version !== 1 || observations.source !== "runtime-forwarded-router-observations"
        || !Array.isArray(observations.rounds) || !["observed", "unavailable"].includes(observations.state)) return null;
      if (observations.coverage === "native-tool-correlated-only") {
        if (!validNativeObservations(observations)) return null;
      } else if (observations.coverage !== "returned-responses-only" || !Number.isSafeInteger(observations.responses)
        || observations.responses < 1) return null;
    } catch { return null; }
  }
  if (v.artifactFormat !== undefined && (v.artifactFormat !== "text-v1" || !["mission:assign", "mission:reply"].includes(String(v.type)))) return null;
  if (v.artifacts !== undefined && (v.type !== "mission:reply" || v.artifactFormat !== "text-v1" || !validMissionArtifacts(v.artifacts)
    || !(v.status === "succeeded" || (v.status === "failed" && Object.keys(v.artifacts).length === 1
      && typeof v.artifacts["kars-python-executions.json"] === "string"
      && Buffer.byteLength(v.artifacts["kars-python-executions.json"]) <= 65536)))) return null;
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
  if (v.status === "failed" && v.output !== undefined
    && (typeof v.output !== "string" || !v.output.trim() || Buffer.byteLength(v.output) > 16384)) return null;
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
  if (v.version !== MISSION_PROTOCOL_VERSION && v.status === "running" && !v.evidence) return null;
  if (v.status === "succeeded") {
    const e = v.evidence as MissionReply["evidence"];
    if (typeof v.output !== "string" || !v.output.trim() || !e?.usage || e.usage.totalTokens <= 0 || e.rounds <= 0) return null;
    if ((value as MissionContract).requiredOutputs?.some(name => v.artifactFormat !== "text-v1"
      || !Object.hasOwn(v.artifacts ?? {}, name))) return null;
  }
  return value as MissionReply;
}

export function sameMissionTarget(a: MissionTarget, b: MissionTarget): boolean {
  return a.taskName === b.taskName && a.taskUid === b.taskUid && a.sandboxUid === b.sandboxUid
    && a.podUid === b.podUid && a.runNonce === b.runNonce && a.agentDid === b.agentDid && a.dispatcherDid === b.dispatcherDid;
}
