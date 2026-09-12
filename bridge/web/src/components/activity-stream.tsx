// kars Bridge Workspace — live activity stream.
//
// The plan's Mission Map right-rail: the real tool-call / round trace and token
// burn the agent emitted as it worked. Events are REAL — the controller
// persists the agent's live execution trace; this surface renders it verbatim,
// and while a mission is running it tails the SSE telemetry stream so events
// tick in flight. When a mission has not run there is no trace, and we say so
// plainly rather than faking ticks.

"use client";

import { useEffect, useMemo, useState } from "react";
import { HonestState } from "@/components/honest-state";
import { LivePulse } from "@/components/live-refresh";
import type { ActivityEvent, MissionTelemetry } from "@/lib/types";
import { Icon } from "@/components/icon";

function fmtMs(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export function ActivityStream({
  running,
  activity,
  telemetry,
  ns,
  name,
  events: externalEvents,
  title = "Activity",
  detail = "The real tool calls, model rounds, and token burn the agent emitted as it worked.",
  focusQuery,
  principalAgentName,
}: {
  running: boolean;
  activity: ActivityEvent[];
  telemetry: MissionTelemetry | null;
  ns?: string;
  name?: string;
  /** When a parent supplies the merged live events (one shared stream), use
   *  them and do NOT open a second EventSource. */
  events?: ActivityEvent[];
  title?: string;
  detail?: string;
  focusQuery?: string;
  principalAgentName?: string;
}) {
  const [live, setLive] = useState<ActivityEvent[]>([]);
  const [query, setQuery] = useState(focusQuery ?? "");
  // Tail the SSE telemetry stream while running so the trace ticks in flight.
  useEffect(() => {
    if (externalEvents || !running || !ns || !name) return;
    const es = new EventSource(`/api/namespaces/${ns}/tasks/${name}/stream`);
    es.onmessage = (m) => {
      try {
        setLive((prev) => [...prev, JSON.parse(m.data)]);
      } catch {
        /* ignore malformed frame */
      }
    };
    es.addEventListener("done", () => es.close());
    return () => es.close();
  }, [running, ns, name, externalEvents]);

  const merged = externalEvents ?? (activity.length >= live.length ? activity : live);
  const hasActivity = merged && merged.length > 0;
  const toolEvents = hasActivity ? merged.filter((e) => e.kind === "tool").length : 0;
  const roundEvents = hasActivity ? merged.filter((e) => e.kind === "round").length : 0;
  const rounds = telemetry?.rounds ?? roundEvents;
  const toolCalls = telemetry?.tool_calls ?? toolEvents;
  const visible = useMemo(() => {
    const rawQuery = query.trim();
    const needle = rawQuery.toLowerCase();
    if (!needle) return merged;
    if (needle.startsWith("actions:")) {
      const parameters = new URLSearchParams(rawQuery.slice("actions:".length));
      const agent = parameters.get("agent")?.trim().toLowerCase() ?? "";
      const instance = parameters.get("instance")?.trim().toLowerCase() ?? "";
      const sequences = new Set(
        (parameters.get("seqs") ?? "")
          .split(",")
          .map((value) => Number(value))
          .filter(Number.isInteger),
      );
      const fallbackEvents = (() => {
        try {
          const parsed = JSON.parse(parameters.get("events") ?? "[]");
          return Array.isArray(parsed) ? parsed as Array<{
            round: number;
            ts: string;
            tool: string;
            args: string;
            result: string;
            instance: string | null;
          }> : [];
        } catch {
          return [];
        }
      })();
      const through = parameters.get("through") ?? "";
      const throughTime = new Date(through).getTime();
      return merged.filter((event) => {
        const agentMatches = agent === "principal"
          ? event.agentRole !== "subagent"
          : event.agent?.trim().toLowerCase() === agent;
        const instanceMatches =
          !instance || event.agentInstance?.trim().toLowerCase() === instance;
        if (sequences.size > 0) {
          return agentMatches
            && instanceMatches
            && event.seq != null
            && sequences.has(event.seq);
        }
        if (fallbackEvents.length > 0) {
          return agentMatches
            && instanceMatches
            && fallbackEvents.some((candidate) =>
              event.round === candidate.round
              && event.ts === candidate.ts
              && (event.kind === "round" ? "model.round" : event.name) === candidate.tool
              && (event.kind === "round" ? `${event.tool_calls} tool call${event.tool_calls === 1 ? "" : "s"} requested` : event.args_preview) === candidate.args
              && (event.kind === "round" ? `${event.finish_reason || "unknown finish"}; ${event.total_tokens.toLocaleString("en-US")} tokens` : event.result_preview) === candidate.result
              && (!candidate.instance || event.agentInstance === candidate.instance)
            );
        }
        const eventTime = new Date(event.ts).getTime();
        return agentMatches
          && instanceMatches
          && Number.isFinite(throughTime)
          && Number.isFinite(eventTime)
          && eventTime <= throughTime;
      });
    }
    if (needle.startsWith("action:")) {
      const parameters = new URLSearchParams(rawQuery.slice("action:".length));
      const agent = parameters.get("agent")?.trim().toLowerCase() ?? "";
      const instance = parameters.get("instance")?.trim().toLowerCase() ?? "";
      const sequenceValue = parameters.get("seq");
      const sequence = sequenceValue == null ? Number.NaN : Number(sequenceValue);
      const round = Number(parameters.get("round"));
      const timestamp = parameters.get("ts");
      const tool = parameters.get("tool")?.trim().toLowerCase() ?? "";
      const args = parameters.get("args") ?? "";
      const result = parameters.get("result") ?? "";
      return merged.filter((event) => {
        const agentMatches = agent === "principal"
          ? event.agentRole !== "subagent"
          : event.agent?.trim().toLowerCase() === agent;
        const instanceMatches =
          !instance || event.agentInstance?.trim().toLowerCase() === instance;
        if (Number.isInteger(sequence)) {
          return agentMatches && instanceMatches && event.seq === sequence;
        }
        const toolMatches = event.kind === "round"
          ? tool === "model.round"
          : event.name.trim().toLowerCase() === tool;
        return agentMatches
          && instanceMatches
          && Number.isInteger(round)
          && event.round === round
          && event.ts === timestamp
          && toolMatches
          && (event.kind === "round" ? `${event.tool_calls} tool call${event.tool_calls === 1 ? "" : "s"} requested` : event.args_preview) === args
          && (event.kind === "round" ? `${event.finish_reason || "unknown finish"}; ${event.total_tokens.toLocaleString("en-US")} tokens` : event.result_preview) === result;
      });
    }
    if (needle.startsWith("agent-instance:")) {
      const instance = needle.slice("agent-instance:".length).trim();
      return merged.filter(
        (event) => event.agentInstance?.trim().toLowerCase() === instance,
      );
    }
    if (needle.startsWith("agent:")) {
      const agent = needle.slice("agent:".length).trim();
      return merged.filter((event) => {
        if (agent === "principal" || agent === principalAgentName?.trim().toLowerCase()) {
          return event.agentRole !== "subagent";
        }
        return event.agent?.trim().toLowerCase() === agent;
      });
    }
    return merged.filter((event) => {
      if (event.kind === "round") {
        return [
          `round ${event.round + 1}`,
          event.finish_reason,
          event.agent,
        ].filter(Boolean).join(" ").toLowerCase().includes(needle);
      }
      return [
        event.name,
        event.args_preview,
        event.result_preview,
        event.agent,
        event.ok ? "success" : "failed",
      ].filter(Boolean).join(" ").toLowerCase().includes(needle);
    });
  }, [merged, principalAgentName, query]);

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">{title}</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">{detail}</p>
        </div>
        {hasActivity ? (
          <div className="flex flex-wrap items-center justify-end gap-2">
            <label className="relative">
              <span className="sr-only">Search activity</span>
              <Icon name="search" size={13} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-foreground-muted" />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Search tools, agents, results"
                className="w-56 rounded-lg border border-border bg-surface-muted/40 py-1.5 pl-8 pr-3 text-xs outline-none focus:border-signal"
              />
            </label>
            <span className="shrink-0 rounded-full bg-surface-muted px-2.5 py-1 text-xs font-medium">
              {query.trim() ? `${visible.length}/${merged.length} events` : `${rounds} round${rounds === 1 ? "" : "s"} · ${toolCalls} tool call${toolCalls === 1 ? "" : "s"}`}
            </span>
          </div>
        ) : running ? (
          <LivePulse label="Working" />
        ) : null}
      </div>

      <div className="mt-4">
        {hasActivity ? (
          visible.length > 0 ? (
            <SpanTree events={visible} />
          ) : (
            <HonestState
              variant="empty"
              compact
              title="No matching activity"
              detail="Try a tool name, agent name, host, argument, result, success, or failed."
            />
          )
        ) : running ? (
          <HonestState
            variant="needs_run"
            compact
            title="No activity captured yet"
            detail="This mission's sandbox is running but hasn't executed a task yet. Run the mission to drive the agent loop; its real tool-call trace and token burn appear here as it works."
          />
        ) : (
          <HonestState
            variant="needs_run"
            compact
            title="No activity yet"
            detail="This mission hasn't run. Launch it, then run it — the agent's real per-tool trace and token cost are captured and shown here."
          />
        )}
      </div>
    </section>
  );
}

