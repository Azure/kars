// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Workspace — the live mission map (design note §4). A single
// at-a-glance view of a governed run: the delegation tree (principal + reports
// + sub-agents) with each node's authority tier, the live token burn against
// budget, and the orchestration shape (rounds + tool calls). Purely a
// projection of data already on the mission — no new fetch, updates with the
// page's live refresh.

import { TIER_LABELS, type TaskDetail } from "@/lib/types";
import { activityForRevision, missionRunState } from "@/lib/mission-run-evidence";

function TierPip({ tier }: { tier: number }) {
  return (
    <span className="inline-flex items-center gap-1 rounded-full bg-surface-muted px-2 py-0.5 text-[10px] font-medium text-foreground-muted">
      T{tier} · {TIER_LABELS[tier] ?? "?"}
    </span>
  );
}

export function MissionMap({ task }: { task: TaskDetail }) {
  const run = missionRunState(task);
  const total = run.currentResult?.total_tokens ?? null;
  const budget = task.envelope.budget?.tokens ?? null;
  const burnPct = total != null && budget != null && budget > 0
    ? Math.min(100, Math.round((total / budget) * 100))
    : null;
  const rounds = run.currentResult ? task.telemetry?.rounds ?? null : null;
  const toolCalls = run.currentResult ? task.telemetry?.tool_calls ?? null : null;
  // Fall back to counts derived from the real activity trace when the run's
  // output ConfigMap didn't roll up loop-shape telemetry (some harnesses report
  // the trace but not the totals) — mirrors ActivityStream so the map and the
  // activity feed always agree instead of the map claiming "Not run yet".
  const activity = task.activity.filter((event) => activityForRevision(event, task.current_run_nonce ?? ""));
  const roundEvents = activity.filter((e) => e.kind === "round").length;
  const toolEvents = activity.filter((e) => e.kind === "tool").length;
  const observations = run.routerActivity?.state === "observed" ? run.routerActivity.trace.events : null;
  const reportedResults = observations?.filter(event => event.kind === "tool_result").length ?? null;
  const roundsShown = rounds ?? (roundEvents > 0 ? roundEvents : null);
  const toolCallsShown = toolCalls ?? (toolEvents > 0 ? toolEvents : reportedResults);
  const showReportedResults = toolCalls == null && toolEvents === 0 && reportedResults != null;

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Mission map</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        Recorded activity for this revision and the configured delegation authority. Run usage is
        not the remaining lifetime budget.
      </p>

      {/* Token burn + orchestration shape */}
      <div className="mt-4 grid gap-4 sm:grid-cols-3">
        <div className="rounded-lg border border-border bg-surface-muted/40 p-3">
          <p className="text-[11px] text-foreground-muted">Recorded run tokens</p>
          {total == null ? (
            <p className="mt-1 text-sm text-foreground-muted">Usage not recorded</p>
          ) : (
            <>
              <p className="mt-1 text-lg font-semibold tabular-nums">{total.toLocaleString()}</p>
              {budget != null ? (
                <div className="mt-1.5">
                  <div className="h-1.5 w-full overflow-hidden rounded-full bg-surface">
                    <div
                      className={`h-full ${burnPct! >= 90 ? "bg-rose-500" : burnPct! >= 60 ? "bg-amber-500" : "bg-emerald-500"}`}
                      style={{ width: `${burnPct}%` }}
                    />
                  </div>
                  <p className="mt-1 text-[11px] text-foreground-muted">
                    This revision: {burnPct}% of {budget.toLocaleString()} lifetime token cap
                  </p>
                </div>
              ) : (
                <p className="mt-1 text-[11px] text-foreground-muted">No cap</p>
              )}
            </>
          )}
        </div>
        <div className="rounded-lg border border-border bg-surface-muted/40 p-3">
          <p className="text-[11px] text-foreground-muted">Model rounds</p>
          <p className="mt-1 text-lg font-semibold tabular-nums">{roundsShown ?? "—"}</p>
        </div>
        <div className="rounded-lg border border-border bg-surface-muted/40 p-3">
          <p className="text-[11px] text-foreground-muted">{showReportedResults ? "Reported tool results" : "Tool calls"}</p>
          <p className="mt-1 text-lg font-semibold tabular-nums">{toolCallsShown ?? "—"}</p>
          {showReportedResults && <p className="mt-1 text-[11px] text-foreground-muted">
            Returned-response observations; not a complete execution audit.
          </p>}
        </div>
      </div>

      {/* Delegation tree */}
      <div className="mt-5">
        <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Delegation tree</p>
        <div className="mt-2 space-y-1.5">
          <div className="flex items-center gap-2 rounded-lg border border-signal/40 bg-signal/5 px-3 py-2">
            <span className="text-sm font-medium">{task.display_name ?? "Lead"}</span>
            <TierPip tier={task.envelope.tier} />
            <span className="ml-auto text-[11px] text-foreground-muted">
              grants up to T{task.envelope.authority_ceiling} · depth {task.envelope.delegation_depth}
            </span>
          </div>
          {task.children.map((c) => (
            <div
              key={c.name}
              className="ml-5 flex items-center gap-2 rounded-lg border border-border bg-surface px-3 py-2"
            >
              <span className="text-sm">{c.display_name ?? c.objective}</span>
              <TierPip tier={c.tier} />
            </div>
          ))}
          {task.sub_agents.map((s) => (
            <div
              key={s.name}
              className="ml-5 flex items-center gap-2 rounded-lg border border-dashed border-border bg-surface px-3 py-2"
            >
              <span className="text-sm">{s.name}</span>
              <span className="rounded-full bg-surface-muted px-2 py-0.5 text-[10px] text-foreground-muted">
                sub-agent
              </span>
            </div>
          ))}
          {task.children.length === 0 && task.sub_agents.length === 0 && (
            <p className="ml-5 text-xs text-foreground-muted">
              Working solo — no delegated reports.
            </p>
          )}
        </div>
      </div>
    </section>
  );
}
