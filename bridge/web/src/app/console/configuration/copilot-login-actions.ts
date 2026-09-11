"use server";

import { revalidatePath } from "next/cache";
import { BffError, copilotLoginStart, copilotLoginPoll, type CopilotLoginStart } from "@/lib/bff";
import type { DiscoveredModel } from "@/lib/types";

export type StartResult = { ok: true; data: CopilotLoginStart } | { ok: false; error: string };

/** Begin GitHub's device-flow sign-in for Copilot — returns the user code +
 *  verification URL to show, and the device code the client polls with. */
export async function copilotLoginStartAction(): Promise<StartResult> {
  try {
    const data = await copilotLoginStart();
    return { ok: true, data };
  } catch (e) {
    return { ok: false, error: e instanceof BffError ? e.message || e.code : "couldn't start GitHub sign-in" };
  }
}

export type PollResult =
  | { status: "pending" }
  | { status: "authorized"; models: DiscoveredModel[] }
  | { status: "error"; error: string };

/** Poll the device flow. On approval the BFF stores the Copilot-authorized
 *  token server-side and returns the seat's live models. */
export async function copilotLoginPollAction(deviceCode: string): Promise<PollResult> {
  try {
    const r = await copilotLoginPoll(deviceCode);
    if (r.status === "authorized") {
      revalidatePath("/console/configuration");
      return { status: "authorized", models: r.models ?? [] };
    }
    return { status: "pending" };
  } catch (e) {
    return { status: "error", error: e instanceof BffError ? e.message || e.code : "sign-in failed" };
  }
}
