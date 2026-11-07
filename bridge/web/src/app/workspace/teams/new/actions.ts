// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use server";

import { redirect } from "next/navigation";
import {
  authenticatedBffFetch,
  BffError,
  createTeam,
  composeTeam,
  getTeam,
  qualifyTeam,
  putEngineeringSource,
  type CreateRole,
} from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import { parseTeamBudget } from "@/lib/team-budget";
import type { ComposeTeamResponse, EngineeringSignal, ExecutionPlan } from "@/lib/types";

export interface NewTeamState { error: string | null }

/** Ask the orchestrator to compose an org chart from a charter (REQ: team
 *  orchestration, efficiency-driven). Falls back honestly when unavailable. */
export async function composeTeamAction(charter: string, profile?: string, qualificationRun?: boolean, spending?: import("@/lib/budget-review").CompositionSpending): Promise<ComposeTeamResponse> {
  try {
    if (qualificationRun) {
      const { currentPrincipal } = await import("@/lib/session");
      const principal = await currentPrincipal();
      if (!principal.roles.some((role) => role === "operator" || role === "admin")) {
        return { available: false, reason: "Only an operator or admin may designate a qualification attempt.", proposal: null, rationale: null, source: null };
      }
    }
    return await composeTeam(defaultNamespace(), charter, profile, qualificationRun, spending);
  } catch (error) {
    const timedOut = error instanceof Error && error.name === "TimeoutError";
    const validationError = error instanceof BffError
      && [400, 409, 422].includes(error.status)
      && ["bad_request", "rejected", "conflict"].includes(error.code)
      && error.message;
    return {
      available: false,
      reason: (error instanceof BffError && error.code === "spending_policy_unavailable" && error.message)
        || validationError || (error instanceof BffError && error.status === 422
        ? "The Team package has invalid fields. Review the charter and package before composing again."
        : timedOut
          ? "The org orchestrator did not finish its bounded proposal and repair workflow within five minutes — start from the editable suggested roster below."
          : "The org orchestrator is unavailable — start from the editable suggested roster below."),
      proposal: null,
      rationale: null,
      source: null,
    };
  }
}

