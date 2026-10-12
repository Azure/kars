// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState } from "react";
import { ActivityStream } from "@/components/activity-stream";
import { AgentGraph } from "@/components/agent-graph";
import { ExecutionLifetime } from "@/components/execution-lifetime";
import { useLiveTrace } from "@/components/use-live-trace";
import { RouterActivityPanel } from "@/components/router-activity-panel";
import { matchesRouterFocus, routerProducerKey, type BoundRouterActivity } from "@/lib/router-activity";
import { reviewedPhaseRouterActivity } from "@/lib/team-run-evidence";
import type { ReviewedExecution } from "@/lib/types/missions";
import type {
  ActivityEvent,
  Approval,
  MissionTelemetry,
  Receipt,
  SubAgent,
  TaskAssignmentEvent,
} from "@/lib/types";

function focusLabel(query: string): string {
  if (query.startsWith("observations:")) {
    return new URLSearchParams(query.slice("observations:".length)).has("producer")
      ? "All observations for selected producer" : "Selected router observations";
  }
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
  runNonce,
  agentLabel,
  agentPhase = null,
  agentRuntime = null,
  agentModel = null,
  subAgents = [],
  identity = null,
  envelopeDigest = null,
  receipt = null,
  routerActivity: recordedRouterActivity = null,
  phases = [],
}: {
  running: boolean;
  activity: ActivityEvent[];
  telemetry: MissionTelemetry | null;
  assignmentEvents: TaskAssignmentEvent[];
  approvals: Approval[];
  ns: string;
  name: string;
  runNonce?: string | null;
  agentLabel: string;
  agentPhase?: string | null;
  agentRuntime?: string | null;
  agentModel?: string | null;
  subAgents?: SubAgent[];
  identity?: import("@/lib/types").AgentIdentity | null;
  envelopeDigest?: string | null;
  receipt?: Receipt | null;
  routerActivity?: BoundRouterActivity | null;
  phases?: ReviewedExecution["phases"];
}) {
  const { events, routerActivity: live } = useLiveTrace(ns, name, running, activity, runNonce);
  const recorded = runNonce && recordedRouterActivity?.producer.run_nonce === runNonce
    ? recordedRouterActivity : null;
  const routerActivity = recorded ?? (live
    ? { producer: live, observations: live.observations, provisional: true } : null);
  const producers = [
    ...(routerActivity ? [{ name, label: agentLabel, binding: routerActivity }] : []),
    ...phases.flatMap(phase => {
      const binding = reviewedPhaseRouterActivity(phase);
      return binding && phase.task_name
        ? [{ name: phase.task_name, label: `${phase.role} / ${phase.phase}`, binding }] : [];
    }),
  ];
  const [tab, setTab] = useState<"lifetime" | "tools">("lifetime");
  const [selection, setSelection] = useState({ nonce: runNonce, query: "" });
  const focus = selection.nonce === runNonce
    && (!selection.query.startsWith("observations:") || producers.some(producer => matchesRouterFocus(producer.binding, selection.query)))
    ? selection.query : "";
  const setFocus = (query: string) => setSelection({ nonce: runNonce, query });

  const inspect = (query: string) => {
    setFocus(query);
    setTab("tools");
  };

  return (
    <div className="space-y-4">
      <AgentGraph
        key={runNonce ?? "unbound"}
        running={running}
        activity={activity}
        events={events}
        routerActivity={routerActivity}
        phases={phases}
        ns={ns}
        name={name}
        runNonce={runNonce}
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
              Select an agent to focus available round and tool records, or inspect the recorded timeline.
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
              Recorded timeline
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
              key={`lifetime-${runNonce}-${focus}`}
              running={running}
              activity={events}
              routerActivity={routerActivity}
              phases={phases}
              assignmentEvents={assignmentEvents}
              approvals={approvals}
              focusQuery={focus}
            />
          ) : producers.length || phases.length ? (
            <div className="space-y-3">
              {producers.filter(producer => matchesRouterFocus(producer.binding, focus)).map(producer => (
                <div key={routerProducerKey(producer.binding.producer)}>
                  <h3 className="px-3 py-2 text-sm font-semibold">{producer.label}</h3>
                  <RouterActivityPanel binding={producer.binding} ns={ns} name={producer.name} focusQuery={focus} />
                </div>
              ))}
              {!producers.some(producer => matchesRouterFocus(producer.binding, focus)) && (
                <p className="p-4 text-sm text-foreground-muted">
                  No retained router observations match this selection. Phase completion and file evidence remain available in Execution flow.
                </p>
              )}
            </div>
          ) : (
            <ActivityStream
              key={`tools-${focus}`}
              running={running}
              activity={activity}
              events={events}
              telemetry={telemetry}
              ns={ns}
              name={name}
              runNonce={runNonce}
              focusQuery={focus}
              principalAgentName={name}
              title="Rounds and tool records"
              detail="Available revision-bound round and tool records. Missing records do not establish that no work occurred."
            />
          )}
        </div>
      </section>
    </div>
  );
}
