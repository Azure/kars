// kars Bridge Operator Console — Insights. Fleet-wide efficiency + governance,
// graphical. Structural facts are real; runtime token/latency render the
// honest "needs a real run" state, never fabricated zeros.

import { BarChart } from "@/components/bar-chart";
import { HonestState } from "@/components/honest-state";
import { LiveRefresh } from "@/components/live-refresh";
import { getArtifacts, getEfficiency, getInsights, getInferenceBudgets } from "@/lib/bff";
import type { Efficiency, Insights, MissionArtifacts, InferenceBudgets } from "@/lib/types";

export const dynamic = "force-dynamic";

export default async function InsightsPage() {
  let insights: Insights | null = null;
  let error = false;
  try {
    insights = await getInsights();
  } catch {
    error = true;
  }

  let efficiency: Efficiency | null = null;
  try {
    efficiency = await getEfficiency();
  } catch {
    efficiency = null;
  }

  let deliverables: MissionArtifacts[] = [];
  try {
    deliverables = (await getArtifacts()).missions;
  } catch {
    deliverables = [];
  }

  let budgets: InferenceBudgets | null = null;
  try {
    budgets = await getInferenceBudgets();
  } catch {
    budgets = null;
  }

  if (error || !insights) {
    return (
      <div className="space-y-6">
        <Header />
        <HonestState variant="not_wired" title="Insights unavailable" detail="The run environment isn't reachable right now." />
      </div>
    );
  }

  const totalMissions = insights.missions_by_phase.reduce((s, p) => s + p.count, 0);
  // Real economics from the efficiency frontier (per-route telemetry).
  // avg_tokens is tokens-per-delivered-outcome, so the true spend on a route is
  // avg_tokens × delivered (NOT × runs, which would overcount failed runs).
  const totalTokens = efficiency
    ? efficiency.routes.reduce((s, r) => s + r.avg_tokens * r.delivered, 0)
    : null;
  const accepted = efficiency ? efficiency.routes.reduce((s, r) => s + r.accepted, 0) : 0;
  const delivered = deliverables.filter((m) => m.summary && m.status !== "error").length;
  const reviewed = deliverables.filter((m) => m.review_status === "approved").length;

  // One consistent, monotonic population for the outcome funnel. The efficiency
  // engine counts run-level outcomes where attempted ≥ delivered ≥ accepted by
  // construction (each run delivers at most once, and is accepted only if
  // delivered). Falling back to mission/deliverable counts (clamped) when the
  // efficiency engine has no data yet.
  const funnelPop = (() => {
    if (efficiency && efficiency.total_runs > 0) {
      const d = efficiency.routes.reduce((s, r) => s + r.delivered, 0);
      const a = efficiency.routes.reduce((s, r) => s + r.accepted, 0);
      return {
        attempted: efficiency.total_runs,
        delivered: Math.min(d, efficiency.total_runs),
        accepted: Math.min(a, d),
      };
    }
    // No efficiency data: use mission counts, clamped so stages never exceed
    // the prior stage.
    const d = Math.min(delivered, totalMissions);
    const a = Math.min(accepted || reviewed, d);
    return { attempted: totalMissions, delivered: d, accepted: a };
  })();

  return (
    <div className="space-y-6">
      {/* Metrics tick live — re-run the server render on an interval so numbers
          update as runs complete, without a manual reload. */}
      <LiveRefresh active intervalMs={8000} />
      <Header />

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-6">
        <Kpi label="Missions" value={totalMissions} accent />
        <Kpi label="Running" value={insights.launched} />
        <Kpi label="Deliverables" value={funnelPop.delivered} />
        <Kpi label="Accepted" value={funnelPop.accepted} />
        <Kpi label="Signed receipts" value={insights.receipts_issued} />
        <Kpi label="Tokens on delivered" value={totalTokens == null ? null : Math.round(totalTokens)} />
      </div>
      <p className="-mt-3 text-xs text-foreground-muted">
        <span className="font-medium text-foreground">Missions</span> counts unique tasks;{" "}
        <span className="font-medium text-foreground">Deliverables</span> and{" "}
        <span className="font-medium text-foreground">Accepted</span> are counted per run — a mission can be re-run, so run totals can exceed the mission count.{" "}
        <span className="font-medium text-foreground">Tokens on delivered</span> sums the token cost of delivered outcomes only; tokens burned by failed or blocked runs are not included.
      </p>

      {/* Deliverable funnel — outcome, not activity. Uses the run-level
          population from the efficiency engine so it is strictly monotonic
          (attempted ≥ delivered ≥ accepted) — never a >100% stage. */}
      <Funnel
        attempted={funnelPop.attempted}
        delivered={funnelPop.delivered}
        accepted={funnelPop.accepted}
        blocked={insights.amplification_rejections}
      />

      <div className="grid gap-6 lg:grid-cols-2">
        <Panel title="Missions by status" subtitle="Where your work stands.">
          {totalMissions === 0 ? (
            <HonestState variant="empty" compact title="No missions yet" />
          ) : (
            <BarChart data={insights.missions_by_phase.map(projPhase)} />
          )}
        </Panel>

        <Panel title="Autonomy mix" subtitle="How much independence you've granted across missions.">
          {insights.missions_by_tier.every((t) => t.count === 0) ? (
            <HonestState variant="empty" compact title="No missions yet" />
          ) : (
            <BarChart data={insights.missions_by_tier} colorClass="bg-accent/70" />
          )}
        </Panel>

        <Panel title="Decisions you made" subtitle="Approvals, denials, and their outcomes.">
          {insights.decisions.length === 0 ? (
            <HonestState variant="empty" compact title="No decisions yet" detail="Decisions appear as missions ask for your approval." />
          ) : (
            <BarChart data={insights.decisions} colorClass="bg-warning/70" />
          )}
        </Panel>

        <Panel title="Governance integrity" subtitle="Proof the system held the line.">
          <dl className="space-y-3">
            <Row label="Receipts issued" value={insights.receipts_issued} />
            <Row label="Tamper-evidence log entries" value={insights.inclusion_log_size} />
            <Row
              label="Over-reach attempts blocked"
              value={insights.amplification_rejections}
              hint="Sub-roles that tried to grant themselves more authority than allowed — and were stopped."
            />
          </dl>
        </Panel>
      </div>

      {/* Budget utilization — the token economy against the hierarchical caps.
          Live measured daily spend vs the cluster + workspace budgets. */}
      {budgets && <BudgetUtilization budgets={budgets} />}

      {/* Cross-harness efficiency frontier (§3B, Pillar B) — the real per-route
          telemetry. Promoted above the honest-gap note. */}
      {efficiency && efficiency.routes.length > 0 ? (
        <EfficiencyPanel efficiency={efficiency} insights={insights} />
      ) : (
        <p className="rounded-lg border border-dashed border-border bg-surface-muted/30 px-4 py-3 text-xs text-foreground-muted">
          Per-mission token cost & latency are captured per run; the cross-harness comparison surfaces here once missions have run on more than one route.
        </p>
      )}
    </div>
  );
}

