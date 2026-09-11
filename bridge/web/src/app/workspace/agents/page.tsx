// kars Bridge Workspace — Active agents. The plain answer to "what is working
// right now, and what just finished?" Sourced from real run telemetry (not idle
// pods): live runs pulse with what they're doing this second; recent runs show
// their outcome, rounds, tools, and token cost. Honest empty when nothing ran.

import Link from "next/link";
import { HonestState } from "@/components/honest-state";
import { LivePulse, LiveRefresh } from "@/components/live-refresh";
import { FleetLive } from "@/components/fleet-live";
import { listAgents, getFleetTelemetry } from "@/lib/bff";
import { TIER_LABELS, type AgentLifecycle, type FleetTelemetry } from "@/lib/types";

export const dynamic = "force-dynamic";

function ago(iso: string | null): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

function statusTone(status: string | null): string {
  if (status === "ok") return "border-ok/40 bg-ok/10 text-ok";
  if (status) return "border-danger/40 bg-danger/10 text-danger";
  return "border-border bg-surface-muted text-foreground-muted";
}

function runMoment(task: string | null): string | null {
  if (!task) return null;
  const m = task.match(/-run-(\d{10})(\d{0,3})$/);
  if (!m) return null;
  const ms = Number(m[1]) * 1000 + (m[2] ? Number(m[2].padEnd(3, "0")) : 0);
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? null : d.toLocaleString();
}

function agentHref(agent: AgentLifecycle): string {
  if (agent.team && agent.task) {
    return `/workspace/teams/${encodeURIComponent(agent.team)}/runs/${encodeURIComponent(agent.task)}`;
  }
  return `/workspace/missions/${encodeURIComponent(agent.task ?? agent.sandbox)}`;
}

function AgentCard({ a, live }: { a: AgentLifecycle; live: boolean }) {
  const moment = runMoment(a.task);
  // A team-owned scheduled run is NOT a spawned sub-agent — only runtime-spawned
  // children (a parent, with no owning team) are sub-agents.
  const isSubAgent = !!a.parent && !a.team;
  const hasTrace = (a.rounds ?? 0) > 0 || (a.tool_calls ?? 0) > 0;
  return (
    <li className="rounded-xl border border-border bg-surface p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <Link href={agentHref(a)} className="truncate font-medium text-signal hover:underline">
              {a.display_name ?? a.task ?? a.sandbox}
            </Link>
            {a.team && (
              <Link href={`/workspace/teams/${a.team}`} className="shrink-0 rounded-full bg-surface-muted px-2 py-0.5 text-[11px] text-foreground-muted hover:text-foreground">
                {a.team}
              </Link>
            )}
            {isSubAgent && <span className="shrink-0 rounded-full bg-surface-muted px-2 py-0.5 text-[11px]">sub-agent</span>}
          </div>
          {moment && <p className="mt-0.5 font-mono text-[11px] text-foreground-muted">{moment}</p>}
          {a.objective && <p className="mt-1 line-clamp-2 text-sm text-foreground-muted">{a.objective}</p>}
        </div>
        {live ? (
          <LivePulse label="Working" />
        ) : (
          <span className={`shrink-0 rounded-full border px-2.5 py-1 text-[11px] font-medium ${statusTone(a.status)}`}>
            {a.phase ?? "Idle"}
          </span>
        )}
      </div>
      <dl className="mt-3 flex flex-wrap items-center gap-x-5 gap-y-1 text-xs text-foreground-muted">
        {a.tier != null && <span>Tier {a.tier} · {TIER_LABELS[a.tier] ?? "?"}</span>}
        {live && !hasTrace ? (
          <span className="italic">warming up — waiting for first model round…</span>
        ) : (
          <>
            <span>{a.rounds} round{a.rounds === 1 ? "" : "s"}</span>
            <span>{a.tool_calls} tool call{a.tool_calls === 1 ? "" : "s"}</span>
            {a.tokens != null && <span>{a.tokens.toLocaleString()} tokens</span>}
          </>
        )}
        {live && a.last_action && (
          <span>now: <span className="font-mono text-foreground">{a.last_action}</span></span>
        )}
        {!live && a.finished_at && <span>{ago(a.finished_at)}</span>}
      </dl>
      {live && a.health && <HealthRow h={a.health} />}
    </li>
  );
}

function HealthRow({ h }: { h: import("@/lib/types").PodHealth }) {
  const ready = h.total_containers > 0 && h.ready_containers === h.total_containers;
  const unhealthy = !!h.waiting_reason || (h.total_containers > 0 && h.ready_containers < h.total_containers);
  return (
    <div className="mt-3 flex flex-wrap items-center gap-2 border-t border-border pt-3 text-[11px]">
      <span className="font-medium uppercase tracking-wide text-foreground-muted/80">Health</span>
      <HealthChip
        tone={unhealthy ? "danger" : ready ? "ok" : "warn"}
        label={`${h.ready_containers}/${h.total_containers} ready`}
        dot
      />
      <HealthChip
        tone={h.restarts > 0 ? "warn" : "muted"}
        label={`${h.restarts} restart${h.restarts === 1 ? "" : "s"}`}
      />
      {h.uptime_seconds != null && <HealthChip tone="muted" label={`up ${fmtUptime(h.uptime_seconds)}`} />}
      {h.node && <HealthChip tone="muted" label={h.node} mono />}
      {h.waiting_reason && <HealthChip tone="danger" label={h.waiting_reason} />}
    </div>
  );
}

