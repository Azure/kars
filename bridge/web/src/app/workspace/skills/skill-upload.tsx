"use client";

// kars Bridge Workspace — skill upload. Thin wrapper around the shared
// SkillComposer: submits via submitSkillAction (lands PENDING operator
// review). See @/components/skill-composer.tsx for the actual form.

import type { RefOption } from "@/lib/types";
import { SkillComposer, type SkillComposerInput, type SkillComposerResult } from "@/components/skill-composer";
import { submitSkillAction } from "./skill-actions";

export function SkillUpload({ toolPolicies }: { toolPolicies: RefOption[] }) {
  async function submit(input: SkillComposerInput): Promise<SkillComposerResult> {
    const res = await submitSkillAction(input);
    return res.ok ? { ok: true } : { ok: false, error: res.error };
  }
  return <SkillComposer toolPolicies={toolPolicies} submit={submit} />;
}
