"use client";

// kars Bridge — governed emergency-stop ("red button"). One click halts a
// running mission: the BFF un-launches it (the controller tears down the
// sandbox, removing the agent from the mesh so it can no longer receive or
// answer delegated work) and records the halt as a governed decision. The
// mission record, deliverable, and audit trail are retained — this is a STOP,
// not a delete. No major agent platform ships a governed kill.

import { useState } from "react";
import { useRouter } from "next/navigation";

export function HaltButton({ ns, task }: { ns: string; task: string }) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function halt() {
    setBusy(true);
    setError(null);
    try {
      const res = await fetch(`/api/namespaces/${ns}/tasks/${task}/halt`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ reason: reason.trim() || null }),
      });
      if (!res.ok) {
        const b = await res.json().catch(() => null);
        throw new Error(b?.error?.message ?? `Halt failed (${res.status})`);
      }
      setOpen(false);
      router.refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Halt failed");
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="inline-flex items-center gap-1.5 rounded-lg border border-danger/40 bg-danger/[0.06] px-3 py-1.5 text-xs font-semibold text-danger transition hover:bg-danger/10"
        title="Governed emergency-stop — halt this running agent and record the decision"
      >
        <span aria-hidden>⏹</span> Halt agent
      </button>
    );
  }

  return (
    <div className="rounded-xl border border-danger/40 bg-danger/[0.05] p-4">
      <p className="text-sm font-semibold text-danger">Halt this mission?</p>
      <p className="mt-1 text-xs text-foreground-muted">
        The agent&apos;s sandbox is torn down immediately — it leaves the mesh and stops all work.
        The deliverable, trace, and receipt are kept. The halt is recorded as a governed decision.
      </p>
      <input
        type="text"
        value={reason}
        onChange={(e) => setReason(e.target.value)}
        placeholder="Reason (recorded on the decision) — e.g. runaway cost, wrong scope"
        className="mt-3 w-full rounded-lg border border-border bg-surface px-3 py-2 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-danger"
      />
      <div className="mt-3 flex items-center gap-2">
        <button
          type="button"
          onClick={halt}
          disabled={busy}
          className="rounded-lg bg-danger px-3 py-1.5 text-xs font-semibold text-white transition hover:opacity-90 disabled:opacity-50"
        >
          {busy ? "Halting…" : "Confirm halt"}
        </button>
        <button
          type="button"
          onClick={() => setOpen(false)}
          disabled={busy}
          className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground"
        >
          Cancel
        </button>
        {error && <span className="text-xs text-danger">{error}</span>}
      </div>
    </div>
  );
}
