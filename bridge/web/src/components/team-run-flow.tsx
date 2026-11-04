// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState } from "react";
import Link from "@/components/navigation-link";
import type { TaskDetail } from "@/lib/types";
import { displayedPhaseRouterActivity, teamRunMetrics, type TeamRunEvidence } from "@/lib/team-run-evidence";
import { routerProducerKey, type BoundRouterActivity } from "@/lib/router-activity";
import { reviewedPhaseFailureMessage } from "@/lib/mission-run-evidence";
import type { ReviewedExecution } from "@/lib/types/missions";
import { actionsFromRouter } from "@/components/agent-graph/activity";
import { newestLifetimeEvents } from "@/components/execution-lifetime";

type FlowCategory = "team" | "tool" | "reasoning" | "outcome" | "system";

type FlowEvent = {
  id: string;
  at: string | null;
  seq?: number | null;
  producer?: string;
  category: FlowCategory;
  actor: string;
  title: string;
  detail: string | null;
  observationDetail?: string;
  status: "ok" | "working" | "error" | "neutral";
  count?: number;
  tokens?: number;
  href?: string;
};

function assignmentTitle(event: TaskDetail["assignment_events"][number]): {
  title: string;
  actor: string;
  status: FlowEvent["status"];
} {
  const actor = event.child_role ?? (event.worker_did ? "principal" : "controller");
  if (event.event_type === "acknowledged") {
    return { title: "Principal acknowledged assignment", actor, status: "working" };
  }
  switch (event.stage ?? event.event_type) {
    case "assigned":
      return { title: "Principal assigned", actor, status: "neutral" };
    case "worker_replaced":
      return { title: "Worker restarted; assignment rerouted", actor, status: "working" };
    case "child_assigned":
      return { title: `${event.child_role ?? "Role"} assigned`, actor, status: "neutral" };
    case "child_progress":
      return { title: `${event.child_role ?? "Role"} working`, actor, status: "working" };
    case "child_handback":
      return {
        title: `${event.child_role ?? "Role"} handback received`,
        actor,
        status: event.outcome === "success" ? "ok" : "error",
      };
    case "completed":
      return {
        title: "Principal handback recorded",
        actor,
        status: event.outcome === "success" ? "ok" : "error",
      };
    default:
      return {
        title: (event.stage ?? event.event_type).replaceAll("_", " "),
        actor,
        status: event.state === "Completed" ? "ok" : "neutral",
      };
  }
}

