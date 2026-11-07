// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { ValidationResult } from "./types";

const UNAVAILABLE = "Kars could not finish this check. Your draft is still here and nothing was started. Try checking again.";

/** A stable HTTP endpoint keeps open editors independent of build-specific Server Action IDs. */
export async function checkPackage(
  blueprint: unknown,
  envelope?: Parameters<typeof import("./bff").validatePackage>[2],
): Promise<ValidationResult> {
  let response: Response;
  try {
    response = await fetch("/api/preflight", {
      method: "POST",
      headers: { "content-type": "application/json" },
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      body: JSON.stringify({ blueprint, envelope }),
    });
  } catch {
    throw new Error(UNAVAILABLE);
  }
  if (!response.ok) {
    const detail = response.status === 401
      ? "Sign in again to check this work. Keep this draft open; nothing was started."
      : response.status === 403
        ? "Your current permissions do not allow this check. Your draft has not been changed."
        : response.status === 413
          ? "This package is too large to check. Shorten its instructions or execution plan; your draft has not been changed."
          : UNAVAILABLE;
    throw new Error(detail);
  }
  let result: ValidationResult;
  try {
    result = await response.json();
    if (!result || typeof result.ok !== "boolean" || !Array.isArray(result.checks)
      || !result.checks.every(check => check && typeof check.id === "string"
        && typeof check.label === "string" && typeof check.detail === "string"
        && ["pass", "warn", "fail"].includes(check.status))
      || (result.ok && (result.checks.length === 0 || result.checks.some(check => check.status === "fail")))) {
      throw new Error("Invalid validation response");
    }
  } catch {
    throw new Error(UNAVAILABLE);
  }
  return result;
}
