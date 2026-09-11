// kars Bridge — launch / un-launch server action (the §20 gate).
"use server";

import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";
import type { ValidationResult } from "@/lib/types";

/** Validate a draft task's own stored blueprint (the §20 gate at launch). */
export async function validateTask(name: string): Promise<ValidationResult> {
  const ns = defaultNamespace();
  const res = await authenticatedBffFetch(
    `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(name)}/validate`,
    { method: "POST", cache: "no-store", headers: { accept: "application/json" } },
  );
  if (!res.ok) {
    return {
      ok: false,
      checks: [
        {
          id: "validate_error",
          label: "Validation could not run",
          status: "fail",
          detail: `The pre-flight check failed to run (${res.status}).`,
        },
      ],
    };
  }
  return (await res.json()) as ValidationResult;
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
