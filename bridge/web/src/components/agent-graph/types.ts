import type { ActivityEvent } from "@/lib/types";

export type ToolEvent = Extract<ActivityEvent, { kind: "tool" }>;

export interface AgentAction {
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

export interface AgentExecution {
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

export type GraphLeaf =
  | { kind: "action"; key: string; agent: AgentExecution; action: AgentAction; label: string; searchText: string }
  | { kind: "folded-actions"; key: string; agent: AgentExecution; actions: AgentAction[]; label: string; searchText: string }
  | { kind: "destination"; key: string; agent: AgentExecution; destination: string; label: string; searchText: string }
  | { kind: "folded-destinations"; key: string; agent: AgentExecution; destinations: string[]; label: string; searchText: string };

export type GraphSelection =
  | { kind: "agent"; key: string; agent: AgentExecution }
  | { kind: "edge"; key: string; parent: AgentExecution; child: AgentExecution }
  | GraphAggregate
  | GraphLeaf;

export interface AgentLayout {
  agent: AgentExecution;
  x: number;
  y: number;
  leaves: Array<GraphLeaf & { x: number; y: number }>;
}

export interface GraphAggregate {
  kind: "specialist-aggregate";
  key: string;
  parent: AgentExecution;
  agents: AgentExecution[];
  expanded: boolean;
  label: string;
  searchText: string;
}

export interface AggregateLayout {
  aggregate: GraphAggregate;
  x: number;
  y: number;
}