/** A behavioral span tree: each model round is a parent span, and the tool calls
 *  it issued nest beneath it (both events carry the same `round` index). This is
 *  the LangSmith-style legibility — you read the loop shape (round → tools →
 *  round), not a flat interleave. Only round/tool spans are shown because those
 *  are the only spans the router actually emits; mesh/policy spans are not
 *  fabricated. */
function SpanTree({ events }: { events: ActivityEvent[] }) {
  const agentKey = (event: ActivityEvent) =>
    event.agentInstance
    ?? event.agent
    ?? (event.agentRole === "subagent" ? "subagent" : "principal");
  const roundKey = (event: ActivityEvent) => `${agentKey(event)}\u0000${event.round ?? 0}`;
  // Every sandbox owns an independent round counter. Group by emitting sandbox
  // plus round so principal/worker round 0 records can never merge.
  const toolsByRound = new Map<string, Extract<ActivityEvent, { kind: "tool" }>[]>();
  const roundsByKey = new Map<string, Extract<ActivityEvent, { kind: "round" }>>();
  for (const e of events) {
    if (e.kind === "tool") {
      const key = roundKey(e);
      const list = toolsByRound.get(key);
      if (list) list.push(e);
      else toolsByRound.set(key, [e]);
    } else if (!roundsByKey.has(roundKey(e))) {
      roundsByKey.set(roundKey(e), e);
    }
  }
  const rounds = [...roundsByKey.entries()];
  // Rounds referenced only by a tool (no round event captured yet) still get a
  // header so no tool is orphaned — e.g. an in-flight round mid-stream.
  const extra = [...toolsByRound.keys()].filter((key) => !roundsByKey.has(key));

  // Display rounds 1, 2, 3, ... in emission order, decoupled from the raw
  // router-side round index (which is a cursor relative to the sandbox's
  // telemetry stream and can start above 1 for a reused sandbox or a
  // warm-up call before this delivery) — grouping above still keys off the
  // raw value so tool association is unaffected.
  const displayOrdinal = new Map<string, number>();
  [...rounds.map(([key]) => key), ...extra].forEach((key, index) =>
    displayOrdinal.set(key, index + 1)
  );

  return (
    <ol className="space-y-2">
      {rounds.map(([key, round]) => (
        <li key={`r-${key}`}>
          <RoundRow
            e={round}
            display={displayOrdinal.get(key) ?? round.round + 1}
            agentLabel={agentKey(round)}
          />
          <RoundTools
            tools={toolsByRound.get(key) ?? []}
            display={displayOrdinal.get(key) ?? round.round + 1}
          />
        </li>
      ))}
      {extra.map((key) => {
        const tools = toolsByRound.get(key) ?? [];
        const first = tools[0];
        return (
        <li key={`x-${key}`}>
          <div className="flex items-center gap-2 px-3 py-1.5 text-xs">
            <span className="inline-block h-1.5 w-1.5 shrink-0 rounded-full bg-border" aria-hidden />
            <span className="font-medium">Model round {displayOrdinal.get(key) ?? 1}</span>
            {first && <span className="font-mono text-[10px] text-foreground-muted">· {agentKey(first)}</span>}
            <span className="text-foreground-muted">· in flight</span>
          </div>
          <RoundTools tools={tools} display={displayOrdinal.get(key) ?? 1} />
        </li>
      )})}
    </ol>
  );
}