function buildEvents(
  task: TaskDetail,
  evidence: TeamRunEvidence,
  routerActivity: BoundRouterActivity | null,
  phases: ReviewedExecution["phases"],
  bindings: BoundRouterActivity[],
): FlowEvent[] {
  const metrics = teamRunMetrics(task);
  const result = metrics.currentResult;
  const failed = result != null && ["error", "failed", "rejected"].includes(result.status ?? "");
  const events: FlowEvent[] = metrics.assignmentEvents.map((event) => {
    const mapped = assignmentTitle(event);
    return {
      id: `assignment-${event.sequence}`,
      at: event.at,
      category: event.child_role ? "team" : "system",
      actor: mapped.actor,
      title: mapped.title,
      detail: event.message,
      status: mapped.status,
    };
  });

  const toolGroups = new Map<string, FlowEvent>();
  for (const phase of phases) {
    const run = phase.result?.run_evidence;
    const href = phase.task_name
      ? `/workspace/missions/${encodeURIComponent(phase.task_name)}?tab=artifacts` : undefined;
    if (run) {
      events.push({
        id: `phase-${phase.task_uid}-start`, at: run.started_at, category: "team",
        actor: phase.role, title: `${phase.phase} started`,
        detail: `Recorded runtime: ${run.agent_name}`, status: "working", href,
      });
    }
    events.push({
      id: `phase-${phase.role}-${phase.phase}-result`,
      at: run?.finished_at ?? null, category: "team", actor: phase.role,
      title: `${phase.phase}: ${phase.state}`,
      detail: phase.result
        ? [phase.state === "failed" ? reviewedPhaseFailureMessage(phase) : phase.result.error,
          phase.state === "failed" ? null : phase.result.output,
          phase.result.usage_known ? `${phase.result.total_tokens?.toLocaleString()} tokens` : "Token usage unavailable"]
          .filter(Boolean).join(" · ")
        : phase.state === "failed" ? reviewedPhaseFailureMessage(phase)
          : "No current terminal record is available for this phase.",
      status: phase.state === "completed" ? "ok" : phase.state === "failed" ? "error"
        : phase.state === "active" ? "working" : "neutral", href,
    });
  }
  const activity = routerActivity ? [] : metrics.activity;
  for (const event of activity) {
    if (event.kind !== "tool") continue;
    const key = [
      event.agent ?? "principal",
      event.name,
      event.args_preview,
      event.ok ? "ok" : "error",
      event.result_preview,
    ].join("|");
    const current = toolGroups.get(key);
    if (current) {
      current.count = (current.count ?? 1) + 1;
      if ((event.ts ?? "") > (current.at ?? "")) current.at = event.ts;
      continue;
    }
    toolGroups.set(key, {
      id: `tool-${toolGroups.size}-${event.round}`,
      at: event.ts,
      category: "tool",
      actor: event.agent ?? "principal",
      title: event.name,
      detail: [event.args_preview, event.result_preview].filter(Boolean).join(" → "),
      status: event.ok ? "ok" : "error",
      count: 1,
    });
  }
  events.push(...toolGroups.values());

  for (const binding of bindings) {
    for (const action of actionsFromRouter(binding)) {
      events.push({
        id: action.id, at: action.ts, seq: action.seq,
        producer: routerProducerKey(binding.producer),
        category: action.source === "router-upstream" ? "reasoning" : "tool",
        actor: binding.producer.agent_name,
        title: action.human,
        detail: [
          binding.stopped ? "Stopped partial observations" : binding.provisional ? "Live provisional observations" : "Retained revision observations",
          `Source: ${action.source}`, action.result,
        ].join(" · "),
        observationDetail: [
          action.args,
          `Run: ${binding.producer.run_nonce}`, `Assignment: ${binding.producer.assignment_id}`,
          `Producer: ${binding.producer.agent_did}`,
        ].join("\n"),
        status: action.ok === false ? "error" : action.ok === true ? "ok" : "neutral",
      });
    }
  }

  const rounds = activity.filter((event) => event.kind === "round");
  if (rounds.length > 0) {
    events.push({
      id: "reasoning-summary",
      at: rounds.at(-1)?.ts ?? null,
      category: "reasoning",
      actor: "model",
      title: `${rounds.length} model reasoning round${rounds.length === 1 ? "" : "s"}`,
      detail: `${rounds.reduce((sum, event) => sum + event.prompt_tokens, 0).toLocaleString()} prompt · ${rounds.reduce((sum, event) => sum + event.completion_tokens, 0).toLocaleString()} completion tokens`,
      status: "neutral",
      tokens: rounds.reduce((sum, event) => sum + event.total_tokens, 0),
    });
  }

  if (result) {
    events.push({
      id: "final-outcome",
      at: result.finished_at,
      category: "outcome",
      actor: "truthfulness gate",
      title:
        failed
          ? "Run rejected as failed"
          : evidence.outcome === "delivered"
            ? "Outcome delivered"
            : "Run completed with issues",
      detail:
        failed
          ? `${evidence.roles.filter((role) => role.state === "delivered").length}/${evidence.roles.filter((role) => role.state !== "skipped").length} selected roles have recorded completions.`
          : evidence.issues[0] ?? null,
      status: failed ? "error"
        : result.reviewable === true && evidence.outcome === "delivered" ? "ok" : "neutral",
      tokens: metrics.tokens ?? undefined,
    });
  }

  return newestLifetimeEvents(events);
}

