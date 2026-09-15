// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use server";

import { revalidatePath } from "next/cache";
import { BffError, copilotLoginStart, copilotLoginPoll } from "@/lib/bff";
import { copilotError, copilotErrors, parseCopilotStart, parseCopilotPoll, type StartResult, type PollResult } from "@/lib/copilot-login";
export type { StartResult, PollResult } from "@/lib/copilot-login";

function failure(error: unknown): string {
  if (!(error instanceof BffError)) return copilotErrors.transport;
  if (error.status === 401 || error.status === 403) return copilotErrors.session;
  return copilotError(error.code);
}

/** Begin GitHub's device-flow sign-in for Copilot — returns the user code +
 *  verification URL to show, and the device code the client polls with. */
export async function copilotLoginStartAction(): Promise<StartResult> {
  try {
    const response = await copilotLoginStart();
    let data;
    try { data = parseCopilotStart(response); }
    catch { return { ok: false, error: copilotErrors.copilot_invalid_response }; }
    return { ok: true, data };
  } catch (e) {
    return { ok: false, error: failure(e) };
  }
}

/** Poll the device flow. On approval the BFF stores the Copilot-authorized
 *  token server-side and returns the seat's live models. */
export async function copilotLoginPollAction(deviceCode: string, interval?: number): Promise<PollResult> {
  try {
    const r = parseCopilotPoll(await copilotLoginPoll(deviceCode, interval));
    if (r.status === "authorized") {
      revalidatePath("/console/configuration");
      return { status: "authorized", models: r.models ?? [] };
    }
    return r;
  } catch (e) {
    return { status: "error", error: failure(e) };
  }
}
