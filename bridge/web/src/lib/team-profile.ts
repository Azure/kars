// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Budget, ProfileSummary, RefOption } from "./types";
import { parseTeamBudget } from "./team-budget";

export interface TeamProfileEnvelope {
  tier: number;
  authorityCeiling: number;
  delegationDepth: number;
  budget?: Budget;
  toolPolicy?: string;
}

export function readTeamProfile(
  profile: ProfileSummary,
  skills: RefOption[],
  toolPolicies: RefOption[] = [],
): { envelope: TeamProfileEnvelope; error: null } | { envelope: null; error: string } {
  const fail = (error: string) => ({ envelope: null, error });
  if (profile.phase !== "Ready") {
    return fail(`Profile status is ${profile.phase ?? "unknown"}. Wait for controller validation or edit the profile before using it.`);
  }
  const envelope = profile.spec.defaultEnvelope;
  if (!envelope || typeof envelope !== "object" || Array.isArray(envelope)) {
    return fail("The profile has no valid default envelope. Edit it before creating a team.");
  }
  const unsupported = Object.keys(envelope).filter(
    (key) => !["tier", "authorityCeiling", "delegationDepth", "budget", "toolPolicyRef"].includes(key),
  );
  if (unsupported.length > 0) {
    return fail(`Team setup cannot yet preserve these profile envelope settings: ${unsupported.join(", ")}. Nothing was copied or launched; these settings must be supported before this profile can be used here.`);
  }
  const tier = "tier" in envelope ? envelope.tier : null;
  const authorityCeiling = "authorityCeiling" in envelope ? envelope.authorityCeiling : null;
  const delegationDepth = "delegationDepth" in envelope ? envelope.delegationDepth : 0;
  if (typeof tier !== "number" || !Number.isInteger(tier) || tier < 1 || tier > 5
    || typeof authorityCeiling !== "number" || !Number.isInteger(authorityCeiling)
    || authorityCeiling < 1 || authorityCeiling > tier
    || typeof delegationDepth !== "number" || !Number.isInteger(delegationDepth)
    || delegationDepth < 0 || delegationDepth > 2147483647) {
    return fail("The profile envelope is invalid. Review its tier, authority ceiling and delegation depth.");
  }
  const available = new Set(skills.map((skill) => skill.name));
  const missing = [...new Set(profile.roles.flatMap((role) => role.skills ?? []))]
    .filter((skill) => !available.has(skill));
  if (missing.length > 0) {
    return fail(`Profile skills are unavailable in this workspace: ${missing.join(", ")}. They have not been silently removed from a draft.`);
  }
  let budget: Budget | undefined;
  if ("budget" in envelope && envelope.budget != null) {
    const raw = envelope.budget;
    if (typeof raw !== "object" || Array.isArray(raw)
      || Object.keys(raw).some((key) => !["scope", "tokens", "usdMicros"].includes(key))
      || !("tokens" in raw) || typeof raw.tokens !== "number"
      || ("usdMicros" in raw && raw.usdMicros != null && typeof raw.usdMicros !== "number")) {
      return fail("The profile budget cannot be preserved. Review its scope, token limit and optional micro-USD limit.");
    }
    const parsed = parseTeamBudget(String(raw.tokens), "scope" in raw ? raw.scope : undefined,
      "usdMicros" in raw && raw.usdMicros != null ? String(raw.usdMicros) : undefined);
    if (parsed.error !== null) return fail(`The profile budget is invalid: ${parsed.error}`);
    budget = parsed.budget;
  }
  let toolPolicy: string | undefined;
  if ("toolPolicyRef" in envelope && envelope.toolPolicyRef != null) {
    const ref = envelope.toolPolicyRef;
    if (typeof ref !== "object" || Array.isArray(ref)
      || Object.keys(ref).some((key) => key !== "name")
      || !("name" in ref) || typeof ref.name !== "string" || !ref.name.trim()
      || !toolPolicies.some((policy) => policy.name === ref.name)) {
      return fail("The profile toolPolicyRef must name an available policy in this workspace.");
    }
    if (profile.tool_policy && profile.tool_policy !== ref.name) {
      return fail("The profile toolPolicyRef conflicts with its default tool policy. Resolve the conflict before copying it.");
    }
    toolPolicy = ref.name;
  }
  return { envelope: { tier, authorityCeiling, delegationDepth,
    ...(budget ? { budget } : {}), ...(toolPolicy ? { toolPolicy } : {}) }, error: null };
}
