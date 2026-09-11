"use client";

import { useMemo, useState } from "react";
import type { TaskDetail } from "@/lib/types";
import type { TeamRunEvidence } from "@/lib/team-run-evidence";

type FlowCategory = "team" | "tool" | "reasoning" | "outcome" | "system";

type FlowEvent = {
  id: string;
  at: string | null;
  category: FlowCategory;
  actor: string;
  title: string;
  detail: string | null;
  status: "ok" | "working" | "error" | "neutral";
  count?: number;
  tokens?: number;
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

function buildEvents(task: TaskDetail, evidence: TeamRunEvidence): FlowEvent[] {
  const events: FlowEvent[] = task.assignment_events.map((event) => {
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
  for (const event of task.activity) {
    if (event.kind !== "tool") continue;
    const key = [
      event.agent ?? "principal",
      event.name,
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

  const rounds = task.activity.filter((event) => event.kind === "round");
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

  if (task.result) {
    events.push({
      id: "final-outcome",
      at: task.result.finished_at,
      category: "outcome",
      actor: "truthfulness gate",
      title:
        task.result.status === "error"
          ? "Run rejected as failed"
          : evidence.outcome === "delivered"
            ? "Outcome delivered"
            : "Run completed with issues",
      detail:
        task.result.status === "error"
          ? `${evidence.roles.filter((role) => role.state === "delivered").length}/${evidence.roles.filter((role) => role.state !== "skipped").length} selected roles returned durable handbacks.`
          : evidence.issues[0] ?? null,
      status: task.result.status === "error" ? "error" : "ok",
      tokens: task.result.total_tokens ?? undefined,
    });
  }

  return events.sort((left, right) => (left.at ?? "").localeCompare(right.at ?? ""));
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
}: {
  task: TaskDetail;
  evidence: TeamRunEvidence;
}) {
  const roles = evidence.roles
    .filter((role) => role.state !== "skipped")
    .map((role) => role.role.name.replaceAll("-", " "));
  const toolCount = task.activity.filter((event) => event.kind === "tool").length;
  const failedTools = task.activity.filter((event) => event.kind === "tool" && !event.ok).length;
  const resultStatus = task.result?.status ?? null;
  const running = resultStatus == null;
  const nodes = [
    { label: "Controller", detail: "assignment", tone: "border-slate-400/40 bg-slate-500/[0.05]" },
    { label: "Principal", detail: "orchestration", tone: "border-sky-500/40 bg-sky-500/[0.06]" },
    {
      label:
        roles.length === 0
          ? "Principal only"
          : roles.length === 1
            ? roles[0]
            : `${roles.length} specialist roles`,
      detail:
        roles.length > 1
          ? roles.join(" · ")
          : roles.length === 1
            ? "delegated work"
            : "no delegation selected",
      tone: "border-violet-500/40 bg-violet-500/[0.06]",
    },
    {
      label: `${toolCount} tool call${toolCount === 1 ? "" : "s"}`,
      detail:
        failedTools > 0
          ? `${failedTools} failed${running ? " · running" : ""}`
          : running
            ? "calls so far"
            : "all returned",
      tone: failedTools > 0
        ? "border-danger/40 bg-danger/[0.05]"
        : running
          ? "border-sky-500/40 bg-sky-500/[0.06]"
          : "border-emerald-500/40 bg-emerald-500/[0.06]",
    },
    {
      label: "Truthfulness gate",
      detail: running
        ? "pending"
        : resultStatus === "error"
          ? "rejected"
          : "verified",
      tone: running
        ? "border-border bg-surface-muted/30"
        : resultStatus === "error"
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
}: {
  task: TaskDetail;
  evidence: TeamRunEvidence;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<(typeof FILTERS)[number]["value"]>("all");
  const events = useMemo(() => buildEvents(task, evidence), [task, evidence]);
  const normalized = query.trim().toLowerCase();
  const visible = events.filter((event) => {
    if (filter === "errors" && event.status !== "error") return false;
    if (filter !== "all" && filter !== "errors" && event.category !== filter) return false;
    if (!normalized) return true;
    return [event.actor, event.title, event.detail, event.category]
      .filter(Boolean)
      .some((value) => value?.toLowerCase().includes(normalized));
  });
  const failedTools = task.activity.filter((event) => event.kind === "tool" && !event.ok).length;
  const rounds = task.activity.filter((event) => event.kind === "round").length;

  return (
    <section className="rounded-xl border border-border bg-surface p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Execution flow</h2>
          <p className="mt-1 text-xs text-foreground-muted">
            Searchable chronology of assignment, agent handoffs, grouped tool calls, reasoning cost,
            and the final truthfulness decision.
          </p>
        </div>
        <div className="flex flex-wrap gap-2 text-[11px] text-foreground-muted">
          <span>{task.assignment_events.length} lifecycle events</span>
          <span>{task.activity.filter((event) => event.kind === "tool").length} tool calls</span>
          <span className={failedTools > 0 ? "text-danger" : ""}>{failedTools} failed</span>
          <span>{rounds} rounds</span>
          <span>{(task.result?.total_tokens ?? 0).toLocaleString()} tokens</span>
        </div>
      </div>

      <SignalPath task={task} evidence={evidence} />

      <div className="mt-4 flex flex-wrap gap-2">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search role, tool, error, URL, or stage"
          className="min-w-64 flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
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

      <ol className="relative mt-5 space-y-3 before:absolute before:bottom-3 before:left-[7.1rem] before:top-3 before:w-px before:bg-border">
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
            <li key={event.id} className="relative grid grid-cols-[6.25rem_1fr] gap-6">
              <time className="pt-3 text-right text-[10px] text-foreground-muted">
                {eventTime(event.at)}
              </time>
              <span
                className={`absolute left-[6.86rem] top-4 h-2.5 w-2.5 rounded-full border-2 border-surface ${
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
              <div className={`rounded-lg border px-3 py-2.5 ${tone}`}>
                <div className="flex flex-wrap items-center gap-2">
                  <span className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[10px]">
                    {event.actor}
                  </span>
                  <span className="text-xs font-semibold">{event.title}</span>
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
                  <p className="mt-1 break-words text-[11px] leading-relaxed text-foreground-muted">
                    {event.detail}
                  </p>
                )}
              </div>
            </li>
          );
        })}
      </ol>
      {visible.length === 0 && (
        <p className="mt-5 text-xs text-foreground-muted">No flow events match this search.</p>
      )}
    </section>
  );
}
