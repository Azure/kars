// kars Bridge — team task backlog server actions. Add/remove discrete tasks on a
// standing team; the controller drains the oldest `pending` task on its next run
// (cadence or Run now) and marks it `done` when that run delivers.
"use server";

import { revalidatePath } from "next/cache";
import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";

export async function addTeamTask(
  team: string,
  title: string,
  description: string,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/tasks`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ title, description }),
      },
    );
    if (!res.ok) {
      let message = `Add task failed (${res.status}).`;
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

export async function deleteTeamTask(
  team: string,
  taskId: string,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/tasks/${encodeURIComponent(taskId)}`,
      { method: "DELETE", cache: "no-store" },
    );
    if (!res.ok && res.status !== 404) {
      return { error: `Remove task failed (${res.status}).` };
    }
  } catch (err) {
    return { error: err instanceof Error ? err.message : "unknown error" };
  }
  revalidatePath(`/workspace/teams/${team}`);
  return { error: null };
}

export async function reviewTeamTask(
  team: string,
  taskId: string,
  decision: "approve" | "request_changes",
  feedback?: string,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const response = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/tasks/${encodeURIComponent(taskId)}/review`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ decision, feedback: feedback?.trim() || null }),
      },
    );
    if (!response.ok) {
      const payload = await response.json().catch(() => null);
      return {
        error:
          payload?.error?.message
          ?? `Milestone review failed (${response.status}).`,
      };
    }
  } catch (error) {
    return { error: error instanceof Error ? error.message : "unknown error" };
  }
  revalidatePath(`/workspace/teams/${team}`);
  return { error: null };
}
