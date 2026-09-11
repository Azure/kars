"use client";

import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { Icon } from "@/components/icon";
import type {
  ActivityEvent,
  AgentIdentity,
  Receipt,
  SubAgent,
} from "@/lib/types";

import {
  ellipsis,
  exactActionQuery,
  exactActionsQuery,
  normalize,
  phaseKind,
  phaseTone,
} from "./agent-graph/activity";
import {
  AGENT_HEIGHT,
  AGENT_WIDTH,
  LEAF_HEIGHT,
  LEAF_WIDTH,
  RECENT_ACTIVITY_MS,
} from "./agent-graph/constants";
import { buildAgentExecutions } from "./agent-graph/execution";
import { ProofPoints, SelectionInspector } from "./agent-graph/inspectors";
import { buildGraphLayout } from "./agent-graph/layout";
import type { AgentExecution, GraphLeaf, GraphSelection } from "./agent-graph/types";

export function AgentGraph({
  running,
  activity,
  ns,
  name,
  agentLabel = "Agent",
  agentPhase = null,
  agentRuntime = null,
  agentModel = null,
  subAgents = [],
  events: externalEvents,
  identity = null,
  envelopeDigest = null,
  receipt = null,
  onInspect,
}: {
  running: boolean;
  activity: ActivityEvent[];
  ns?: string;
  name?: string;
  agentLabel?: string;
  agentPhase?: string | null;
  agentRuntime?: string | null;
  agentModel?: string | null;
  subAgents?: SubAgent[];
  events?: ActivityEvent[];
  identity?: AgentIdentity | null;
  envelopeDigest?: string | null;
  receipt?: Receipt | null;
  onInspect?: (query: string) => void;
}) {
  const [live, setLive] = useState<ActivityEvent[]>([]);
  const [query, setQuery] = useState("");
  const [selectedKey, setSelectedKey] = useState("agent:principal");
  const [selectedAgentId, setSelectedAgentId] = useState("principal");
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(() => new Set());
  const [clientNow, setClientNow] = useState<number | null>(null);
  const [enlarged, setEnlarged] = useState(false);

  useEffect(() => {
    const updateClock = () => setClientNow(Date.now());
    const frame = requestAnimationFrame(updateClock);
    const timer = window.setInterval(updateClock, 15_000);
    return () => {
      cancelAnimationFrame(frame);
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    if (!enlarged) return;
    const priorOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setEnlarged(false);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => {
      document.body.style.overflow = priorOverflow;
      window.removeEventListener("keydown", closeOnEscape);
    };
  }, [enlarged]);

  useEffect(() => {
    if (externalEvents || !running || !ns || !name) return;
    const stream = new EventSource(`/api/namespaces/${ns}/tasks/${name}/stream`);
    stream.onmessage = (message) => {
      try {
        setLive((previous) => [...previous, JSON.parse(message.data)]);
      } catch {
        // Ignore malformed telemetry frames.
      }
    };
    stream.addEventListener("done", () => stream.close());
    return () => stream.close();
  }, [externalEvents, name, ns, running]);

  const events = externalEvents ?? (activity.length >= live.length ? activity : live);
  const hasRecentActivity = (agent: AgentExecution): boolean => {
    if (!running || clientNow === null || !agent.lastActivity) return false;
    const timestamp = new Date(agent.lastActivity).getTime();
    return Number.isFinite(timestamp)
      && clientNow >= timestamp
      && clientNow - timestamp <= RECENT_ACTIVITY_MS;
  };

  const agents = useMemo<AgentExecution[]>(() => buildAgentExecutions({
    agentLabel,
    agentModel,
    agentPhase,
    agentRuntime,
    events,
    name,
    running,
    subAgents,
  }), [agentLabel, agentModel, agentPhase, agentRuntime, events, name, running, subAgents]);

  const normalizedQuery = normalize(query);
  const graph = useMemo(() => buildGraphLayout(agents, expandedGroups, normalizedQuery, selectedAgentId), [agents, expandedGroups, normalizedQuery, selectedAgentId]);

  const selections = useMemo(() => {
    const values = new Map<string, GraphSelection>();
    for (const layout of graph.layouts) {
      values.set(`agent:${layout.agent.id}`, {
        kind: "agent",
        key: `agent:${layout.agent.id}`,
        agent: layout.agent,
      });
      for (const leaf of layout.leaves) values.set(leaf.key, leaf);
      const parentId = graph.parentById.get(layout.agent.id);
      if (parentId) {
        const parent = graph.byId.get(parentId);
        if (parent) {
          const key = `edge:${parent.id}:${layout.agent.id}`;
          values.set(key, { kind: "edge", key, parent, child: layout.agent });
        }
      }
    }
    for (const layout of graph.aggregateLayouts) {
      values.set(layout.aggregate.key, layout.aggregate);
    }
    return values;
  }, [graph]);

  const principal = agents.find((agent) => agent.isPrincipal) ?? agents[0];
  const selected = selections.get(selectedKey)
    ?? selections.get(`agent:${principal.id}`)
    ?? { kind: "agent" as const, key: `agent:${principal.id}`, agent: principal };
  const matchAgent = (agent: AgentExecution) =>
    !normalizedQuery || agent.searchText.includes(normalizedQuery);
  const matchLeaf = (leaf: GraphLeaf) =>
    !normalizedQuery
    || leaf.searchText.includes(normalizedQuery)
    || leaf.agent.identitySearchText.includes(normalizedQuery);
  const matchCount = normalizedQuery
    ? graph.layouts.reduce(
        (count, layout) =>
          count
          + (matchAgent(layout.agent) ? 1 : 0)
          + layout.leaves.filter((leaf) => leaf.searchText.includes(normalizedQuery)).length,
        0,
      )
    : 0;

  const activate = (selection: GraphSelection) => {
    setSelectedKey(selection.key);
    if (selection.kind === "agent") {
      setSelectedAgentId(selection.agent.id);
      onInspect?.(selection.agent.traceQuery);
    } else if (selection.kind === "edge") {
      setSelectedAgentId(selection.child.id);
      onInspect?.(selection.child.traceQuery);
    } else if (selection.kind === "specialist-aggregate") {
      setSelectedAgentId(selection.parent.id);
      setExpandedGroups((current) => {
        const next = new Set(current);
        if (selection.expanded) next.delete(selection.parent.id);
        else next.add(selection.parent.id);
        return next;
      });
      onInspect?.(selection.parent.traceQuery);
    }
    else if (selection.kind === "action") {
      setSelectedAgentId(selection.agent.id);
      onInspect?.(exactActionQuery(selection.agent, selection.action));
    } else if (selection.kind === "folded-actions") {
      setSelectedAgentId(selection.agent.id);
      onInspect?.(exactActionsQuery(selection.agent, selection.actions));
    } else if (selection.kind === "destination") {
      setSelectedAgentId(selection.agent.id);
      onInspect?.(selection.destination);
    } else {
      setSelectedAgentId(selection.agent.id);
      onInspect?.(selection.agent.traceQuery);
    }
  };

  const graphSection = (
    <section
      className={
        enlarged
          ? "h-full w-full overflow-y-auto rounded-2xl border border-border bg-surface p-4 shadow-2xl sm:p-5"
          : "rounded-2xl border border-border bg-surface p-4 sm:p-5"
      }
      aria-label={enlarged ? "Enlarged agent execution graph" : undefined}
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="max-w-2xl">
          <h2 id="execution-graph-title" className="text-sm font-semibold">Agent execution graph</h2>
          <p id="execution-graph-description" className="mt-0.5 text-xs leading-relaxed text-foreground-muted">
            Principal-to-specialist topology with live action and destination leaves. Select nodes for retained evidence or connectors for relationship context.
          </p>
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2">
          <label className="relative">
            <span className="sr-only">Search agents, roles, actions, tools, and destinations</span>
            <Icon
              name="search"
              size={13}
              className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-foreground-muted"
            />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search graph"
              className="w-full min-w-64 rounded-lg border border-border bg-surface-muted/40 py-1.5 pl-8 pr-3 text-xs outline-none focus:border-signal sm:w-72"
            />
          </label>
          {normalizedQuery && (
            <span className="rounded-full border border-border px-2 py-1 text-[10px] text-foreground-muted">
              {matchCount} match{matchCount === 1 ? "" : "es"}
            </span>
          )}
          {running && (
            <span className="inline-flex items-center gap-1.5 rounded-full bg-accent/10 px-2 py-0.5 text-[11px] font-medium text-accent">
              <span className="kb-pulse inline-block h-1.5 w-1.5 rounded-full bg-accent" />
              live
            </span>
          )}
          <button
            type="button"
            onClick={() => setEnlarged((current) => !current)}
            className="rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium text-foreground hover:bg-surface-muted"
            aria-expanded={enlarged}
          >
            {enlarged ? "Close enlarged view" : "Enlarge graph"}
          </button>
        </div>
      </div>

      <div
        className={`mt-5 rounded-xl border border-border bg-surface-muted/20 ${
          enlarged ? "h-[calc(100vh-13rem)] min-h-[32rem] overflow-auto" : "overflow-x-auto"
        }`}
        aria-labelledby="execution-graph-title execution-graph-description"
      >
        <div
          className="relative mx-auto"
          style={{ width: graph.width, height: graph.height }}
        >
          <svg
            className="absolute inset-0 h-full w-full"
            viewBox={`0 0 ${graph.width} ${graph.height}`}
            role="img"
            aria-hidden="true"
          >
            <defs>
              <linearGradient id="agent-graph-surface" x1="0" y1="0" x2="1" y2="1">
                <stop offset="0%" stopColor="var(--surface)" stopOpacity="0.96" />
                <stop offset="100%" stopColor="var(--surface-muted)" stopOpacity="0.35" />
              </linearGradient>
              <filter id="agent-graph-glow" x="-50%" y="-50%" width="200%" height="200%">
                <feGaussianBlur stdDeviation="4" result="blur" />
                <feMerge>
                  <feMergeNode in="blur" />
                  <feMergeNode in="SourceGraphic" />
                </feMerge>
              </filter>
            </defs>
            <rect width={graph.width} height={graph.height} rx="16" fill="url(#agent-graph-surface)" />
            <g opacity="0.32">
              {Array.from({ length: Math.ceil(graph.width / 32) }, (_, index) => (
                <line key={`grid-x-${index}`} x1={index * 32} y1="0" x2={index * 32} y2={graph.height} stroke="var(--border)" strokeWidth="0.5" />
              ))}
              {Array.from({ length: Math.ceil(graph.height / 32) }, (_, index) => (
                <line key={`grid-y-${index}`} x1="0" y1={index * 32} x2={graph.width} y2={index * 32} stroke="var(--border)" strokeWidth="0.5" />
              ))}
            </g>

            {graph.layouts.map((layout) => {
              const parentId = graph.parentById.get(layout.agent.id);
              if (!parentId) return null;
              const parent = graph.byAgentId.get(parentId);
              if (!parent) return null;
              const active = hasRecentActivity(layout.agent);
              const matched = !normalizedQuery || matchAgent(layout.agent) || matchAgent(parent.agent);
              const startX = parent.x + AGENT_WIDTH / 2;
              const startY = parent.y + AGENT_HEIGHT;
              const endX = layout.x + AGENT_WIDTH / 2;
              const endY = layout.y;
              const bend = Math.max(44, (endY - startY) * 0.5);
              return (
                <path
                  key={`delegation-path-${layout.agent.id}`}
                  d={`M ${startX} ${startY} C ${startX} ${startY + bend}, ${endX} ${endY - bend}, ${endX} ${endY}`}
                  fill="none"
                  stroke={phaseKind(layout.agent.phase) === "failed" ? "var(--danger)" : "var(--signal)"}
                  strokeWidth={active ? 2.5 : 1.75}
                  strokeDasharray={active ? "7 7" : undefined}
                  className={active ? "kb-graph-edge-active" : undefined}
                  opacity={matched ? 0.78 : 0.14}
                />
              );
            })}

            {graph.aggregateLayouts.map((layout) => {
              const parent = graph.byAgentId.get(layout.aggregate.parent.id);
              if (!parent) return null;
              const startX = parent.x + AGENT_WIDTH / 2;
              const startY = parent.y + AGENT_HEIGHT;
              const endX = layout.x + AGENT_WIDTH / 2;
              const endY = layout.y;
              const bend = Math.max(44, (endY - startY) * 0.5);
              const matched = !normalizedQuery || layout.aggregate.searchText.includes(normalizedQuery);
              return (
                <path
                  key={`aggregate-path-${layout.aggregate.key}`}
                  d={`M ${startX} ${startY} C ${startX} ${startY + bend}, ${endX} ${endY - bend}, ${endX} ${endY}`}
                  fill="none"
                  stroke="var(--accent)"
                  strokeWidth="1.75"
                  strokeDasharray="4 6"
                  opacity={matched ? 0.7 : 0.12}
                />
              );
            })}

            {graph.layouts.flatMap((layout) =>
              layout.leaves.map((leaf) => {
                const active =
                  hasRecentActivity(layout.agent)
                  && leaf.kind === "action"
                  && layout.agent.actions.at(-1)?.id === leaf.action.id;
                const startX = layout.x + AGENT_WIDTH;
                const startY = layout.y + AGENT_HEIGHT / 2;
                const endX = leaf.x;
                const endY = leaf.y + LEAF_HEIGHT / 2;
                return (
                  <path
                    key={`leaf-path-${leaf.key}`}
                    d={`M ${startX} ${startY} C ${startX + 16} ${startY}, ${endX - 16} ${endY}, ${endX} ${endY}`}
                    fill="none"
                    stroke={leaf.kind === "destination" || leaf.kind === "folded-destinations" ? "var(--accent)" : "var(--signal)"}
                    strokeWidth={active ? 2 : 1.25}
                    strokeDasharray={active ? "5 6" : leaf.kind.startsWith("folded") ? "3 5" : undefined}
                    className={active ? "kb-graph-edge-active" : undefined}
                    opacity={matchLeaf(leaf) ? 0.58 : 0.1}
                  />
                );
              }),
            )}
          </svg>

          {graph.layouts.map((layout) => {
            const kind = phaseKind(layout.agent.phase);
            const recentlyActive = hasRecentActivity(layout.agent);
            const selectedAgent = selected.key === `agent:${layout.agent.id}`;
            const latest = layout.agent.actions.at(-1);
            return (
              <button
                key={`agent-node-${layout.agent.id}`}
                type="button"
                onClick={() => activate({
                  kind: "agent",
                  key: `agent:${layout.agent.id}`,
                  agent: layout.agent,
                })}
                aria-pressed={selectedAgent}
                aria-label={`Inspect ${layout.agent.displayName}, ${layout.agent.role}, ${layout.agent.phase}`}
                className={`absolute overflow-hidden rounded-2xl border px-4 py-3 text-left shadow-sm transition-[opacity,border-color,box-shadow,transform] hover:-translate-y-0.5 hover:shadow-lg ${
                  selectedAgent
                    ? "border-signal bg-surface shadow-lg ring-2 ring-signal/20"
                    : layout.agent.isPrincipal
                      ? "border-signal/55 bg-surface"
                      : "border-border bg-surface"
                } ${recentlyActive ? "kb-agent-node-active" : ""} ${matchAgent(layout.agent) ? "opacity-100" : "opacity-25"}`}
                style={{ left: layout.x, top: layout.y, width: AGENT_WIDTH, height: AGENT_HEIGHT }}
              >
                <span className="flex items-center gap-2">
                  <span className={`relative flex h-8 w-8 shrink-0 items-center justify-center rounded-xl border text-[10px] font-bold uppercase ${
                    layout.agent.isPrincipal
                      ? "border-signal/40 bg-signal/10 text-signal"
                      : "border-accent/35 bg-accent/10 text-accent"
                  }`}>
                    {layout.agent.isPrincipal ? "core" : "agt"}
                    {recentlyActive && <span className="kb-graph-orbit absolute -inset-1 rounded-[14px] border border-signal/45" />}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="flex items-center gap-1.5">
                      <span className="truncate text-sm font-semibold">{layout.agent.displayName}</span>
                      {layout.agent.isPrincipal && (
                        <span className="rounded-full bg-signal/10 px-1.5 py-0.5 text-[8px] font-bold uppercase tracking-wider text-signal">
                          root
                        </span>
                      )}
                    </span>
                    <span className="block truncate text-[10px] font-medium text-foreground-muted">{layout.agent.role}</span>
                  </span>
                  <span className={`h-2.5 w-2.5 shrink-0 rounded-full ${
                    kind === "failed" ? "bg-danger" : kind === "active" ? "bg-signal" : kind === "paused" ? "bg-accent" : "bg-foreground-muted/50"
                  }`} />
                </span>
                <span className="mt-2 flex items-center gap-1.5 text-[9px]">
                  <span className={`rounded-full border px-1.5 py-0.5 font-medium ${phaseTone(layout.agent.phase)}`}>
                    {layout.agent.phase}
                  </span>
                  {(layout.agent.model || layout.agent.runtime) && (
                    <span className="truncate rounded-full border border-border bg-surface-muted/40 px-1.5 py-0.5 font-mono text-foreground-muted">
                      {layout.agent.model ?? layout.agent.runtime}
                    </span>
                  )}
                </span>
                <span className="mt-2 block border-t border-border/70 pt-2 text-[10px] text-foreground-muted">
                  <span className="font-medium text-foreground">{layout.agent.actions.length} records</span>
                  {" · "}
                  {ellipsis(latest?.human ?? "Awaiting first action", 34)}
                </span>
              </button>
            );
          })}

          {graph.aggregateLayouts.map((layout) => {
            const aggregate = layout.aggregate;
            const matched = !normalizedQuery || aggregate.searchText.includes(normalizedQuery);
            return (
              <button
                key={`aggregate-node-${aggregate.key}`}
                type="button"
                onClick={() => activate(aggregate)}
                aria-expanded={aggregate.expanded}
                aria-pressed={selected.key === aggregate.key}
                aria-label={`${aggregate.expanded ? "Collapse" : "Expand"} specialists delegated by ${aggregate.parent.displayName}`}
                className={`absolute flex flex-col justify-center rounded-2xl border border-dashed px-4 py-3 text-left shadow-sm transition-[opacity,border-color,box-shadow,transform] hover:-translate-y-0.5 hover:shadow-md ${
                  selected.key === aggregate.key
                    ? "border-accent bg-accent/[0.08] ring-2 ring-accent/20"
                    : "border-accent/45 bg-surface"
                } ${matched ? "opacity-100" : "opacity-20"}`}
                style={{ left: layout.x, top: layout.y, width: AGENT_WIDTH, height: 86 }}
              >
                <span className="flex w-full items-center gap-2">
                  <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-xl border border-accent/35 bg-accent/10 text-sm font-bold text-accent">
                    {aggregate.expanded ? "−" : "+"}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-semibold">{aggregate.label}</span>
                    <span className="block text-[10px] text-foreground-muted">
                      {aggregate.expanded ? "Activate to fold this sibling group" : "Activate to reveal this sibling group"}
                    </span>
                  </span>
                </span>
              </button>
            );
          })}

          {graph.layouts.flatMap((layout) =>
            layout.leaves.map((leaf) => {
              const leafSelected = selected.key === leaf.key;
              const isDestination = leaf.kind === "destination" || leaf.kind === "folded-destinations";
              const isFolded = leaf.kind === "folded-actions" || leaf.kind === "folded-destinations";
              const failed = leaf.kind === "action" && leaf.action.ok === false;
              return (
                <button
                  key={`leaf-node-${leaf.key}`}
                  type="button"
                  onClick={() => activate(leaf)}
                  aria-pressed={leafSelected}
                  aria-label={`Inspect ${leaf.label} for ${layout.agent.displayName}`}
                  className={`absolute flex items-center gap-2 rounded-xl border px-3 py-2 text-left shadow-sm transition-[opacity,border-color,box-shadow,transform] hover:-translate-y-0.5 hover:shadow-md ${
                    leafSelected
                      ? "border-signal bg-surface ring-2 ring-signal/20"
                      : failed
                        ? "border-danger/30 bg-danger/[0.06]"
                        : isDestination
                          ? "border-accent/30 bg-surface"
                          : "border-border bg-surface"
                  } ${matchLeaf(leaf) ? "opacity-100" : "opacity-20"}`}
                  style={{ left: leaf.x, top: leaf.y, width: LEAF_WIDTH, height: LEAF_HEIGHT }}
                >
                  <span className={`flex h-6 w-6 shrink-0 items-center justify-center rounded-lg text-[10px] font-bold ${
                    failed
                      ? "bg-danger/10 text-danger"
                      : isDestination
                        ? "bg-accent/10 text-accent"
                        : isFolded
                          ? "border border-dashed border-signal/40 text-signal"
                          : "bg-signal/10 text-signal"
                  }`}>
                    {failed ? "!" : isDestination ? "↗" : isFolded ? "+" : "→"}
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate text-[10px] font-semibold">{leaf.label}</span>
                    <span className="block truncate text-[9px] text-foreground-muted">
                      {isDestination ? "destination" : isFolded ? "folded cluster" : "recorded action"}
                    </span>
                  </span>
                </button>
              );
            }),
          )}

          {graph.layouts.map((layout) => {
            const parentId = graph.parentById.get(layout.agent.id);
            if (!parentId) return null;
            const parent = graph.byAgentId.get(parentId);
            if (!parent) return null;
            const key = `edge:${parent.agent.id}:${layout.agent.id}`;
            const edge = selections.get(key);
            if (!edge || edge.kind !== "edge") return null;
            const x = ((parent.x + AGENT_WIDTH / 2) + (layout.x + AGENT_WIDTH / 2)) / 2;
            const y = ((parent.y + AGENT_HEIGHT) + layout.y) / 2;
            const matched = !normalizedQuery || matchAgent(parent.agent) || matchAgent(layout.agent);
            return (
              <button
                key={`edge-control-${key}`}
                type="button"
                onClick={() => activate(edge)}
                aria-pressed={selected.key === key}
                aria-label={`Inspect delegation from ${parent.agent.displayName} to ${layout.agent.displayName}`}
                className={`absolute flex h-7 w-7 items-center justify-center rounded-full border bg-surface text-[11px] font-bold shadow-sm transition hover:scale-110 ${
                  selected.key === key ? "border-signal text-signal ring-2 ring-signal/20" : "border-border text-foreground-muted"
                } ${matched ? "opacity-100" : "opacity-20"}`}
                style={{ left: x - 14, top: y - 14 }}
              >
                ↓
              </button>
            );
          })}

          {graph.aggregateLayouts.map((layout) => {
            const parent = graph.byAgentId.get(layout.aggregate.parent.id);
            if (!parent) return null;
            const x = ((parent.x + AGENT_WIDTH / 2) + (layout.x + AGENT_WIDTH / 2)) / 2;
            const y = ((parent.y + AGENT_HEIGHT) + layout.y) / 2;
            const matched = !normalizedQuery || layout.aggregate.searchText.includes(normalizedQuery);
            return (
              <button
                key={`aggregate-edge-control-${layout.aggregate.key}`}
                type="button"
                onClick={() => activate(layout.aggregate)}
                aria-label={`${layout.aggregate.expanded ? "Collapse" : "Expand"} folded specialist connection`}
                className={`absolute flex h-7 w-7 items-center justify-center rounded-full border border-accent/40 bg-surface text-[11px] font-bold text-accent shadow-sm transition hover:scale-110 ${
                  matched ? "opacity-100" : "opacity-20"
                }`}
                style={{ left: x - 14, top: y - 14 }}
              >
                {layout.aggregate.expanded ? "−" : "+"}
              </button>
            );
          })}

          {graph.layouts.flatMap((layout) =>
            layout.leaves.map((leaf) => {
              const x = (layout.x + AGENT_WIDTH + leaf.x) / 2;
              const y = ((layout.y + AGENT_HEIGHT / 2) + (leaf.y + LEAF_HEIGHT / 2)) / 2;
              return (
                <button
                  key={`leaf-edge-control-${leaf.key}`}
                  type="button"
                  onClick={() => activate(leaf)}
                  aria-label={`Inspect connection to ${leaf.label}`}
                  className={`absolute flex h-5 w-5 items-center justify-center rounded-full border border-border bg-surface text-[9px] text-foreground-muted shadow-sm transition hover:border-signal hover:text-signal ${
                    matchLeaf(leaf) ? "opacity-100" : "opacity-15"
                  }`}
                  style={{ left: x - 10, top: y - 10 }}
                >
                  ›
                </button>
              );
            }),
          )}

          {agents.length === 1 && graph.layouts[0]?.leaves.length === 0 && (
            <div
              className="absolute rounded-xl border border-dashed border-border bg-surface/85 px-4 py-3 text-xs text-foreground-muted"
              style={{
                left: graph.layouts[0].x + AGENT_WIDTH + 28,
                top: graph.layouts[0].y + 30,
                width: LEAF_WIDTH,
              }}
            >
              {running
                ? "The principal is live. Action leaves will unfold here."
                : events.length > 0
                  ? "This execution completed without retained tool actions."
                  : "Launch the execution to populate its action graph."}
            </div>
          )}
        </div>
      </div>

      <SelectionInspector selection={selected} />

      <ProofPoints
        envelopeDigest={envelopeDigest}
        identity={identity}
        subCount={agents.filter((agent) => !agent.isPrincipal).length}
        receipt={receipt}
      />
    </section>
  );
  if (enlarged && typeof document !== "undefined") {
    return createPortal(
      <div className="fixed inset-0 z-[100] bg-background/85 p-2 backdrop-blur-sm sm:p-4">
        {graphSection}
      </div>,
      document.body,
    );
  }
  return graphSection;
}
