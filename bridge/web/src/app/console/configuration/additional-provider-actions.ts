"use server";

import { revalidatePath } from "next/cache";
import { BffError, putAdditionalProvider, deleteAdditionalProvider } from "@/lib/bff";

export interface AdditionalProviderState { error: string | null; ok: string | null }

export async function addAdditionalProviderAction(_p: AdditionalProviderState, form: FormData): Promise<AdditionalProviderState> {
  const tag = String(form.get("tag") ?? "").trim();
  const endpoint = String(form.get("endpoint") ?? "").trim();
  const apiKey = String(form.get("api_key") ?? "").trim();
  const models = String(form.get("models") ?? "").trim();
  if (!tag) return { error: "A provider tag is required (e.g. \"foundry\", \"github-models\").", ok: null };
  if (!models) return { error: "List at least one model deployment id (comma-separated).", ok: null };
  try {
    const r = await putAdditionalProvider({ tag, endpoint: endpoint || undefined, api_key: apiKey || undefined, models });
    revalidatePath("/console/configuration");
    revalidatePath("/console/policies");
    return { error: null, ok: r.note };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "configure failed", ok: null };
  }
}

export async function removeAdditionalProviderAction(_p: AdditionalProviderState, form: FormData): Promise<AdditionalProviderState> {
  const tag = String(form.get("tag") ?? "").trim();
  if (!tag) return { error: "Missing tag.", ok: null };
  try {
    await deleteAdditionalProvider(tag);
    revalidatePath("/console/configuration");
    revalidatePath("/console/policies");
    return { error: null, ok: `${tag} removed.` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "remove failed", ok: null };
  }
}
