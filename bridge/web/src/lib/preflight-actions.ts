// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
    delegation_depth?: number;
    budget_tokens?: number | null;
    budget_scope?: "GovernedInference" | null;
    budget_usd_micros?: number | null;
    workload?: "mission" | "team";
    team_roles?: import("./bff-contracts").CreateRole[];
    git_write_repos?: string[];
    qualification_run?: boolean;
  },
): Promise<ValidationResult> {
  if (envelope?.qualification_run) {
    const { currentPrincipal } = await import("@/lib/session");
    const principal = await currentPrincipal();
    if (!principal.roles.some((role) => role === "operator" || role === "admin")) {
      return { ok: false, checks: [{ id: "qualification_authorization", label: "Qualification designation denied", status: "fail", detail: "Only an operator or admin may designate a qualification attempt." }] };
    }
  }
  const { BffError, validatePackage } = await import("@/lib/bff");
  try {
    return await validatePackage(defaultNamespace(), blueprint, envelope);
  } catch (error) {
    let detail = "Package validation is unavailable. Retry before launching; no validation succeeded.";
    if (error instanceof BffError) {
      if (error.status === 401) {
        detail = "Sign in again to validate this package.";
      } else if (error.status === 403) {
        detail = "Your current permissions do not allow this package to be validated.";
      } else if ([400, 409, 422].includes(error.status)
        && ["bad_request", "conflict", "rejected"].includes(error.code)) {
        detail = error.message;
      } else if (error.status === 422) {
        detail = "The package does not match the API schema. Check required fields and value types. Execution plan JSON uses snake_case fields, including max_parallel and max_tool_calls.";
      }
    }
    return { ok: false, checks: [{ id: "package_validation", label: "Package validation failed", status: "fail", detail }] };
  }
}
