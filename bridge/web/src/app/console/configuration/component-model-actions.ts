// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use server";

import { revalidatePath } from "next/cache";
import { BffError, setComponentModel } from "@/lib/bff";

export async function saveComponentModel(_previous: { error: string | null; ok: string | null }, form: FormData) {
  const component = String(form.get("component") ?? "");
  const selection = String(form.get("selection") ?? "");
  const [provider, deployment, extra] = selection.split("::");
  if (!["orchestrator", "sre"].includes(component) || (selection !== "inherit" && (!provider || !deployment || extra))) {
    return { error: "Select a component and a provider-qualified model.", ok: null };
  }
  try {
    const result = await setComponentModel(component, {
      mode: selection === "inherit" ? "inherit" : "override",
      ...(selection === "inherit" ? {} : { model: { provider, deployment } }),
      uid: String(form.get("uid") ?? ""), resource_version: String(form.get("resource_version") ?? ""),
    });
    revalidatePath("/console/configuration");
    revalidatePath("/console/policies");
    return { error: null, ok: result.note };
  } catch (error) {
    return { error: error instanceof BffError ? error.message : "Couldn't save the component model.", ok: null };
  }
}
