"use client";

import { useRouter } from "next/navigation";
import { useState } from "react";

export function HaltTeamRunButton({
  ns,
  team,
  run,
}: {
  ns: string;
  team: string;
  run: string;
}) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function halt() {
    setBusy(true);
    setError(null);
    try {
      const response = await fetch(
        `/api/namespaces/${encodeURIComponent(ns)}/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(run)}/halt`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ reason: reason.trim() || null }),
        },
      );
      if (!response.ok) {
        const body = await response.json().catch(() => null);
        throw new Error(body?.error?.message ?? `Emergency stop failed (${response.status})`);
      }
      setOpen(false);
      router.refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Emergency stop failed");
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="inline-flex items-center gap-1.5 rounded-lg border border-danger/40 bg-danger/[0.06] px-3 py-1.5 text-xs font-semibold text-danger hover:bg-danger/10"
        title="Stop this run, preserve its evidence, and pause the standing team"
      >
        <span aria-hidden>■</span> Emergency stop
      </button>
    );
  }

  return (
    <div className="rounded-xl border border-danger/40 bg-danger/[0.05] p-4">
      <p className="text-sm font-semibold text-danger">Stop this team run?</p>
      <p className="mt-1 text-xs text-foreground-muted">
        The active run sandbox is torn down, retained evidence remains, and the standing team is
        paused so it cannot immediately launch replacement work.
      </p>
      <input
        type="text"
        value={reason}
        onChange={(event) => setReason(event.target.value)}
        placeholder="Reason — e.g. runaway cost, wrong scope, unsafe behavior"
        className="mt-3 w-full rounded-lg border border-border bg-surface px-3 py-2 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-danger"
      />
      <div className="mt-3 flex items-center gap-2">
        <button
          type="button"
          onClick={halt}
          disabled={busy}
          className="rounded-lg bg-danger px-3 py-1.5 text-xs font-semibold text-white disabled:opacity-50"
        >
          {busy ? "Stopping…" : "Confirm stop"}
        </button>
        <button
          type="button"
          onClick={() => setOpen(false)}
          disabled={busy}
          className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium text-foreground-muted"
        >
          Cancel
        </button>
        {error && <span className="text-xs text-danger">{error}</span>}
      </div>
    </div>
  );
}
