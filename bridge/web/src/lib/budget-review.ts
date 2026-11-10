// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export function planBudgetRoles(plan: unknown): Array<{ index: number; name: string; tokens: unknown }> {
  if (!plan || typeof plan !== "object" || !("roles" in plan) || !Array.isArray(plan.roles)) return [];
  return plan.roles.flatMap((role: unknown, index: number) => {
    if (!role || typeof role !== "object" || !("name" in role) || typeof role.name !== "string") return [];
    return [{ index, name: role.name, tokens: "budget_tokens" in role ? role.budget_tokens : undefined }];
  });
}

export function validTokenLimit(value: unknown): boolean {
  return value == null || (typeof value === "number" && Number.isSafeInteger(value) && value > 0);
}

export function tokenLimit(value: unknown, inherited = false): string {
  if (value == null) return inherited ? "Uses the shared limit" : "No shared token limit requested";
  return validTokenLimit(value) ? `${(value as number).toLocaleString("en-US")} tokens` : "Invalid token limit";
}

export function roleBudgetError(plan: unknown): string | null {
  const invalid = planBudgetRoles(plan).find((role) => !validTokenLimit(role.tokens));
  return invalid ? `For ${invalid.name}, enter a positive whole number up to ${Number.MAX_SAFE_INTEGER.toLocaleString("en-US")}, or leave the role limit blank to use the shared limit.` : null;
}

export function changeRoleBudget(draft: string, index: number, value: string): string {
  const plan = JSON.parse(draft);
  if (!planBudgetRoles(plan).some((role) => role.index === index)) throw new Error("This role is no longer in the work plan.");
  const trimmed = value.trim();
  const tokens = trimmed === "" ? null : /^\d+$/.test(trimmed) && validTokenLimit(Number(trimmed)) ? Number(trimmed) : value;
  return JSON.stringify({ ...plan, roles: plan.roles.map((role: unknown, i: number) => i === index
    ? { ...(role as object), budget_tokens: tokens } : role) }, null, 2);
}

export type CompositionSpending = { budget_tokens: number | null; role_tokens: Record<string, number> };

export function compositionSpending(tokens: string, draft: string):
  { spending: CompositionSpending; error: null } | { spending: null; error: string } {
  const total = tokens.trim() === "" ? null : Number(tokens);
  if (!validTokenLimit(total)) return { spending: null, error: "Review the total token limit before composing again. Use a positive whole number or leave it blank to inherit existing controls." };
  let plan: unknown = null;
  if (draft.trim()) {
    try { plan = JSON.parse(draft); } catch {
      return { spending: null, error: "The work plan has invalid JSON. Fix it before composing again so your role limits can be kept." };
    }
    if (!plan || typeof plan !== "object" || !("roles" in plan) || !Array.isArray(plan.roles)
      || planBudgetRoles(plan).length !== plan.roles.length) {
      return { spending: null, error: "Each work-plan role needs a name before its spending limit can be kept." };
    }
  }
  const error = roleBudgetError(plan);
  if (error) return { spending: null, error };
  const roles = planBudgetRoles(plan);
  if (roles.some((role) => !role.name.trim()) || new Set(roles.map((role) => role.name)).size !== roles.length) {
    return { spending: null, error: "Give each work-plan role a unique name before composing again." };
  }
  return { spending: { budget_tokens: total,
    role_tokens: Object.fromEntries(roles.filter((role) => role.tokens != null).map((role) => [role.name, role.tokens as number])),
  }, error: null };
}

export type BudgetReview = { blueprint: unknown; tokens?: number | null; usdMicros?: number | null };

export function requestedBudgetLines(budget: BudgetReview): string[] {
  const blueprint = budget.blueprint;
  const plan = blueprint && typeof blueprint === "object" && "execution_plan" in blueprint ? blueprint.execution_plan : null;
  return [
    `Shared AI usage: ${tokenLimit(budget.tokens)}`,
    `Shared cost limit: ${budget.usdMicros == null ? "No cost limit requested" : validTokenLimit(budget.usdMicros) ? `$${(budget.usdMicros / 1_000_000).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 6 })} USD` : "Invalid cost limit"}`,
    ...planBudgetRoles(plan).map((role) => `${role.name}: ${tokenLimit(role.tokens, true)}`),
  ];
}
