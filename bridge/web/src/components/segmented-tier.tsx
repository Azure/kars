"use client";

// kars Bridge — segmented autonomy-tier control.
//
// A precise 1–5 selector that mirrors the tier-scale visualization on the
// task detail page, so the *input* and the *evidence* speak the same visual
// language. Far clearer than a bare range slider for a 5-value authority
// choice, and it can render a "ceiling" marker for the authority relationship.

import { TIER_LABELS } from "@/lib/types";

export function SegmentedTier({
  name,
  value,
  onChange,
  ceiling,
  invalidAbove,
}: {
  name: string;
  value: number;
  onChange: (v: number) => void;
  /** When set, draws a ceiling ring on this tier (authority ceiling). */
  ceiling?: number;
  /** When set, tiers strictly above this are marked invalid (ceiling > tier). */
  invalidAbove?: number;
}) {
  return (
    <div>
      <input type="hidden" name={name} value={value} />
      <div
        role="radiogroup"
        aria-label={name}
        className="grid grid-cols-5 gap-2"
      >
        {[1, 2, 3, 4, 5].map((t) => {
          const active = t === value;
          const within = t <= value;
          const isCeiling = ceiling === t;
          const invalid = invalidAbove != null && t > invalidAbove;
          return (
            <button
              key={t}
              type="button"
              role="radio"
              aria-checked={active}
              onClick={() => onChange(t)}
              className={[
                "flex flex-col items-center gap-1 rounded-lg border px-2 py-2.5 text-center transition",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal",
                active
                  ? "border-signal bg-signal/10"
                  : within
                    ? "border-signal/30 bg-signal/5"
                    : "border-border bg-surface hover:border-foreground-muted/40",
                invalid ? "opacity-40" : "",
                isCeiling ? "ring-2 ring-warning ring-offset-1 ring-offset-surface" : "",
              ].join(" ")}
            >
              <span
                className={[
                  "relative grid h-6 w-6 place-items-center rounded text-xs font-semibold",
                  active
                    ? "bg-signal text-signal-fg"
                    : within
                      ? "text-signal"
                      : "text-foreground-muted",
                ].join(" ")}
              >
                {t}
              </span>
              <span className="text-[11px] leading-tight text-foreground-muted">
                {TIER_LABELS[t]}
              </span>
              {active && (
                <span className="text-[10px] font-medium leading-none text-signal">
                  selected
                </span>
              )}
              {isCeiling && !active && (
                <span className="text-[10px] font-medium leading-none text-warning">
                  ceiling
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}
