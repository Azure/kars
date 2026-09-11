"use client";

import Link from "next/link";
import { useMemo, useState } from "react";
import { TeamTiming } from "@/components/team-timing";
import { formatWarmIdle } from "@/lib/format";
import { TIER_LABELS, type TeamSummary } from "@/lib/types";

type Sort = "recent" | "newest" | "oldest" | "name";

function lifecycleLabel(team: TeamSummary): string {
  if (team.lifecycle_mode === "resourceOptimized") {
    const idle = formatWarmIdle(team.warm_idle_seconds);
    return idle === "immediately"
      ? "Resource optimized · immediate hibernation"
      : `Resource optimized · ${idle} warm`;
  }
  return team.lifecycle_mode === "persistent" ? "Persistent runtime" : "Ephemeral runtime";
}

function PhaseDot({ team }: { team: TeamSummary }) {
  const awaitingReview = team.health === "AwaitingReview";
  const unhealthy =
    team.health === "Stalled" || team.health === "Unproductive" || team.phase === "Degraded";
  const label = team.paused
    ? "Hibernating"
    : awaitingReview
      ? "Awaiting review"
    : unhealthy
      ? (team.health ?? team.phase)
      : (team.runtime_state ?? team.phase);
  const color = team.paused
    ? "bg-foreground-muted"
    : awaitingReview
      ? "bg-amber-500"
    : unhealthy
      ? team.health === "Stalled" || team.phase === "Degraded"
        ? "bg-rose-500"
        : "bg-amber-500"
      : label === "Working"
        ? "bg-sky-500"
        : label === "Warm"
          ? "bg-emerald-500"
          : label === "Hibernating"
            ? "bg-foreground-muted"
            : "bg-amber-500";
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-foreground-muted">
      <span className={`h-2 w-2 rounded-full ${color}`} aria-hidden />
      {label}
    </span>
  );
}

function timestamp(value: string | null): number {
  if (!value) return 0;
  const parsed = new Date(value).getTime();
  return Number.isFinite(parsed) ? parsed : 0;
}

function recentTimestamp(team: TeamSummary): number {
  return Math.max(
    timestamp(team.last_activity_at),
    timestamp(team.last_run_at),
    timestamp(team.created_at),
  );
}

export function TeamsList({ teams }: { teams: TeamSummary[] }) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<Sort>("recent");
  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return teams
      .filter((team) =>
        !needle ||
        [team.display_name ?? "", team.name, team.charter]
          .join(" ")
          .toLowerCase()
          .includes(needle),
      )
      .sort((left, right) => {
        if (sort === "name") {
          return (left.display_name ?? left.name).localeCompare(right.display_name ?? right.name);
        }
        if (sort === "newest") return timestamp(right.created_at) - timestamp(left.created_at);
        if (sort === "oldest") return timestamp(left.created_at) - timestamp(right.created_at);
        return recentTimestamp(right) - recentTimestamp(left);
      });
  }, [query, sort, teams]);

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search teams by name or charter"
          className="min-w-64 flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm outline-none focus:border-signal"
        />
        <select
          value={sort}
          onChange={(event) => setSort(event.target.value as Sort)}
          aria-label="Sort teams"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-xs"
        >
          <option value="recent">Latest activity first</option>
          <option value="newest">Newest team first</option>
          <option value="oldest">Oldest team first</option>
          <option value="name">Name A–Z</option>
        </select>
      </div>
      {visible.length === 0 ? (
        <p className="rounded-xl border border-dashed border-border px-5 py-8 text-center text-sm text-foreground-muted">
          No teams match this search.
        </p>
      ) : (
        <ul className="grid gap-4 sm:grid-cols-2">
          {visible.map((team) => (
            <li key={team.name}>
              <Link
                href={`/workspace/teams/${encodeURIComponent(team.name)}`}
                className="flex h-full flex-col rounded-xl border border-border bg-surface p-5 transition hover:border-signal/40 hover:bg-surface-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
              >
                <div className="flex items-start justify-between gap-3">
                  <p className="truncate text-sm font-semibold">{team.display_name ?? team.name}</p>
                  <PhaseDot team={team} />
                </div>
                <p className="mt-2 line-clamp-2 text-xs text-foreground-muted">
                  {team.charter?.trim() ? team.charter : <span className="italic">No charter configured — open to set one.</span>}
                </p>
                <div className="mt-4 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-foreground-muted">
                  <span>Tier {team.tier} · {TIER_LABELS[team.tier] ?? "?"}</span>
                  <span>{team.member_count} {team.member_count === 1 ? "member" : "members"}</span>
                  {team.every_minutes != null && <span>Checks every {team.every_minutes} min</span>}
                  <span>{lifecycleLabel(team)}</span>
                  {team.current_assignment_task && <span>Assignment: {team.current_assignment_task}</span>}
                  <span>{team.generated_task_count.toLocaleString()} checks all-time</span>
                  <span className={team.retained_failed > 0 ? "text-rose-600" : ""}>
                    Retained: {team.retained_delivered} delivered · {team.retained_no_action} no-action ·{" "}
                    {team.retained_failed} failed
                  </span>
                  {team.created_at && (
                    <span suppressHydrationWarning>Created {new Date(team.created_at).toLocaleString()}</span>
                  )}
                </div>
                <TeamTiming
                  compact
                  lastActivityAt={team.last_activity_at}
                  nextActivityAt={team.paused ? null : team.next_run_at}
                  paused={team.paused}
                />
                {team.reporting_to && (
                  <p className="mt-3 text-xs text-foreground-muted">Reports to {team.reporting_to}</p>
                )}
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
