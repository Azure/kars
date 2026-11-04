// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { ActivityEvent, MissionArtifact, MissionResult, TaskAssignmentStatus } from "./types";
import type { ReviewedExecution } from "./types/missions";
import { isSystemEvidenceArtifact, type RouterActivity } from "./router-activity";

export function isStandaloneMission(task: { team: string | null; parent: string | null }): boolean {
  return !task.team && !task.parent;
}

export function reviewKind(artifacts: MissionArtifact[] | undefined): string {
  const exts = (artifacts ?? []).filter(a => !isSystemEvidenceArtifact(a.name))
    .map(a => a.name.split(".").pop()?.toLowerCase() ?? "");
  const code = ["ts", "tsx", "js", "jsx", "rs", "py", "go", "java", "rb", "c", "cpp", "h", "sh", "yaml", "yml", "toml", "json"];
  const docs = ["md", "mdx", "txt", "rst", "adoc", "html"];
  const data = ["csv", "tsv", "parquet", "xlsx"];
  if (exts.some(e => code.includes(e))) return "code";
  if (exts.some(e => data.includes(e))) return "data";
  if (exts.some(e => docs.includes(e))) return "docs";
  return "output";
}

export function missionFailureMessage(result: MissionResult | null): string {
  const messages = [result?.error?.trim(), result?.output.trim()]
    .filter((message): message is string => Boolean(message));
  return [...new Set(messages)].join("\n\n") || "No failure message was recorded for this revision.";
}

export function reviewedPhaseFailureMessage(phase: ReviewedExecution["phases"][number]): string {
  // Generated output is retained evidence, not a diagnosis, even for committed failures.
  if (phase.result) return phase.result.error?.trim() || missionFailureMessage(null);
  const diagnostic = phase.state === "failed" && phase.task_name && phase.task_uid && phase.run_nonce
    ? phase.failure_diagnostic?.trim() : null;
  return diagnostic || missionFailureMessage(null);
}

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
  router_activity?: RouterActivity | null;
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
  const recordedStatus = currentResult?.run_evidence?.status;
  const routerActivity = currentResult?.source === "durable_agent"
    && ((currentResult.status === "ok" && recordedStatus === "succeeded")
      || (currentResult.status === "failed" && recordedStatus === "failed"))
    && currentResult.run_evidence?.run_nonce === nonce
    ? task.router_activity ?? null : null;
  return { currentResult, awaitingAssignment, assignmentInFlight, realDelivery, failed, routerActivity };
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

export interface RevisionArtifactContext {
  namespace: string;
  task: string;
  runNonce: string | null;
  artifacts: Pick<MissionArtifact, "name" | "source_path">[];
}

function sandboxArtifactPath(value: string): string | undefined {
  let path: string;
  try {
    path = decodeURIComponent(value.startsWith("sandbox:") ? value.slice(8) : value);
  } catch {
    return undefined;
  }
  if (path.startsWith("/sandbox/")) path = path.slice(9);
  else if (path.startsWith("./")) path = path.slice(2);
  if (/[\\\\?#:]/.test(path)
      || [...path].some(char => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)
      || path.split("/").some(part => !part || part === "." || part === "..")) return undefined;
  return path;
}

/** Resolve unambiguous captured names, source paths, or flat native export paths for this revision. */
export function resolveRevisionArtifactLink(url: string, context?: RevisionArtifactContext): string | undefined {
  const path = sandboxArtifactPath(url);
  if (!path || !context?.runNonce) return undefined;
  const matches = new Set(context.artifacts.filter(artifact =>
    sandboxArtifactPath(artifact.name) === path
    || (artifact.source_path != null && sandboxArtifactPath(artifact.source_path) === path)
    // Native exports are captured by flat filename, without source-path metadata.
    || (artifact.source_path == null && !artifact.name.includes("/")
      && sandboxArtifactPath(`exports/${artifact.name}`) === path),
  ).map(artifact => artifact.name));
  if (matches.size !== 1) return undefined;
  return revisionArtifactUrl(context.namespace, context.task, [...matches][0], context.runNonce);
}

export function revisionStreamUrl(ns?: string, name?: string, nonce?: string | null): string | null {
  if (!ns || !name || !nonce) return null;
  return `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(name)}/stream?run_nonce=${encodeURIComponent(nonce)}`;
}
