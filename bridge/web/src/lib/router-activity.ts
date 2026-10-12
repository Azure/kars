// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export const ROUTER_OBSERVATIONS_ARTIFACT = "kars-router-observations.json";

interface RouterEventIdentity {
  scope_id: string;
  round: number;
  seq: number;
  observed_at?: string;
}

export type ToolResultSummary =
  | { kind: "file_read"; bytes: number; returned_bytes: number; truncated: boolean }
  | { kind: "file_write"; bytes: number; artifact_name?: string }
  | { kind: "failure"; reason: "filesystem_operation_failed" | "invalid_arguments" | "policy_denied" | "reviewed_phase_denied" | "tool_execution_failed" };

export function toolResultSummary(summary?: ToolResultSummary): string {
  if (!summary) return "Result detail was not reported.";
  if (summary.kind === "file_read") return `Read ${summary.returned_bytes} of ${summary.bytes} bytes${summary.truncated ? " (truncated)" : ""}.`;
  if (summary.kind === "file_write") return `Wrote ${summary.bytes} bytes${summary.artifact_name ? `; exported ${summary.artifact_name}` : " to local storage"}.`;
  return ({
    filesystem_operation_failed: "Filesystem operation failed.",
    invalid_arguments: "Tool arguments were invalid.",
    policy_denied: "Policy denied the operation.",
    reviewed_phase_denied: "The reviewed phase did not permit the operation.",
    tool_execution_failed: "Tool execution failed.",
  })[summary.reason];
}

export type RouterActivityEvent = RouterEventIdentity & (
  | {
    kind: "round";
    source: "router-upstream";
    provider: string;
    model: string | null;
    http_status: number | null;
    accepted: boolean | null;
    outcome: string;
    usage: {
      prompt_tokens: number | null;
      completion_tokens: number | null;
      total_tokens: number | null;
    } | null;
    usage_state: "present" | "partial" | "missing";
    finish_reason: string | null;
    partial_observation: boolean;
    ms: number;
    tool_calls_observed: number;
  }
  | { kind: "tool_proposed"; source: "model-proposed"; call_id: string; name: string; ok: null }
  | { kind: "tool_result"; source: "harness-reported"; call_id: string; name: string; ok: boolean | null;
    reported_in_round: number; ms?: number; result_summary?: ToolResultSummary }
);

export type RouterActivity = {
  version: 1;
  source: "runtime-forwarded-router-observations";
  coverage: "returned-responses-only";
  responses: number;
  scope_id: string | null;
  rounds: number[];
} & (
  | { state: "unavailable"; reason: string }
  | {
    state: "observed";
    trace: {
      scope_id: string;
      rounds: number[];
      coverage: "returned-responses-only";
      durable: false;
      missing_rounds: number[];
      dropped_events: number;
      truncated: boolean;
      events: RouterActivityEvent[];
    };
  }
);

export type RouterActivityProducer = Pick<import("./types/missions").MissionRunEvidence,
  "task_uid" | "run_nonce" | "assignment_id" | "agent_name" | "agent_did" | "dispatcher_did" |
  "sandbox_uid" | "pod_uid" | "runtime_boot_id">;

export interface BoundRouterActivity {
  producer: RouterActivityProducer;
  observations: RouterActivity;
  provisional: boolean;
}

export type LiveRouterActivity = RouterActivityProducer & {
  attempt_uid: string;
  progress_sequence: number;
  observations: RouterActivity;
};

const producerFields = ["task_uid", "run_nonce", "assignment_id", "agent_name", "agent_did",
  "dispatcher_did", "sandbox_uid", "pod_uid", "runtime_boot_id"] as const;

export function routerProducerKey(producer: RouterActivityProducer): string {
  return JSON.stringify(producerFields.map(field => producer[field]));
}

export function routerObservationKey(producer: RouterActivityProducer, event: RouterActivityEvent): string {
  return JSON.stringify([routerProducerKey(producer), event.scope_id, event.seq, event.kind, event.source]);
}

export function matchesRouterFocus(binding: BoundRouterActivity, query: string): boolean {
  if (!query) return true;
  if (!query.startsWith("observations:")) return false;
  const params = new URLSearchParams(query.slice("observations:".length));
  if (params.size !== 1) return false;
  if (params.has("producer")) return params.get("producer") === routerProducerKey(binding.producer);
  if (!params.has("ids") || binding.observations.state !== "observed") return false;
  try {
    const ids: unknown = JSON.parse(params.get("ids")!);
    return Array.isArray(ids) && ids.every(id => typeof id === "string")
      && binding.observations.trace.events.some(event => ids.includes(routerObservationKey(binding.producer, event)));
  } catch {
    return false;
  }
}

const record = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const count = (value: unknown): value is number => Number.isSafeInteger(value) && Number(value) >= 0;
const positive = (value: unknown): value is number => count(value) && value > 0;
const text = (value: unknown): value is string => typeof value === "string" && value.length <= 2048;
const nullableText = (value: unknown) => value === null || text(value);
const nullableBool = (value: unknown) => value === null || typeof value === "boolean";
const nullableCount = (value: unknown) => value === null || count(value);
const rounds = (value: unknown): value is number[] => Array.isArray(value) && value.length <= 32
  && value.every((round, index) => positive(round) && (index === 0 || round > value[index - 1]));

