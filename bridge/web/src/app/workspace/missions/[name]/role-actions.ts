// kars Bridge Workspace — add a delegated role to a mission (the §12 org chart).
//
// A "role" is a child KarsTask whose authority is a verified subset of the
// principal's: lower-or-equal tier, the same tool policy, and an egress
// allow-list that is a subset of the principal's. The controller enforces the
// attenuation; this action only composes the child and submits it. Over-reach
// is surfaced honestly (the role lands Degraded with the reason) rather than
// prevented by hiding controls.

"use server";

import { revalidatePath } from "next/cache";
import { BffError, createTask } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import type { Blueprint, BlueprintEgress, CreateTaskRequest } from "@/lib/types";

export interface AddRoleInput {
  principal: string;
  principalTier: number;
  principalCeiling: number;
  principalDelegationDepth: number;
  toolPolicy: string | null;
  roleName: string;
  objective: string;
  tier: number;
  instructions: string;
  egress: BlueprintEgress[];
}

export interface AddRoleState {
  error: string | null;
}

function slugify(s: string): string {
  const base = s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 28)
    .replace(/-+$/g, "");
  const suffix = Math.random().toString(36).slice(2, 6);
  return `${base.length ? base : "role"}-${suffix}`;
}

export async function addRole(input: AddRoleInput): Promise<AddRoleState> {
  const ns = defaultNamespace();
  const objective = input.objective.trim();
  const roleName = input.roleName.trim();
  if (roleName.length === 0) return { error: "Give the role a name." };
  if (objective.length === 0) return { error: "Describe what this role does." };

  // Attenuate: the role never holds more than the principal grants a descendant.
  const tier = Math.min(Math.max(1, input.tier), input.principalCeiling);
  const delegationDepth = Math.max(0, input.principalDelegationDepth - 1);

  const blueprint: Blueprint = {};
  if (input.toolPolicy) blueprint.tool_policy = input.toolPolicy;
  if (input.egress.length) blueprint.egress = input.egress;
  if (input.instructions.trim()) blueprint.instructions = input.instructions.trim();

  const body: CreateTaskRequest = {
    name: slugify(roleName),
    objective,
    display_name: roleName,
    parent: input.principal,
    envelope: {
      tier,
      authority_ceiling: tier,
      delegation_depth: delegationDepth,
      budget: null,
      tool_policy: null,
      egress_allowlist: null,
    },
    blueprint: Object.keys(blueprint).length ? blueprint : null,
    launch: false,
  };

  try {
    await createTask(ns, body);
  } catch (err) {
    if (err instanceof BffError) {
      if (err.code === "rejected" && err.message) return { error: err.message };
      if (err.code === "cluster_unavailable") {
        return { error: "The run environment isn't connected, so the role can't be added yet." };
      }
      return { error: "Couldn't add this role. Adjust and try again." };
    }
    throw err;
  }

  revalidatePath(`/workspace/missions/${input.principal}`);
  return { error: null };
}
