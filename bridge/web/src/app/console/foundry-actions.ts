"use server";

import { revalidatePath } from "next/cache";
import { BffError, connectFoundry, disconnectFoundry, verifyFoundry, type FoundryCheck, type FoundryDiscovered } from "@/lib/bff";

export interface FoundryState {
  error: string | null;
  ok: string | null;
  checks?: FoundryCheck[];
  discovered?: FoundryDiscovered;
}

export async function connectFoundryAction(_prev: FoundryState, form: FormData): Promise<FoundryState> {
  const project_endpoint = String(form.get("project_endpoint") ?? "").trim();
  const inference_endpoint = String(form.get("inference_endpoint") ?? "").trim();
  const memory_store_id = String(form.get("memory_store_id") ?? "").trim();
  const auth = String(form.get("auth") ?? "auto") as "api" | "managed-identity";
  const api_key = String(form.get("api_key") ?? "").trim();
  if (!project_endpoint.startsWith("https://")) {
    return { error: "Enter the Foundry project endpoint (https://…/api/projects/<project>).", ok: null };
  }
  if (auth === "api" && !api_key) {
    return { error: "API-key auth requires the Foundry project key.", ok: null };
  }
  try {
    await connectFoundry({
      project_endpoint,
      inference_endpoint: inference_endpoint || undefined,
      memory_store_id: memory_store_id || undefined,
      auth,
      api_key: auth === "api" ? api_key : undefined,
    });
    // Immediately discover — one guided step: connect then verify + list what's there.
    let checks, discovered;
    try {
      const res = await verifyFoundry();
      checks = res.checks;
      discovered = res.discovered;
    } catch {
      /* discovery best-effort; connection still saved */
    }
    revalidatePath("/console/configuration");
    const failed = checks?.some((c) => c.status === "fail");
    return {
      error: null,
      ok: failed ? "Connected, but discovery found issues — see below." : "Connected to Foundry.",
      checks,
      discovered,
    };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "connect failed", ok: null };
  }
}

export async function verifyFoundryAction(_prev: FoundryState, _form: FormData): Promise<FoundryState> {
  try {
    const res = await verifyFoundry();
    const failed = res.checks.some((c) => c.status === "fail");
    return {
      error: null,
      ok: failed ? "Verification found issues — see below." : "Foundry verified.",
      checks: res.checks,
      discovered: res.discovered,
    };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "verify failed", ok: null };
  }
}

export async function disconnectFoundryAction(_prev: FoundryState, _form: FormData): Promise<FoundryState> {
  try {
    await disconnectFoundry();
    revalidatePath("/console/configuration");
    return { error: null, ok: "Foundry disconnected." };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "disconnect failed", ok: null };
  }
}
