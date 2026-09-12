// kars Bridge — shared pre-flight validation action. Validates a launch package
// (mission OR team) against the live cluster: model served, tool policy compiled,
// MCP servers reconciled + endpoints resolve, egress hosts resolve, budget/tier
// sane. One action so mission and team flows run the IDENTICAL pre-flight.
"use server";

import { defaultNamespace } from "@/lib/config";
import type { ValidationResult } from "@/lib/types";

export async function validatePackageAction(
  blueprint: unknown,
  envelope?: {
    tier?: number;
    budget_tokens?: number | null;
    workload?: "mission" | "team";
  },
): Promise<ValidationResult> {
  const { validatePackage } = await import("@/lib/bff");
  return validatePackage(defaultNamespace(), blueprint, envelope);
}
