"use client";

// Reliability runner — the pass^k trigger. Runs a delivered mission's EXACT
// package k more times so the efficiency frontier can measure pass^k
// reliability (fraction of the repeated package accepted on every attempt).
// These are real governed runs, so it's a deliberate two-step action.

import { useActionState, useState } from "react";
import { replicateMissionAction, type ReplicateState } from "./run-actions";

const init: ReplicateState = { error: null, ok: null };

export function ReliabilityRunner({ ns, name }: { ns: string; name: string }) {
  const [state, action, pending] = useActionState(replicateMissionAction, init);
  const [count, setCount] = useState(2);
  const [open, setOpen] = useState(false);

  if (state.ok) {
    return (
      <div className="rounded-lg border border-signal/30 bg-signal/5 px-3 py-2 text-xs text-foreground">
        {state.ok}
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-border bg-surface-muted/30 px-3 py-2.5">
      <div className="flex items-center justify-between gap-3">
        <div>
          <p className="text-xs font-semibold">Measure reliability (pass^k)</p>
          <p className="mt-0.5 text-[11px] text-foreground-muted">
            Run this exact package again to see how consistently it succeeds — the honest reliability signal.
          </p>
        </div>
        {!open ? (
          <button type="button" onClick={() => setOpen(true)} className="shrink-0 rounded-md border border-border px-2.5 py-1 text-[11px] font-medium hover:bg-surface">
            Run again…
          </button>
        ) : (
          <form action={action} className="flex shrink-0 items-center gap-2">
            <input type="hidden" name="ns" value={ns} />
            <input type="hidden" name="name" value={name} />
            <label className="text-[11px] text-foreground-muted">
              ×
              <select name="count" aria-label="Number of repeat runs" value={count} onChange={(e) => setCount(Number(e.target.value))} className="ml-1 rounded border border-border bg-surface px-1.5 py-1 text-[11px]">
                {[2, 3, 4, 5].map((n) => <option key={n} value={n}>{n}</option>)}
              </select>
            </label>
            <button type="submit" disabled={pending} className="rounded-md border border-signal/40 bg-signal/10 px-2.5 py-1 text-[11px] font-semibold text-signal disabled:opacity-50">
              {pending ? "Launching…" : `Run ${count}× more`}
            </button>
            <button type="button" onClick={() => setOpen(false)} className="text-[11px] text-foreground-muted hover:text-foreground">Cancel</button>
          </form>
        )}
      </div>
      {state.error && <p className="mt-1.5 text-[11px] text-danger">{state.error}</p>}
    </div>
  );
}