function pct(x: number): string {
  return `${Math.round(x * 100)}%`;
}

/** A real efficiency frontier: a scatter of routes by cost (tokens per
 *  delivered outcome, X) vs quality (delivery success rate, Y). Point area ~
 *  run count; the recommended route is ringed. The Pareto-optimal routes
 *  (cheaper AND higher-quality than any other) are connected as the frontier.
 *  Renders as SVG — not a table row. */
function FrontierChart({ efficiency }: { efficiency: Efficiency }) {
  const pts = efficiency.routes.filter((r) => r.delivered > 0 && r.tokens_per_outcome > 0);
  if (pts.length === 0) {
    return (
      <p className="mt-4 rounded-lg border border-dashed border-border bg-surface-muted/30 px-4 py-3 text-xs text-foreground-muted">
        The frontier plots once at least one route has a delivered outcome with recorded token cost.
      </p>
    );
  }
  const W = 640, H = 240, padL = 52, padR = 16, padT = 28, padB = 40;
  const maxCost = Math.max(...pts.map((p) => p.tokens_per_outcome));
  const minCost = Math.min(...pts.map((p) => p.tokens_per_outcome));
  const costSpan = Math.max(maxCost - minCost, 1);
  const maxRuns = Math.max(...pts.map((p) => p.runs), 1);
  const x = (cost: number) =>
    padL + ((cost - minCost) / costSpan) * (W - padL - padR) * (pts.length === 1 ? 0 : 1) + (pts.length === 1 ? (W - padL - padR) / 2 : 0);
  const y = (q: number) => padT + (1 - q) * (H - padT - padB);
  const r = (runs: number) => 5 + Math.sqrt(runs / maxRuns) * 13;

  // Label placement that never clips off the chart: anchor start/middle/end
  // based on which third of the plot width the point falls in (so a label
  // near the left or right edge extends INTO the chart instead of past its
  // boundary), and flip above/below based on whether "above" would push the
  // label past the top edge (points near 100% success previously rendered
  // their label at a negative Y — invisible, clipped by the viewBox).
  const plotW = W - padL - padR;
  function labelFor(px: number, py: number, radius: number): { anchor: "start" | "middle" | "end"; lx: number; ly: number } {
    const frac = (px - padL) / plotW;
    const anchor = frac < 0.22 ? "start" : frac > 0.78 ? "end" : "middle";
    const lx = anchor === "start" ? Math.max(px, padL) : anchor === "end" ? Math.min(px, W - padR) : px;
    const above = py - radius - 6;
    const ly = above > padT + 6 ? above : py + radius + 12;
    return { anchor, lx, ly };
  }

  // Pareto frontier: a route dominates if it is cheaper (lower cost) AND higher
  // quality. Keep the non-dominated set, sorted by cost, and connect them.
  const frontier = pts
    .filter((p) => !pts.some((o) => o !== p && o.tokens_per_outcome <= p.tokens_per_outcome && o.success_rate >= p.success_rate && (o.tokens_per_outcome < p.tokens_per_outcome || o.success_rate > p.success_rate)))
    .sort((a, b) => a.tokens_per_outcome - b.tokens_per_outcome);

  const gridY = [0, 0.25, 0.5, 0.75, 1];
  return (
    <div className="mt-4 overflow-x-auto">
      <svg viewBox={`0 0 ${W} ${H}`} className="w-full min-w-[420px]" role="img" aria-label="Efficiency frontier: cost versus delivery success by route">
        {/* Y gridlines + labels (delivery success). */}
        {gridY.map((g) => (
          <g key={g}>
            <line x1={padL} y1={y(g)} x2={W - padR} y2={y(g)} className="stroke-border" strokeWidth={1} strokeDasharray="2 3" />
            <text x={padL - 8} y={y(g) + 3} textAnchor="end" className="fill-foreground-muted text-[9px]">{Math.round(g * 100)}%</text>
          </g>
        ))}
        {/* Axis titles. */}
        <text x={(padL + W - padR) / 2} y={H - 8} textAnchor="middle" className="fill-foreground-muted text-[10px]">Cost — tokens per delivered outcome (cheaper →)</text>
        <text x={14} y={(padT + H - padB) / 2} textAnchor="middle" transform={`rotate(-90 14 ${(padT + H - padB) / 2})`} className="fill-foreground-muted text-[10px]">Delivery success</text>
        {/* X min/max cost labels. */}
        <text x={padL} y={H - 24} textAnchor="start" className="fill-foreground-muted text-[9px]">{minCost.toLocaleString()}</text>
        {pts.length > 1 && <text x={W - padR} y={H - 24} textAnchor="end" className="fill-foreground-muted text-[9px]">{maxCost.toLocaleString()}</text>}
        {/* Frontier line. */}
        {frontier.length > 1 && (
          <polyline
            points={frontier.map((p) => `${x(p.tokens_per_outcome)},${y(p.success_rate)}`).join(" ")}
            className="stroke-signal/50" fill="none" strokeWidth={2} strokeDasharray="4 3"
          />
        )}
        {/* Points. */}
        {pts.map((p) => {
          const isRec = p.route === efficiency.recommended && p.harness === efficiency.recommended_harness;
          const px = x(p.tokens_per_outcome);
          const py = y(p.success_rate);
          const radius = r(p.runs);
          const label = labelFor(px, py, radius);
          return (
            <g key={`${p.route}-${p.harness}`}>
              <circle cx={px} cy={py} r={radius} className={isRec ? "fill-emerald-500/25 stroke-emerald-500" : "fill-signal/20 stroke-signal"} strokeWidth={isRec ? 2.5 : 1.5} />
              <text x={label.lx} y={label.ly} textAnchor={label.anchor} className="fill-foreground text-[9px] font-medium">
                {p.route}{p.harness ? ` · ${p.harness}` : ""}
              </text>
            </g>
          );
        })}
      </svg>
      <p className="mt-1 text-[11px] text-foreground-muted">
        {pts.length < 2 ? (
          <>Point size ∝ run count. Only one route has delivered outcomes so far — a frontier needs at least two routes to compare, so run missions on another route or harness to populate it.</>
        ) : (
          <>Point size ∝ run count. Up-and-left is better (cheaper, higher delivery success). The dashed line is the Pareto frontier; the ringed point is recommended.</>
        )}
      </p>
    </div>
  );
}

