"use client";

import { useState } from "react";
import { ActivityStream } from "@/components/activity-stream";
import { AgentGraph } from "@/components/agent-graph";
import { ExecutionLifetime } from "@/components/execution-lifetime";
import { useLiveTrace } from "@/components/use-live-trace";
import type {
  ActivityEvent,
  Approval,
  MissionTelemetry,
  Receipt,
  SubAgent,
  TaskAssignmentEvent,
} from "@/lib/types";

function focusLabel(query: string): string {
  if (query.startsWith("agent:")) return query.slice("agent:".length);
  if (query.startsWith("actions:")) {
    const parameters = new URLSearchParams(query.slice("actions:".length));
    return `${parameters.get("agent") ?? "agent"} · folded actions`;
  }
  if (query.startsWith("action:")) {
    const parameters = new URLSearchParams(query.slice("action:".length));
    const tool = parameters.get("tool") ?? "action";
    const round = Number(parameters.get("round"));
    return `${tool} · round ${Number.isInteger(round) ? round + 1 : "?"}`;
  }
  return query;
}

export function ExecutionExplorer({
  running,
  activity,
  telemetry,
  assignmentEvents,
  approvals,
  ns,
  name,
  agentLabel,
  agentPhase = null,
  agentRuntime = null,
  agentModel = null,
  subAgents = [],
  identity = null,
  envelopeDigest = null,
  receipt = null,
}: {
  running: boolean;
  activity: ActivityEvent[];
  telemetry: MissionTelemetry | null;
  assignmentEvents: TaskAssignmentEvent[];
  approvals: Approval[];
  ns: string;
  name: string;
  agentLabel: string;
  agentPhase?: string | null;
  agentRuntime?: string | null;
  agentModel?: string | null;
  subAgents?: SubAgent[];
  identity?: import("@/lib/types").AgentIdentity | null;
  envelopeDigest?: string | null;
  receipt?: Receipt | null;
}) {
  const events = useLiveTrace(ns, name, running, activity);
  const [tab, setTab] = useState<"lifetime" | "tools">("lifetime");
  const [focus, setFocus] = useState("");

  const inspect = (query: string) => {
    setFocus(query);
    setTab("tools");
  };

  return (
    <div className="space-y-4">
      <AgentGraph
        running={running}
        activity={activity}
        events={events}
        ns={ns}
        name={name}
        agentLabel={agentLabel}
        agentPhase={agentPhase}
        agentRuntime={agentRuntime}
        agentModel={agentModel}
        subAgents={subAgents}
        identity={identity}
        envelopeDigest={envelopeDigest}
        receipt={receipt}
        onInspect={inspect}
      />
      <section className="rounded-2xl border border-border bg-surface p-2">
        <div className="flex flex-wrap items-center justify-between gap-2 px-2 py-1">
          <div>
            <h2 className="text-sm font-semibold">Drill-down</h2>
            <p className="text-[11px] text-foreground-muted">
              Select an agent to focus its exact round and tool records, or inspect the complete chronological lifetime.
            </p>
          </div>
          <div className="flex items-center gap-1 rounded-lg border border-border bg-surface-muted/40 p-1">
            <button
              type="button"
              onClick={() => {
                setFocus("");
                setTab("lifetime");
              }}
              className={`rounded-md px-3 py-1.5 text-xs font-medium ${tab === "lifetime" ? "bg-surface text-foreground shadow-sm" : "text-foreground-muted"}`}
            >
              Complete lifetime
            </button>
            <button
              type="button"
              onClick={() => setTab("tools")}
              className={`rounded-md px-3 py-1.5 text-xs font-medium ${tab === "tools" ? "bg-surface text-foreground shadow-sm" : "text-foreground-muted"}`}
            >
              Rounds &amp; tools
            </button>
          </div>
        </div>
        {focus && (
          <div className="mx-2 mt-2 flex items-center gap-2 rounded-lg border border-signal/30 bg-signal/5 px-3 py-2 text-xs">
            <span className="text-foreground-muted">Focused from graph:</span>
            <span className="font-mono font-semibold">{focusLabel(focus)}</span>
            <button type="button" onClick={() => setFocus("")} className="ml-auto text-foreground-muted hover:text-foreground">
              Clear
            </button>
          </div>
        )}
        <div className="mt-2">
          {tab === "lifetime" ? (
            <ExecutionLifetime
              key={`lifetime-${focus}`}
              running={running}
              activity={events}
              assignmentEvents={assignmentEvents}
              approvals={approvals}
              focusQuery={focus}
            />
          ) : (
            <ActivityStream
              key={`tools-${focus}`}
              running={running}
              activity={activity}
              events={events}
              telemetry={telemetry}
              ns={ns}
              name={name}
              focusQuery={focus}
              principalAgentName={name}
              title="Rounds and tool records"
              detail="Every retained model round and tool invocation, including arguments, result preview, duration, agent, and success state."
            />
          )}
        </div>
      </section>
    </div>
  );
}
