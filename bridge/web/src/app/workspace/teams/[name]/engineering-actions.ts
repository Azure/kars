"use server";

import { revalidatePath } from "next/cache";
import {
  authenticatedBffFetch,
  deleteEngineeringSource,
  putEngineeringSource,
  syncEngineeringSource,
} from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";
import type { EngineeringSignal, EngineeringSource } from "@/lib/types";

type Result = { source: EngineeringSource | null; error: string | null };

function message(error: unknown): string {
  return error instanceof Error ? error.message : "Engineering intake request failed.";
}

export async function configureEngineeringSource(
  team: string,
  body: {
    enabled: boolean;
    auto_run: boolean;
    repos: string[];
    signals: EngineeringSignal[];
    poll_interval_seconds: number;
  },
): Promise<Result> {
  try {
    const source = await putEngineeringSource(defaultNamespace(), team, body);
    revalidatePath(`/workspace/teams/${team}`);
    return { source, error: null };
  } catch (error) {
    return { source: null, error: message(error) };
  }
}

export async function decideEngineeringReview(
  team: string,
  body: {
    decision: "request_changes";
    repo: string;
    pr_number: number;
    pr_url: string;
    head_sha: string;
    run: string;
    comment?: string;
  },
): Promise<{ error: string | null }> {
  try {
    const ns = defaultNamespace();
    const response = await authenticatedBffFetch(
      `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/engineering-review`,
      {
        method: "POST",
        cache: "no-store",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      },
    );
    if (!response.ok) {
      const payload = await response.json().catch(() => null);
      return {
        error:
          payload?.error?.message ??
          `Engineering review decision failed (${response.status}).`,
      };
    }
    revalidatePath(`/workspace/teams/${team}`);
    return { error: null };
  } catch (error) {
    return { error: message(error) };
  }
}

export async function runEngineeringSync(team: string): Promise<Result> {
  try {
    const source = await syncEngineeringSource(defaultNamespace(), team);
    revalidatePath(`/workspace/teams/${team}`);
    return { source, error: null };
  } catch (error) {
    return { source: null, error: message(error) };
  }
}

export async function disconnectEngineeringSource(team: string): Promise<Result> {
  try {
    const source = await deleteEngineeringSource(defaultNamespace(), team);
    revalidatePath(`/workspace/teams/${team}`);
    return { source, error: null };
  } catch (error) {
    return { source: null, error: message(error) };
  }
}
