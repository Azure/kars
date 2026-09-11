"use server";

// kars Bridge Operator Console — GitHub App self-service setup. Replaces the
// manual `kubectl create secret` step with a real form: verifies the App
// credentials against GitHub's own API before storing them, so a typo'd key
// fails loudly here instead of silently later.

import { revalidatePath } from "next/cache";
import { putGithubApp, deleteGithubApp, BffError } from "@/lib/bff";

export interface GithubAppState {
  error: string | null;
  ok: string | null;
}

export async function putGithubAppAction(_prev: GithubAppState, form: FormData): Promise<GithubAppState> {
  const app_id = String(form.get("app_id") ?? "").trim();
  const private_key = String(form.get("private_key") ?? "").trim();
  if (!app_id || !private_key) {
    return { error: "App ID and private key are both required.", ok: null };
  }
  try {
    const r = await putGithubApp({ app_id, private_key });
    revalidatePath("/console/configuration");
    return { error: null, ok: `Verified against GitHub${r.name ? ` — ${r.name}` : ""}. ${r.note}` };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "Setup failed.", ok: null };
  }
}

export async function disconnectGithubAppAction(_prev: GithubAppState, _form: FormData): Promise<GithubAppState> {
  try {
    await deleteGithubApp();
    revalidatePath("/console/configuration");
    return { error: null, ok: "Disconnected — the shared App credential was removed." };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "Disconnect failed.", ok: null };
  }
}