export async function createTeamAction(_p: NewTeamState, form: FormData): Promise<NewTeamState> {
  const displayName = String(form.get("display_name") ?? "").trim();
  const charter = String(form.get("charter") ?? "").trim();
  const slugify = (s: string) =>
    s.trim().toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
  // Auto-derive the slug from the display name or charter when the operator
  // didn't type one, so "Create team" never silently no-ops on an empty Name
  // (audit f41). An explicit Name still wins.
  let name = slugify(String(form.get("name") ?? ""));
  if (!name) name = slugify(displayName || charter);
  const parsedBudget = parseTeamBudget(form.get("budget_tokens"), form.get("budget_scope"), form.get("budget_usd_micros"));
  if (parsedBudget.error !== null) return { error: parsedBudget.error };
  const tier = Number(form.get("tier") ?? 3);
  const ceilingRaw = String(form.get("authority_ceiling") ?? "").trim();
  const depthRaw = String(form.get("delegation_depth") ?? "").trim();
  const authorityCeiling = ceilingRaw ? Number(ceilingRaw) : undefined;
  const delegationDepth = depthRaw ? Number(depthRaw) : undefined;
  if (!Number.isInteger(tier) || tier < 1 || tier > 5) {
    return { error: "Autonomy tier must be an integer from 1 to 5." };
  }
  if (authorityCeiling !== undefined && (!Number.isInteger(authorityCeiling) || authorityCeiling < 1 || authorityCeiling > tier)) {
    return { error: "Authority ceiling must be a positive integer no greater than tier." };
  }
  if (delegationDepth !== undefined && (!Number.isInteger(delegationDepth) || delegationDepth < 0 || delegationDepth > 2147483647)) {
    return { error: "Delegation depth must be a non-negative 32-bit integer." };
  }
  const cadence = Number(form.get("cadence") ?? 0);
  const lifecycleMode = String(form.get("lifecycle_mode") ?? "").trim().toLowerCase();
  const warmIdleSeconds = Number(form.get("warm_idle_seconds") ?? 0);
  if ((lifecycleMode !== "" && lifecycleMode !== "ephemeral") || warmIdleSeconds !== 0) {
    return { error: "Warm and always-on agent runtimes are not currently supported. Reload this form to create a durable Team with on-demand agent pods." };
  }
  const reporting = String(form.get("reporting_to") ?? "").trim();
  const toolPolicy = String(form.get("tool_policy") ?? "").trim();
  const runtime = String(form.get("runtime") ?? "").trim();
  const model = String(form.get("model") ?? "").trim();
  let modelFallbacks: string[] = [];
  try {
    const parsed = JSON.parse(String(form.get("model_fallbacks_json") ?? "[]"));
    if (Array.isArray(parsed)) {
      modelFallbacks = parsed
        .filter((route): route is string => typeof route === "string")
        .map((route) => route.trim())
        .filter(Boolean);
    }
  } catch {
    modelFallbacks = [];
  }
  const mcpServers = String(form.get("mcp_servers") ?? "")
    .split(",")
    .map((server) => server.trim())
    .filter(Boolean);
  const knowledgeCommons = String(form.get("knowledge_commons") ?? "").trim();
  const memory = String(form.get("memory") ?? "").trim();
  if (String(form.get("egress_mode") ?? "strict").trim().toLowerCase() !== "strict") {
    return { error: "Standing Teams use Strict egress. Review external hosts or request access during a run." };
  }
  let egress: { host: string; port?: number }[] = [];
  try {
    egress = JSON.parse(String(form.get("egress_json") ?? "[]"));
  } catch {
    egress = [];
  }
  // Governance: teams are created PAUSED by default. Launching is an explicit
  // human approval — only auto-launch when the operator ticked "launch now".
  const launch = String(form.get("launch") ?? "") === "on";
  const qualificationRun = form.get("qualification_run") === "true";
  let roles: CreateRole[] = [];
  try {
    roles = JSON.parse(String(form.get("roles_json") ?? "[]"));
  } catch {
    roles = [];
  }
  let milestones: Array<{
    id: string;
    title: string;
    description: string;
    owner_role: string | null;
    depends_on: string[];
    acceptance_criteria: string[];
    review_required: boolean;
  }> = [];
  try {
    milestones = JSON.parse(String(form.get("milestones_json") ?? "[]"));
  } catch {
    milestones = [];
  }
  let executionPlan: ExecutionPlan | undefined;
  try {
    const parsed = JSON.parse(String(form.get("execution_plan_json") ?? "null"));
    if (parsed && typeof parsed === "object") executionPlan = parsed as ExecutionPlan;
  } catch {
    executionPlan = undefined;
  }
  const gitWriteRepos = String(form.get("git_write_repos") ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  const engineeringEnabled = String(form.get("engineering_enabled") ?? "") === "true";
  const engineeringSignals = String(form.get("engineering_signals") ?? "")
    .split(",")
    .map((signal) => signal.trim())
    .filter(Boolean) as EngineeringSignal[];
  const engineeringPoll = Math.min(
    86400,
    Math.max(300, Number(form.get("engineering_poll_interval_seconds") ?? 900)),
  );
  const engineeringAutoRun = String(form.get("engineering_auto_run") ?? "") !== "false";
  if (!name || charter.length < 8) return { error: "A real charter (8+ characters) is required — the team name is derived from it if you leave Name blank." };
  if (engineeringEnabled && (gitWriteRepos.length === 0 || engineeringSignals.length === 0)) {
    return {
      error:
        "Continuous engineering intake requires at least one connected repository and one signal.",
    };
  }
  const { currentPrincipal } = await import("@/lib/session");
  const principal = await currentPrincipal();
  if (qualificationRun) {
    if (!principal.roles.some((role) => role === "operator" || role === "admin")) {
      return { error: "Only an operator or admin may designate a qualification attempt." };
    }
    if (engineeringEnabled || milestones.length === 0 || milestones[0].depends_on.length !== 0) {
      return { error: "Qualification requires a reviewed first milestone with no dependencies and continuous engineering intake disabled." };
    }
  }
  try {
    await createTeam(defaultNamespace(), {
      name, charter, tier,
      authority_ceiling: authorityCeiling,
      delegation_depth: delegationDepth,
      budget: parsedBudget.budget,
      display_name: displayName || undefined,
      cadence_minutes: cadence || undefined,
      reporting_to: reporting || undefined,
      tool_policy: toolPolicy || undefined,
      pin_tool_policy: form.get("pin_tool_policy") === "on",
      runtime: runtime || undefined,
      model: model || undefined,
      model_fallbacks: modelFallbacks,
      mcp_servers: mcpServers,
      egress: egress.filter((entry) => entry.host?.trim()),
      egress_mode: "strict",
      knowledge_commons: knowledgeCommons || undefined,
      memory: memory || undefined,
      launch: !qualificationRun && launch && milestones.length === 0,
      qualification_run: qualificationRun || undefined,
      roles: roles.filter((r) => r.name?.trim()),
      execution_plan: executionPlan,
      git_write_repos: gitWriteRepos.length ? gitWriteRepos : undefined,
      created_by: principal.name,
    });
    try {
      let reviewedFirst: import("@/lib/types").TeamTask | null = null;
      for (const milestone of milestones) {
        const response = await authenticatedBffFetch(
          `/api/namespaces/${encodeURIComponent(defaultNamespace())}/teams/${encodeURIComponent(name)}/tasks`,
          {
            method: "POST",
            cache: "no-store",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              id: milestone.id,
              title: milestone.title,
              description: [
                milestone.description,
                milestone.owner_role ? `Preferred owner role: ${milestone.owner_role}` : "",
              ].filter(Boolean).join("\n\n"),
              depends_on: milestone.depends_on,
              acceptance_criteria: milestone.acceptance_criteria,
              review_required: milestone.review_required,
            }),
          },
        );
        if (!response.ok) {
          const payload = await response.json().catch(() => null);
          throw new Error(
            payload?.error?.message
              ?? `Milestone ${milestone.id || milestone.title} could not be created (${response.status}).`,
          );
        }
        if (qualificationRun) {
          const created = await response.json() as import("@/lib/types").TeamTask;
          reviewedFirst ??= created;
        }
      }
      if (qualificationRun && launch && reviewedFirst) {
        const team = await getTeam(defaultNamespace(), name);
        await qualifyTeam(defaultNamespace(), name, team.uid, reviewedFirst);
      }
      if (engineeringEnabled) {
        await putEngineeringSource(defaultNamespace(), name, {
          enabled: true,
          auto_run: engineeringAutoRun,
          repos: gitWriteRepos,
          signals: engineeringSignals,
          poll_interval_seconds: engineeringPoll,
        });
      }
      if (!qualificationRun && launch && milestones.length > 0) {
        const resume = await authenticatedBffFetch(
          `/api/namespaces/${encodeURIComponent(defaultNamespace())}/teams/${encodeURIComponent(name)}`,
          {
            method: "PATCH",
            cache: "no-store",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ paused: false }),
          },
        );
        if (!resume.ok) {
          const payload = await resume.json().catch(() => null);
          throw new Error(
            payload?.error?.message
              ?? `Team milestones were created, but launch failed (${resume.status}).`,
          );
        }
      }
    } catch (error) {
      if (qualificationRun) {
        return { error: `Qualification Team ${name} is retained for inspection. ${error instanceof Error ? error.message : "Setup or attempt submission failed."} Open the Team before retrying; a request may already have been accepted.` };
      }
      const cleanup = await authenticatedBffFetch(
        `/api/namespaces/${encodeURIComponent(defaultNamespace())}/teams/${encodeURIComponent(name)}`,
        { method: "DELETE", cache: "no-store" },
      ).catch(() => null);
      if (cleanup == null || (!cleanup.ok && cleanup.status !== 404)) {
        return {
          error:
            `Team ${name} was created, but milestone/intake setup and automatic cleanup both failed. ` +
            "Open the team and either finish configuration or delete it explicitly.",
        };
      }
      throw error;
    }
  } catch (e) {
    return {
      error:
        e instanceof BffError
          ? e.message || e.code
          : e instanceof Error
            ? e.message
            : "create failed",
    };
  }
  redirect(`/workspace/teams/${name}`);
}
