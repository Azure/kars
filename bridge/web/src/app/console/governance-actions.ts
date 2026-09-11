"use server";

import { revalidatePath } from "next/cache";
import { BffError, applyGovernance, deleteGovernance } from "@/lib/bff";

export interface GovState {
  error: string | null;
  ok: string | null;
}

const PLURAL: Record<string, "toolpolicies" | "mcpservers" | "skills" | "profiles" | "inferencepolicies"> = {
  ToolPolicy: "toolpolicies",
  McpServer: "mcpservers",
  KarsSkill: "skills",
  KarsProfile: "profiles",
  InferencePolicy: "inferencepolicies",
};

/// Author/edit a governance CRD from the operator console. Reads `kind` from a
/// hidden form field so one action serves all three kinds. The spec is authored
/// as JSON; the API server's admission/CEL validation is the real gate and its
/// message is surfaced verbatim on rejection.
export async function applyGovernanceAction(_prev: GovState, form: FormData): Promise<GovState> {
  const kind = String(form.get("kind") ?? "");
  const plural = PLURAL[kind];
  if (!plural) return { error: "Unknown resource kind.", ok: null };

  const name = String(form.get("name") ?? "").trim();
  if (!name) return { error: "Name is required.", ok: null };
  if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
    return { error: "Name must be lowercase alphanumeric + hyphens (a Kubernetes object name).", ok: null };
  }

  const specRaw = String(form.get("spec") ?? "").trim();
  let spec: unknown;
  try {
    spec = JSON.parse(specRaw);
  } catch {
    return { error: "Spec must be valid JSON.", ok: null };
  }
  if (typeof spec !== "object" || spec === null || Array.isArray(spec)) {
    return { error: "Spec must be a JSON object.", ok: null };
  }

  const force = form.get("force") === "on";
  try {
    const r = await applyGovernance(plural, { name, spec, force });
    revalidatePath("/console/policies");
    revalidatePath("/console/configuration");
    revalidatePath("/console/capabilities");
    return { error: null, ok: `${r.kind} ${r.name} applied — ${r.note}` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "apply failed", ok: null };
  }
}

/// Approve + version-lock a skill (operator trust gate), or revoke approval.
/// `name` + `action` come from hidden form fields. The BFF enforces that a
/// skill must be scanned + attestation-verified before it can be approved.
export async function reviewSkillAction(_prev: GovState, form: FormData): Promise<GovState> {
  const name = String(form.get("name") ?? "").trim();
  const action = String(form.get("action") ?? "");
  if (!name) return { error: "Name is required.", ok: null };
  try {
    const { approveSkill, revokeSkill } = await import("@/lib/bff");
    if (action === "approve") {
      const s = await approveSkill(name);
      revalidatePath("/console/capabilities");
      return { error: null, ok: `Skill ${s.name} approved & locked to ${s.locked_digest?.slice(0, 12) ?? "digest"} — now available to users.` };
    }
    if (action === "revoke") {
      const s = await revokeSkill(name);
      revalidatePath("/console/capabilities");
      return { error: null, ok: `Skill ${s.name} approval revoked — withdrawn from users.` };
    }
    return { error: "Unknown action.", ok: null };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "review failed", ok: null };
  }
}

const DELETE_PLURAL: Record<string, "toolpolicies" | "mcpservers" | "skills" | "profiles" | "egress" | "inferencepolicies"> = {
  ToolPolicy: "toolpolicies",
  McpServer: "mcpservers",
  KarsSkill: "skills",
  KarsProfile: "profiles",
  EgressApproval: "egress",
  InferencePolicy: "inferencepolicies",
};

/// Delete an operator-authored governance object (or revoke an egress grant).
/// `kind` + `name` come from hidden form fields so one action serves every row.
export async function deleteGovernanceAction(_prev: GovState, form: FormData): Promise<GovState> {
  const kind = String(form.get("kind") ?? "");
  const plural = DELETE_PLURAL[kind];
  if (!plural) return { error: "Unknown resource kind.", ok: null };
  const name = String(form.get("name") ?? "").trim();
  if (!name) return { error: "Name is required.", ok: null };
  try {
    const r = await deleteGovernance(plural, name);
    revalidatePath("/console/policies");
    revalidatePath("/console/configuration");
    revalidatePath("/console/capabilities");
    revalidatePath("/console/approvals");
    return { error: null, ok: `${r.kind} ${r.name} removed — ${r.note}` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "delete failed", ok: null };
  }
}

/// Approve/reject a kars-SRE self-remediation proposal. `ns` + `name` + `action`
/// come from hidden form fields. The BFF only patches `spec.approval` — the
/// controller is the sole executor of the remediation itself.
export async function decideSreActionAction(_prev: GovState, form: FormData): Promise<GovState> {
  const ns = String(form.get("ns") ?? "").trim();
  const name = String(form.get("name") ?? "").trim();
  const action = String(form.get("action") ?? "");
  if (!ns || !name) return { error: "Namespace and name are required.", ok: null };
  if (action !== "approve" && action !== "reject") return { error: "Unknown action.", ok: null };
  try {
    const { decideSreAction } = await import("@/lib/bff");
    const note = String(form.get("note") ?? "").trim() || undefined;
    const r = await decideSreAction(ns, name, { verdict: action, note });
    revalidatePath("/console/sre-actions");
    revalidatePath("/console");
    return {
      error: null,
      ok: action === "approve"
        ? `Approved — the controller will execute ${r.action_type} and record the outcome.`
        : `Rejected — no action will be taken.`,
    };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "decision failed", ok: null };
  }
}
