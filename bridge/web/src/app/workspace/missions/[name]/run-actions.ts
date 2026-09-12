// kars Bridge Workspace — reliability runner (pass^k) server action.
//
// Replicates a delivered mission's EXACT package k times so the efficiency
// frontier can measure pass^k reliability. Each replica is a real governed run.

"use server";

import { revalidatePath } from "next/cache";
import { BffError, replicateMission } from "@/lib/bff";

export interface ReplicateState {
  error: string | null;
  ok: string | null;
}

export async function replicateMissionAction(
  _prev: ReplicateState,
  form: FormData,
): Promise<ReplicateState> {
  const ns = String(form.get("ns") ?? "").trim();
  const name = String(form.get("name") ?? "").trim();
  const count = Math.min(5, Math.max(2, Number(form.get("count") ?? 2)));
  if (!ns || !name) return { error: "Missing mission reference.", ok: null };
  try {
    const r = await replicateMission(ns, name, count);
    revalidatePath(`/workspace/missions/${name}`);
    return { error: null, ok: r.note };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "replicate failed", ok: null };
  }
}

/// Request a per-mission autonomy-tier promotion (§12). The controller opens a
/// human approval and widens the envelope only on approval.
export async function promoteMissionAction(
  _prev: ReplicateState,
  form: FormData,
): Promise<ReplicateState> {
  const ns = String(form.get("ns") ?? "").trim();
  const name = String(form.get("name") ?? "").trim();
  const tier = Math.min(5, Math.max(1, Number(form.get("tier") ?? 0)));
  if (!ns || !name) return { error: "Missing mission reference.", ok: null };
  try {
    const { promoteMission } = await import("@/lib/bff");
    const r = await promoteMission(ns, name, tier);
    revalidatePath(`/workspace/missions/${name}`);
    return { error: null, ok: r.note };
  } catch (e) {
    return { error: e instanceof BffError ? e.message || e.code : "promote failed", ok: null };
  }
}
