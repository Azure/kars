"use client";

// kars Bridge Operator Console — detailed eval report. Expands an eval to show
// exactly what the adversarial baseline probes (each case + the expected
// decision) and the latest per-case verdict (pass/fail + what the router
// actually did). Sourced from the corpus + report ConfigMaps the controller
// persists — real evidence, downloadable for an auditor.

import { useState } from "react";
import type { EvalReport, EvalCase } from "@/lib/types";

/** Robust file download — appends the anchor to the DOM (required by Firefox)
 *  and revokes the object URL, so the file always lands with the right name. */
function downloadFile(filename: string, content: string, mime: string) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function CaseRow({ c }: { c: EvalCase }) {
  const tone = c.errored
    ? "border-warning/40 bg-warning/10 text-warning"
    : c.pass === true
      ? "border-ok/40 bg-ok/10 text-ok"
      : c.pass === false
        ? "border-danger/40 bg-danger/10 text-danger"
        : "border-border bg-surface-muted text-foreground-muted";
  const label = c.errored ? "ERRORED" : c.pass === true ? "PASS" : c.pass === false ? "FAIL" : "—";
  return (
    <li className="border-t border-border px-3 py-2.5 text-xs">
      <div className="flex items-start gap-2">
        <span className={`mt-0.5 shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-semibold ${tone}`}>
          {label}
        </span>
        <div className="min-w-0">
          <p className="font-mono font-medium">{c.id}</p>
          {c.probe && <p className="mt-0.5 break-words text-foreground-muted">Probe: {c.probe}</p>}
          <p className="mt-0.5 text-[11px] text-foreground-muted">
            expected <span className="font-medium text-foreground">{c.expected ?? "—"}</span>
            {c.actual != null && (
              <>
                {" "}· actual{" "}
                <span className={`font-medium ${c.errored ? "text-warning" : c.pass === false ? "text-danger" : "text-foreground"}`}>
                  {c.actual}
                </span>
              </>
            )}
            {c.tags.length > 0 && <> · {c.tags.join(", ")}</>}
          </p>
          {c.errored && c.actual_reason && (
            <p className="mt-0.5 break-words text-[11px] text-warning/80">
              Couldn&apos;t evaluate (inconclusive): {c.actual_reason}
            </p>
          )}
          {!c.errored && c.pass === false && c.actual_reason && (
            <p className="mt-0.5 break-words text-[11px] text-danger/80">Reason: {c.actual_reason}</p>
          )}
        </div>
      </div>
    </li>
  );
}

export function EvalDetail({ name }: { name: string }) {
  const [open, setOpen] = useState(false);
  const [report, setReport] = useState<EvalReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function toggle() {
    const next = !open;
    setOpen(next);
    if (next && !report && !loading) {
      setLoading(true);
      setError(null);
      try {
        const res = await fetch(`/api/operator/evals/${encodeURIComponent(name)}/report`, {
          headers: { accept: "application/json" },
        });
        if (!res.ok) throw new Error(`load failed (${res.status})`);
        setReport(await res.json());
      } catch (e) {
        setError(e instanceof Error ? e.message : "Couldn't load the report.");
      } finally {
        setLoading(false);
      }
    }
  }

  return (
    <div className="mt-3">
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={toggle}
          className="rounded-lg border border-border px-3 py-1.5 text-xs font-medium text-foreground-muted hover:text-foreground"
        >
          {open ? "Hide" : "View"} detailed report{report ? ` · ${report.cases.length} cases` : ""}
        </button>
        {report && (
          <button
            type="button"
            onClick={() => downloadFile(`eval-report-${name}.json`, JSON.stringify(report, null, 2), "application/json")}
            className="rounded-lg border border-border bg-surface-muted px-3 py-1.5 text-xs font-medium hover:bg-surface-muted/70"
          >
            ⭳ Download report (JSON)
          </button>
        )}
      </div>

      {open && (
        <div className="mt-2">
          {loading && <p className="text-xs text-foreground-muted">Loading the baseline + verdicts…</p>}
          {error && <p className="text-xs text-danger">{error}</p>}
          {report && (
            <>
              <p className="text-[11px] text-foreground-muted">
                Baseline: <span className="font-mono">{report.corpus ?? name}</span> — {report.cases.length} adversarial
                case{report.cases.length === 1 ? "" : "s"}
                {report.completed_at ? ` · last run ${new Date(report.completed_at).toLocaleString()}` : " · not run yet"}.
              </p>
              {!report.per_case_available && report.completed_at && (
                <p className="mt-1 rounded-lg border border-warning/40 bg-warning/[0.06] px-3 py-2 text-[11px] text-foreground-muted">
                  Per-case verdicts weren&apos;t captured for the last run (it predates detailed reporting) — the
                  summary shows {report.passed}/{report.total} passed. Re-run this eval to capture which specific
                  cases pass or fail and why. The baseline it probes is shown below.
                </p>
              )}
              <ul className="mt-2 overflow-hidden rounded-lg border border-border bg-surface">
                {report.cases.map((c) => (
                  <CaseRow key={c.id} c={c} />
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </div>
  );
}
