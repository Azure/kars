"use client";

// kars Bridge — "Run now" control. Triggers an immediate team run so a team
// (especially a cadence-less "on demand" one) actually acts, and the operator
// can watch the principal launch → spawn sub-agents → deliver. Disabled while
// paused.

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { runTeamNow } from "./run-actions";

export function RunNowControl({
  team,
  paused,
  inFlight,
}: {
  team: string;
  paused: boolean;
  inFlight: boolean;
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  // Run now mints a real, budget-spending run — require an explicit confirm so a
  // single stray click can't launch work (audit B30).
  const [confirming, setConfirming] = useState(false);

  function submit() {
    setError(null);
    setConfirming(false);
    startTransition(async () => {
      const res = await runTeamNow(team);
      if (res.error) {
        setError(res.error);
        return;
      }
      setDone(true);
      router.refresh();
      for (const delay of [2_000, 5_000, 10_000, 20_000]) {
        setTimeout(() => router.refresh(), delay);
      }
    });
  }

  if (paused) {
    return (
      <span
        className="rounded-lg border border-border bg-surface-muted px-3 py-1.5 text-xs text-foreground-muted"
        title="Resume the team to run it."
      >
        Hibernating
      </span>
    );
  }

  if (inFlight) {
    return (
      <span className="rounded-lg border border-sky-500/30 bg-sky-500/5 px-3 py-1.5 text-xs text-sky-600">
        Run in progress
      </span>
    );
  }

  if (done) {
    return (
      <span className="rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-3 py-1.5 text-xs text-emerald-600">
        Run requested — launching…
      </span>
    );
  }

  if (confirming) {
    return (
      <div className="flex items-center gap-2">
        <span className="text-xs text-foreground-muted">Mint a run now? It spends budget.</span>
        <button
          type="button"
          disabled={pending}
          onClick={submit}
          className="rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
        >
          {pending ? "Requesting…" : "Confirm run"}
        </button>
        <button
          type="button"
          onClick={() => setConfirming(false)}
          className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium text-foreground-muted hover:bg-surface-muted"
        >
          Cancel
        </button>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        disabled={pending}
        onClick={() => setConfirming(true)}
        className="rounded-lg bg-signal px-3 py-1.5 text-xs font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
        title="Trigger an immediate run of this team (asks for confirmation)."
      >
        Run now
      </button>
      {error && <span className="text-xs text-rose-600">{error}</span>}
    </div>
  );
}