function EfficiencyPanel({ efficiency, insights }: { efficiency: Efficiency; insights: Insights }) {
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Efficiency frontier</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            {insights.runtime_metrics_available
              ? "Real per-run telemetry by model/harness route."
              : "Outcome-based comparison by model/harness route (token/latency runtime metrics not yet wired for every route)."}{" "}
            <strong>Accepted</strong> = a human approved the deliverable (the honest outcome
            signal), not merely that tokens were spent. Computed from {efficiency.total_runs} run
            {efficiency.total_runs === 1 ? "" : "s"}.
          </p>
          {!insights.runtime_metrics_available && insights.runtime_metrics_note && (
            <p className="mt-1 text-[11px] text-foreground-muted">{insights.runtime_metrics_note}</p>
          )}
        </div>
        {efficiency.recommended && efficiency.recommended.toLowerCase() !== "unknown" && (
          <div className="shrink-0 text-right">
            <span
              className={`inline-block rounded-full border px-2.5 py-1 text-xs font-medium ${
                efficiency.recommended_low_confidence
                  ? "border-warning/40 bg-warning/10 text-warning"
                  : "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
              }`}
              title={efficiency.recommended_basis ?? undefined}
            >
              {efficiency.recommended_low_confidence ? "Best available: " : "Recommended: "}
              {efficiency.recommended}
            </span>
            {efficiency.recommended_basis && (
              <p className="mt-1 max-w-xs text-right text-[11px] leading-snug text-foreground-muted">
                {efficiency.recommended_basis}
              </p>
            )}
          </div>
        )}
      </div>

      {/* Real frontier chart — cost (tokens/outcome) vs quality (delivery
          success), one point per route with runs, the recommended route
          highlighted. A frontier, not a spreadsheet row. */}
      <FrontierChart efficiency={efficiency} />
      <div className="mt-4 overflow-x-auto">
        <table className="w-full text-left text-xs">
          <thead className="text-foreground-muted">
            <tr className="border-b border-border">
              <th className="py-2 pr-4 font-medium">Route</th>
              <th className="py-2 pr-4 font-medium">Harness</th>
              <th className="py-2 pr-4 font-medium">Runs</th>
              <th className="py-2 pr-4 font-medium">Delivered</th>
              <th className="py-2 pr-4 font-medium">Accepted</th>
              <th className="py-2 pr-4 font-medium" title="Most common fault among this route's unaccepted runs">Top fault</th>
              <th className="py-2 pr-4 font-medium" title="pass^k: fraction of repeated packages accepted on EVERY attempt — a reliability measure">Reliability</th>
              <th className="py-2 pr-4 font-medium">Tokens/outcome</th>
              {efficiency.priced && <th className="py-2 pr-4 font-medium">$/outcome</th>}
              <th className="py-2 pr-4 font-medium" title="mean wall-clock (p95)">Latency</th>
              <th className="py-2 pr-4 font-medium" title="fraction of tool calls that failed">Tool-fail</th>
              <th className="py-2 pr-4 font-medium" title="fraction of input tokens served from the provider cache">Cache</th>
              <th className="py-2 pr-4 font-medium">Rounds</th>
              <th className="py-2 pr-4 font-medium">Tool calls</th>
            </tr>
          </thead>
          <tbody>
            {efficiency.routes.map((r) => (
              <tr
                key={`${r.route}-${r.harness ?? ""}`}
                className={`border-b border-border last:border-0 ${
                  r.route === efficiency.recommended &&
                  (r.harness ?? "") === (efficiency.recommended_harness ?? "")
                    ? "bg-emerald-500/5"
                    : ""
                }`}
              >
                <td className="py-2 pr-4 font-mono">{r.route}</td>
                <td className="py-2 pr-4">
                  {r.harness ? (
                    <span className="rounded-full border border-accent/30 bg-accent/10 px-2 py-0.5 text-[11px] font-medium text-accent">{r.harness}</span>
                  ) : (
                    <span className="text-foreground-muted">—</span>
                  )}
                </td>
                <td className="py-2 pr-4 tabular-nums">{r.runs}</td>
                <td className="py-2 pr-4 tabular-nums">{r.delivered}</td>
                <td className="py-2 pr-4 tabular-nums">{r.accepted} <span className="text-foreground-muted">({pct(r.acceptance_rate)})</span></td>
                <td className="py-2 pr-4">{r.top_fault ? <span className="rounded bg-danger/10 px-1.5 py-0.5 text-[10px] font-medium text-danger">{r.top_fault}</span> : <span className="text-foreground-muted">—</span>}</td>
                <td className="py-2 pr-4 tabular-nums">{r.reliability_rate === null ? <span className="text-foreground-muted">—</span> : <span title={`pass^${r.reliability_k}: accepted on every one of ${r.reliability_k} repeated attempts, measured over ${r.reliability_samples} sample${r.reliability_samples === 1 ? "" : "s"}`}>{pct(r.reliability_rate)} <span className="text-foreground-muted">(n={r.reliability_samples})</span></span>}</td>
                <td className="py-2 pr-4 tabular-nums">{r.tokens_per_outcome.toLocaleString()}</td>
                {efficiency.priced && <td className="py-2 pr-4 tabular-nums">{r.usd_per_outcome === null ? <span className="text-foreground-muted">—</span> : `$${r.usd_per_outcome.toFixed(3)}`}</td>}
                <td className="py-2 pr-4 tabular-nums">{r.avg_wall_ms > 0 ? <>{(r.avg_wall_ms / 1000).toFixed(1)}s <span className="text-foreground-muted">(p95 {(r.p95_wall_ms / 1000).toFixed(0)}s)</span></> : <span className="text-foreground-muted">—</span>}</td>
                <td className="py-2 pr-4 tabular-nums">{r.avg_tool_calls > 0 ? pct(r.tool_fail_rate) : <span className="text-foreground-muted">—</span>}</td>
                <td className="py-2 pr-4 tabular-nums">{r.cache_hit_rate > 0 ? pct(r.cache_hit_rate) : <span className="text-foreground-muted">—</span>}</td>
                <td className="py-2 pr-4 tabular-nums">{r.avg_rounds.toFixed(1)}</td>
                <td className="py-2 pr-4 tabular-nums">{r.avg_tool_calls.toFixed(1)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {(() => {
        const a = efficiency.routes[0];
        const b = efficiency.routes[1];
        // Only show A/B when the top two are genuinely DIFFERENT routes (route
        // or harness differs) — never compare a (route,harness) pair to itself,
        // which rendered two identically-labeled columns.
        if (!a || !b) return null;
        const distinct = a.route !== b.route || (a.harness ?? "") !== (b.harness ?? "");
        if (!distinct) return null;
        return (
          <AbCompare
            a={a}
            b={b}
            recommended={efficiency.recommended}
            recommendedHarness={efficiency.recommended_harness}
          />
        );
      })()}
    </section>
  );
}

function AbCompare({ a, b, recommended, recommendedHarness }: { a: import("@/lib/types").RouteEfficiency; b: import("@/lib/types").RouteEfficiency; recommended: string | null; recommendedHarness?: string | null }) {
  const isRec = (r: import("@/lib/types").RouteEfficiency): boolean =>
    r.route === recommended && (r.harness ?? "") === (recommendedHarness ?? "");
  const routeLabel = (r: import("@/lib/types").RouteEfficiency): string =>
    r.harness ? `${r.route} · ${r.harness}` : r.route;
  const rows: { label: string; av: string; bv: string; aWins: boolean | null }[] = [
    { label: "Acceptance", av: pct(a.acceptance_rate), bv: pct(b.acceptance_rate), aWins: a.acceptance_rate === b.acceptance_rate ? null : a.acceptance_rate > b.acceptance_rate },
    { label: "Reliability (pass^k)", av: a.reliability_rate === null ? "—" : pct(a.reliability_rate), bv: b.reliability_rate === null ? "—" : pct(b.reliability_rate), aWins: (a.reliability_rate ?? -1) === (b.reliability_rate ?? -1) ? null : (a.reliability_rate ?? -1) > (b.reliability_rate ?? -1) },
    { label: "Tokens / outcome", av: a.tokens_per_outcome.toLocaleString(), bv: b.tokens_per_outcome.toLocaleString(), aWins: a.tokens_per_outcome === b.tokens_per_outcome ? null : a.tokens_per_outcome < b.tokens_per_outcome },
    { label: "Latency (wall)", av: a.avg_wall_ms > 0 ? `${(a.avg_wall_ms / 1000).toFixed(1)}s` : "—", bv: b.avg_wall_ms > 0 ? `${(b.avg_wall_ms / 1000).toFixed(1)}s` : "—", aWins: a.avg_wall_ms === b.avg_wall_ms || a.avg_wall_ms === 0 || b.avg_wall_ms === 0 ? null : a.avg_wall_ms < b.avg_wall_ms },
    { label: "Tool-fail rate", av: pct(a.tool_fail_rate), bv: pct(b.tool_fail_rate), aWins: a.tool_fail_rate === b.tool_fail_rate ? null : a.tool_fail_rate < b.tool_fail_rate },
    { label: "Avg rounds", av: a.avg_rounds.toFixed(1), bv: b.avg_rounds.toFixed(1), aWins: a.avg_rounds === b.avg_rounds ? null : a.avg_rounds < b.avg_rounds },
    { label: "Avg tool calls", av: a.avg_tool_calls.toFixed(1), bv: b.avg_tool_calls.toFixed(1), aWins: a.avg_tool_calls === b.avg_tool_calls ? null : a.avg_tool_calls < b.avg_tool_calls },
  ];
  return (
    <div className="mt-6 rounded-lg border border-border bg-background/40 p-4">
      <h3 className="text-xs font-semibold">A/B head-to-head — top two routes</h3>
      <p className="mt-0.5 text-[11px] text-foreground-muted">Side-by-side on the same outcome metrics. The winner is the cheaper-per-accepted-outcome route, not the cheaper-per-token one.</p>
      <div className="mt-3 grid grid-cols-[1fr_auto_auto] gap-x-4 gap-y-1.5 text-xs">
        <div className="text-foreground-muted">Route</div>
        <div className="font-mono text-right">{routeLabel(a)}{isRec(a) ? " ★" : ""}</div>
        <div className="font-mono text-right">{routeLabel(b)}{isRec(b) ? " ★" : ""}</div>
        {rows.map((r) => (
          <FragmentRow key={r.label} {...r} />
        ))}
      </div>
    </div>
  );
}

function FragmentRow({ label, av, bv, aWins }: { label: string; av: string; bv: string; aWins: boolean | null }) {
  const win = "font-semibold text-emerald-600";
  return (
    <>
      <div className="text-foreground-muted">{label}</div>
      <div className={`text-right tabular-nums ${aWins === true ? win : ""}`}>{av}</div>
      <div className={`text-right tabular-nums ${aWins === false ? win : ""}`}>{bv}</div>
    </>
  );
}

function projPhase(p: { label: string; count: number }) {
  const map: Record<string, string> = {
    Ready: "Ready / running",
    Degraded: "Blocked",
    Pending: "Starting",
  };
  return { label: map[p.label] ?? p.label, count: p.count };
}

function Header() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Insights</h1>
      <p className="mt-1 text-sm text-foreground-muted">
        How the fleet&apos;s missions and teams are performing and how rigorously they&apos;re
        governed — across all work on this cluster.
      </p>
    </div>
  );
}

function Kpi({ label, value, accent }: { label: string; value: number | null; accent?: boolean }) {
  return (
    <div className={`rounded-xl border p-4 ${accent ? "border-signal/30 bg-signal/5" : "border-border bg-surface"}`}>
      <p className="text-2xl font-semibold tabular-nums">{value == null ? "—" : value.toLocaleString()}</p>
      <p className="mt-0.5 text-xs text-foreground-muted">{label}</p>
    </div>
  );
}

function Funnel({ attempted, delivered, accepted, blocked }: { attempted: number; delivered: number; accepted: number; blocked: number }) {
  // Strictly monotonic by construction (attempted ≥ delivered ≥ accepted), so
  // the widest bar is always stage 1 and no rate can exceed 100%.
  const max = Math.max(attempted, 1);
  const rate = (n: number, base: number) => (base > 0 ? Math.min(100, Math.round((n / base) * 100)) : 0);
  const rows = [
    { label: "Attempted", n: attempted, cls: "bg-signal", sub: "runs started" },
    { label: "Delivered", n: delivered, cls: "bg-signal/70", sub: `${rate(delivered, attempted)}% of runs produced output` },
    { label: "Accepted", n: accepted, cls: "bg-ok", sub: `${rate(accepted, delivered)}% of deliverables approved` },
  ];
  return (
    <section className="kb-card p-5 sm:p-6">
      <div className="flex items-baseline justify-between">
        <div>
          <h2 className="text-sm font-semibold">Outcome funnel</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">Outcome, not activity — attempted → delivered → human-accepted.</p>
        </div>
        {blocked > 0 && <span className="rounded-full border border-warning/30 bg-warning/10 px-2 py-0.5 text-xs text-warning">{blocked} over-reach blocked</span>}
      </div>
      <div className="mt-4 space-y-3">
        {rows.map((r) => (
          <div key={r.label} className="flex items-center gap-3">
            <span className="w-20 shrink-0 text-xs font-medium">{r.label}</span>
            <div className="h-7 flex-1 overflow-hidden rounded-lg bg-surface-muted">
              <div className={`flex h-7 items-center justify-end rounded-lg px-2 ${r.cls} transition-all`} style={{ width: `${Math.max((r.n / max) * 100, 8)}%` }}>
                <span className="text-xs font-semibold tabular-nums text-white">{r.n}</span>
              </div>
            </div>
            <span className="hidden w-56 shrink-0 text-[11px] text-foreground-muted sm:block">{r.sub}</span>
          </div>
        ))}
      </div>
    </section>
  );
}

function Panel({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">{title}</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">{subtitle}</p>
      <div className="mt-4">{children}</div>
    </section>
  );
}

function Row({ label, value, hint }: { label: string; value: number; hint?: string }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-border pb-2 last:border-0">
      <div>
        <dt className="text-sm">{label}</dt>
        {hint && <p className="mt-0.5 text-xs text-foreground-muted">{hint}</p>}
      </div>
      <dd className="text-lg font-semibold tabular-nums">{value}</dd>
    </div>
  );
}

