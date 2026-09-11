import type { ActivityEvent, SubAgent } from "@/lib/types";
import { actionFromEvent, destinationsFrom, normalize } from "./activity";
import type { AgentExecution, ToolEvent } from "./types";

export function buildAgentExecutions({
  agentLabel,
  agentModel,
  agentPhase,
  agentRuntime,
  events,
  name,
  running,
  subAgents,
}: {
  agentLabel: string;
  agentModel: string | null;
  agentPhase: string | null;
  agentRuntime: string | null;
  events: ActivityEvent[];
  name: string | undefined;
  running: boolean;
  subAgents: SubAgent[];
}): AgentExecution[] {
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
  }
