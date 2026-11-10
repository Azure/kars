// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export const ROUTER_OBSERVATIONS_ARTIFACT = "kars-router-observations.json";

interface RouterEventIdentity {
  scope_id: string;
  round: number;
  seq: number;
  observed_at?: string;
}

export interface ToolArgumentPreview {
  text: string;
  truncated: boolean;
  redacted: boolean;
}

export function toolArgumentPreview(event: RouterActivityEvent, events: RouterActivityEvent[]): string {
  if (event.kind === "round") return "";
  const proposal = event.kind === "tool_proposed" ? event : events.find(candidate =>
    candidate.kind === "tool_proposed" && candidate.scope_id === event.scope_id
    && candidate.round === event.round && candidate.call_id === event.call_id && candidate.name === event.name);
  const preview = proposal?.kind === "tool_proposed" ? proposal.argument_preview : undefined;
  if (!preview) return "Arguments were not recorded for this call.";
  return `Model argument preview${preview.redacted ? " · redacted" : ""}${preview.truncated ? " · truncated" : ""} (not execution proof):\n${preview.text}`;
}

export type ToolResultSummary =
  | { kind: "file_read"; bytes: number; returned_bytes: number; truncated: boolean }
  | { kind: "file_write"; bytes: number; artifact_name?: string }
  | { kind: "python_execution"; bytes: number; receipt_sha256: string; exit_code: number | null; reason: string }
  | { kind: "failure"; reason: "filesystem_operation_failed" | "invalid_arguments" | "policy_denied" | "reviewed_phase_denied" | "tool_execution_failed" };

export function toolResultSummary(summary?: ToolResultSummary): string {
  if (!summary) return "Result detail was not reported.";
  if (summary.kind === "file_read") return `Read ${summary.returned_bytes} of ${summary.bytes} bytes${summary.truncated ? " (truncated)" : ""}.`;
  if (summary.kind === "file_write") return `Wrote ${summary.bytes} bytes${summary.artifact_name ? `; exported ${summary.artifact_name}` : " to local storage"}.`;
  if (summary.kind === "python_execution") return `Python execution reported ${summary.reason.replaceAll("_", " ")}; exit code ${summary.exit_code ?? "unavailable"}. Receipt: ${summary.receipt_sha256} (${summary.bytes} bytes). An exit code alone does not establish application acceptance.`;
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
  | { kind: "router_operation"; source: "router-policy" | "router"; call_id: string; name: string;
    operation: "tool_policy" | "egress_policy" | "egress_fetch"; flag: string; allowed: boolean | null;
    http_status: number | null; complete: boolean | null; destination_host: string | null; destination_port: number | null; ms: number }
  | { kind: "tool_proposed"; source: "model-proposed"; call_id: string; name: string; ok: null; argument_preview?: ToolArgumentPreview }
  | { kind: "tool_result"; source: "harness-reported"; call_id: string; name: string; ok: boolean | null;
    reported_in_round?: number; ms?: number; result_summary?: ToolResultSummary }
);

