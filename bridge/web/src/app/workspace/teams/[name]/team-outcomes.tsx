"use client";

import Link from "next/link";
import { useState } from "react";
import type { TeamOutcome, TeamOutcomeSummary } from "@/lib/types";

const DISPOSITION = {
  change_proposed: {
    label: "Change proposed",
    glyph: "↗",
    cls: "border-sky-500/30 bg-sky-500/10 text-sky-600",
  },
  no_action_needed: {
    label: "No action needed",
    glyph: "✓",
    cls: "border-emerald-500/30 bg-emerald-500/10 text-emerald-600",
  },
  completed: {
    label: "Completed",
    glyph: "✓",
    cls: "border-emerald-500/30 bg-emerald-500/10 text-emerald-600",
  },
  failed: {
    label: "Failed",
    glyph: "!",
    cls: "border-rose-500/30 bg-rose-500/10 text-rose-600",
  },
} as const;

function compactNumber(value: number): string {
  return new Intl.NumberFormat(undefined, {
    notation: value >= 10_000 ? "compact" : "standard",
    maximumFractionDigits: 1,
  }).format(value);
}

function duration(seconds: number | null): string | null {
  if (seconds == null) return null;
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return remainder ? `${minutes}m ${remainder}s` : `${minutes}m`;
}

