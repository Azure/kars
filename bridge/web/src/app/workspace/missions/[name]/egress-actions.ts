// kars Bridge Workspace — request temporary website access for a mission.
//
// Files an EgressApproval the controller reconciles through human approval; the
// BFF/UI never widens the sandbox allowlist directly. This is the §20 "agent
// asks to reach an extra site" path — scoped host, reason, and a TTL.

"use server";

import { revalidatePath } from "next/cache";
import { BffError, requestEgress } from "@/lib/bff";
import { defaultNamespace } from "@/lib/config";

export interface EgressState {
  error: string | null;
  ok: string | null;
}

export async function requestEgressAction(
  _prev: EgressState,
  form: FormData,
): Promise<EgressState> {
  const mission = String(form.get("mission") ?? "");
  const host = String(form.get("host") ?? "").trim();
  const reason = String(form.get("reason") ?? "").trim();
  const portRaw = String(form.get("port") ?? "443").trim();
  const ttl = String(form.get("ttl") ?? "2h").trim();
  if (!host) return { error: "Enter a website host.", ok: null };
  if (reason.length < 3) return { error: "Give a short reason.", ok: null };
  const port = portRaw ? Number(portRaw) : 443;
  try {
    const r = await requestEgress(defaultNamespace(), mission, { host, port, reason, ttl });
    revalidatePath(`/workspace/missions/${mission}`);
    return { error: null, ok: r.note };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "request failed", ok: null };
  }
}
