// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { CopilotLoginStart, CopilotPendingReason } from "./bff-contracts";
import type { DiscoveredModel } from "./types";

export const copilotErrors = {
  copilot_not_ready: "Copilot credential storage is not ready. Ask an operator to repair the workspace grant and enrolled provider store before sign-in. No GitHub token was requested.",
  copilot_storage_unconfirmed: "GitHub approved sign-in, but credential storage could not be confirmed. Ask an operator to check the provider store and grant. Do not repeat approval: the token may already have been consumed and a new sign-in may be required.",
  copilot_invalid_request: "The sign-in request is invalid. Polling stopped.",
  copilot_invalid_response: "The sign-in service returned an invalid response. Polling stopped.",
  copilot_upstream: "GitHub sign-in could not be reached. Polling stopped; check connectivity before trying again.",
  copilot_expired: "The sign-in code expired. A new sign-in is required.",
  copilot_denied: "Sign-in was cancelled on GitHub.",
  copilot_rate_limited: "GitHub rate-limited sign-in. Polling stopped; wait before trying again.",
  copilot_seat_unavailable: "Copilot eligibility could not be verified. Check the account's Copilot seat and Chat access before trying again.",
  transport: "The sign-in connection failed. Polling stopped. Check the connection and provider status before trying again.",
  session: "A signed Bridge operator session is required. Polling stopped.",
} as const;

export type StartResult = { ok: true; data: CopilotLoginStart } | { ok: false; error: string };
export type PollResult =
  | { status: "pending"; interval?: number; reason?: CopilotPendingReason }
  | { status: "authorized"; models: DiscoveredModel[] }
  | { status: "error"; error: string };

function object(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : null;
}

function seconds(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 1 && value <= 900;
}

export function copilotError(code?: string): string {
  return typeof code === "string" && Object.hasOwn(copilotErrors, code)
    ? copilotErrors[code as keyof typeof copilotErrors] : copilotErrors.transport;
}

function safeMessage(value: unknown): string {
  return Object.values(copilotErrors).some(message => message === value)
    ? value as string : copilotErrors.copilot_invalid_response;
}

export function parseCopilotStart(value: unknown): CopilotLoginStart {
  const data = object(value);
  if (!data || data.error !== undefined || typeof data.device_code !== "string" || !/^[\x21-\x7e]{1,4096}$/.test(data.device_code)
    || typeof data.user_code !== "string" || !/^[A-Z0-9-]{1,64}$/.test(data.user_code)
    || data.verification_uri !== "https://github.com/login/device"
    || !seconds(data.interval) || !seconds(data.expires_in)) {
    throw new Error(copilotErrors.copilot_invalid_response);
  }
  return {
    device_code: data.device_code, user_code: data.user_code, verification_uri: data.verification_uri,
    interval: data.interval, expires_in: data.expires_in,
  };
}

export function parseCopilotPoll(value: unknown): PollResult {
  const data = object(value);
  if (data?.status === "error") return { status: "error", error: safeMessage(data.error) };
  if (data?.error !== undefined) return { status: "error", error: copilotErrors.copilot_invalid_response };
  if (data?.status === "pending"
    && (data.interval === undefined || seconds(data.interval))
    && (data.reason === undefined || data.reason === "authorization_pending" || data.reason === "slow_down")) {
    return { status: "pending", interval: data.interval as number | undefined, reason: data.reason as CopilotPendingReason | undefined };
  }
  if (data?.status === "authorized") {
    // Older BFFs may omit models; other malformed catalogues are not authorization.
    const models = data.models === undefined ? [] : data.models;
    if (Array.isArray(models) && models.length <= 1000) {
      const parsed: DiscoveredModel[] = [];
      for (const value of models) {
        const model = object(value);
        if (!model || typeof model.id !== "string" || !model.id || model.id.length > 512
          || (model.recommended !== undefined && typeof model.recommended !== "boolean")
          || (model.label != null && typeof model.label !== "string")
          || (model.detail != null && typeof model.detail !== "string")) {
          return { status: "error", error: copilotErrors.copilot_invalid_response };
        }
        parsed.push({
          id: model.id, recommended: model.recommended as boolean | undefined,
          label: typeof model.label === "string" ? model.label : null, detail: model.detail as string | undefined,
        });
      }
      return { status: "authorized", models: parsed };
    }
  }
  return { status: "error", error: copilotErrors.copilot_invalid_response };
}

