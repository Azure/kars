// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useMemo, useState } from "react";
import { Icon } from "@/components/icon";
import { LivePulse } from "@/components/live-refresh";
import { actionsFromRouter } from "@/components/agent-graph/activity";
import type { BoundRouterActivity } from "@/lib/router-activity";
import type { ActivityEvent, Approval, TaskAssignmentEvent } from "@/lib/types";
import type { ReviewedExecution } from "@/lib/types/missions";
import { reviewedPhaseRouterActivity } from "@/lib/team-run-evidence";

type LifetimeEvent = {
  id: string;
  at: string | null;
  seq?: number | null;
  kind: "lifecycle" | "round" | "tool" | "approval";
  title: string;
  summary: string;
  detail: string | null;
  ok: boolean | null;
};

function fmtAt(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const pad = (part: number, width = 2) => String(part).padStart(width, "0");
  return `${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())}.${pad(date.getUTCMilliseconds(), 3)} UTC`;
}

export function ExecutionLifetime({
  running,
  activity,
  assignmentEvents,
  approvals,
  focusQuery,
  routerActivity = null,
  phases = [],
}: {
  running: boolean;
  activity: ActivityEvent[];
  assignmentEvents: TaskAssignmentEvent[];
  approvals: Approval[];
  focusQuery?: string;
  routerActivity?: BoundRouterActivity | null;
  phases?: ReviewedExecution["phases"];
}) {
  const [query, setQuery] = useState(focusQuery ?? "");
  const events = useMemo<LifetimeEvent[]>(() => {
    const rows: LifetimeEvent[] = [];
    for (const phase of phases) {
      const terminal = phase.result?.run_evidence;
      if (!terminal || terminal.task_uid !== phase.task_uid || terminal.run_nonce !== phase.run_nonce) continue;
      for (const [stage, at] of [["started", terminal.started_at], [phase.state, terminal.finished_at]]) {
        if (!at) continue;
        rows.push({
          id: `phase-${phase.task_uid}-${stage}`, at, kind: "lifecycle",
          title: `${phase.role} / ${phase.phase}: ${stage}`,
          summary: phase.task_name ?? phase.role,
          detail: `Run: ${terminal.run_nonce}\nAssignment: ${terminal.assignment_id}\nProducer: ${terminal.agent_did}`,
          ok: stage === "failed" ? false : stage === "completed" ? true : null,
        });
      }
    }
    for (const event of assignmentEvents) {
      rows.push({
        id: `assignment-${event.event_id}`,
        at: event.at,
        kind: "lifecycle",
        title: event.event_type.replace(/_/g, " "),
        summary: [
          event.state,
          event.stage,
          event.child_role,
          event.worker_did ? `worker ${event.worker_did}` : null,
        ].filter(Boolean).join(" · "),
        detail: event.message ?? event.outcome,
        ok: event.state === "Failed" ? false : null,
      });
    }
    for (const [index, event] of (routerActivity ? [] : activity).entries()) {
      if (event.kind === "round") {
        rows.push({
          id: `round-${event.round}-${index}`,
          at: event.ts,
          kind: "round",
          title: `Model round ${event.round + 1}`,
          summary: `${event.total_tokens.toLocaleString("en-US")} tokens · ${event.tool_calls} tool call${event.tool_calls === 1 ? "" : "s"} · ${event.ms} ms`,
          detail: event.finish_reason || null,
          ok: null,
        });
      } else {
        rows.push({
          id: `tool-${event.round}-${index}`,
          at: event.ts,
          kind: "tool",
          title: event.name,
          summary: event.args_preview || "No arguments retained",
          detail: event.result_preview || null,
          ok: event.ok,
        });
      }
    }
    for (const approval of approvals) {
      const at = approval.decided_at ?? approval.requested_at;
      if (!at) continue;
      rows.push({
        id: `approval-${approval.name}-${approval.phase}`,
        at,
        kind: "approval",
        title: approval.phase === "Pending" ? "Approval requested" : `Approval ${approval.phase.toLowerCase()}`,
        summary: approval.summary,
        detail: [
          approval.detail,
          approval.decider ? `Decider: ${approval.decider}` : null,
        ].filter(Boolean).join("\n") || null,
        ok: approval.phase === "Approved" ? true : approval.phase === "Denied" || approval.phase === "Expired" ? false : null,
      });
    }
    rows.sort((a, b) => (a.at ?? "").localeCompare(b.at ?? ""));
    for (const binding of [
      ...(routerActivity ? [routerActivity] : []),
      ...phases.flatMap(phase => {
        const binding = reviewedPhaseRouterActivity(phase);
        return binding ? [binding] : [];
      }),
    ]) {
      const { producer } = binding;
      for (const action of actionsFromRouter(binding)) {
        rows.push({
          id: action.id,
          at: action.ts,
          seq: action.seq,
          kind: action.source === "router-upstream" ? "round" : "tool",
          title: action.human,
          summary: `${producer.agent_name} · ${action.result}`,
          detail: [
            `Source: ${action.source}`,
            action.ts ? `Router observed at: ${action.ts}` : "Timestamp not retained by this recorder version",
            `Router round: ${action.round}`,
            action.args,
            action.result,
            `Run: ${producer.run_nonce}`,
            `Assignment: ${producer.assignment_id}`,
            `Producer: ${producer.agent_did}`,
          ].join("\n"),
          ok: action.ok,
        });
      }
    }
    return rows.sort((a, b) => {
      if (a.at && b.at) return a.at.localeCompare(b.at);
      if (a.at) return -1;
      if (b.at) return 1;
      return 0;
    });
  }, [activity, approvals, assignmentEvents, routerActivity, phases]);

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return events;
    return events.filter((event) =>
      [event.kind, event.title, event.summary, event.detail]
        .filter(Boolean)
        .join(" ")
        .toLowerCase()
        .includes(needle),
    );
  }, [events, query]);

  return (
    <section className="rounded-2xl border border-border bg-surface p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Recorded execution timeline</h2>
          <p className="mt-0.5 max-w-2xl text-xs text-foreground-muted">
            {routerActivity || phases.some(phase => reviewedPhaseRouterActivity(phase))
              ? "Timestamped records appear in recorded time order. Router times identify observations, not exact tool start times. Older untimestamped records follow in each producer's sequence order; no ordering between producers is inferred for those records."
              : "Available assignment, model round, tool call and approval records in timestamp order."}
            {" "}Missing records do not establish that no work occurred.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="relative">
            <span className="sr-only">Search recorded timeline</span>
            <Icon name="search" size={13} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-foreground-muted" />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search recorded events"
              className="w-64 rounded-lg border border-border bg-surface-muted/40 py-1.5 pl-8 pr-3 text-xs outline-none focus:border-signal"
            />
          </label>
          {running ? <LivePulse label="Recording live" /> : (
            <span className="rounded-full border border-border bg-surface-muted px-2.5 py-1 text-xs font-medium text-foreground-muted">
              {events.length > 0 ? `${visible.length}/${events.length} recorded events` : "No records available"}
            </span>
          )}
        </div>
      </div>
      {routerActivity && (
        <div className="mt-3 space-y-1 text-xs text-foreground-muted">
          <p>
            {routerActivity.provisional ? "Live provisional observations" : "Retained revision observations"}
            {" · "}{routerActivity.producer.agent_name}. Returned responses only; not independent proof of tool execution or mission completion.
          </p>
          <p className="break-all font-mono text-[10px]">
            Run: {routerActivity.producer.run_nonce} · Assignment: {routerActivity.producer.assignment_id} · Producer: {routerActivity.producer.agent_did}
          </p>
          {routerActivity.observations.state === "unavailable" ? (
            <p>Router observations unavailable: {routerActivity.observations.reason}</p>
          ) : (
            <p>
              Coverage gaps — missing rounds: {routerActivity.observations.trace.missing_rounds.join(", ") || "none reported"}
              {" · "}Recorder-wide dropped events: {routerActivity.observations.trace.dropped_events}
              {" · "}Selection truncated: {routerActivity.observations.trace.truncated ? "yes" : "no"}
            </p>
          )}
        </div>
      )}
      <ol className="mt-4 space-y-2">
        {visible.map((event) => (
          <li key={event.id}>
            <details className="group rounded-xl border border-border bg-surface-muted/25">
              <summary className="flex cursor-pointer list-none items-start gap-3 px-3 py-2.5">
                <span className={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${
                  event.ok === false ? "bg-danger" : event.ok === true ? "bg-ok" : event.kind === "approval" ? "bg-warning" : "bg-signal"
                }`} />
                <span className="w-32 shrink-0 font-mono text-[10px] text-foreground-muted">
                  {event.at === null ? <>
                    Sequence {event.seq}
                    <span className="block">Timestamp unavailable</span>
                  </> : fmtAt(event.at)}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-xs font-semibold">{event.title}</span>
                    <span className="rounded-full border border-border px-1.5 py-0.5 text-[9px] uppercase tracking-wide text-foreground-muted">
                      {event.kind}
                    </span>
                  </span>
                  <span className="mt-0.5 block truncate text-xs text-foreground-muted">{event.summary}</span>
                </span>
                <span className="text-xs text-foreground-muted transition group-open:rotate-180">⌄</span>
              </summary>
              <div className="border-t border-border px-3 py-3">
                <p className="whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed">
                  {event.detail || event.summary}
                </p>
              </div>
            </details>
          </li>
        ))}
      </ol>
      {visible.length === 0 && (
        <p className="mt-4 rounded-lg border border-dashed border-border p-4 text-center text-xs text-foreground-muted">
          {events.length === 0
            ? "No timeline records are available here. This does not mean the agent did no work."
            : "No retained event matches this search."}
        </p>
      )}
    </section>
  );
}
