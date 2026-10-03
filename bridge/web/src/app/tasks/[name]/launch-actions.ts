// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge — launch / un-launch server action (the §20 gate).
"use server";

import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";
import type { ValidationResult } from "@/lib/types";

function validationFailure(detail: string): ValidationResult {
  return {
    ok: false,
    checks: [{ id: "validate_error", label: "Validation could not run", status: "fail", detail }],
  };
}

/** Validate a draft task's own stored blueprint (the §20 gate at launch). */
export async function validateTask(name: string): Promise<ValidationResult> {
  try {
    const ns = defaultNamespace();
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(name)}/validate`,
      { method: "POST", cache: "no-store", headers: { accept: "application/json" } },
    );
    if (!res.ok) {
      return validationFailure(`The pre-flight check failed to run (${res.status}).`);
    }
    const data: unknown = await res.json();
    if (
      data === null || typeof data !== "object" || !("ok" in data)
      || typeof data.ok !== "boolean" || !("checks" in data)
      || !Array.isArray(data.checks) || data.checks.length === 0
      || !data.checks.every((check: unknown) => (
        check !== null && typeof check === "object"
        && "id" in check && typeof check.id === "string"
        && "label" in check && typeof check.label === "string"
        && "detail" in check && typeof check.detail === "string"
        && "status" in check && typeof check.status === "string"
        && ["pass", "warn", "fail"].includes(check.status)
      ))
    ) {
      return validationFailure("The pre-flight check returned an invalid response. Retry validation.");
    }
    const result = data as ValidationResult;
    return { ...result, ok: result.ok && !result.checks.some((check) => check.status === "fail") };
  } catch {
    return validationFailure("The pre-flight check could not be reached or read. Retry validation.");
  }
}

export async function setLaunch(
  name: string,
  launch: boolean,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(name)}/launch`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ launch }),
      },
    );
    if (!res.ok) {
      return { error: `Launch request failed (${res.status}).` };
    }
  } catch (err) {
    return { error: err instanceof Error ? err.message : "unknown error" };
  }
  return { error: null };
}
