"use client";

// kars Bridge Operator Console — audit search. The auditor's investigative
// entry point: type a run (mission/team-run) or agent name and see exactly that
// subject's receipts and where each sits in the hash-chained inclusion log — the
// chain of custody for one run, not the whole fleet at once. Auditing is an
// OPERATOR capability: the workspace (employee) surface never exposes the
// fleet-wide audit log; an operator investigates a specific run/agent here.

import { useMemo, useState } from "react";
import { Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { AuditReceiptRow } from "./audit-receipt-row";
import type { ReceiptSummary } from "@/lib/types";

export function AuditSearch({ receipts }: { receipts: ReceiptSummary[] }) {
  const [query, setQuery] = useState("");
  const [verdict, setVerdict] = useState<"all" | "verified" | "partial" | "failed">("all");
  const [newestFirst, setNewestFirst] = useState(true);
  const q = query.trim().toLowerCase();

  const matches = useMemo(() => {
    let out = receipts.filter((r) => {
      if (verdict !== "all" && r.verdict !== verdict) return false;
      if (!q) return true;
      const hay = [r.task ?? "", r.name, r.namespace].join(" ").toLowerCase();
      return hay.includes(q);
    });
    out = [...out].sort((a, b) => {
      const av = a.created ?? "";
      const bv = b.created ?? "";
      return newestFirst ? bv.localeCompare(av) : av.localeCompare(bv);
    });
    return out;
  }, [receipts, q, verdict, newestFirst]);

  const verdictCounts = useMemo(() => {
    const c = { verified: 0, partial: 0, failed: 0 };
    for (const r of receipts) {
      if (r.verdict === "verified") c.verified++;
      else if (r.verdict === "partial") c.partial++;
      else if (r.verdict === "failed") c.failed++;
    }
    return c;
  }, [receipts]);

  // Chain of custody for the current result set: the ordered inclusion-log
  // positions the matched receipts occupy. Contiguity is a signal, not a
  // requirement — a run's receipts interleave with other runs in one global log.
  const chain = useMemo(
    () =>
      matches
        .filter((r) => r.inclusion_seq != null)
        .map((r) => r.inclusion_seq as number)
        .sort((a, b) => a - b),
    [matches],
  );
  const unchained = matches.filter((r) => r.inclusion_seq == null).length;

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <label className="relative flex-1">
          <span className="sr-only">Search audit records by run or agent</span>
          <svg
            viewBox="0 0 20 20"
            aria-hidden
            className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-foreground-muted"
            fill="currentColor"
          >
            <path d="M9 3.5a5.5 5.5 0 1 0 3.4 9.83l3.13 3.14a1 1 0 0 0 1.42-1.42l-3.14-3.13A5.5 5.5 0 0 0 9 3.5Zm0 2a3.5 3.5 0 1 1 0 7 3.5 3.5 0 0 1 0-7Z" />
          </svg>
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search by run or agent name (e.g. teamx-run-… or a mission name)"
            className="w-full rounded-lg border border-border bg-surface py-2 pl-9 pr-3 text-sm outline-none transition focus:border-signal focus:ring-2 focus:ring-signal/30"
          />
        </label>
        <Badge tone="muted">
          {q || verdict !== "all" ? `${matches.length} of ${receipts.length}` : `${receipts.length}`} receipt
          {matches.length === 1 ? "" : "s"}
        </Badge>
      </div>

      {/* Verdict filter + sort (f35) — narrow by outcome and order by time. */}
      <div className="flex flex-wrap items-center gap-2">
        {([
          ["all", `All ${receipts.length}`],
          ["verified", `Verified ${verdictCounts.verified}`],
          ["partial", `Partial ${verdictCounts.partial}`],
          ["failed", `Failed ${verdictCounts.failed}`],
        ] as const).map(([v, label]) => (
          <button
            key={v}
            type="button"
            onClick={() => setVerdict(v)}
            className={`rounded-full border px-3 py-1 text-xs font-medium transition ${
              verdict === v ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted hover:text-foreground"
            }`}
          >
            {label}
          </button>
        ))}
        <button
          type="button"
          onClick={() => setNewestFirst((v) => !v)}
          className="ml-auto rounded-full border border-border px-3 py-1 text-xs font-medium text-foreground-muted hover:text-foreground"
          title="Toggle chronological order"
        >
          {newestFirst ? "Newest first ↓" : "Oldest first ↑"}
        </button>
      </div>

      {/* Chain-of-custody strip for the current subject. */}
      {q && chain.length > 0 && (
        <div className="rounded-lg border border-signal/25 bg-signal/[0.04] p-3">
          <p className="text-[11px] font-semibold uppercase tracking-wide text-signal">
            Chain of custody · {matches.length} receipt{matches.length === 1 ? "" : "s"} for “{query.trim()}”
          </p>
          <p className="mt-1 font-mono text-xs text-foreground-muted">
            inclusion-log positions: {chain.map((n) => `#${n}`).join(" → ")}
            {unchained > 0 && (
              <span className="text-amber-600"> · {unchained} not yet chained</span>
            )}
          </p>
        </div>
      )}

      {matches.length === 0 ? (
        <HonestState
          variant="empty"
          compact
          title={
            q
              ? "No matching receipts"
              : verdict !== "all"
                ? `No ${verdict} receipts`
                : "No receipts yet"
          }
          detail={
            q
              ? "No governance receipt matches that run or agent. Check the exact run/agent name, or clear the search to see all."
              : verdict !== "all"
                ? `${receipts.length} receipt${receipts.length === 1 ? "" : "s"} exist, but none currently carry the “${verdict}” verdict. Clear the filter to see them.`
                : "A receipt is issued for each governance-validated run. None have been produced."
          }
        />
      ) : (
        <ul className="overflow-hidden rounded-lg border border-border">
          {matches.map((r) => (
            <AuditReceiptRow
              key={`${r.namespace}/${r.name}`}
              ns={r.namespace}
              task={r.task ?? r.name}
              summarySeq={r.inclusion_seq}
              summaryTask={r.task ?? r.name}
              summaryVerdict={r.verdict}
              summaryCreated={r.created}
            />
          ))}
        </ul>
      )}
    </div>
  );
}
