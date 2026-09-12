"use server";

import { revalidatePath } from "next/cache";
import { BffError, setDefaultModel } from "@/lib/bff";

export interface SetDefaultModelState { error: string | null; ok: string | null }

/** Make one specific model the cluster default (Model catalogue "Set as
 *  default"). The BFF promotes the model's provider and pins the model. */
export async function setDefaultModelAction(_p: SetDefaultModelState, form: FormData): Promise<SetDefaultModelState> {
  const deployment = String(form.get("deployment") ?? "").trim();
  const provider = String(form.get("provider") ?? "").trim();
  if (!deployment || !provider) return { error: "Missing model or provider.", ok: null };
  try {
    await setDefaultModel(deployment, provider);
    revalidatePath("/console/configuration");
    return { error: null, ok: `${deployment} is now the cluster default.` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "couldn't set default", ok: null };
  }
}
