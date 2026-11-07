// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use server";

import { authenticatedBffFetch, qualifyTeam } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import { currentPrincipal } from "@/lib/session";
import type { TeamTask } from "@/lib/types";

export async function teamQualificationAction(name: string, uid: string, action: "attempt" | "pause" | "review", expected?: TeamTask): Promise<{ error: string | null }> {
  const principal = await currentPrincipal();
  if (!principal.roles.some((role) => role === "operator" || role === "admin")) {
    return { error: "Only an operator or admin may manage qualification attempts." };
  }
  if (!["attempt", "pause", "review"].includes(action)) return { error: "Unknown qualification action." };
  try {
    if (action === "attempt") {
      if (!expected) return { error: "Review an eligible pending backlog item first." };
      await qualifyTeam(defaultNamespace(), name, uid, expected);
    } else {
      const response = await authenticatedBffFetch(`/api/namespaces/${encodeURIComponent(defaultNamespace())}/teams/${encodeURIComponent(name)}`, {
        method: "PATCH", cache: "no-store", headers: { "content-type": "application/json" },
        body: JSON.stringify(action === "pause" ? { paused: true } : { qualification_reviewed: true, qualification_team_uid: uid }),
      });
      if (!response.ok) {
        const payload = await response.json().catch(() => null);
        return { error: payload?.error?.message ?? `Qualification update refused (${response.status}).` };
      }
    }
    return { error: null };
  } catch (error) {
    return { error: `${error instanceof Error ? error.message : "Qualification request failed."} Refresh before retrying; a request may already have been accepted.` };
  }
}
