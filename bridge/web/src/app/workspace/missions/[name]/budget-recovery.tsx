"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { runMissionClient } from "@/lib/run-mission-client";

function suggestedBudget(current: number | null, spent: number | null) {
  const baseline = current ?? 200_000;
  const target = Math.max(baseline * 2, (spent ?? baseline) + baseline);
  return Math.ceil(target / 50_000) * 50_000;
}

export function BudgetRecovery({
  namespace,
  name,
  current,
  spent,
  stoppedLimit,
  approvalPending,
}: {
  namespace: string;
  name: string;
  current: number | null;
  spent: number | null;
  stoppedLimit: number | null;
  approvalPending: boolean;
}) {
  const router = useRouter();
  const [budget, setBudget] = useState(suggestedBudget(current, spent));
  const [pending, startTransition] = useTransition();
  const [requested, setRequested] = useState(approvalPending);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  function requestIncrease() {
    setError(null);
    setMessage(null);
    startTransition(async () => {
      try {
        const response = await fetch(
          `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/budget`,
          {
            method: "POST",
            cache: "no-store",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ daily_tokens: budget }),
          },
        );
        const payload = await response.json().catch(() => null);
        if (!response.ok || !payload?.requested) {
          setError(payload?.error ?? `Budget request failed (${response.status}).`);
          return;
        }
        setRequested(true);
        setMessage(
          `Requested a ${budget.toLocaleString()} token ceiling. Approve the typed budget request below or in Inbox.`,
        );
        router.refresh();
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : "Budget request failed.");
      }
    });
  }

  function continueMission() {
    setError(null);
    setMessage(null);
    startTransition(async () => {
      try {
        const run = await runMissionClient(namespace, name);
        if (run.error) {
          setError(run.error);
          return;
        }
        setMessage(
          `The next governed run has started with the ${current?.toLocaleString()} token ceiling.`,
        );
        router.refresh();
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : "Mission restart failed.");
      }
    });
  }

  if (stoppedLimit != null && current != null && current > stoppedLimit) {
    return (
      <div className="mt-3 rounded-lg border border-ok/30 bg-ok/[0.05] px-3 py-3">
        <p className="text-xs font-medium">
          Budget increase approved — the enforced ceiling is now {current.toLocaleString()} tokens.
        </p>
        <button
          type="button"
          disabled={pending}
          onClick={continueMission}
          className="mt-2 rounded-md bg-signal px-3 py-2 text-xs font-semibold text-signal-fg disabled:opacity-50"
        >
          {pending ? "Starting…" : "Continue mission"}
        </button>
        {error && <p className="mt-2 text-xs text-danger">{error}</p>}
        {message && <p className="mt-2 text-xs text-ok">{message}</p>}
      </div>
    );
  }

  if (requested) {
    return (
      <div className="mt-3 rounded-lg border border-warning/30 bg-surface px-3 py-3">
        <p className="text-xs font-medium">Budget increase awaiting human approval.</p>
        <p className="mt-1 text-[11px] text-foreground-muted">
          The controller will widen the envelope only after the typed approval is accepted.
        </p>
        <a href="/workspace/inbox" className="mt-2 inline-block text-xs font-medium text-signal hover:underline">
          Open Inbox approval →
        </a>
        {message && <p className="mt-2 text-xs text-ok">{message}</p>}
        {error && <p className="mt-2 text-xs text-danger">{error}</p>}
      </div>
    );
  }

  return (
    <div className="mt-3 rounded-lg border border-warning/30 bg-surface px-3 py-3">
      <div className="flex flex-wrap items-end gap-2">
        <label className="text-xs font-medium">
          Requested daily token budget
          <input
            type="number"
            min={(current ?? 0) + 1}
            step={50_000}
            value={budget}
            onChange={(event) => setBudget(Number(event.target.value))}
            className="mt-1 block w-44 rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm tabular-nums"
          />
        </label>
        <button
          type="button"
          disabled={pending || budget <= (current ?? 0)}
          onClick={requestIncrease}
          className="rounded-md bg-signal px-3 py-2 text-xs font-semibold text-signal-fg disabled:opacity-50"
        >
          {pending ? "Requesting…" : "Request budget increase"}
        </button>
      </div>
      <p className="mt-2 text-[11px] text-foreground-muted">
        Opens a typed approval. The controller—not Bridge—widens the trust envelope after approval.
      </p>
      {error && <p className="mt-2 text-xs text-danger">{error}</p>}
    </div>
  );
}