export type RouterActivity = {
  version: 1;
  source: "runtime-forwarded-router-observations";
  scope_id: string | null;
  rounds: number[];
} & (
  | { coverage: "returned-responses-only"; responses: number; correlated_tools?: never }
  | { coverage: "native-tool-correlated-only"; correlated_tools: number; responses?: never }
) & (
  | { state: "unavailable"; reason: string }
  | {
    state: "observed";
    trace: {
      scope_id: string;
      rounds: number[];
      coverage: "returned-responses-only" | "native-tool-correlated-only";
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
  "sandbox_uid" | "pod_uid" | "runtime_boot_id" | "runtime">;

export interface BoundRouterActivity {
  producer: RouterActivityProducer;
  observations: RouterActivity;
  provisional: boolean;
  stopped?: boolean;
}

export type LiveRouterActivity = RouterActivityProducer & {
  attempt_uid: string;
  progress_sequence: number;
  observations: RouterActivity;
};

export function stoppedRouterActivity(value: unknown, runNonce?: string | null): BoundRouterActivity | null {
  const snapshot = runNonce ? liveRouterActivity(value, runNonce) : null;
  return snapshot ? { producer: snapshot, observations: snapshot.observations, provisional: true, stopped: true } : null;
}

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

export function reviewedPhaseKey(phase: { role: string; phase: string }): string {
  return JSON.stringify([phase.role, phase.phase]);
}

/** Independent custody pins survive transport reconnects; null revokes only that phase. */
export class ReviewedActivityPins {
  private pins = new Map<string, LiveRouterActivity>();
  private observed = new Map<string, LiveRouterActivity>();
  private revoked = new Set<string>();
  readonly current: Record<string, LiveRouterActivity> = {};

  clearVisible(): void {
    for (const key of Object.keys(this.current)) delete this.current[key];
  }

  observe(value: unknown, rootNonce: string): boolean {
    if (!record(value) || value.run_nonce !== rootNonce || typeof value.phase_key !== "string") return false;
    let parts: unknown;
    try { parts = JSON.parse(value.phase_key); } catch { return false; }
    if (!Array.isArray(parts) || parts.length !== 2
      || !parts.every(part => typeof part === "string" && part.length > 0 && part.length <= 128)
      || JSON.stringify(parts) !== value.phase_key) return false;
    const key = value.phase_key;
    if (value.activity === null) {
      delete this.current[key];
      this.revoked.add(key);
      return true;
    }
    if (!record(value.activity) || typeof value.activity.run_nonce !== "string" || this.revoked.has(key)) return false;
    const next = liveRouterActivity(value.activity, value.activity.run_nonce);
    if (!next || !advancesRouterActivity(this.pins.get(key) ?? null, next)
      || !advancesRouterActivity(this.observed.get(key) ?? null, next)) return false;
    this.pins.set(key, next);
    if (next.observations.state === "observed") this.observed.set(key, next);
    this.current[key] = next;
    return true;
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
const rounds = (value: unknown): value is number[] => Array.isArray(value) && value.length <= 130
  && value.every((round, index) => positive(round) && (index === 0 || round > value[index - 1]));

function validSummary(value: unknown, name: unknown, ok: unknown): boolean {
  if (!record(value)) return false;
  const only = (keys: string[]) => Object.keys(value).every(key => keys.includes(key));
  if (value.kind === "file_read") return ok === true && ["file_read", "read_file", "kars_mission_read", "kars_mission_file_read"].includes(String(name))
    && only(["kind", "bytes", "returned_bytes", "truncated"])
    && count(value.bytes) && count(value.returned_bytes) && value.returned_bytes <= 16 * 1024 * 1024
    && value.returned_bytes <= value.bytes && value.truncated === (value.returned_bytes < value.bytes);
  if (value.kind === "file_write") return ok === true && ["file_write", "write_file", "kars_mission_write", "kars_mission_file_write"].includes(String(name))
    && only(["kind", "bytes", "artifact_name"]) && count(value.bytes) && value.bytes <= 16 * 1024 * 1024
    && (value.artifact_name === undefined || (typeof value.artifact_name === "string"
      && /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/.test(value.artifact_name)));
  if (value.kind === "python_execution") return ["python_execute", "kars_mission_python_execute"].includes(String(name))
    && only(["kind", "bytes", "receipt_sha256", "exit_code", "reason"])
    && positive(value.bytes) && value.bytes <= 65536
    && typeof value.receipt_sha256 === "string" && /^sha256:[a-f0-9]{64}$/.test(value.receipt_sha256)
    && (value.exit_code === null || (typeof value.exit_code === "number" && Number.isInteger(value.exit_code) && Math.abs(value.exit_code) <= 2147483647))
    && ["exited", "timeout", "cancelled", "output_limit", "process_limit", "launch_failed", "cleanup_failed"].includes(String(value.reason))
    && ok === (value.reason === "exited" && value.exit_code === 0);
  return value.kind === "failure" && ok === false && only(["kind", "reason"])
    && ["filesystem_operation_failed", "invalid_arguments", "policy_denied", "reviewed_phase_denied",
      "tool_execution_failed"].includes(String(value.reason));
}

function validRouterOperation(event: Record<string, unknown>): boolean {
  const identifier = (value: unknown) => typeof value === "string" && /^[A-Za-z0-9._:-]{1,128}$/.test(value);
  const status = count(event.http_status) && event.http_status >= 100 && event.http_status <= 599;
  if (!identifier(event.call_id) || !identifier(event.name) || !count(event.ms) || event.ms > 3_600_000
    || !(event.destination_host === null && event.destination_port === null
      || typeof event.destination_host === "string" && /^[A-Za-z0-9.:[\]-]{1,253}$/.test(event.destination_host)
      && positive(event.destination_port) && event.destination_port <= 65535)) return false;
  if (event.operation === "tool_policy") return event.source === "router-policy" && typeof event.allowed === "boolean"
    && event.http_status === null && event.complete === null && event.destination_host === null
    && (event.flag === "governance" || event.flag === "rate_limit" && event.allowed === false);
  if (event.operation === "egress_policy") return event.source === "router-policy" && typeof event.allowed === "boolean"
    && event.http_status === null && event.complete === null
    && (event.flag === "egress_check" || ["private_target", "dispatch_recheck"].includes(String(event.flag)) && event.allowed === false);
  return event.operation === "egress_fetch" && event.source === "router" && event.allowed === null
    && (event.flag === "response" && status && typeof event.complete === "boolean"
      || event.flag === "body_error" && status && event.complete === false
      || event.flag === "transport_error" && event.http_status === null && event.complete === false);
}

export function routerOperationSummary(event: Extract<RouterActivityEvent, { kind: "router_operation" }>): string {
  const destination = event.destination_host ? ` Destination: ${event.destination_host}:${event.destination_port}.` : "";
  return event.operation === "egress_fetch"
    ? `Router fetch · flag: ${event.flag}; upstream HTTP ${event.http_status ?? "unavailable"}; response complete: ${event.complete ? "yes" : "no"}.${destination} Not a harness outcome.`
    : `Router ${event.operation === "tool_policy" ? "tool" : "egress"} policy · flag: ${event.flag}; decision: ${event.allowed === true ? "allow" : event.allowed === false ? "deny" : "unavailable"}.${destination}`;
}

/** Browser shape guard; custody and authorization are enforced by the BFF. */
export function liveRouterActivity(value: unknown, nonce: string): LiveRouterActivity | null {
  if (!record(value) || value.run_nonce !== nonce || !nonce
    || ![...producerFields, "attempt_uid"].every(field => text(value[field]) && value[field] !== "")
    || !positive(value.progress_sequence)
    || !(value.runtime == null || value.runtime === "OpenClaw" || value.runtime === "Hermes")) return null;
  const activity = value.observations;
  if (!record(activity)) return null;
  const native = activity.coverage === "native-tool-correlated-only";
  const selectedCount = native ? activity.correlated_tools : activity.responses;
  if (activity.version !== 1 || activity.source !== "runtime-forwarded-router-observations"
    || (!native && activity.coverage !== "returned-responses-only") || !positive(selectedCount)
    || (native ? selectedCount > 256 || Object.hasOwn(activity, "responses") : activity.correlated_tools !== undefined)
    || !rounds(activity.rounds) || activity.rounds.length > selectedCount
    || !nullableText(activity.scope_id)) return null;
  if (activity.state === "unavailable") {
    return (native ? activity.reason === "Native tool-correlated observations unavailable (reset, expiry, truncation or read failure)"
      : text(activity.reason)) ? value as LiveRouterActivity : null;
  }
  const selectedRounds = activity.rounds;
  const trace = activity.trace;
  if (activity.state !== "observed" || !text(activity.scope_id) || !activity.scope_id
    || !record(trace) || trace.scope_id !== activity.scope_id || trace.durable !== false
    || trace.coverage !== activity.coverage || !rounds(trace.rounds)
    || JSON.stringify(trace.rounds) !== JSON.stringify(activity.rounds)
    || !rounds(trace.missing_rounds) || !trace.missing_rounds.every(round => selectedRounds.includes(round))
    || !count(trace.dropped_events) || typeof trace.truncated !== "boolean"
    || !Array.isArray(trace.events) || trace.events.length > 1024
    || (native && (!activity.rounds.length || trace.missing_rounds.length > 0 || trace.dropped_events !== 0 || trace.truncated))) return null;
  const proposals = new Map<string, { name: string; round: number }>();
  const completed = new Set<string>();
  const proposingRounds = new Set<number>();
  const acceptedRounds = new Set<number>();
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
      if (native) {
        if (event.accepted !== true || acceptedRounds.has(event.round)) return null;
        acceptedRounds.add(event.round);
      }
    } else if (event.kind === "router_operation") {
      if (!validRouterOperation(event)) return null;
      const proposal = proposals.get(event.call_id as string);
      if (!proposal || proposal.name !== event.name || proposal.round !== event.round) return null;
    } else if (event.kind === "tool_proposed" || event.kind === "tool_result") {
      if (!text(event.call_id) || !text(event.name)) return null;
      if (event.kind === "tool_proposed") {
        if (event.source !== "model-proposed" || event.ok !== null) return null;
        if (event.argument_preview !== undefined) {
          const preview = event.argument_preview;
          if (!record(preview) || Object.keys(preview).some(key => !["text", "truncated", "redacted"].includes(key))
            || typeof preview.text !== "string" || !preview.text.length || new TextEncoder().encode(preview.text).length > 2048
            || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u.test(preview.text)
            || typeof preview.truncated !== "boolean" || typeof preview.redacted !== "boolean") return null;
        }
        if (native && (!event.call_id || !event.name || proposals.has(event.call_id))) return null;
        proposals.set(event.call_id, { name: event.name, round: event.round });
        if (native) proposingRounds.add(event.round);
      } else if (event.source !== "harness-reported" || !nullableBool(event.ok)
        || !(!Object.hasOwn(event, "reported_in_round") && typeof event.ok === "boolean"
          || positive(event.reported_in_round) && event.reported_in_round > event.round
          && activity.rounds.includes(event.reported_in_round))
        || (event.ms !== undefined && (!count(event.ms) || event.ms > 3_600_000))
        || (event.result_summary !== undefined && !validSummary(event.result_summary, event.name, event.ok))) return null;
      if (event.kind === "tool_result" && (native || !Object.hasOwn(event, "reported_in_round"))) {
        const proposal = proposals.get(event.call_id);
        if (!proposal || proposal.name !== event.name || proposal.round !== event.round || completed.has(event.call_id)) return null;
        completed.add(event.call_id);
      }
    } else return null;
  }
  if (native && (proposals.size !== selectedCount || proposingRounds.size !== selectedRounds.length
    || acceptedRounds.size !== selectedRounds.length
    || !selectedRounds.every(round => proposingRounds.has(round) && acceptedRounds.has(round)))) return null;
  return value as LiveRouterActivity;
}

export function routerActivityCount(activity: RouterActivity): number {
  return activity.coverage === "native-tool-correlated-only" ? activity.correlated_tools : activity.responses;
}

export function toolReportingLabel(event: Extract<RouterActivityEvent, { kind: "tool_result" }>): string {
  return event.reported_in_round === undefined ? "Immediate harness report"
    : `Reported in router round ${event.reported_in_round}`;
}

export function advancesRouterActivity(previous: LiveRouterActivity | null, next: LiveRouterActivity): boolean {
  if (!previous) return true;
  if (routerProducerKey(previous) !== routerProducerKey(next) || previous.attempt_uid !== next.attempt_uid
    || (previous.runtime ?? null) !== (next.runtime ?? null)
    || next.progress_sequence < previous.progress_sequence || next.observations.coverage !== previous.observations.coverage
    || routerActivityCount(next.observations) < routerActivityCount(previous.observations)) return false;
  if (next.progress_sequence === previous.progress_sequence) return JSON.stringify(previous) === JSON.stringify(next);
  if (previous.observations.scope_id !== null && next.observations.scope_id !== previous.observations.scope_id) return false;
  if (previous.observations.state !== "observed" || next.observations.state !== "observed") return true;
  const before = new Map(previous.observations.trace.events.map(event => [event.seq, JSON.stringify(event)]));
  return next.observations.trace.events.every(event => !before.has(event.seq) || before.get(event.seq) === JSON.stringify(event));
}

export function isSystemEvidenceArtifact(name: string): boolean {
  return name === ROUTER_OBSERVATIONS_ARTIFACT;
}
