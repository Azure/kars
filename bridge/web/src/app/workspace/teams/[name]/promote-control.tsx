"use client";

// kars Bridge — team promotion control (§12). Requests a higher autonomy tier;
// the controller opens a human approval and only widens the envelope on
// approval, so this never grants authority directly.

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { requestPromotion } from "./promote-actions";

const TIER_LABELS: Record<number, string> = {
  1: "Manual",
  2: "Shared",
  3: "Conditional",
  4: "Supervised",
  5: "Full",
};

export function PromoteControl({ team, currentTier }: { team: string; currentTier: number }) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [open, setOpen] = useState(false);
  const [tier, setTier] = useState(Math.min(currentTier + 1, 5));
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  if (currentTier >= 5 && !done) {
    return null; // already at the ceiling
  }

  function submit() {
    setError(null);
    startTransition(async () => {
      const res = await requestPromotion(team, tier);
      if (res.error) {
        setError(res.error);
        return;
      }
      setDone(true);
      setOpen(false);
      router.refresh();
    });
  }

  if (done) {
    return (
      <p className="rounded-lg border border-sky-500/30 bg-sky-500/5 px-3 py-1.5 text-xs text-foreground-muted">
        Promotion requested — awaiting approval in the inbox.
      </p>
    );
  }

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted"
      >
        Request promotion
      </button>
    );
  }

  return (
    <div className="flex flex-wrap items-center gap-2">
      <select
        value={tier}
        onChange={(e) => setTier(Number(e.target.value))}
        className="rounded-lg border border-border bg-surface px-2 py-1.5 text-xs"
      >
        {[currentTier + 1, currentTier + 2, currentTier + 3, currentTier + 4]
          .filter((t) => t <= 5)
          .map((t) => (
            <option key={t} value={t}>
              Tier {t} · {TIER_LABELS[t]}
            </option>
          ))}
      </select>
      <button
        type="button"
        disabled={pending}
        onClick={submit}
        className="rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
      >
        {pending ? "Requesting…" : "Request approval"}
      </button>
      <button
        type="button"
        disabled={pending}
        onClick={() => setOpen(false)}
        className="rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted"
      >
        Cancel
      </button>
      {error && <span className="text-xs text-rose-600">{error}</span>}
    </div>
  );
}