const FILTERS: Array<{ value: "all" | FlowCategory | "errors"; label: string }> = [
  { value: "all", label: "All flow" },
  { value: "team", label: "Team handoffs" },
  { value: "tool", label: "Tools" },
  { value: "errors", label: "Errors" },
  { value: "reasoning", label: "Reasoning cost" },
];

function eventTime(value: string | null): string {
  if (!value) return "time unavailable";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? "time unavailable"
    : `${parsed.toISOString().slice(11, 19)} UTC`;
}

function SignalPath({
  task,
  evidence,
  observedToolResults,
}: {
  task: TaskDetail;
  evidence: TeamRunEvidence;
  observedToolResults: number | null;
}) {
  const roles = evidence.roles
    .filter((role) => role.state !== "skipped")
    .map((role) => role.role.name.replaceAll("-", " "));
  const metrics = teamRunMetrics(task);
  const toolCount = metrics.toolCalls;
  const failedTools = metrics.failedToolCalls;
  const result = metrics.currentResult;
  const running = result == null;
  const nodes = [
    { label: "Controller", detail: "assignment", tone: "border-slate-400/40 bg-slate-500/[0.05]" },
    { label: "Principal", detail: "orchestration", tone: "border-sky-500/40 bg-sky-500/[0.06]" },
    {
      label:
        metrics.selectedRoles == null
          ? "Role selection unavailable"
          : roles.length === 0
          ? "Principal only"
          : roles.length === 1
            ? roles[0]
            : `${roles.length} specialist roles`,
      detail:
        metrics.selectedRoles == null
          ? "no published selection record"
          : roles.length > 1
          ? roles.join(" · ")
          : roles.length === 1
            ? "delegated work"
            : "no delegation selected",
      tone: "border-violet-500/40 bg-violet-500/[0.06]",
    },
    {
      label: observedToolResults != null ? `${observedToolResults} observed reported tool results`
        : metrics.reportedToolResults != null ? `${metrics.reportedToolResults} retained reported tool results`
        : toolCount == null ? "Tool detail unavailable" : `${toolCount} recorded tool call${toolCount === 1 ? "" : "s"}`,
      detail:
        observedToolResults != null ? "producer reports; not independent execution proof"
        : toolCount == null
          ? metrics.reportedToolResults != null ? "inspect producer records in Drill-down" : "missing detail is not zero calls"
          : failedTools != null && failedTools > 0
          ? `${failedTools} failed${running ? " · running" : ""}`
          : running
            ? "calls so far"
            : "retained calls",
      tone: failedTools != null && failedTools > 0
        ? "border-danger/40 bg-danger/[0.05]"
        : running
          ? "border-sky-500/40 bg-sky-500/[0.06]"
          : "border-emerald-500/40 bg-emerald-500/[0.06]",
    },
    {
      label: "Truthfulness gate",
      detail: running
        ? "pending"
        : result.status === "error"
          ? "rejected"
          : result.reviewable === true
            ? "reviewable result"
            : "evidence unavailable",
      tone: running
        ? "border-border bg-surface-muted/30"
        : result.status === "error"
          ? "border-danger/40 bg-danger/[0.05]"
          : "border-amber-500/40 bg-amber-500/[0.06]",
    },
  ];

  return (
    <div className="mt-4 rounded-lg border border-border bg-surface-muted/20 p-3">
      <div className="mb-2 flex items-center justify-between gap-2">
        <p className="text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
          Signal path
        </p>
        <p className="text-[10px] text-foreground-muted">
          assign → delegate → call → hand back → verify
        </p>
      </div>
      <div className="flex items-stretch gap-2 overflow-x-auto pb-1">
        {nodes.map((node, index) => (
          <div key={node.label} className="flex min-w-0 items-center gap-2">
            {index > 0 && (
              <span className="shrink-0 text-base text-foreground-muted" aria-hidden>
                →
              </span>
            )}
            <div className={`min-w-32 rounded-lg border px-3 py-2 ${node.tone}`}>
              <p className="truncate text-xs font-semibold">{node.label}</p>
              <p className="mt-0.5 max-w-48 truncate text-[10px] text-foreground-muted" title={node.detail}>
                {node.detail}
              </p>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export function TeamRunFlow({
  task,
  evidence,
  routerActivity,
  phases,
}: {
  task: TaskDetail;
  evidence: TeamRunEvidence;
  routerActivity: BoundRouterActivity | null;
  phases: ReviewedExecution["phases"];
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<(typeof FILTERS)[number]["value"]>("all");
  const metrics = teamRunMetrics(task);
  const bindings = [
    ...(routerActivity ? [routerActivity] : []),
    ...phases.flatMap(phase => {
      const binding = displayedPhaseRouterActivity(phase);
      return binding ? [binding] : [];
    }),
  ];
  const observed = bindings.flatMap(binding => binding.observations.state === "observed" ? [binding.observations] : []);
  const recordedRounds = observed.flatMap(observation => observation.trace.events.filter(event => event.kind === "round"));
  const recordedTools = observed.flatMap(observation => observation.trace.events.filter(event => event.kind === "tool_result"));
  const observedTokens = recordedRounds.length && recordedRounds.every(event => event.usage_state === "present"
    && event.usage?.total_tokens != null && Number.isSafeInteger(event.usage.total_tokens) && event.usage.total_tokens >= 0)
    ? recordedRounds.reduce((sum, event) => sum + (event.usage?.total_tokens ?? 0), 0) : null;
  const events = buildEvents(task, evidence, routerActivity, phases, bindings);
  const normalized = query.trim().toLowerCase();
  const visible = events.filter((event) => {
    if (filter === "errors" && event.status !== "error") return false;
    if (filter !== "all" && filter !== "errors" && event.category !== filter) return false;
    if (!normalized) return true;
    return [event.actor, event.title, event.detail, event.observationDetail, event.category]
      .filter(Boolean)
      .some((value) => value?.toLowerCase().includes(normalized));
  });

  return (
    <section className="min-w-0 rounded-xl border border-border bg-surface p-3 [overflow-wrap:anywhere] sm:p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Execution flow</h2>
          <p className="mt-1 text-xs text-foreground-muted">
            Newest timestamped records first; untimestamped records follow. Shared router observations
            retain tool arguments and distinguish proposals from reported outcomes. Observed usage covers
            returned records only, not complete run totals.
          </p>
        </div>
        <div className="flex flex-wrap gap-2 text-[11px] text-foreground-muted">
          <span>{metrics.phases?.length ? `${metrics.phases.length} reserved phases` : `${metrics.assignmentEvents.length} recorded lifecycle events`}</span>
          <span>{observed.length ? `${recordedTools.length} observed reported tool results`
            : metrics.reportedToolResults != null ? `${metrics.reportedToolResults} retained reported tool results`
            : metrics.toolCalls == null ? "Tool detail unavailable" : `${metrics.toolCalls} recorded tool calls`}</span>
          {!observed.length && metrics.failedToolCalls != null && <span className={metrics.failedToolCalls > 0 ? "text-danger" : ""}>{metrics.failedToolCalls} recorded failures</span>}
          {observed.length > 0 && <>
            <span>{recordedRounds.length} observed router rounds</span>
            <span>{observedTokens != null && Number.isSafeInteger(observedTokens)
              ? `${observedTokens.toLocaleString()} observed tokens` : "Observed token usage unavailable"}</span>
          </>}
          {(metrics.rounds != null || !observed.length) && <span>{metrics.rounds == null ? "Round count unavailable" : `${metrics.rounds} recorded rounds`}</span>}
          {(metrics.tokens != null || !observed.length) && <span>{metrics.tokens == null ? "Token usage unavailable" : `${metrics.tokens.toLocaleString()} recorded tokens`}</span>}
        </div>
      </div>

      <SignalPath task={task} evidence={evidence} observedToolResults={observed.length ? recordedTools.length : null} />
      {bindings.map(binding => (
        <p key={routerProducerKey(binding.producer)} className="mt-3 text-xs text-foreground-muted">
          {binding.producer.agent_name}: {binding.stopped ? "Stopped partial observations" : binding.provisional ? "Live provisional observations" : "Retained revision observations"}.
          {binding.observations.state === "unavailable" ? ` Observations unavailable: ${binding.observations.reason}`
            : ` Coverage gaps — missing rounds: ${binding.observations.trace.missing_rounds.join(", ") || "none reported"}; recorder-wide dropped events: ${binding.observations.trace.dropped_events}; selection truncated: ${binding.observations.trace.truncated ? "yes" : "no"}.`}
        </p>
      ))}
      {task.reviewed_execution?.issue && (
        <p className="mt-3 text-xs text-warning">{task.reviewed_execution.issue}</p>
      )}

      <div className="mt-4 flex flex-wrap gap-2">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search role, tool, error, URL, or stage"
          aria-label="Search execution flow"
          className="min-w-0 basis-64 flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
        />
        <select
          value={filter}
          onChange={(event) =>
            setFilter(event.target.value as (typeof FILTERS)[number]["value"])
          }
          aria-label="Filter execution flow"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-xs"
        >
          {FILTERS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </div>

      <ol className="relative mt-5 space-y-3 sm:before:absolute sm:before:bottom-3 sm:before:left-[7.1rem] sm:before:top-3 sm:before:w-px sm:before:bg-border">
        {visible.map((event) => {
          const tone =
            event.status === "error"
              ? "border-danger/40 bg-danger/[0.05]"
              : event.status === "ok"
                ? "border-emerald-500/30 bg-emerald-500/[0.04]"
                : event.status === "working"
                  ? "border-sky-500/30 bg-sky-500/[0.04]"
                  : "border-border bg-surface-muted/20";
          return (
            <li key={event.id} className="relative grid min-w-0 grid-cols-1 gap-1 sm:grid-cols-[6.25rem_minmax(0,1fr)] sm:gap-6">
              <time className="text-[10px] text-foreground-muted sm:pt-3 sm:text-right">
                {eventTime(event.at)}
              </time>
              <span
                className={`absolute left-[6.86rem] top-4 hidden h-2.5 w-2.5 rounded-full border-2 border-surface sm:block ${
                  event.status === "error"
                    ? "bg-danger"
                    : event.status === "ok"
                      ? "bg-emerald-500"
                      : event.status === "working"
                        ? "bg-sky-500"
                        : "bg-foreground-muted"
                }`}
                aria-hidden
              />
              <div className={`min-w-0 rounded-lg border px-3 py-2.5 ${tone}`}>
                <div className="flex flex-wrap items-center gap-2">
                  <span className="min-w-0 rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[10px]">
                    {event.actor}
                  </span>
                  <span className="text-xs font-semibold">{event.title}</span>
                  {event.href && <Link href={event.href} prefetch={false} className="text-xs text-signal underline">Phase evidence and files</Link>}
                  {(event.count ?? 1) > 1 && (
                    <span className="rounded-full border border-border px-1.5 py-0.5 text-[9px]">
                      ×{event.count}
                    </span>
                  )}
                  <span className="ml-auto text-[9px] uppercase tracking-wide text-foreground-muted">
                    {event.category}
                  </span>
                </div>
                {event.detail && (
                  <p className="mt-1 whitespace-pre-wrap text-[11px] leading-relaxed text-foreground-muted">
                    {event.detail}
                  </p>
                )}
                {event.observationDetail && (
                  <details className="mt-2 min-w-0 text-[11px] text-foreground-muted">
                    <summary className="cursor-pointer rounded py-1 text-signal focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-signal">
                      Arguments and provenance
                    </summary>
                    <p className="mt-1 whitespace-pre-wrap leading-relaxed">
                      {event.observationDetail}
                    </p>
                  </details>
                )}
              </div>
            </li>
          );
        })}
      </ol>
      {visible.length === 0 && (
        <p className="mt-5 text-xs text-foreground-muted">
          {events.length === 0
            ? "Activity evidence is not yet available on this run. This does not establish that no work occurred."
            : "No flow events match this search."}
        </p>
      )}
    </section>
  );
}
