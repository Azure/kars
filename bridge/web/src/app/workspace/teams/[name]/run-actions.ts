// kars Bridge — "Run now" server action. Triggers an immediate team run by
// setting the controller's `run-now` annotation via the BFF. The controller
// mints one taskforce run under the normal readiness gates and clears the
// annotation, so this is a single-shot request (idempotent per click).
"use server";

import { revalidatePath } from "next/cache";
import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";

export async function runTeamNow(team: string): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/run`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
      },
    );
    if (!res.ok) {
      let message = `Run request failed (${res.status}).`;
      try {
        const body = await res.json();
        if (body?.error?.message) message = body.error.message;
      } catch {
        // keep status message
      }
      return { error: message };
    }
  } catch (err) {
    return { error: err instanceof Error ? err.message : "unknown error" };
  }
  revalidatePath(`/workspace/teams/${team}`);
  return { error: null };
}