/** Budget utilization — the token economy against the hierarchical caps (item 2).
 *  Shows today's measured cluster spend and every configured cap with a live
 *  meter + enforcement status, so operators see the spend story next to outcomes. */
function BudgetUtilization({ budgets }: { budgets: InferenceBudgets }) {
  const levels = [
    ...(budgets.cluster ? [budgets.cluster] : []),
    ...budgets.workspaces,
  ];
  const statusMeta: Record<string, { label: string; bar: string; tone: string }> = {
    ok: { label: "Within budget", bar: "bg-signal", tone: "text-signal" },
    alert: { label: "Over budget — alerting", bar: "bg-warning", tone: "text-warning" },
    over_buffer_headroom: { label: "In buffer headroom", bar: "bg-warning", tone: "text-warning" },
    blocking: { label: "Blocking new work", bar: "bg-danger", tone: "text-danger" },
  };
  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Budget utilization</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Today&rsquo;s measured inference token spend against the hierarchical caps (UTC day).
            Edit the caps in Policies → Inference budgets.
          </p>
        </div>
        <div className="shrink-0 text-right">
          <p className="text-2xl font-semibold tabular-nums">{budgets.cluster_used_today.toLocaleString()}</p>
          <p className="text-[11px] text-foreground-muted">tokens today (cluster)</p>
        </div>
      </div>
      {levels.length === 0 ? (
        <p className="mt-4 rounded-lg border border-dashed border-border bg-surface-muted/30 px-4 py-3 text-xs text-foreground-muted">
          No cluster or workspace caps set yet — spend is measured but unbounded. Set a cap in
          Policies → Inference budgets to enforce passive alerts, a buffer, or strict limits.
        </p>
      ) : (
        <ul className="mt-4 space-y-3">
          {levels.map((lv) => {
            const meta = statusMeta[lv.status] ?? statusMeta.ok;
            const pctW = Math.min(100, Math.round(lv.percent * 100));
            return (
              <li key={lv.scope}>
                <div className="flex items-center justify-between text-xs">
                  <span className="font-medium">
                    {lv.scope === "cluster" ? "Cluster" : lv.label}
                    <span className="ml-2 rounded-full border border-border px-1.5 py-0.5 text-[10px] font-medium capitalize text-foreground-muted">
                      {lv.mode}
                    </span>
                  </span>
                  <span className="tabular-nums text-foreground-muted">
                    {lv.used_today.toLocaleString()} / {lv.daily_tokens.toLocaleString()} · {Math.round(lv.percent * 100)}%
                  </span>
                </div>
                <div className="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-surface-muted">
                  <div className={`h-full ${meta.bar}`} style={{ width: `${pctW}%` }} />
                </div>
                <p className={`mt-0.5 text-[11px] ${meta.tone}`}>{meta.label}</p>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
