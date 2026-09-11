"use server";

// kars Bridge Workspace — user skill submission (server action). A team member
// uploads a skill package; it lands as a PENDING KarsSkill for the operator to
// scan, review, and sign. Never marks a skill approved — that's the operator's
// trust gate.

import { submitSkill, type SubmitSkillInput } from "@/lib/bff";
import { operatorIdentity } from "@/lib/config";
import { revalidatePath } from "next/cache";

export type SubmitSkillResult = { ok: true } | { ok: false; error: string };

export async function submitSkillAction(input: SubmitSkillInput): Promise<SubmitSkillResult> {
  try {
    await submitSkill({ ...input, uploaded_by: input.uploaded_by ?? operatorIdentity() });
    revalidatePath("/workspace/skills");
    return { ok: true };
  } catch (e) {
    const msg = e instanceof Error ? e.message : "Couldn't submit the skill.";
    return { ok: false, error: msg };
  }
}
