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

type ToolEvent = Extract<ActivityEvent, { kind: "tool" }>;

interface AgentAction {
  id: string;
  human: string;
  raw: string;
  args: string;
  result: string;
  ok: boolean | null;
  round: number;
  ms: number;
  ts: string;
  seq: number | null;
  agentInstance: string | null;
}

interface AgentExecution {
  id: string;
  displayName: string;
  technicalName: string;
  role: string;
  relationship: string;
  phase: string;
  runtime: string | null;
  model: string | null;
  parent: string | null;
  parentId: string | null;
  observedAgentName: string | null;
  observedAgentInstance: string | null;
  isPrincipal: boolean;
  aliases: string[];
  rounds: number;
  toolCalls: number;
  failures: number;
  lastActivity: string | null;
  destinations: string[];
  actions: AgentAction[];
  identitySearchText: string;
  searchText: string;
  traceQuery: string;
}

type GraphLeaf =
  | { kind: "action"; key: string; agent: AgentExecution; action: AgentAction; label: string; searchText: string }
  | { kind: "folded-actions"; key: string; agent: AgentExecution; actions: AgentAction[]; label: string; searchText: string }
  | { kind: "destination"; key: string; agent: AgentExecution; destination: string; label: string; searchText: string }
  | { kind: "folded-destinations"; key: string; agent: AgentExecution; destinations: string[]; label: string; searchText: string };

type GraphSelection =
  | { kind: "agent"; key: string; agent: AgentExecution }
  | { kind: "edge"; key: string; parent: AgentExecution; child: AgentExecution }
  | GraphAggregate
  | GraphLeaf;

interface AgentLayout {
  agent: AgentExecution;
  x: number;
  y: number;
  leaves: Array<GraphLeaf & { x: number; y: number }>;
}

interface GraphAggregate {
  kind: "specialist-aggregate";
  key: string;
  parent: AgentExecution;
  agents: AgentExecution[];
  expanded: boolean;
  label: string;
  searchText: string;
}

interface AggregateLayout {
  aggregate: GraphAggregate;
  x: number;
  y: number;
}

const AGENT_WIDTH = 248;
const AGENT_HEIGHT = 128;
const LEAF_WIDTH = 194;
const LEAF_HEIGHT = 42;
const LEAF_GAP = 9;
const LANE_WIDTH = 500;
const CLUSTER_WIDTH = AGENT_WIDTH + 28 + LEAF_WIDTH;
const GRAPH_PADDING = 40;
const ACTION_LIMIT = 3;
const DESTINATION_LIMIT = 2;
const SPECIALIST_LIMIT = 6;
const VISIBLE_AGENT_BUDGET = 24;
const COLLAPSED_DEPTH_LIMIT = 4;
const BASELINE_LAYER_LIMIT = 6;
const RECENT_ACTIVITY_MS = 90_000;

function normalize(value: string): string {
  return value.trim().toLowerCase();
}

function humanizeTool(name: string): string {
  const tool = name.toLowerCase().replaceAll("-", "_");
  if (/(browser_)?navigate|open_url|goto/.test(tool)) return "Opened a browser page";
  if (/screenshot|capture_screen/.test(tool)) return "Captured a screenshot";
  if (/browser_(click|dblclick)|click_element/.test(tool)) return "Clicked a page control";
  if (/fill_form|browser_fill|select_option/.test(tool)) return "Filled in a form";
  if (/browser_type|press_key|keyboard/.test(tool)) return "Entered text on a page";
  if (/browser_snapshot|page_snapshot|accessibility_tree/.test(tool)) return "Inspected a browser page";
  if (/browser_wait|wait_for/.test(tool)) return "Waited for a page update";
  if (/network_request|network_requests/.test(tool)) return "Inspected browser network activity";
  if (/file_upload/.test(tool)) return "Uploaded a file";
  if (/(web_)?search|brave|tavily|exa|perplexity/.test(tool)) return "Searched the web";
  if (/fetch|http|curl|crawl|download/.test(tool)) return "Retrieved network content";
  if (/pull_request|create_pr|open_pr/.test(tool)) return "Worked with a pull request";
  if (/git|commit|branch|push|pull/.test(tool)) return "Worked with source control";
  if (/write|create_file|save|edit|patch|append/.test(tool)) return "Updated a file";
  if (/read|view|list|glob|grep|find|search_files/.test(tool)) return "Inspected files";
  if (/shell|bash|exec|run_command|terminal/.test(tool)) return "Ran a command";
  if (/message|handoff|send|relay/.test(tool)) return "Sent an agent message";
  return "Used a governed tool";
}

