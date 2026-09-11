"use server";

import { revalidatePath } from "next/cache";
import { BffError, promoteAdditionalProvider } from "@/lib/bff";

export interface SetDefaultProviderState { error: string | null; ok: string | null }

export async function setDefaultProviderAction(_p: SetDefaultProviderState, form: FormData): Promise<SetDefaultProviderState> {
  const tag = String(form.get("tag") ?? "").trim();
  if (!tag) return { error: "Missing tag.", ok: null };
  try {
    const r = await promoteAdditionalProvider(tag);
    revalidatePath("/console/configuration");
    revalidatePath("/console/policies");
    return { error: null, ok: r.note };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "set default failed", ok: null };
  }
}
