import { normalize } from "./activity";
import {
  ACTION_LIMIT,
  AGENT_HEIGHT,
  AGENT_WIDTH,
  BASELINE_LAYER_LIMIT,
  CLUSTER_WIDTH,
  COLLAPSED_DEPTH_LIMIT,
  DESTINATION_LIMIT,
  GRAPH_PADDING,
  LANE_WIDTH,
  LEAF_GAP,
  LEAF_HEIGHT,
  SPECIALIST_LIMIT,
  VISIBLE_AGENT_BUDGET,
} from "./constants";
import type {
  AgentExecution,
  AgentLayout,
  AggregateLayout,
  GraphAggregate,
  GraphLeaf,
} from "./types";

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

export function buildGraphLayout(
  agents: AgentExecution[],
  expandedGroups: Set<string>,
  normalizedQuery: string,
  selectedAgentId: string,
) {
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
  }
