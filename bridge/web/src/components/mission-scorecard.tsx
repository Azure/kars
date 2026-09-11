// kars Bridge Workspace — mission scorecard (graphical, honest).
//
// The efficiency numbers the plan promises to deliver to USERS. Structural
// facts (decisions, budget, receipt) are real; runtime token/latency render the
// explicit "needs a real run" state rather than fabricated zeros.

import { HonestState } from "@/components/honest-state";
import type { Scorecard } from "@/lib/types";

export function MissionScorecard({ scorecard }: { scorecard: Scorecard }) {
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Efficiency scorecard</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        How this mission performed and what it was allowed to spend.
      </p>

      <dl className="mt-4 grid grid-cols-2 gap-4 sm:grid-cols-4">
        <Stat label="Decisions you made" value={String(scorecard.decisions_recorded)} />
        <Stat
          label="Approved / Denied"
          value={`${scorecard.approvals_granted} / ${scorecard.approvals_denied}`}
        />
        <Stat
          label="Token budget"
          value={scorecard.token_budget != null ? scorecard.token_budget.toLocaleString() : "No cap"}
        />
        <Stat
          label="Receipt"
          value={scorecard.receipt_issued ? "Signed ✓" : "—"}
          ok={scorecard.receipt_issued}
        />
      </dl>

      <div className="mt-5 border-t border-border pt-4">
        <p className="text-xs font-medium uppercase tracking-wide text-foreground-muted">
          Token cost &amp; speed
        </p>
        <div className="mt-2">
          {scorecard.runtime_metrics_available ? (
            <div className="space-y-2">
              <div className="flex flex-wrap gap-4">
                <Stat label="Total tokens" value={scorecard.run_total_tokens?.toLocaleString() ?? "—"} />
                <Stat
                  label="In / out"
                  value={
                    scorecard.run_prompt_tokens != null && scorecard.run_completion_tokens != null
                      ? `${scorecard.run_prompt_tokens.toLocaleString()} / ${scorecard.run_completion_tokens.toLocaleString()}`
                      : "—"
                  }
                />
                {scorecard.run_model && <Stat label="Model" value={scorecard.run_model} />}
              </div>
              <p className="text-xs text-foreground-muted">
                Real tokens from the latest governed run. Streaming latency / time-to-first-result is
                a named next step.
              </p>
            </div>
          ) : (
            <HonestState
              variant="needs_run"
              compact
              title="No run captured yet"
              detail={
                scorecard.runtime_metrics_note ??
                "Run the mission to capture a real governed run with its real token cost."
              }
            />
          )}
        </div>
      </div>
    </section>
  );
}

function Stat({ label, value, ok }: { label: string; value: string; ok?: boolean }) {
  return (
    <div>
      <dt className="text-xs text-foreground-muted">{label}</dt>
      <dd className={`mt-1 text-lg font-semibold tabular-nums ${ok ? "text-ok" : "text-foreground"}`}>
        {value}
      </dd>
    </div>
  );
}
