"use server";

import { BffError, putProvider } from "@/lib/bff";

export interface ProviderState { error: string | null; ok: string | null }

export async function onboardProviderAction(_p: ProviderState, form: FormData): Promise<ProviderState> {
  const kind = String(form.get("kind") ?? "github-models");
  const auth = String(form.get("auth") ?? "api");
  const endpoint = String(form.get("endpoint") ?? "").trim();
  const models = String(form.get("models") ?? "").trim();
  const key = String(form.get("key") ?? "");
  if (!models) return { error: "List at least one model deployment.", ok: null };
  if (auth === "api" && !key) return { error: "API auth needs a key.", ok: null };
  try {
    const r = await putProvider({ kind, auth, endpoint: endpoint || undefined, models, key: key || undefined });
    return { error: null, ok: r.note };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "onboard failed", ok: null };
  }
}
