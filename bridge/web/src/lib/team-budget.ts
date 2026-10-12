// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Budget } from "./types";

export function parseTeamBudget(tokens: unknown, scope: unknown, usdMicros?: unknown):
  { budget: Budget; error: null } | { budget: null; error: string } {
  const value = typeof tokens === "string" && tokens.trim() ? Number(tokens) : NaN;
  if (!Number.isSafeInteger(value) || value <= 0) {
    return { budget: null, error: "Review a finite lifetime token limit. Team token limits must be positive safe whole numbers; an invalid cap cannot be discarded." };
  }
  if (scope !== "GovernedInference") {
    return { budget: null, error: "A Team lifetime limit requires explicit GovernedInference scope." };
  }
  let cost: number | null = null;
  if (usdMicros !== undefined && usdMicros !== null && usdMicros !== "") {
    cost = typeof usdMicros === "string" && usdMicros.trim() ? Number(usdMicros) : NaN;
    if (!Number.isSafeInteger(cost) || cost <= 0) {
      return { budget: null, error: "The optional lifetime cost limit must be a positive safe whole number of micro-USD; an invalid cap cannot be discarded." };
    }
  }
  return { budget: { scope, tokens: value, usd_micros: cost }, error: null };
}
