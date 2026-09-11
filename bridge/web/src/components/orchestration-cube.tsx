"use client";

// Orchestration cube — the visual spine of the compose/validate/execute flow.
// A slowly tumbling governed "core" surrounded by tiles that carry the REAL
// orchestration facts (how many models/harnesses/tools the cluster actually
// offers, what the efficiency frontier learned, the objective being composed).
// The cube's motion is pure presentation; every number shown is real data
// passed in by the caller — nothing here is fabricated. When the caller signals
// completion, the cube settles and the active phase advances to the end.

import { useEffect, useState } from "react";
import { RubiksCube } from "./rubiks-cube";
import { Icon, type IconName } from "./icon";

export interface OrchestrationPhase {
  /** Short label, e.g. "Reading cluster palette". */
  label: string;
  /** Real supporting fact, e.g. "12 models · 3 harnesses · 5 tool policies". */
  detail: string;
  /** Icon marker (see components/icon.tsx). */
  icon: IconName;
}

export function OrchestrationCube({
  title,
  phases,
  active,
  done,
  doneLabel,
}: {
  title: string;
  phases: OrchestrationPhase[];
  /** Index of the phase currently lit (caller-driven or auto-advanced). */
  active: number;
  /** True when the underlying real work (compose/validate) has completed. */
  done: boolean;
  doneLabel?: string;
}) {
  // Auto-advance through phases while work is in flight, but never past the
  // last "in-progress" phase until the caller reports `done` — so the animation
  // can't claim completion the real work hasn't reached.
  const [autoIdx, setAutoIdx] = useState(active);
  useEffect(() => {
    if (done) return; // `lit` already pins to the final phase when done.
    const id = setInterval(() => {
      setAutoIdx((i) => Math.min(i + 1, Math.max(phases.length - 2, 0)));
    }, 1400);
    return () => clearInterval(id);
  }, [done, phases.length]);

  const lit = done ? phases.length - 1 : Math.max(active, autoIdx);

  return (
    <div className="rounded-2xl border border-border bg-surface p-5">
      <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-foreground-muted">
        <span className={`inline-block h-1.5 w-1.5 rounded-full ${done ? "bg-signal" : "bg-accent"} ${done ? "" : "kb-pulse"}`} />
        {title}
      </div>
      <div className="mt-4 flex flex-col items-center gap-5 sm:flex-row sm:items-center sm:gap-8">
        <div className="grid shrink-0 place-items-center" style={{ width: 140, height: 140 }}>
          <RubiksCube size={104} settled={done} assembling={!done} />
        </div>
        <ol className="min-w-0 flex-1 space-y-2">
          {phases.map((p, i) => {
            const state = i < lit ? "done" : i === lit ? (done && i === phases.length - 1 ? "done" : "active") : "pending";
            return (
              <li
                key={p.label}
                className={`flex items-start gap-3 rounded-lg border px-3 py-2 transition ${
                  state === "active"
                    ? "border-accent/50 bg-accent/5"
                    : state === "done"
                      ? "border-signal/40 bg-signal/5"
                      : "border-border bg-surface-muted/30 opacity-60"
                }`}
              >
                <span
                  className={`mt-0.5 grid h-6 w-6 shrink-0 place-items-center rounded-md text-sm ${
                    state === "done" ? "bg-signal/15 text-signal" : state === "active" ? "bg-accent/15 text-accent" : "bg-surface text-foreground-muted"
                  }`}
                  aria-hidden
                >
                  {state === "done" ? "✓" : state === "active" ? <span className="kb-pulse inline-block h-2 w-2 rounded-full bg-accent" /> : <Icon name={p.icon} size={14} />}
                </span>
                <div className="min-w-0">
                  <p className="text-sm font-medium">{p.label}</p>
                  <p className="truncate text-xs text-foreground-muted">{p.detail}</p>
                </div>
              </li>
            );
          })}
        </ol>
      </div>
      {done && doneLabel && (
        <p className="mt-3 text-center text-xs font-medium text-signal sm:text-left">{doneLabel}</p>
      )}
    </div>
  );
}
