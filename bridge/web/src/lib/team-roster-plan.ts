// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { ExecutionPlan, ExecutionRole } from "./types";

export function newMemberPlan(name: string, plan: ExecutionPlan): ExecutionRole {
  return {
    name,
    objective: "Complete this member's assigned work and report verified findings.",
    depends_on: [],
    phases: [{
      name: "contribute",
      objective: "Follow the member instructions, read shared Team knowledge and report verified findings.",
      capabilities: ["filesystem-read", "filesystem-write"],
      max_tool_calls: Math.max(1, plan.synthesis.max_tool_calls,
        ...plan.roles.flatMap((role) => role.phases.map((phase) => phase.max_tool_calls))),
      fresh_context: true,
      execution_mode: "native",
    }],
  };
}

export function phaseObjectiveError(objective: string): string | null {
  const bytes = new TextEncoder().encode(objective).length;
  if (bytes < 20) return "Add more detail about what this member should produce.";
  if (bytes > 1200) return "This description is too long. Shorten the expected work before saving; your text has not been removed.";
  return null;
}

export function rosterExecutionPlan(plan: ExecutionPlan, roles: ExecutionRole[]): ExecutionPlan {
  const names = new Set(roles.map((role) => role.name));
  if (names.size !== roles.length || roles.some((role) =>
    !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(role.name))) {
    throw new Error("Give every member a unique name using lowercase letters, numbers and hyphens.");
  }
  if (roles.some((role) => role.depends_on.some((name) => !names.has(name) || name === role.name))) {
    throw new Error("Review who each member waits for: choose another member still in this Team.");
  }
  for (const role of roles) {
    for (const phase of role.phases) {
      const objectiveError = phaseObjectiveError(phase.objective);
      if (objectiveError) throw new Error(`${role.name} / ${phase.name}: ${objectiveError}`);
      if (phase.capabilities.includes("python-execute") && (phase.execution_mode !== "native" ||
        !["filesystem-read", "filesystem-write", "network"].every((capability) =>
          phase.capabilities.some((selected) => selected === capability)))) {
        throw new Error(`${role.name} / ${phase.name}: Python execution needs a native phase with read, write and approved-network permissions. Review and select each permission explicitly; saving will not add them for you.`);
      }
    }
  }
  const completed = new Set<string>();
  const visiting = new Set<string>();
  function visit(role: ExecutionRole) {
    if (visiting.has(role.name)) throw new Error("Members cannot wait for each other in a circle. Review the work order.");
    if (completed.has(role.name)) return;
    visiting.add(role.name);
    for (const dependency of role.depends_on) visit(roles.find((entry) => entry.name === dependency)!);
    visiting.delete(role.name);
    completed.add(role.name);
  }
  roles.forEach(visit);
  return { ...plan, roles, max_parallel: Math.min(plan.max_parallel, Math.max(1, roles.length)) };
}
