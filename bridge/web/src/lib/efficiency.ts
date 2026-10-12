// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Efficiency, RouteEfficiency } from "./types/operations";

export function measured(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

export function metric(value: number | null | undefined, digits?: number): string {
  if (!measured(value)) return "—";
  return digits === undefined ? value.toLocaleString() : value.toFixed(digits);
}

export function lowerWins(a: number | null, b: number | null): boolean | null {
  return !measured(a) || !measured(b) || a === b ? null : a < b;
}

export function recommendedStats(efficiency: Efficiency | null | undefined): RouteEfficiency | null {
  if (!efficiency?.recommended || !efficiency.recommended_harness) return null;
  return efficiency.routes.find(route => route.route === efficiency.recommended
    && route.harness === efficiency.recommended_harness) ?? null;
}

export function measuredDeliveredTokens(efficiency: Efficiency | null): number | null {
  const values = efficiency?.routes.map(route => route.measured_tokens).filter(measured) ?? [];
  return values.length ? values.reduce((sum, value) => sum + value, 0) : null;
}

export function measuredRoutes(efficiency: Efficiency): (RouteEfficiency & { tokens_per_outcome: number })[] {
  return efficiency.routes.filter((route): route is RouteEfficiency & { tokens_per_outcome: number } =>
    route.delivered > 0 && measured(route.tokens_per_outcome));
}