function HealthChip({
  tone,
  label,
  dot,
  mono,
}: {
  tone: "ok" | "warn" | "danger" | "muted";
  label: string;
  dot?: boolean;
  mono?: boolean;
}) {
  const cls = {
    ok: "border-ok/30 bg-ok/10 text-ok",
    warn: "border-warning/30 bg-warning/10 text-warning",
    danger: "border-danger/30 bg-danger/10 text-danger",
    muted: "border-border bg-surface-muted text-foreground-muted",
  }[tone];
  return (
    <span className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 ${cls} ${mono ? "font-mono" : ""}`}>
      {dot && <span className="h-1.5 w-1.5 rounded-full bg-current" aria-hidden />}
      {label}
    </span>
  );
}

function fmtUptime(s: number): string {
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86400)}d`;
}

function fmtTok(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return `${n}`;
}

/** Spend-against-budget gauge for a single run — spent tokens over the envelope
 *  ceiling, with a bar and an over-budget flag. Pure surfacing of data the
 *  substrate already records (mission-output tokens + envelope budget). */
function SpendGauge({ spent, budget }: { spent: number | null; budget: number | null }) {
  if (spent == null && budget == null) return null;
  if (budget == null) {
    return <span className="tabular-nums text-xs text-foreground-muted">{fmtTok(spent ?? 0)} tok</span>;
  }
  const pct = spent != null ? Math.min(100, Math.round((spent / Math.max(budget, 1)) * 100)) : 0;
  const over = spent != null && spent > budget;
  const near = pct >= 80 && !over;
  const tone = over ? "bg-danger" : near ? "bg-warning" : "bg-ok";
  return (
    <span className="flex items-center gap-2" title={`${(spent ?? 0).toLocaleString()} of ${budget.toLocaleString()} token budget (${pct}%)`}>
      <span className="hidden h-1.5 w-16 overflow-hidden rounded-full bg-surface-muted sm:block">
        <span className={`block h-full ${tone}`} style={{ width: `${Math.max(pct, 2)}%` }} />
      </span>
      <span className={`tabular-nums text-xs ${over ? "text-danger" : "text-foreground-muted"}`}>
        {fmtTok(spent ?? 0)}/{fmtTok(budget)}
      </span>
    </span>
  );
}

function RecentRunRow({ a }: { a: AgentLifecycle }) {
  const moment = runMoment(a.task);
  const label = a.display_name && a.display_name !== `${a.team ?? ""} — standing run` ? a.display_name : null;
  return (
    <li>
      <Link
        href={agentHref(a)}
        className="flex items-center gap-3 px-4 py-2.5 text-sm transition hover:bg-surface-muted/50"
      >
        <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${a.status === "ok" ? "bg-ok" : a.status ? "bg-danger" : "bg-foreground-muted"}`} aria-hidden />
        <span className="min-w-0 flex-1 truncate">
          <span className="font-mono text-xs text-foreground-muted">{moment ?? a.task ?? a.sandbox}</span>
          {label && <span className="ml-2 text-foreground">{label}</span>}
          {a.team && (
            <span className="ml-2 rounded-full bg-surface-muted px-1.5 py-0.5 text-[10px] text-foreground-muted">{a.team}</span>
          )}
        </span>
        <span className="hidden shrink-0 gap-3 text-xs text-foreground-muted sm:flex">
          <span>{a.rounds} rd</span>
          <span>{a.tool_calls} tools</span>
          <SpendGauge spent={a.tokens} budget={a.budget_tokens} />
        </span>
        <span className={`shrink-0 rounded-full border px-2 py-0.5 text-[11px] font-medium ${statusTone(a.status)}`}>
          {a.status === "ok" ? "Delivered" : a.status ? "Errored" : a.phase ?? "Idle"}
        </span>
        {a.finished_at && <span className="w-14 shrink-0 text-right text-[11px] text-foreground-muted">{ago(a.finished_at)}</span>}
      </Link>
    </li>
  );
}

export default async function AgentsPage() {
  let agents: AgentLifecycle[] = [];
  let fleet: FleetTelemetry | null = null;
  let error = false;
  try {
    [agents, fleet] = await Promise.all([listAgents(), getFleetTelemetry().catch(() => null)]);
  } catch {
    error = true;
  }

  const live = agents.filter((a) => a.live);
  const recent = agents.filter((a) => !a.live);

  return (
    <div className="space-y-6">
      <LiveRefresh active intervalMs={4000} />
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Active agents</h1>
        <p className="mt-1 text-sm text-foreground-muted">
          What&apos;s working right now, and what just finished — real run telemetry, not idle pods.
        </p>
      </div>

      {/* Fleet-wide live telemetry — the at-scale view: aggregate live metrics +
          a single streaming feed of what every working agent is doing now. */}
      {!error && <FleetLive initial={fleet} />}

      {/* Cost cockpit — total token spend across recent runs, and how it sits
          against the budgets those runs carried. Surfaces data the substrate
          already records (mission-output tokens + envelope budgets); "costs
          opaque" is the #6 enterprise complaint, so this is a headline gauge. */}
      {!error && agents.some((a) => a.tokens != null) && (() => {
        const spend = agents.reduce((s, a) => s + (a.tokens ?? 0), 0);
        const budgeted = agents.filter((a) => a.budget_tokens != null);
        const budgetSum = budgeted.reduce((s, a) => s + (a.budget_tokens ?? 0), 0);
        const overCount = agents.filter((a) => a.tokens != null && a.budget_tokens != null && a.tokens > a.budget_tokens).length;
        const pct = budgetSum > 0 ? Math.min(100, Math.round((budgeted.reduce((s, a) => s + (a.tokens ?? 0), 0) / budgetSum) * 100)) : null;
        return (
          <section className="rounded-2xl border border-border bg-surface p-5">
            <div className="flex items-baseline justify-between">
              <h2 className="text-sm font-semibold">Spend &amp; budget</h2>
              <span className="text-[11px] text-foreground-muted">across {agents.length} recent run{agents.length === 1 ? "" : "s"}</span>
            </div>
            <div className="mt-3 grid grid-cols-2 gap-4 sm:grid-cols-4">
              <div>
                <p className="text-2xl font-semibold tabular-nums">{spend.toLocaleString()}</p>
                <p className="text-xs text-foreground-muted">tokens spent</p>
              </div>
              <div>
                <p className="text-2xl font-semibold tabular-nums">{budgetSum > 0 ? budgetSum.toLocaleString() : "—"}</p>
                <p className="text-xs text-foreground-muted">budgeted ({budgeted.length}/{agents.length} capped)</p>
              </div>
              <div>
                <p className={`text-2xl font-semibold tabular-nums ${pct != null && pct >= 80 ? "text-warning" : ""}`}>{pct != null ? `${pct}%` : "—"}</p>
                <p className="text-xs text-foreground-muted">of budget used</p>
              </div>
              <div>
                <p className={`text-2xl font-semibold tabular-nums ${overCount > 0 ? "text-danger" : "text-ok"}`}>{overCount}</p>
                <p className="text-xs text-foreground-muted">over budget</p>
              </div>
            </div>
            {pct != null && (
              <div className="mt-3 h-2 w-full overflow-hidden rounded-full bg-surface-muted">
                <div className={`h-full ${pct >= 100 ? "bg-danger" : pct >= 80 ? "bg-warning" : "bg-ok"}`} style={{ width: `${Math.max(pct, 2)}%` }} />
              </div>
            )}
          </section>
        );
      })()}

      {error ? (
        <HonestState variant="not_wired" title="Run environment unreachable" detail="Agents will appear once it reconnects." />
      ) : agents.length === 0 ? (
        <HonestState variant="empty" title="No agent runs yet" detail="Launch a mission or a standing team and its agents — live and recent — show here." />
      ) : (
        <div className="space-y-6">
          <section>
            <div className="mb-2 flex items-center gap-2">
              <h2 className="text-sm font-semibold">Live agent runs</h2>
              <span className="rounded-full bg-surface-muted px-2 py-0.5 text-[11px] text-foreground-muted">{live.length}</span>
              <span className="text-[11px] text-foreground-muted" title="The headline 'Working now' above counts every running sandbox including spawned sub-agents; this lists the top-level runs.">top-level runs</span>
            </div>
            {live.length === 0 ? (
              <p className="rounded-lg border border-dashed border-border px-4 py-3 text-sm text-foreground-muted">
                No agent is working this moment. Standing teams mint a run on their cadence; recent results are below.
              </p>
            ) : (
              <ul className="space-y-3">{live.map((a) => <AgentCard key={a.sandbox} a={a} live />)}</ul>
            )}
          </section>

          {recent.length > 0 && (
            <section>
              <div className="mb-2 flex items-center gap-2">
                <h2 className="text-sm font-semibold">Recent runs</h2>
                <span className="rounded-full bg-surface-muted px-2 py-0.5 text-[11px] text-foreground-muted">{recent.length}</span>
              </div>
              <ul className="divide-y divide-border overflow-hidden rounded-xl border border-border bg-surface">
                {recent.map((a) => <RecentRunRow key={a.sandbox} a={a} />)}
              </ul>
            </section>
          )}
        </div>
      )}
    </div>
  );
}
