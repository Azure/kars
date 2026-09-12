// kars Bridge Workspace — mission intake server action.
//
// Creates a governed mission from the reviewed package. The user never sees a
// Kubernetes name — we derive a stable slug from the objective. By default a
// mission is created *governed but not launched* (the §20 review-then-launch
// gate); the user opts into launching. Admission (CEL) enforces the envelope
// invariants; its rejection is surfaced verbatim.

"use server";

import { redirect } from "next/navigation";
import { BffError, createTask } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import type { CreateTaskRequest } from "@/lib/types";

export interface IntakeState {
  error: string | null;
}

/** Validate the composed package against the live cluster (the §20 gate). */
export async function validateMissionAction(
  blueprint: unknown,
  envelope?: { tier?: number; budget_tokens?: number | null },
): Promise<import("@/lib/types").ValidationResult> {
  const { validatePackage } = await import("@/lib/bff");
  return validatePackage(defaultNamespace(), blueprint, envelope);
}

/** Ask the orchestrator to compose a launch package from a plain objective. */
export async function composeMissionAction(
  objective: string,
): Promise<import("@/lib/types").ComposeResponse> {
  try {
    const { composePackage } = await import("@/lib/bff");
    return await composePackage(defaultNamespace(), objective);
  } catch (error) {
    const timedOut = error instanceof Error && error.name === "TimeoutError";
    return {
      available: false,
      reason: timedOut
        ? "The orchestrator did not respond within two minutes — compose the package manually below."
        : "The orchestrator is unavailable — compose the package manually below.",
      proposal: null,
      rationale: null,
      source: null,
    };
  }
}

/** Ask the orchestrator to PROPOSE a loop for an intent, for review. */
export async function proposeLoopAction(
  intent: string,
  surface: "mission" | "team",
): Promise<import("@/lib/types").LoopProposal | null> {
  try {
    const { proposeLoop } = await import("@/lib/bff");
    return await proposeLoop(defaultNamespace(), intent, surface);
  } catch {
    return null;
  }
}

function slugify(objective: string): string {
  const base = objective
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32)
    .replace(/-+$/g, "");
  const suffix = Math.random().toString(36).slice(2, 7);
  const stem = base.length > 0 ? base : "mission";
  return `${stem}-${suffix}`;
}

/** A human title for the mission, derived from the objective when the user
 *  didn't name it — so a mission never falls back to the generic "Mission".
 *  Takes the first sentence/line, trims trailing punctuation, caps length. */
function deriveDisplayName(objective: string): string {
  const firstLine = objective.split(/\n/)[0]?.trim() ?? "";
  const firstSentence = firstLine.split(/(?<=[.!?])\s/)[0] ?? firstLine;
  const t = firstSentence.replace(/[.,;:\s]+$/, "").slice(0, 80).trim();
  return t.length > 0 ? t : "Mission";
}

export async function createMissionAction(
  _prev: IntakeState,
  formData: FormData,
): Promise<IntakeState> {
  const objective = String(formData.get("objective") ?? "").trim();
  const displayName = String(formData.get("display_name") ?? "").trim();
  const tier = Number(formData.get("tier")) || 3;
  const tokens = ((): number | null => {
    const v = formData.get("budget_tokens");
    if (v == null || v === "") return null;
    const n = Number(v);
    return Number.isFinite(n) ? Math.trunc(n) : null;
  })();
  const launch = formData.get("launch") === "on";
  // The authority ceiling for delegated sub-roles defaults to one tier below
  // the mission (a mission never grants a child more than it holds).
  // Sub-roles default to one tier BELOW the mission (a mission never grants a
  // child more than it holds). Tier 1 is the floor — there is no tier 0 — so a
  // tier-1 mission's sub-roles necessarily inherit tier 1. (The previous
  // `tier - 1 || 1` silently coerced tier-1 to 1 via JS falsy-zero, but read as
  // if it were computing "one below"; this is explicit.)
  const authorityCeiling = tier <= 1 ? 1 : tier - 1;
  let delegation: import("@/lib/types").MissionDelegation | null = null;
  const delegationRaw = formData.get("delegation_json");
  if (typeof delegationRaw === "string" && delegationRaw.trim() !== "") {
    try {
      delegation = JSON.parse(delegationRaw) as import("@/lib/types").MissionDelegation;
    } catch {
      delegation = null;
    }
  }
  const delegationDepth = delegation?.mode === "principal-specialists" ? 1 : 0;

  // The editable composition (model/harness/instructions/tools/MCP/egress/
  // isolation/memory) — serialized by the package UI. Parsed defensively; an
  // empty/invalid payload simply omits the blueprint and the controller uses
  // its defaults (honest, never a hard failure on a malformed optional field).
  let blueprint: CreateTaskRequest["blueprint"] = null;
  const bpRaw = formData.get("blueprint_json");
  if (typeof bpRaw === "string" && bpRaw.trim() !== "") {
    try {
      const parsed = JSON.parse(bpRaw) as NonNullable<CreateTaskRequest["blueprint"]>;
      if (parsed && typeof parsed === "object") blueprint = parsed;
    } catch {
      blueprint = null;
    }
  }

  if (objective.length === 0 || objective.length > 4096) {
    return { error: "Describe what you want done (1–4096 characters)." };
  }

  const name = slugify(displayName || objective);
  const gitWriteRepos = String(formData.get("git_write_repos") ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  const { currentPrincipal } = await import("@/lib/session");
  const principal = await currentPrincipal();
  const body: CreateTaskRequest = {
    name,
    objective,
    display_name: displayName === "" ? deriveDisplayName(objective) : displayName,
    envelope: {
      tier,
      authority_ceiling: authorityCeiling,
      delegation_depth: delegationDepth,
      budget: tokens == null ? null : { tokens, usd_micros: null },
      tool_policy: null,
      egress_allowlist: null,
    },
    blueprint,
    delegation,
    launch,
    git_write_repos: gitWriteRepos.length ? gitWriteRepos : null,
    created_by: principal.name,
  };

  try {
    await createTask(defaultNamespace(), body);
  } catch (err) {
    if (err instanceof BffError) {
      if (err.code === "cluster_unavailable") {
        return {
          error:
            "The run environment isn't connected, so this mission can't be created yet. Nothing was charged.",
        };
      }
      if (err.code === "rejected" && err.message) return { error: err.message };
      return { error: "We couldn't create this mission. Please adjust and try again." };
    }
    throw err;
  }

  redirect(`/workspace/missions/${encodeURIComponent(name)}`);
}
