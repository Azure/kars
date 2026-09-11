"use server";

// kars Bridge Operator Console — skill submission (server action). Same
// validated create path the Workspace user-submit uses (submitSkill / POST
// /api/skills): the operator gets the identical guided form + backend
// validation instead of a bare-file-picker + raw-governance-apply path.
// Lands PENDING like any submission — the operator's own scan+approve gate
// (SkillApproval) still governs before it becomes usable.

import { revalidatePath } from "next/cache";
import { submitSkill, BffError } from "@/lib/bff";
import { operatorIdentity } from "@/lib/config";
import type { SkillComposerInput, SkillComposerResult } from "@/components/skill-composer";

export async function submitSkillConsoleAction(input: SkillComposerInput): Promise<SkillComposerResult> {
  try {
    await submitSkill({ ...input, uploaded_by: operatorIdentity() });
    revalidatePath("/console/configuration");
    revalidatePath("/console/capabilities");
    return { ok: true };
  } catch (e) {
    const msg = e instanceof BffError ? e.message || e.code : e instanceof Error ? e.message : "Couldn't submit the skill.";
    return { ok: false, error: msg };
  }
}
