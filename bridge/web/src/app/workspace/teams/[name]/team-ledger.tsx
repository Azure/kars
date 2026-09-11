"use client";

// kars Bridge — team activity ledger list. Client component so the operator can
// expand past the latest 30 events (audit BUG-20: the list truncated at 30 with
// no way to reach the rest). Rendering matches the server-side list it replaced.

import { useState } from "react";
import Link from "next/link";
import type { LedgerEvent } from "@/lib/types";
import { toPlainPreview } from "@/components/deliverable-view";

const PAGE = 30;

export function TeamLedger({ team, ledger }: { team: string; ledger: LedgerEvent[] }) {
  const [showAll, setShowAll] = useState(false);

  if (ledger.length === 0) {
    return <p className="mt-4 text-xs text-foreground-muted">No activity recorded yet.</p>;
  }

  const shown = showAll ? ledger : ledger.slice(0, PAGE);

  return (
    <>
      <ul className="mt-4 space-y-1.5">
        {shown.map((e, i) => (
          <li key={`${e.at}-${i}`} className="flex items-start gap-3 text-xs">
            <span
              className={`mt-1 h-1.5 w-1.5 shrink-0 rounded-full ${
                e.kind === "delivery"
                  ? "bg-emerald-500"
                  : e.kind === "delivery_error"
                    ? "bg-rose-500"
                    : e.kind === "knowledge"
                      ? "bg-sky-500"
                      : "bg-foreground-muted"
              }`}
            />
            <span className="w-32 shrink-0 text-foreground-muted">
              {new Date(e.at).toLocaleString()}
            </span>
            <span className="w-20 shrink-0 font-medium capitalize">
              {e.kind.replace("_", " ")}
            </span>
            <span className="min-w-0 flex-1 truncate text-foreground-muted">
              {e.task ? (
                <Link href={`/workspace/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(e.task)}`} className="hover:text-foreground hover:underline">
                  {toPlainPreview(e.summary)}
                </Link>
              ) : (
                toPlainPreview(e.summary)
              )}
            </span>
            {e.tokens != null && (
              <span className="shrink-0 text-foreground-muted">{e.tokens.toLocaleString()}t</span>
            )}
          </li>
        ))}
      </ul>
      {ledger.length > PAGE && (
        <button
          type="button"
          onClick={() => setShowAll((v) => !v)}
          className="mt-3 text-[11px] font-medium text-signal hover:underline"
        >
          {showAll
            ? `Show latest ${PAGE} only`
            : `Show all ${ledger.length} events`}
        </button>
      )}
    </>
  );
}