interface Clock {
  now(): number;
  setTimeout(callback: () => void, milliseconds: number): ReturnType<typeof setTimeout>;
  clearTimeout(timer: ReturnType<typeof setTimeout>): void;
}

/** One in-memory attempt; cancellation suppresses callbacks, not an already executing BFF write. */
export function createCopilotLoginController(
  service: { start(): Promise<unknown>; poll(deviceCode: string, interval: number): Promise<unknown> },
  ui: {
    flow(value: CopilotLoginStart | null): void;
    starting(value: boolean): void;
    error(value: string | null): void;
    pending(value: CopilotPendingReason | undefined): void;
    authorized(models: DiscoveredModel[]): void;
  },
  clock: Clock = { now: Date.now, setTimeout, clearTimeout },
) {
  let generation = 0;
  let disposed = false;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  let expiryTimer: ReturnType<typeof setTimeout> | undefined;
  function invalidate() {
    generation++;
    if (pollTimer !== undefined) clock.clearTimeout(pollTimer);
    if (expiryTimer !== undefined) clock.clearTimeout(expiryTimer);
    pollTimer = expiryTimer = undefined;
  }
  function clear() {
    ui.flow(null);
    ui.starting(false);
    ui.pending(undefined);
  }
  function fail(message: string) {
    invalidate();
    clear();
    ui.error(message);
  }
  return {
    async begin() {
      if (disposed) return;
      invalidate();
      const attempt = generation;
      const startedAt = clock.now();
      const current = () => !disposed && attempt === generation;
      clear();
      ui.error(null);
      ui.starting(true);
      expiryTimer = clock.setTimeout(() => {
        if (current()) fail(copilotErrors.copilot_expired);
      }, 900_000);
      try {
        const result = object(await service.start());
        if (!current()) return;
        if (result?.ok === false) { fail(safeMessage(result.error)); return; }
        if (result?.ok !== true) { fail(copilotErrors.copilot_invalid_response); return; }
        let flow: CopilotLoginStart;
        try { flow = parseCopilotStart(result.data); }
        catch { fail(copilotErrors.copilot_invalid_response); return; }
        const expiresAt = startedAt + flow.expires_in * 1000;
        if (clock.now() >= expiresAt) { fail(copilotErrors.copilot_expired); return; }
        let interval = flow.interval;
        clock.clearTimeout(expiryTimer!);
        expiryTimer = clock.setTimeout(() => {
          if (current()) fail(copilotErrors.copilot_expired);
        }, expiresAt - clock.now());
        ui.starting(false);
        ui.flow(flow);
        ui.pending(undefined);
        const tick = async () => {
          if (!current()) return;
          if (clock.now() >= expiresAt) { fail(copilotErrors.copilot_expired); return; }
          try {
            const result = parseCopilotPoll(await service.poll(flow.device_code, interval));
            if (!current()) return;
            if (clock.now() >= expiresAt) { fail(copilotErrors.copilot_expired); return; }
            if (result.status === "error") { fail(result.error); return; }
            if (result.status === "authorized") {
              invalidate();
              clear();
              ui.authorized(result.models);
              return;
            }
            interval = Math.max(interval + (result.reason === "slow_down" ? 5 : 0), result.interval ?? interval);
            if (interval > 900) { fail(copilotErrors.copilot_rate_limited); return; }
            ui.pending(result.reason);
            schedule();
          } catch {
            if (current()) fail(copilotErrors.transport);
          }
        };
        const schedule = () => {
          // Schedule after settlement, never on a fixed interval or beyond the original deadline.
          if (clock.now() + interval * 1000 < expiresAt) {
            pollTimer = clock.setTimeout(() => { void tick(); }, interval * 1000);
          }
        };
        schedule();
      } catch {
        if (current()) fail(copilotErrors.transport);
      }
    },
    cancel() {
      if (disposed) return;
      invalidate();
      clear();
      ui.error(null);
    },
    dispose() { disposed = true; invalidate(); },
  };
}
