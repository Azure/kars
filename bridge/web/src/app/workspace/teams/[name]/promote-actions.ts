// kars Bridge — team promotion server action (§12). Records a requested higher
// tier; the controller opens a human approval and widens the envelope only on
// approval (the BFF never raises the envelope directly).
"use server";

import { revalidatePath } from "next/cache";
import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";

export async function requestPromotion(
  team: string,
  tier: number,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/promote`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ tier }),
      },
    );
    if (!res.ok) {
      let message = `Promotion request failed (${res.status}).`;
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
  revalidatePath("/workspace/inbox");
  return { error: null };
}
