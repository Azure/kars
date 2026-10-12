// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
/// hidden form field. Admission checks the schema; controllers can perform
/// additional asynchronous validation after the object has been saved.
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
  if (kind === "KarsProfile") {
    if (!("domain" in spec) || typeof spec.domain !== "string" || !spec.domain.trim()) {
      return { error: "Profile domain must not be empty.", ok: null };
    }
    if (!("charterTemplate" in spec) || typeof spec.charterTemplate !== "string" || !spec.charterTemplate.trim()) {
      return { error: "Profile charter must not be empty.", ok: null };
    }
    const envelope = "defaultEnvelope" in spec ? spec.defaultEnvelope : null;
    if (!envelope || typeof envelope !== "object" || Array.isArray(envelope)) {
      return { error: "A default envelope is required.", ok: null };
    }
    const tier = "tier" in envelope ? envelope.tier : null;
    const ceiling = "authorityCeiling" in envelope ? envelope.authorityCeiling : null;
    const depth = "delegationDepth" in envelope ? envelope.delegationDepth : 0;
    if (typeof tier !== "number" || !Number.isInteger(tier) || tier < 1 || tier > 5) {
      return { error: "Profile tier must be an integer from 1 to 5.", ok: null };
    }
    if (typeof ceiling !== "number" || !Number.isInteger(ceiling) || ceiling < 1 || ceiling > tier) {
      return { error: "Authority ceiling must be a positive integer no greater than tier.", ok: null };
    }
    if (typeof depth !== "number" || !Number.isInteger(depth) || depth < 0 || depth > 2147483647) {
      return { error: "Delegation depth must be a non-negative 32-bit integer.", ok: null };
    }
  }

  const force = form.get("force") === "on";
  try {
    const r = await applyGovernance(plural, { name, spec, force });
    revalidatePath("/console/policies");
    revalidatePath("/console/configuration");
    revalidatePath("/console/capabilities");
    return {
      error: null,
      ok: kind === "KarsProfile"
        ? `${r.kind} ${r.name} saved. Saving is not validation; check the controller status on the profile card before use.`
        : `${r.kind} ${r.name} applied — ${r.note}`,
    };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "apply failed", ok: null };
  }
}

/// Approve + version-lock a skill (operator trust gate), or revoke approval.
/// `name` + `action` come from hidden form fields. The BFF enforces that a
/// skill must be currently validated and reviewed by a different subject.
export async function reviewSkillAction(_prev: GovState, form: FormData): Promise<GovState> {
  const name = String(form.get("name") ?? "").trim();
  const action = String(form.get("action") ?? "");
  if (!name) return { error: "Name is required.", ok: null };
  const uid = String(form.get("uid") ?? "");
  if (!uid) return { error: "Skill identity is unavailable. Refresh the package review.", ok: null };
  try {
    const { approveSkill, revokeSkill } = await import("@/lib/bff");
    if (action === "approve") {
      const generation = Number(form.get("generation"));
      const version_digest = String(form.get("version_digest") ?? "");
      if (!Number.isSafeInteger(generation) || generation < 1 || !version_digest) {
        return { error: "Inspect a currently validated package before approval.", ok: null };
      }
      const s = await approveSkill(name, { uid, generation, version_digest });
      revalidatePath("/console/capabilities");
      revalidatePath(`/console/capabilities/skills/${name}`);
      revalidatePath("/workspace/skills");
      return { error: null, ok: `Skill ${s.name} approved & locked to ${s.locked_digest?.slice(0, 12) ?? "digest"} — now available to users.` };
    }
    if (action === "revoke") {
      const s = await revokeSkill(name, uid);
      revalidatePath("/console/capabilities");
      revalidatePath(`/console/capabilities/skills/${name}`);
      revalidatePath("/workspace/skills");
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