function OutcomeRow({ team, outcome }: { team: string; outcome: TeamOutcome }) {
  const meta = DISPOSITION[outcome.disposition];
  const elapsed = duration(outcome.duration_seconds);
  return (
    <li>
      <Link
        href={`/workspace/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(outcome.run)}`}
        className="group grid gap-3 rounded-xl border border-border bg-surface px-4 py-3 transition hover:border-signal/40 hover:bg-surface-muted sm:grid-cols-[auto_minmax(0,1fr)_auto]"
      >
        <span
          className={`mt-0.5 inline-flex h-7 w-7 items-center justify-center rounded-full border text-xs font-semibold ${meta.cls}`}
          aria-hidden
        >
          {meta.glyph}
        </span>
        <span className="min-w-0">
          <span className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold group-hover:text-signal">{outcome.headline}</span>
            <span className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${meta.cls}`}>
              {meta.label}
            </span>
          </span>
          {outcome.objective && (
            <span className="mt-0.5 block line-clamp-1 text-xs text-foreground-muted">
              {outcome.objective}
            </span>
          )}
          <span className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-foreground-muted">
            {outcome.finished_at && (
              <span suppressHydrationWarning>
                {new Date(outcome.finished_at).toLocaleString()}
              </span>
            )}
            {elapsed && <span>{elapsed}</span>}
            {outcome.artifact_count > 0 && (
              <span>{outcome.artifact_count} evidence file{outcome.artifact_count === 1 ? "" : "s"}</span>
            )}
            {outcome.pull_requests.map((pr) => (
              <span key={pr.url}>PR #{pr.number}</span>
            ))}
          </span>
        </span>
        <span className="self-center text-right text-[11px] text-foreground-muted">
          {outcome.tokens != null ? `${compactNumber(outcome.tokens)} tokens` : "cost unavailable"}
        </span>
      </Link>
    </li>
  );
}

export function TeamValueSummary({
  summary,
  generated,
  retained,
  queued,
  active,
  tokens,
}: {
  summary: TeamOutcomeSummary;
  generated: number;
  retained: number;
  queued: number;
  active: number;
  tokens: number;
}) {
  const resolved =
    summary.change_proposed + summary.no_action_needed + summary.completed;
  const tokensPerResolved = resolved > 0 ? Math.round(tokens / resolved) : null;
  return (
    <section className="rounded-2xl border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Value & operating health</h2>
          <p className="mt-0.5 max-w-2xl text-xs text-foreground-muted">
            Outcomes from retained evidence. A scheduled check that correctly finds nothing to do is
            resolved work, not a failed delivery.
          </p>
        </div>
        <span className="rounded-full border border-border bg-surface-muted px-2.5 py-1 text-xs text-foreground-muted">
          {retained} retained outcomes · {generated.toLocaleString()} checks all-time
        </span>
      </div>
      <dl className="mt-5 grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-6">
        {[
          ["Changes proposed", summary.change_proposed, "text-sky-600"],
          ["No action needed", summary.no_action_needed, "text-emerald-600"],
          ["Other completed", summary.completed, "text-emerald-600"],
          ["Failed", summary.failed, summary.failed > 0 ? "text-rose-600" : ""],
          ["Work queued", queued, ""],
          ["Working now", active, active > 0 ? "text-sky-600" : ""],
        ].map(([label, value, cls]) => (
          <div key={String(label)} className="rounded-xl border border-border bg-surface-muted/25 p-3">
            <dt className="text-[11px] text-foreground-muted">{label}</dt>
            <dd className={`mt-1 text-xl font-semibold tabular-nums ${cls}`}>{value}</dd>
          </div>
        ))}
      </dl>
      <p className="mt-4 text-[11px] text-foreground-muted">
        Observed token volume: {compactNumber(tokens)}
        {tokensPerResolved != null
          ? ` · ${compactNumber(tokensPerResolved)} per resolved retained outcome`
          : ""}
        . Cost is diagnostic context, not a measure of value.
      </p>
    </section>
  );
}

export function RecentTeamOutcomes({
  team,
  outcomes,
  limit = 4,
}: {
  team: string;
  outcomes: TeamOutcome[];
  limit?: number;
}) {
  const shown = outcomes.slice(0, limit);
  return (
    <section className="rounded-2xl border border-border bg-surface p-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Recent outcomes</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            What changed, what was safely ruled out, and what failed.
          </p>
        </div>
        <Link
          href={`/workspace/teams/${encodeURIComponent(team)}?tab=runs`}
          className="text-xs font-medium text-signal hover:underline"
        >
          Full history →
        </Link>
      </div>
      {shown.length === 0 ? (
        <p className="mt-4 text-xs text-foreground-muted">No retained outcomes yet.</p>
      ) : (
        <ul className="mt-4 space-y-2">
          {shown.map((outcome) => (
            <OutcomeRow key={outcome.run} team={team} outcome={outcome} />
          ))}
        </ul>
      )}
    </section>
  );
}

export function TeamRunHistory({
  team,
  runs,
  outcomes,
  generated,
}: {
  team: string;
  runs: string[];
  outcomes: TeamOutcome[];
  generated: number;
}) {
  const byRun = new Map(outcomes.map((outcome) => [outcome.run, outcome]));
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [order, setOrder] = useState<"newest" | "oldest">("newest");
  const normalizedQuery = query.trim().toLowerCase();
  const visibleRuns = runs
    .filter((run) => {
      const outcome = byRun.get(run);
      if (filter !== "all" && outcome?.disposition !== filter) return false;
      if (!normalizedQuery) return true;
      return [run, outcome?.headline, outcome?.detail, outcome?.objective]
        .filter(Boolean)
        .some((value) => value?.toLowerCase().includes(normalizedQuery));
    })
    .sort((left, right) => {
      const time = (run: string) => {
        const finished = byRun.get(run)?.finished_at;
        if (finished) {
          const parsed = new Date(finished).getTime();
          if (Number.isFinite(parsed)) return parsed;
        }
        const suffix = run.match(/(\d{10,})$/)?.[1];
        return suffix ? Number(suffix) : 0;
      };
      return order === "newest" ? time(right) - time(left) : time(left) - time(right);
    });
  return (
    <section className="rounded-2xl border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Outcome history</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Searchable outcome history is the customer record; opaque run IDs are retained only as
            provenance.
          </p>
        </div>
        <span className="rounded-full bg-surface-muted px-2.5 py-1 text-xs text-foreground-muted">
          {runs.length} retained · {generated.toLocaleString()} checks all-time
        </span>
      </div>
      <div className="mt-4 flex flex-wrap gap-2">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search outcomes, work items, repositories, or run IDs"
          className="min-w-64 flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
        />
        <select
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
          aria-label="Filter outcome disposition"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-xs"
        >
          <option value="all">All outcomes</option>
          <option value="change_proposed">Changes proposed</option>
          <option value="no_action_needed">No action needed</option>
          <option value="completed">Completed</option>
          <option value="failed">Failed</option>
        </select>
        <select
          value={order}
          onChange={(event) => setOrder(event.target.value as "newest" | "oldest")}
          aria-label="Sort team outcomes"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-xs"
        >
          <option value="newest">Newest first</option>
          <option value="oldest">Oldest first</option>
        </select>
      </div>
      <ul className="mt-4 space-y-2">
        {visibleRuns.map((run) => {
          const outcome = byRun.get(run);
          if (outcome) return <OutcomeRow key={run} team={team} outcome={outcome} />;
          return (
            <li key={run}>
              <Link
                href={`/workspace/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(run)}`}
                className="flex items-center justify-between gap-4 rounded-xl border border-border px-4 py-3 text-sm hover:bg-surface-muted"
              >
                <span>
                  <span className="font-medium">Outcome unavailable</span>
                  <span className="mt-0.5 block text-xs text-foreground-muted">
                    The retained run record has no durable outcome payload.
                  </span>
                </span>
                <span className="font-mono text-[10px] text-foreground-muted">{run}</span>
              </Link>
            </li>
          );
        })}
      </ul>
      {visibleRuns.length === 0 && (
        <p className="mt-4 text-xs text-foreground-muted">No outcomes match this search.</p>
      )}
    </section>
  );
}