function validSummary(value: unknown, name: unknown, ok: unknown): boolean {
  if (!record(value)) return false;
  const only = (keys: string[]) => Object.keys(value).every(key => keys.includes(key));
  if (value.kind === "file_read") return ok === true && ["file_read", "kars_mission_read"].includes(String(name))
    && only(["kind", "bytes", "returned_bytes", "truncated"])
    && count(value.bytes) && count(value.returned_bytes) && value.returned_bytes <= 16 * 1024 * 1024
    && value.returned_bytes <= value.bytes && value.truncated === (value.returned_bytes < value.bytes);
  if (value.kind === "file_write") return ok === true && ["file_write", "kars_mission_write"].includes(String(name))
    && only(["kind", "bytes", "artifact_name"]) && count(value.bytes) && value.bytes <= 16 * 1024 * 1024
    && (value.artifact_name === undefined || (typeof value.artifact_name === "string"
      && /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/.test(value.artifact_name)));
  return value.kind === "failure" && ok === false && only(["kind", "reason"])
    && ["filesystem_operation_failed", "invalid_arguments", "policy_denied", "reviewed_phase_denied",
      "tool_execution_failed"].includes(String(value.reason));
}

/** Browser shape guard; custody and authorization are enforced by the BFF. */
export function liveRouterActivity(value: unknown, nonce: string): LiveRouterActivity | null {
  if (!record(value) || value.run_nonce !== nonce || !nonce
    || ![...producerFields, "attempt_uid"].every(field => text(value[field]) && value[field] !== "")
    || !positive(value.progress_sequence)) return null;
  const activity = value.observations;
  if (!record(activity) || activity.version !== 1 || activity.source !== "runtime-forwarded-router-observations"
    || activity.coverage !== "returned-responses-only" || !positive(activity.responses)
    || !rounds(activity.rounds) || activity.rounds.length > activity.responses
    || !nullableText(activity.scope_id)) return null;
  if (activity.state === "unavailable") {
    return text(activity.reason) ? value as LiveRouterActivity : null;
  }
  const selectedRounds = activity.rounds;
  const trace = activity.trace;
  if (activity.state !== "observed" || !text(activity.scope_id) || !activity.scope_id
    || !record(trace) || trace.scope_id !== activity.scope_id || trace.durable !== false
    || trace.coverage !== activity.coverage || !rounds(trace.rounds)
    || JSON.stringify(trace.rounds) !== JSON.stringify(activity.rounds)
    || !rounds(trace.missing_rounds) || !trace.missing_rounds.every(round => selectedRounds.includes(round))
    || !count(trace.dropped_events) || typeof trace.truncated !== "boolean"
    || !Array.isArray(trace.events) || trace.events.length > 256) return null;
  let sequence = 0;
  for (const event of trace.events) {
    if (!record(event) || event.scope_id !== activity.scope_id || !positive(event.seq) || event.seq <= sequence
      || !positive(event.round) || !activity.rounds.includes(event.round)
      || (event.observed_at !== undefined && (typeof event.observed_at !== "string"
        || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(event.observed_at)
        || !Number.isFinite(Date.parse(event.observed_at))
        || new Date(event.observed_at).toISOString() !== event.observed_at))) return null;
    sequence = event.seq;
    if (event.kind === "round") {
      const usage = event.usage;
      if (event.source !== "router-upstream" || !text(event.provider) || !nullableText(event.model)
        || !(event.http_status === null || (count(event.http_status) && event.http_status >= 100 && event.http_status <= 599))
        || !nullableBool(event.accepted) || !text(event.outcome) || !nullableText(event.finish_reason)
        || !["present", "partial", "missing"].includes(String(event.usage_state))
        || typeof event.partial_observation !== "boolean" || !count(event.ms) || !count(event.tool_calls_observed)
        || !(usage === null || (record(usage)
          && ["prompt_tokens", "completion_tokens", "total_tokens"].every(field => nullableCount(usage[field]))))) return null;
    } else if (event.kind === "tool_proposed" || event.kind === "tool_result") {
      if (!text(event.call_id) || !text(event.name)) return null;
      if (event.kind === "tool_proposed") {
        if (event.source !== "model-proposed" || event.ok !== null) return null;
      } else if (event.source !== "harness-reported" || !nullableBool(event.ok)
        || !positive(event.reported_in_round) || event.reported_in_round <= event.round
        || !activity.rounds.includes(event.reported_in_round)
        || (event.ms !== undefined && (!count(event.ms) || event.ms > 3_600_000))
        || (event.result_summary !== undefined && !validSummary(event.result_summary, event.name, event.ok))) return null;
    } else return null;
  }
  return value as LiveRouterActivity;
}

export function advancesRouterActivity(previous: LiveRouterActivity | null, next: LiveRouterActivity): boolean {
  if (!previous) return true;
  if (routerProducerKey(previous) !== routerProducerKey(next) || previous.attempt_uid !== next.attempt_uid
    || next.progress_sequence < previous.progress_sequence || next.observations.responses < previous.observations.responses) return false;
  if (next.progress_sequence === previous.progress_sequence) return JSON.stringify(previous) === JSON.stringify(next);
  if (previous.observations.scope_id !== null && next.observations.scope_id !== previous.observations.scope_id) return false;
  if (previous.observations.state !== "observed" || next.observations.state !== "observed") return true;
  const before = new Map(previous.observations.trace.events.map(event => [event.seq, JSON.stringify(event)]));
  return next.observations.trace.events.every(event => !before.has(event.seq) || before.get(event.seq) === JSON.stringify(event));
}

export function isSystemEvidenceArtifact(name: string): boolean {
  return name === ROUTER_OBSERVATIONS_ARTIFACT;
}
