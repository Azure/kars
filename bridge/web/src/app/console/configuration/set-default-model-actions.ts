// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use server";

import { revalidatePath } from "next/cache";
import { BffError, setDefaultModel } from "@/lib/bff";

export interface SetDefaultModelState { error: string | null; ok: string | null }

/** Save the provider-qualified default for new missions and teams. */
export async function setDefaultModelAction(_p: SetDefaultModelState, form: FormData): Promise<SetDefaultModelState> {
  const deployment = String(form.get("deployment") ?? "").trim();
  const provider = String(form.get("provider") ?? "").trim();
  if (!deployment || !provider) return { error: "Missing model or provider.", ok: null };
  try {
    await setDefaultModel(deployment, provider);
    revalidatePath("/console/configuration");
    return { error: null, ok: `${deployment} is now the cluster default. Inheriting Orchestrator/SRE policies will reconcile automatically; check their Applied status. Existing explicit and reviewed routes are unchanged.` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "couldn't set default", ok: null };
  }
}