/** The tool spans nested under a round, with a left rail so the tree is legible. */
function RoundTools({ tools, display }: { tools: Extract<ActivityEvent, { kind: "tool" }>[]; display: number }) {
  if (tools.length === 0) return null;
  return (
    <ol className="ml-[7px] mt-1 space-y-1 border-l border-border pl-3">
      {tools.map((e, i) => (
        <li key={e.seq ?? `${e.agentInstance ?? e.agent ?? "agent"}-${e.round}-${e.ts}-${i}`}>
          <ToolRow e={e} display={display} />
        </li>
      ))}
    </ol>
  );
}

function ToolRow({ e, display }: { e: Extract<ActivityEvent, { kind: "tool" }>; display: number }) {
  return (
    <details className="group rounded-lg border border-border bg-surface-muted/30">
      <summary className="flex cursor-pointer items-center gap-2 px-3 py-2">
        <span
          className={`inline-block h-1.5 w-1.5 shrink-0 rounded-full ${e.ok ? "bg-signal" : "bg-rose-500"}`}
          aria-hidden
        />
        <span className="font-mono text-xs font-medium">{e.name}</span>
        <span className="truncate text-xs text-foreground-muted">{e.args_preview}</span>
        <span className="ml-auto shrink-0 text-[11px] text-foreground-muted">
          r{display} · {fmtMs(e.ms)}
        </span>
      </summary>
      <div className="space-y-2 border-t border-border px-3 py-2 text-xs">
        <div>
          <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Arguments</p>
          <p className="mt-0.5 break-words font-mono">{e.args_preview || "—"}</p>
        </div>
        <div>
          <p className="text-[11px] uppercase tracking-wide text-foreground-muted">
            Result {e.ok ? "" : "(error)"}
          </p>
          <p className="mt-0.5 break-words font-mono">{e.result_preview || "—"}</p>
        </div>
      </div>
    </details>
  );
}

function RoundRow({
  e,
  display,
  agentLabel,
}: {
  e: Extract<ActivityEvent, { kind: "round" }>;
  display: number;
  agentLabel: string;
}) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 text-xs">
      <span className="inline-block h-1.5 w-1.5 shrink-0 rounded-full bg-border" aria-hidden />
      <span className="font-medium">Model round {display}</span>
      <span className="font-mono text-[10px] text-foreground-muted">· {agentLabel}</span>
      <span className="text-foreground-muted">
        {e.finish_reason ? `· ${e.finish_reason}` : ""}
        {e.tool_calls > 0 ? ` · ${e.tool_calls} tool call${e.tool_calls === 1 ? "" : "s"}` : ""}
      </span>
      <span className="ml-auto shrink-0 text-[11px] text-foreground-muted">
        {e.total_tokens.toLocaleString("en-US")} tok · {fmtMs(e.ms)}
      </span>
    </div>
  );
}
