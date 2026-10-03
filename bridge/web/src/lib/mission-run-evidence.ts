// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { ActivityEvent, MissionResult, TaskAssignmentStatus } from "./types";

export function missionFailureAdvice(reason: string): { cause: string; remedy: string } {
  const message = reason.toLowerCase();
  if (message.includes("wire request bounds")) {
    return {
      cause: "The runtime reported that governed inference request bounds were unavailable or denied. This does not establish token-budget exhaustion.",
      remedy: "Inspect the request-bound admission evidence before changing the budget or requesting another revision.",
    };
  }
  if (/token budget.*(?:exceeded|budget at)|out of tokens|token cap.*(?:reached|exceeded)/.test(message)) {
    return {
      cause: "The recorded message reports a token-budget stop; message counts are not independently verified accounting.",
      remedy: "Check the governed budget account and approval requirements. A budget change does not by itself prove the run resumed or establish a daily reset.",
    };
  }
  return {
    cause: "No eligible successful deliverable is recorded for this revision. The message alone does not establish the underlying cause.",
    remedy: "Inspect the recorded result and revision-bound evidence. Current sandbox diagnostics may help investigate, but do not prove what happened in this revision.",
  };
}

export function missionRunState(task: {
  current_run_nonce: string | null;
  result: MissionResult | null;
  assignment: TaskAssignmentStatus | null;
}) {
  const nonce = task.current_run_nonce;
  const result = task.result && nonce && task.result.assignment_nonce === nonce
    ? task.result : null;
  const terminal = result != null && ["ok", "failed", "rejected", "error"].includes(result.status ?? "");
  const assignmentMatches = Boolean(nonce && task.assignment?.task_id === nonce);
  const awaitingAssignment = Boolean(nonce && !assignmentMatches && !terminal);
  const assignmentInFlight = !terminal && (awaitingAssignment || (
    assignmentMatches && task.assignment?.completed_at == null
    && ["Assigned", "Running"].includes(task.assignment?.state ?? "")
  ));
  const currentResult = assignmentInFlight ? null : result;
  const realDelivery = currentResult?.reviewable === true
    && currentResult.status === "ok" && Boolean(currentResult.output.trim())
    && currentResult.blocked == null;
  const failed = currentResult != null
    ? !realDelivery && currentResult.blocked == null
    : assignmentMatches && task.assignment?.completed_at != null && task.assignment.state === "Failed";
  return { currentResult, awaitingAssignment, assignmentInFlight, realDelivery, failed };
}

export function activityForRevision(event: unknown, nonce: string): event is ActivityEvent {
  if (!nonce || !event || typeof event !== "object" || Array.isArray(event)) return false;
  const value = event as Record<string, unknown>;
  const bindings = ["runNonce", "assignmentNonce", "assignment_nonce"].filter(key => key in value);
  if (!bindings.length || bindings.some(key => value[key] !== nonce)) return false;
  const number = (key: string) => typeof value[key] === "number" && Number.isFinite(value[key]) && value[key] >= 0;
  const string = (key: string) => typeof value[key] === "string";
  const count = (key: string) => number(key) && Number.isSafeInteger(value[key]);
  if (!count("round") || !number("ms") || !string("ts") || !Number.isFinite(Date.parse(value.ts as string))) return false;
  if (["agent", "agentInstance"].some(key => key in value && !string(key))) return false;
  if ("seq" in value && (!number("seq") || !Number.isSafeInteger(value.seq))) return false;
  if ("agentRole" in value && value.agentRole !== "principal" && value.agentRole !== "subagent") return false;
  if (value.kind === "tool") {
    return ["name", "args_preview", "result_preview"].every(string) && typeof value.ok === "boolean"
      && (!("source" in value) || ["router", "harness", "governance"].includes(value.source as string));
  }
  return value.kind === "round"
    && ["prompt_tokens", "completion_tokens", "total_tokens", "tool_calls"].every(count)
    && string("finish_reason");
}

export function mergeRevisionActivity(seed: ActivityEvent[], live: ActivityEvent[], nonce?: string | null): ActivityEvent[] {
  if (!nonce) return [];
  const events = new Map<string, ActivityEvent>();
  for (const event of [...seed, ...live]) {
    if (!activityForRevision(event, nonce)) continue;
    const key = event.seq == null ? JSON.stringify(event)
      : `${event.agentInstance ?? event.agent ?? "principal"}:${event.kind}:${event.seq}`;
    events.set(key, event);
  }
  return [...events.values()];
}

export function revisionArtifactUrl(ns: string, name: string, file: string, nonce?: string | null): string | undefined {
  if (!ns || !name || !file || !nonce) return undefined;
  return `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(name)}/artifact/${encodeURIComponent(file)}?run_nonce=${encodeURIComponent(nonce)}`;
}

export function revisionStreamUrl(ns?: string, name?: string, nonce?: string | null): string | null {
  if (!ns || !name || !nonce) return null;
  return `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(name)}/stream?run_nonce=${encodeURIComponent(nonce)}`;
}
