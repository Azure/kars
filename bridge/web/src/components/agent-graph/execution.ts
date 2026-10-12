// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { ActivityEvent, SubAgent } from "@/lib/types";
import { actionFromEvent, actionsFromRouter, destinationsFrom, normalize } from "./activity";
import { routerProducerKey, type BoundRouterActivity } from "@/lib/router-activity";
import type { AgentExecution, ToolEvent } from "./types";
import type { ReviewedExecution } from "@/lib/types/missions";
import { reviewedPhaseRouterActivity } from "@/lib/team-run-evidence";

export function buildAgentExecutions({
  agentLabel,
  agentModel,
  agentPhase,
  agentRuntime,
  events,
  name,
  running,
  subAgents,
  routerActivity,
  phases = [],
}: {
  agentLabel: string;
  agentModel: string | null;
  agentPhase: string | null;
  agentRuntime: string | null;
  events: ActivityEvent[];
  name: string | undefined;
  running: boolean;
  subAgents: SubAgent[];
  routerActivity?: BoundRouterActivity | null;
  phases?: ReviewedExecution["phases"];
}): AgentExecution[] {
    if (phases.length) {
      const principal = buildAgentExecutions({
        agentLabel, agentModel, agentPhase, agentRuntime, events, name, running,
        subAgents: [], routerActivity,
      });
      return [...principal, ...phases.map((phase, index) => {
        const binding = reviewedPhaseRouterActivity(phase);
        const [agent] = buildAgentExecutions({
          agentLabel: `${phase.role} / ${phase.phase}`,
          agentModel: phase.result?.model ?? null,
          agentPhase: phase.state,
          agentRuntime: null,
          events: [], name: phase.task_name ?? undefined,
          running: phase.state === "active", subAgents: [], routerActivity: binding,
        });
        return {
          ...agent,
          id: `phase-${phase.task_uid ?? index}`,
          displayName: `${phase.role} / ${phase.phase}`,
          technicalName: phase.task_name ?? `${phase.role} / ${phase.phase}`,
          role: phase.role,
          relationship: "Phase reserved by the reviewed Team execution plan",
          parent: name ?? null,
          parentId: "principal",
          isPrincipal: false,
          traceQuery: binding ? agent.traceQuery : `phase:${phase.task_uid ?? index}`,
          lastActivity: phase.result?.run_evidence?.finished_at ?? agent.lastActivity,
          searchText: `${agent.searchText} ${normalize(`${phase.role} ${phase.phase}`)}`,
        };
      })];
    }
    if (routerActivity) {
      const { producer, observations, provisional } = routerActivity;
      const actions = actionsFromRouter(routerActivity);
      const records = observations.state === "observed" ? observations.trace.events : [];
      const models = [...new Set(records.flatMap(event => event.kind === "round" && event.model ? [event.model] : []))];
      const [agent] = buildAgentExecutions({
        agentLabel: producer.agent_name, name: producer.agent_name,
        agentModel: models.length === 1 ? models[0] : null, agentRuntime: null,
        agentPhase: provisional ? "Running" : agentPhase ?? "Finished",
        events: [], running: provisional, subAgents: [],
      });
      return [{
        ...agent,
        role: "Recorded producer",
        relationship: "Producer bound to this run and assignment; delegation is not recorded here",
        traceQuery: `observations:${new URLSearchParams({ producer: routerProducerKey(producer) })}`,
        observedAgentName: producer.agent_name,
        observedAgentInstance: producer.pod_uid,
        identitySearchText: normalize(`${producer.agent_name} ${producer.agent_did} ${producer.assignment_id}`),
        searchText: normalize(`${producer.agent_name} ${producer.agent_did} ${producer.assignment_id} ${models.join(" ")} ${actions.map(action => `${action.human} ${action.raw} ${action.args} ${action.result}`).join(" ")}`),
        rounds: records.filter(event => event.kind === "round").length,
        toolCalls: records.filter(event => event.kind === "tool_result").length,
        failures: records.filter(event => event.kind === "tool_result" && event.ok === false).length,
        lastActivity: actions.reduce<string | null>((latest, action) =>
          action.ts && (!latest || action.ts > latest) ? action.ts : latest, null),
        actions,
      }];
    }
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
        .sort((left, right) => (left.ts ?? "").localeCompare(right.ts ?? ""));
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
  }
