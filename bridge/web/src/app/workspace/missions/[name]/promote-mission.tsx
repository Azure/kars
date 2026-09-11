"use client";

// Per-mission autonomy promotion (§12). Requests a higher tier; the controller
// opens a human approval and only widens the envelope once approved — this
// never grants authority directly. Mirrors the standing-team promote control.

import { useActionState } from "react";
import { promoteMissionAction, type ReplicateState } from "./run-actions";

const init: ReplicateState = { error: null, ok: null };
const TIERS: Record<number, string> = { 1: "Manual", 2: "Shared", 3: "Conditional", 4: "Supervised", 5: "Full" };

export function PromoteMission({ ns, name, currentTier }: { ns: string; name: string; currentTier: number }) {
  const [state, action, pending] = useActionState(promoteMissionAction, init);
  const next = Math.min(currentTier + 1, 5);
  if (currentTier >= 5 && !state.ok) return null;
  if (state.ok) {
    return <div className="rounded-lg border border-signal/30 bg-signal/5 px-3 py-2 text-xs">{state.ok}</div>;
  }
  return (
    <form action={action} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-surface-muted/30 px-3 py-2.5">
      <input type="hidden" name="ns" value={ns} />
      <input type="hidden" name="name" value={name} />
      <div>
        <p className="text-xs font-semibold">Request more autonomy</p>
        <p className="mt-0.5 text-[11px] text-foreground-muted">Currently Tier {currentTier} ({TIERS[currentTier] ?? "?"}). A promotion opens a human approval — it never widens authority directly.</p>
      </div>
      <div className="flex items-center gap-2">
        <label className="text-[11px] text-foreground-muted">
          →
          <select name="tier" aria-label="Target autonomy tier" defaultValue={next} className="ml-1 rounded border border-border bg-surface px-1.5 py-1 text-[11px]">
            {[2, 3, 4, 5].filter((t) => t > currentTier).map((t) => <option key={t} value={t}>Tier {t} ({TIERS[t]})</option>)}
          </select>
        </label>
        <button type="submit" disabled={pending} className="rounded-md border border-accent/40 bg-accent/10 px-2.5 py-1 text-[11px] font-semibold text-accent disabled:opacity-50">
          {pending ? "Requesting…" : "Request promotion"}
        </button>
      </div>
      {state.error && <span className="w-full text-[11px] text-danger">{state.error}</span>}
    </form>
  );
}
