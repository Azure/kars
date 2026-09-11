"use client";

// kars Bridge — "Delete mission" control. Deleting a mission is destructive: it
// removes the KarsTask and sweeps its deliverable, files, trace, and review
// record. Gated behind an explicit two-step confirm before the server action.

import { useState, useTransition } from "react";
import { deleteMission } from "./delete-actions";

export function DeleteMissionControl({ name }: { name: string }) {
  const [pending, startTransition] = useTransition();
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function submit() {
    setError(null);
    startTransition(async () => {
      const res = await deleteMission(name);
      // On success the action redirects; only an error returns here.
      if (res?.error) {
        setError(res.error);
        setConfirming(false);
      }
    });
  }

  if (!confirming) {
    return (
      <button
        type="button"
        onClick={() => setConfirming(true)}
        className="cursor-pointer rounded-lg border border-rose-500/60 bg-rose-500/10 px-3 py-1.5 text-xs font-semibold text-rose-600 transition hover:border-rose-600 hover:bg-rose-600 hover:text-white"
        title="Permanently delete this mission and its deliverable, files, trace, and review."
      >
        Delete mission
      </button>
    );
  }

  return (
    <div className="flex items-center gap-2">
      <span className="text-xs text-foreground-muted">Delete this mission and its records?</span>
      <button
        type="button"
        disabled={pending}
        onClick={submit}
        className="cursor-pointer rounded-lg bg-rose-600 px-3 py-1.5 text-xs font-semibold text-white transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
      >
        {pending ? "Deleting…" : "Yes, delete"}
      </button>
      <button
        type="button"
        disabled={pending}
        onClick={() => setConfirming(false)}
        className="cursor-pointer rounded-lg border border-border px-3 py-1.5 text-xs text-foreground-muted transition hover:bg-surface-muted"
      >
        Cancel
      </button>
      {error && <span className="text-xs text-rose-600">{error}</span>}
    </div>
  );
}
