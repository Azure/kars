// kars Bridge — "Delete team" server action. Permanently removes a standing
// team via the BFF (which deletes the KarsTeam and sweeps its runs, member
// sandboxes, shared memory, task backlog, and channel secret). Destructive and
// irreversible; the control gates it behind an explicit confirm.
"use server";

import { redirect } from "next/navigation";
import { revalidatePath } from "next/cache";
import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";

export async function deleteTeam(team: string): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}`,
      { method: "DELETE", cache: "no-store" },
    );
    if (!res.ok) {
      let message = `Delete failed (${res.status}).`;
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
  revalidatePath("/workspace/teams");
  redirect("/workspace/teams");
}