function meaningfulDestination(host: string): boolean {
  const value = host.toLowerCase().replace(/^\[|\]$/g, "");
  return !/^(localhost|0\.0\.0\.0|127(?:\.\d+){3}|::1)(:\d+)?$/.test(value);
}

function destinationsFrom(event: ToolEvent): string[] {
  const values = `${event.args_preview} ${event.result_preview}`;
  const destinations = new Set<string>();
  for (const match of values.matchAll(/https?:\/\/([^/\s"')\]]+)/gi)) {
    const host = match[1].toLowerCase().replace(/[.,;]+$/, "");
    if (meaningfulDestination(host)) destinations.add(host);
  }
  return [...destinations];
}

function formatLastActivity(value: string | null): string {
  if (!value) return "No recorded activity";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const months = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  const pad = (part: number) => String(part).padStart(2, "0");
  return `${months[date.getUTCMonth()]} ${date.getUTCDate()}, ${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())} UTC`;
}

function phaseKind(phase: string): "active" | "failed" | "paused" | "complete" | "idle" {
  const value = phase.toLowerCase();
  if (/(failed|degraded|error|blocked)/.test(value)) return "failed";
  if (/(running|launching|active|working|executing)/.test(value)) return "active";
  if (/(paused|hibernating|suspended|waiting)/.test(value)) return "paused";
  if (/(completed|finished|succeeded|delivered)/.test(value)) return "complete";
  return "idle";
}

function phaseTone(phase: string): string {
  switch (phaseKind(phase)) {
    case "failed":
      return "border-danger/35 bg-danger/10 text-danger";
    case "active":
      return "border-signal/35 bg-signal/10 text-signal";
    case "paused":
      return "border-accent/35 bg-accent/10 text-accent";
    case "complete":
      return "border-signal/25 bg-signal/[0.06] text-foreground-muted";
    default:
      return "border-border bg-surface-muted text-foreground-muted";
  }
}

function actionFromEvent(event: ActivityEvent, index: number): AgentAction {
  if (event.kind === "round") {
    return {
      id: `round-${event.agent ?? "principal"}-${event.round}-${index}`,
      human: `Completed model round ${event.round + 1}`,
      raw: "model.round",
      args: `${event.tool_calls} tool call${event.tool_calls === 1 ? "" : "s"} requested`,
      result: `${event.finish_reason || "unknown finish"}; ${event.total_tokens.toLocaleString("en-US")} tokens`,
      ok: null,
      round: event.round,
      ms: event.ms,
      ts: event.ts,
      seq: event.seq ?? null,
      agentInstance: event.agentInstance ?? null,
    };
  }
  return {
    id: `tool-${event.agent ?? "principal"}-${event.round}-${index}`,
    human: humanizeTool(event.name),
    raw: event.name,
    args: event.args_preview,
    result: event.result_preview,
    ok: event.ok,
    round: event.round,
    ms: event.ms,
    ts: event.ts,
    seq: event.seq ?? null,
    agentInstance: event.agentInstance ?? null,
  };
}

function leavesFor(agent: AgentExecution): GraphLeaf[] {
  const foldedActions = agent.actions.slice(0, Math.max(0, agent.actions.length - ACTION_LIMIT));
  const visibleActions = agent.actions.slice(-ACTION_LIMIT);
  const visibleDestinations = agent.destinations.slice(0, DESTINATION_LIMIT);
  const foldedDestinations = agent.destinations.slice(DESTINATION_LIMIT);
  const leaves: GraphLeaf[] = visibleActions.map((action) => ({
    kind: "action",
    key: `action:${agent.id}:${action.id}`,
    agent,
    action,
    label: action.human,
    searchText: normalize(`${action.human} ${action.raw} ${action.args} ${action.result}`),
  }));
  if (foldedActions.length > 0) {
    leaves.unshift({
      kind: "folded-actions",
      key: `folded-actions:${agent.id}`,
      agent,
      actions: foldedActions,
      label: `+${foldedActions.length} action${foldedActions.length === 1 ? "" : "s"}`,
      searchText: normalize(foldedActions.flatMap((action) => [action.human, action.raw, action.args, action.result]).join(" ")),
    });
  }
  leaves.push(...visibleDestinations.map((destination) => ({
    kind: "destination" as const,
    key: `destination:${agent.id}:${destination}`,
    agent,
    destination,
    label: destination,
    searchText: normalize(destination),
  })));
  if (foldedDestinations.length > 0) {
    leaves.push({
      kind: "folded-destinations",
      key: `folded-destinations:${agent.id}`,
      agent,
      destinations: foldedDestinations,
      label: `+${foldedDestinations.length} destination${foldedDestinations.length === 1 ? "" : "s"}`,
      searchText: normalize(foldedDestinations.join(" ")),
    });
  }
  return leaves;
}

function ellipsis(value: string, length: number): string {
  return value.length > length ? `${value.slice(0, length - 1)}…` : value;
}

function queryAgentIdentity(agent: AgentExecution): string {
  return agent.isPrincipal
    ? "principal"
    : normalize(agent.observedAgentName ?? agent.technicalName);
}

function exactActionQuery(agent: AgentExecution, action: AgentAction): string {
  const parameters = new URLSearchParams({
    agent: queryAgentIdentity(agent),
    round: String(action.round),
    ts: action.ts,
    tool: action.raw,
    args: action.args,
    result: action.result,
  });
  if (action.seq != null) parameters.set("seq", String(action.seq));
  if (action.agentInstance) parameters.set("instance", action.agentInstance);
  return `action:${parameters.toString()}`;
}

function exactActionsQuery(agent: AgentExecution, actions: AgentAction[]): string {
  const through = actions.at(-1)?.ts ?? "";
  const parameters = new URLSearchParams({
    agent: queryAgentIdentity(agent),
    through,
  });
  const sequenceValues = actions.flatMap((action) =>
    action.seq == null ? [] : [String(action.seq)]
  );
  if (sequenceValues.length === actions.length && sequenceValues.length > 0) {
    parameters.set("seqs", sequenceValues.join(","));
  } else {
    parameters.set(
      "events",
      JSON.stringify(actions.map((action) => ({
        round: action.round,
        ts: action.ts,
        tool: action.raw,
        args: action.args,
        result: action.result,
        instance: action.agentInstance,
      }))),
    );
  }
  const instances = [...new Set(actions.flatMap((action) =>
    action.agentInstance ? [action.agentInstance] : []
  ))];
  if (instances.length === 1) parameters.set("instance", instances[0]);
  return `actions:${parameters.toString()}`;
}

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

  const agents = useMemo<AgentExecution[]>(() => {
    const rootInactive = Boolean(
      !running
      && agentPhase
      && /(completed|failed|hibernating|paused|idle|finished)/i.test(agentPhase),
    );
    const definitions = [
      {
        id: "principal",
        displayName: agentLabel,
        technicalName: name ?? agentLabel,
        role: "Principal",
        phase: agentPhase ?? (running ? "Running" : events.length > 0 ? "Finished" : "Ready"),
        runtime: agentRuntime,
        model: agentModel,
        parent: null,
        isPrincipal: true,
        aliases: ["principal", agentLabel, name ?? ""].filter(Boolean),
      },
      ...subAgents.map((agent) => ({
        id: `sub-${agent.name}`,
        displayName: agent.logical_agent_id ?? agent.role ?? agent.name,
        technicalName: agent.name,
        role: agent.role ?? "Specialist",
        phase: rootInactive ? agentPhase! : agent.phase ?? "Discovered",
        runtime: agent.runtime,
        model: agent.model,
        parent: agent.parent,
        isPrincipal: false,
        aliases: [agent.name, agent.logical_agent_id ?? "", agent.role ?? ""].filter(Boolean),
      })),
    ];

    const records = new Map<string, ActivityEvent[]>(
      definitions.map((definition) => [definition.id, []]),
    );
    const aliasToId = new Map<string, string>();
    for (const definition of definitions) {
      for (const alias of definition.aliases) aliasToId.set(normalize(alias), definition.id);
    }

    for (const event of events) {
      let owner = "principal";
      if (event.agentRole === "subagent" && event.agent) {
        const instance = event.agentInstance ?? event.agent;
        owner = aliasToId.get(normalize(instance))
          ?? aliasToId.get(normalize(event.agent))
          ?? `live-${instance}`;
        if (!records.has(owner)) records.set(owner, []);
      }
      records.get(owner)?.push(event);
    }

    const allDefinitions = [...definitions];
    for (const [id] of records) {
      if (!id.startsWith("live-")) continue;
      const technicalName = id.slice(5);
      allDefinitions.push({
        id,
        displayName: technicalName,
        technicalName,
        role: "Specialist",
        phase: running ? "Active" : "Observed",
        runtime: null,
        model: null,
        parent: null,
        isPrincipal: false,
        aliases: [technicalName],
      });
    }

    const completeAliasToId = new Map<string, string>();
    for (const definition of allDefinitions) {
      for (const alias of definition.aliases) completeAliasToId.set(normalize(alias), definition.id);
      completeAliasToId.set(normalize(definition.technicalName), definition.id);
    }
    const namesById = new Map(allDefinitions.map((definition) => [definition.id, definition.displayName]));

    return allDefinitions.map((definition) => {
      const agentEvents = records.get(definition.id) ?? [];
      const actions = agentEvents
        .map(actionFromEvent)
        .sort((left, right) => left.ts.localeCompare(right.ts));
      const tools = agentEvents.filter(
        (event): event is ToolEvent => event.kind === "tool",
      );
      const destinations = [...new Set(tools.flatMap(destinationsFrom))].sort();
      const lastActivity = agentEvents.reduce<string | null>(
        (latest, event) => (!latest || event.ts > latest ? event.ts : latest),
        null,
      );
      const observedAgentName = agentEvents.find((event) => event.agent)?.agent ?? null;
      const observedAgentInstance =
        agentEvents.find((event) => event.agentInstance)?.agentInstance ?? null;
      const parentId = definition.isPrincipal
        ? null
        : completeAliasToId.get(normalize(definition.parent ?? "")) ?? "principal";
      const parentName = parentId ? namesById.get(parentId) ?? agentLabel : null;
      const relationship = definition.isPrincipal
        ? "Orchestration root responsible for the execution"
        : `Specialist delegated by ${parentName}`;
      const searchText = [
        definition.displayName,
        definition.technicalName,
        definition.role,
        relationship,
        definition.runtime,
        definition.model,
        definition.phase,
        ...destinations,
        ...actions.flatMap((action) => [action.human, action.raw, action.args, action.result]),
      ]
        .filter(Boolean)
        .join(" ")
        .toLowerCase();
      const identitySearchText = [
        definition.displayName,
        definition.technicalName,
        definition.role,
        relationship,
        definition.runtime,
        definition.model,
        definition.phase,
      ]
        .filter(Boolean)
        .join(" ")
        .toLowerCase();

      return {
        ...definition,
        relationship,
        parentId,
        observedAgentName,
        observedAgentInstance,
        rounds: agentEvents.filter((event) => event.kind === "round").length,
        toolCalls: tools.length,
        failures: tools.filter((event) => !event.ok).length,
        lastActivity,
        destinations,
        actions,
        identitySearchText,
        searchText,
        traceQuery: definition.isPrincipal
          ? "agent:principal"
          : observedAgentInstance
            ? `agent-instance:${normalize(observedAgentInstance)}`
            : `agent:${normalize(observedAgentName ?? definition.technicalName)}`,
      };
    });
  }, [agentLabel, agentModel, agentPhase, agentRuntime, events, name, running, subAgents]);

  const normalizedQuery = normalize(query);
  const graph = useMemo(() => {
    const byId = new Map(agents.map((agent) => [agent.id, agent]));
    const principal = agents.find((agent) => agent.isPrincipal) ?? agents[0];
    const parentById = new Map<string, string | null>();

    for (const agent of agents) {
      if (agent.isPrincipal) {
        parentById.set(agent.id, null);
        continue;
      }
      const candidate = agent.parentId && byId.has(agent.parentId)
        ? agent.parentId
        : principal.id;
      const seen = new Set([agent.id]);
      let cursor: string | null = candidate;
      let cyclic = false;
      while (cursor) {
        if (seen.has(cursor)) {
          cyclic = true;
          break;
        }
        seen.add(cursor);
        cursor = byId.get(cursor)?.parentId ?? null;
      }
      parentById.set(agent.id, cyclic ? principal.id : candidate);
    }

    const childrenByParent = new Map<string, AgentExecution[]>();
    for (const agent of agents) {
      const parentId = parentById.get(agent.id);
      if (!parentId) continue;
      childrenByParent.set(parentId, [...(childrenByParent.get(parentId) ?? []), agent]);
    }

    const depthCache = new Map<string, number>([[principal.id, 0]]);
    const depthOf = (agentId: string): number => {
      const cached = depthCache.get(agentId);
      if (cached != null) return cached;
      const parentId = parentById.get(agentId);
      const depth = parentId ? depthOf(parentId) + 1 : 0;
      depthCache.set(agentId, depth);
      return depth;
    };

    const selectedPath = new Set<string>([principal.id, selectedAgentId]);
    let selectedParentId = parentById.get(selectedAgentId);
    while (selectedParentId) {
      selectedPath.add(selectedParentId);
      selectedParentId = parentById.get(selectedParentId);
    }
    const searchPath = new Set<string>();
    if (normalizedQuery) {
      for (const agent of agents) {
        if (!agent.searchText.includes(normalizedQuery)) continue;
        searchPath.add(agent.id);
        let parentId = parentById.get(agent.id);
        while (parentId) {
          if (searchPath.has(parentId)) break;
          searchPath.add(parentId);
          parentId = parentById.get(parentId);
        }
      }
    }

    const baselineVisible = new Set<string>([principal.id]);
    const baselineLayerCounts = new Map<number, number>([[0, 1]]);
    const queue: AgentExecution[] = [principal];
    for (let index = 0; index < queue.length && baselineVisible.size < VISIBLE_AGENT_BUDGET; index += 1) {
      const parent = queue[index];
      const parentDepth = depthOf(parent.id);
      if (parentDepth >= COLLAPSED_DEPTH_LIMIT) continue;
      const childDepth = parentDepth + 1;
      const children = childrenByParent.get(parent.id) ?? [];
      for (const child of children.slice(0, SPECIALIST_LIMIT)) {
        if (baselineVisible.size >= VISIBLE_AGENT_BUDGET) break;
        const layerCount = baselineLayerCounts.get(childDepth) ?? 0;
        if (layerCount >= BASELINE_LAYER_LIMIT) break;
        baselineVisible.add(child.id);
        baselineLayerCounts.set(childDepth, layerCount + 1);
        queue.push(child);
      }
    }

    const visible = new Set<string>([
      ...baselineVisible,
      ...selectedPath,
      ...searchPath,
    ]);
    let expandedChanged = true;
    while (expandedChanged) {
      expandedChanged = false;
      for (const parentId of expandedGroups) {
        if (!visible.has(parentId)) continue;
        for (const child of childrenByParent.get(parentId) ?? []) {
          if (visible.has(child.id)) continue;
          visible.add(child.id);
          expandedChanged = true;
        }
      }
    }

    const aggregates: GraphAggregate[] = [];
    for (const parent of agents) {
      if (!visible.has(parent.id)) continue;
      const children = childrenByParent.get(parent.id) ?? [];
      const expanded = expandedGroups.has(parent.id);
      const hidden = children.filter((child) => !visible.has(child.id));
      if (hidden.length === 0 && !expanded) continue;
      const aggregateAgents = expanded ? children : hidden;
      if (aggregateAgents.length === 0) continue;
      aggregates.push({
        kind: "specialist-aggregate",
        key: `specialist-aggregate:${parent.id}`,
        parent,
        agents: aggregateAgents,
        expanded,
        label: expanded
          ? `Collapse ${children.length} specialists`
          : `+${hidden.length} specialist${hidden.length === 1 ? "" : "s"}`,
        searchText: normalize([
          "specialists agents descendants folded expand collapse",
          ...aggregateAgents.map((agent) => agent.searchText),
        ].filter(Boolean).join(" ")),
      });
    }

    type LayerItem =
      | { kind: "agent"; agent: AgentExecution }
      | { kind: "aggregate"; aggregate: GraphAggregate };
    const layers = new Map<number, LayerItem[]>();
    for (const agent of agents) {
      if (!visible.has(agent.id)) continue;
      const depth = depthOf(agent.id);
      layers.set(depth, [...(layers.get(depth) ?? []), { kind: "agent", agent }]);
    }
    for (const aggregate of aggregates) {
      const depth = depthOf(aggregate.parent.id) + 1;
      layers.set(depth, [...(layers.get(depth) ?? []), { kind: "aggregate", aggregate }]);
    }

    const orderedLayers = [...layers.entries()].sort(([left], [right]) => left - right);
    const largestLayer = Math.max(1, ...orderedLayers.map(([, layer]) => layer.length));
    const width = Math.max(760, largestLayer * LANE_WIDTH + GRAPH_PADDING * 2);
    const layouts: AgentLayout[] = [];
    const aggregateLayouts: AggregateLayout[] = [];
    let rankY = 54;

    for (const [, layer] of orderedLayers) {
      const leafSets = layer.map((item) => item.kind === "agent" ? leavesFor(item.agent) : []);
      const rankHeight = Math.max(
        AGENT_HEIGHT,
        ...leafSets.map((leaves) => Math.max(AGENT_HEIGHT, leaves.length * (LEAF_HEIGHT + LEAF_GAP) - LEAF_GAP)),
      );
      const layerWidth = layer.length * LANE_WIDTH;
      const layerStart = (width - layerWidth) / 2;
      layer.forEach((item, index) => {
        const laneStart = layerStart + index * LANE_WIDTH;
        if (item.kind === "aggregate") {
          aggregateLayouts.push({
            aggregate: item.aggregate,
            x: laneStart + (LANE_WIDTH - AGENT_WIDTH) / 2,
            y: rankY + (rankHeight - 86) / 2,
          });
          return;
        }
        const leaves = leafSets[index];
        const leafStackHeight = leaves.length > 0
          ? leaves.length * (LEAF_HEIGHT + LEAF_GAP) - LEAF_GAP
          : 0;
        const x = laneStart + (LANE_WIDTH - CLUSTER_WIDTH) / 2;
        const y = rankY + Math.max(0, (rankHeight - AGENT_HEIGHT) / 2);
        const leafY = rankY + Math.max(0, (rankHeight - leafStackHeight) / 2);
        layouts.push({
          agent: item.agent,
          x,
          y,
          leaves: leaves.map((leaf, leafIndex) => ({
            ...leaf,
            x: x + AGENT_WIDTH + 28,
            y: leafY + leafIndex * (LEAF_HEIGHT + LEAF_GAP),
          })),
        });
      });
      rankY += rankHeight + 116;
    }

    return {
      width,
      height: Math.max(330, rankY - 62),
      layouts,
      aggregateLayouts,
      byId,
      parentById,
      byAgentId: new Map(layouts.map((layout) => [layout.agent.id, layout])),
    };
  }, [agents, expandedGroups, normalizedQuery, selectedAgentId]);

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

function SelectionInspector({ selection }: { selection: GraphSelection }) {
  if (selection.kind === "agent") return <AgentInspector agent={selection.agent} />;
  if (selection.kind === "edge") {
    const childKind = phaseKind(selection.child.phase);
    return (
      <InspectorShell eyebrow="Delegation relationship" title={`${selection.parent.displayName} → ${selection.child.displayName}`}>
        <p className="text-xs leading-relaxed text-foreground-muted">
          Runtime metadata identifies{" "}
          <span className="font-medium text-foreground">{selection.parent.displayName}</span>
          {" as the parent of "}
          <span className="font-medium text-foreground">{selection.child.displayName}</span>
          {selection.child.role ? ` as ${selection.child.role}` : ""}. The child is currently{" "}
          <span className={childKind === "failed" ? "font-medium text-danger" : "font-medium text-foreground"}>
            {selection.child.phase}
          </span>.
        </p>
        <p className="mt-2 text-[10px] leading-relaxed text-foreground-muted">
          No delegation-event ledger is attached to this trace. The drill-down below focuses the child agent&apos;s exact retained activity, not an inferred edge event.
        </p>
        <div className="mt-3 grid gap-2 sm:grid-cols-3">
          <Metric label="Child runtime" value={selection.child.runtime ?? "Not reported"} small />
          <Metric label="Child model" value={selection.child.model ?? "Not reported"} small />
          <Metric label="Latest activity" value={formatLastActivity(selection.child.lastActivity)} small />
        </div>
      </InspectorShell>
    );
  }
  if (selection.kind === "specialist-aggregate") {
    return (
      <InspectorShell eyebrow="Folded specialist group" title={selection.label}>
        <p className="text-xs text-foreground-muted">
          Specialists sharing <span className="font-medium text-foreground">{selection.parent.displayName}</span> as their runtime parent.
        </p>
        <FoldedList
          items={selection.agents.map((agent) => ({
            title: `${agent.displayName} · ${agent.role}`,
            detail: `${agent.technicalName} · ${agent.model ?? "model not reported"} · ${agent.phase}`,
            failed: phaseKind(agent.phase) === "failed",
          }))}
        />
      </InspectorShell>
    );
  }
  if (selection.kind === "action") {
    const action = selection.action;
    return (
      <InspectorShell eyebrow="Recorded action" title={action.human}>
        <div className="flex flex-wrap gap-2 text-[10px] text-foreground-muted">
          <span className="rounded-full border border-border px-2 py-1">Agent {selection.agent.displayName}</span>
          <span className="rounded-full border border-border px-2 py-1">Round {action.round + 1}</span>
          <span className="rounded-full border border-border px-2 py-1">{action.ms} ms</span>
          <span className={`rounded-full border px-2 py-1 ${
            action.ok === false ? "border-danger/30 text-danger" : action.ok === true ? "border-signal/30 text-signal" : "border-border"
          }`}>
            {action.ok === false ? "Failed" : action.ok === true ? "Succeeded" : "Model round"}
          </span>
          <span className="rounded-full border border-border px-2 py-1">{formatLastActivity(action.ts)}</span>
        </div>
        <p className="mt-3 break-all rounded-lg bg-surface-muted/45 px-3 py-2 font-mono text-[10px]">
          <span className="font-sans font-medium text-foreground-muted">Raw tool: </span>{action.raw}
        </p>
        <div className="mt-2 grid gap-2 text-[10px] sm:grid-cols-2">
          <DetailBlock label="Arguments" value={action.args || "No input preview retained"} />
          <DetailBlock label={action.ok === false ? "Failure / result" : "Result"} value={action.result || "No result preview retained"} />
        </div>
      </InspectorShell>
    );
  }
  if (selection.kind === "folded-actions") {
    return (
      <InspectorShell eyebrow="Folded action cluster" title={`${selection.actions.length} earlier actions`}>
        <FoldedList
          items={selection.actions.map((action) => ({
            title: action.human,
            detail: `${action.raw} · round ${action.round + 1} · ${action.ms} ms · ${formatLastActivity(action.ts)}`,
            failed: action.ok === false,
          }))}
        />
      </InspectorShell>
    );
  }
  if (selection.kind === "destination") {
    return (
      <InspectorShell eyebrow="Network destination" title={selection.destination}>
        <p className="text-xs text-foreground-muted">
          Referenced by retained action evidence from <span className="font-medium text-foreground">{selection.agent.displayName}</span>.
        </p>
      </InspectorShell>
    );
  }
  return (
    <InspectorShell eyebrow="Folded destination cluster" title={`${selection.destinations.length} additional destinations`}>
      <FoldedList items={selection.destinations.map((destination) => ({ title: destination, detail: "Referenced in retained action evidence" }))} />
    </InspectorShell>
  );
}

function AgentInspector({ agent }: { agent: AgentExecution }) {
  const latest = agent.actions.at(-1);
  return (
    <InspectorShell eyebrow={agent.isPrincipal ? "Principal agent" : "Specialist agent"} title={agent.displayName}>
      <div className="flex flex-wrap items-center gap-2">
        <span className={`rounded-full border px-2 py-1 text-[10px] font-medium ${phaseTone(agent.phase)}`}>
          {agent.phase}
        </span>
        <span className="text-[11px] text-foreground-muted">{agent.relationship}</span>
      </div>
      <dl className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <Metric label="Role" value={agent.role} small />
        <Metric label="Runtime" value={agent.runtime ?? "Not reported"} small />
        <Metric label="Model" value={agent.model ?? "Not reported"} small />
        <Metric label="Exact agent name" value={agent.technicalName} small />
      </dl>
      <div className="mt-2 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <Metric label="Retained rounds" value={agent.rounds} />
        <Metric label="Tool calls" value={agent.toolCalls} />
        <Metric label="Failures" value={agent.failures} danger={agent.failures > 0} />
        <Metric label="Latest activity" value={formatLastActivity(agent.lastActivity)} small />
      </div>
      <div className="mt-2 rounded-lg border border-border bg-surface px-3 py-2">
        <p className="text-[9px] font-medium uppercase tracking-wide text-foreground-muted">Latest action</p>
        <p className="mt-0.5 text-xs font-medium">{latest?.human ?? "No retained action yet"}</p>
      </div>
    </InspectorShell>
  );
}

function InspectorShell({
  eyebrow,
  title,
  children,
}: {
  eyebrow: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mt-4 rounded-xl border border-signal/25 bg-signal/[0.035] p-4" aria-live="polite">
      <p className="text-[9px] font-semibold uppercase tracking-[0.16em] text-signal">{eyebrow}</p>
      <h3 className="mt-0.5 break-words text-sm font-semibold">{title}</h3>
      <div className="mt-2">{children}</div>
    </div>
  );
}

function DetailBlock({ label, value }: { label: string; value: string }) {
  return (
    <p className="break-words rounded-lg bg-surface-muted/45 px-3 py-2">
      <span className="font-medium text-foreground-muted">{label}: </span>
      <span className="font-mono">{value}</span>
    </p>
  );
}

function FoldedList({
  items,
}: {
  items: Array<{ title: string; detail: string; failed?: boolean }>;
}) {
  return (
    <ol className="max-h-56 space-y-1.5 overflow-y-auto pr-1">
      {items.map((item, index) => (
        <li key={`${item.title}-${index}`} className="flex gap-2 rounded-lg border border-border bg-surface px-3 py-2">
          <span className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${item.failed ? "bg-danger" : "bg-signal"}`} />
          <span className="min-w-0">
            <span className="block text-[11px] font-medium">{item.title}</span>
            <span className="block break-all font-mono text-[9px] text-foreground-muted">{item.detail}</span>
          </span>
        </li>
      ))}
    </ol>
  );
}

function Metric({
  label,
  value,
  danger = false,
  small = false,
}: {
  label: string;
  value: string | number;
  danger?: boolean;
  small?: boolean;
}) {
  return (
    <div className="rounded-md border border-border bg-surface px-2 py-1.5">
      <dt className="text-[9px] uppercase tracking-wide text-foreground-muted">{label}</dt>
      <dd className={`mt-0.5 break-words ${small ? "text-[10px] leading-tight" : "font-semibold tabular-nums"} ${danger ? "text-danger" : ""}`}>
        {value}
      </dd>
    </div>
  );
}

function ProofPoints({
  envelopeDigest,
  identity,
  subCount,
  receipt,
}: {
  envelopeDigest: string | null;
  identity: AgentIdentity | null;
  subCount: number;
  receipt: Receipt | null;
}) {
  const short = (value: string, head = 10, tail = 6) =>
    value.length > head + tail + 1
      ? `${value.slice(0, head)}...${value.slice(-tail)}`
      : value;
  const points = [
    {
      when: "At admission",
      title: "Trust envelope signed",
      detail: envelopeDigest
        ? `Digest ${short(envelopeDigest.replace(/^sha256:/, ""))} - tier, budget, tools, and reach were sealed before launch.`
        : "Tier, budget, tools, and reach are sealed into a signed envelope before launch.",
      proven: Boolean(envelopeDigest),
    },
    {
      when: "At registration",
      title: "Agent mesh identity (DID)",
      detail: identity?.did
        ? `${short(identity.did, 16, 8)} - signed mesh participant${identity.reputation_score != null ? `, reputation ${identity.reputation_score}` : ""}.`
        : "No per-run DID registration proof is attached to this retained view.",
      proven: Boolean(identity?.did),
    },
    {
      when: "At spawn",
      title: "Sub-agent attenuation enforced",
      detail:
        subCount > 0
          ? `${subCount} specialist${subCount === 1 ? "" : "s"} spawned after the controller verified each envelope was a strict subset of the principal's authority.`
          : "If the principal delegates, the controller rejects any sub-agent envelope that is not a strict authority subset.",
      proven: subCount > 0,
    },
    {
      when: "At delivery",
      title: "Governance receipt (DSSE)",
      detail: receipt
        ? `${receipt.scheme || "DSSE"} - key ${short(receipt.key_id || "-", 8, 6)}${receipt.inclusion_seq != null ? ` - inclusion log #${receipt.inclusion_seq}` : ""}.`
        : "No per-run DSSE receipt object is attached to this retained view.",
      proven: Boolean(receipt),
    },
  ];
  const verified = points.filter((point) => point.proven).length;
  const notRetained = points.length - verified;

  return (
    <details className="mt-4 rounded-xl border border-border bg-surface-muted/20">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 text-xs font-medium text-foreground-muted">
        <Icon name="seal" size={13} />
        Cryptographic proofs and attestations
        <span className="ml-auto text-[10px]">
          {verified} verified · {notRetained} not retained
        </span>
      </summary>
      <ol className="space-y-2 border-t border-border p-3">
        {points.map((point) => (
          <li key={point.title} className="flex gap-2.5">
            <span className={`mt-0.5 inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full text-[9px] ${point.proven ? "bg-signal/15 text-signal" : "border border-dashed border-border text-foreground-muted"}`}>
              {point.proven ? "ok" : "n/a"}
            </span>
            <div className="min-w-0">
              <p className="text-[11px] font-medium">
                {point.title}
                <span className="ml-2 rounded-full border border-border px-1.5 py-0.5 text-[9px] font-normal text-foreground-muted">
                  {point.when}
                </span>
              </p>
              <p className="text-[11px] leading-relaxed text-foreground-muted">{point.detail}</p>
            </div>
          </li>
        ))}
      </ol>
      <p className="border-t border-border px-3 py-2 text-[10px] leading-relaxed text-foreground-muted">
        “Verified” means the exact per-run proof object is attached here. “Not retained” means this
        archived run predates that retained evidence surface; it is not counted as cryptographic proof,
        even when the platform control was enforced.
      </p>
    </details>
  );
}
