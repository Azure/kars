// kars Bridge — artifact review server action (§16). request_changes re-drives
// the producing task on the reviewer's delta.
"use server";

import { revalidatePath } from "next/cache";
import { defaultNamespace } from "@/lib/config";
import { authenticatedBffFetch } from "@/lib/bff";

export async function submitReview(
  task: string,
  assignmentNonce: string,
  decision: "approve" | "request_changes",
  comment?: string,
): Promise<{ error: string | null }> {
  const ns = defaultNamespace();
  try {
    const res = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/review`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          decision,
          comment: comment || undefined,
          assignment_nonce: assignmentNonce,
        }),
      },
    );
    if (!res.ok) {
      let message = `Review failed (${res.status}).`;
      try {
        const body = await res.json();
        if (body?.error?.message) message = body.error.message;
      } catch {
        // keep status-derived message
      }
      return { error: message };
    }
  } catch (err) {
    return { error: err instanceof Error ? err.message : "unknown error" };
  }
  revalidatePath(`/workspace/missions/${task}`);
  revalidatePath("/workspace/artifacts");
  return { error: null };
}
