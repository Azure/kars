"use client";

// kars Bridge — "Delete team" control. A standing team is long-lived and owns
// runs, member sandboxes, shared memory and a task backlog, so deletion is
// destructive: this gates it behind an explicit two-step confirm before calling
// the server action (which cascade-removes everything the team owns).

import { useState, useTransition } from "react";
import { deleteTeam } from "./delete-actions";

export function DeleteTeamControl({ team }: { team: string }) {
  const [pending, startTransition] = useTransition();
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function submit() {
    setError(null);
    startTransition(async () => {
      const res = await deleteTeam(team);
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
        className="rounded-lg border border-rose-500/60 bg-rose-500/10 px-3 py-1.5 text-xs font-semibold text-rose-600 transition hover:bg-rose-600 hover:text-white hover:border-rose-600"
        title="Permanently delete this team and everything it owns."
      >
        Delete team
      </button>
    );
  }

  return (
    <div className="flex items-center gap-2">
      <span className="text-xs text-foreground-muted">Delete team and all its work?</span>
      <button
        type="button"
        disabled={pending}
        onClick={submit}
        className="rounded-lg bg-rose-600 px-3 py-1.5 text-xs font-semibold text-white transition hover:opacity-90 disabled:opacity-50"
      >
        {pending ? "Deleting…" : "Yes, delete"}
      </button>
      <button
        type="button"
        disabled={pending}
        onClick={() => setConfirming(false)}
        className="rounded-lg border border-border px-3 py-1.5 text-xs text-foreground-muted transition hover:bg-surface-muted"
      >
        Cancel
      </button>
      {error && <span className="text-xs text-rose-600">{error}</span>}
    </div>
  );
}
