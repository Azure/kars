// kars Bridge — team communication-channel server actions. Tokens are sent to
// the BFF (which stores them only in a K8s Secret) and never returned to the
// browser. GET/state reports enablement plus route-qualification status.
"use server";

import { revalidatePath } from "next/cache";
import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";

export async function setTeamChannel(
  team: string,
  channel: string,
  token: string,
  allowFrom?: string,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/channels`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ channel, token, allow_from: allowFrom ?? null }),
      },
    );
    if (!res.ok) {
      let message = `Enable channel failed (${res.status}).`;
      try {
        const body = await res.json();
        if (body?.error?.message) message = body.error.message;
      } catch {
        /* keep status message */
      }
      return { error: message };
    }
  } catch (err) {
    return { error: err instanceof Error ? err.message : "unknown error" };
  }
  revalidatePath(`/workspace/teams/${team}`);
  return { error: null };
}

export async function deleteTeamChannel(
  team: string,
  channel: string,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/channels/${encodeURIComponent(channel)}`,
      { method: "DELETE", cache: "no-store" },
    );
    if (!res.ok) return { error: `Disable channel failed (${res.status}).` };
  } catch (err) {
    return { error: err instanceof Error ? err.message : "unknown error" };
  }
  revalidatePath(`/workspace/teams/${team}`);
  return { error: null };
}
