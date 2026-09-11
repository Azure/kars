"use client";

// kars Bridge Workspace — the missions list as a filterable surface (audit f7):
// search by name/objective and filter by status, so a growing mission fleet
// stays navigable instead of an unbounded scroll of full-width cards.

import { useMemo, useState } from "react";
import Link from "next/link";
import { MissionStatusBadge, missionStatus, type MissionStatus } from "@/components/mission-status";
import { TIER_LABELS, type TaskSummary } from "@/lib/types";

type Filter = "all" | "active" | "delivered" | "failed" | "drafts";
type Sort = "newest" | "oldest" | "name";

function statusOf(t: TaskSummary): MissionStatus {
  return missionStatus(t.phase, t.execution_phase, { delivered: t.delivered, failed: t.failed, launched: t.launched });
}

export function MissionsList({ tasks }: { tasks: TaskSummary[] }) {
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [sort, setSort] = useState<Sort>("newest");

  const counts = useMemo(() => {
    const c = { all: tasks.length, active: 0, delivered: 0, failed: 0, drafts: 0 };
    for (const t of tasks) {
      const s = statusOf(t);
      if (s === "running" || s === "deploying" || s === "needs_you") c.active++;
      else if (s === "done") c.delivered++;
      else if (s === "failed" || s === "blocked") c.failed++;
      else c.drafts++;
    }
    return c;
  }, [tasks]);

  const matches = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return tasks.filter((t) => {
      const s = statusOf(t);
      if (filter === "active" && !(s === "running" || s === "deploying" || s === "needs_you")) return false;
      if (filter === "delivered" && s !== "done") return false;
      if (filter === "failed" && !(s === "failed" || s === "blocked")) return false;
      if (filter === "drafts" && !(s === "drafting")) return false;
      if (!needle) return true;
      return [t.display_name ?? "", t.objective, t.name].join(" ").toLowerCase().includes(needle);
    }).sort((left, right) => {
      if (sort === "name") {
        return (left.display_name ?? left.name).localeCompare(right.display_name ?? right.name);
      }
      const leftTime = left.created_at ? new Date(left.created_at).getTime() : 0;
      const rightTime = right.created_at ? new Date(right.created_at).getTime() : 0;
      return sort === "newest" ? rightTime - leftTime : leftTime - rightTime;
    });
  }, [tasks, q, filter, sort]);

  const chips: { id: Filter; label: string; n: number }[] = [
    { id: "all", label: "All", n: counts.all },
    { id: "active", label: "Active", n: counts.active },
    { id: "delivered", label: "Delivered", n: counts.delivered },
    { id: "failed", label: "Failed", n: counts.failed },
    { id: "drafts", label: "Drafts", n: counts.drafts },
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <label className="relative flex-1">
          <span className="sr-only">Search missions</span>
          <svg viewBox="0 0 20 20" aria-hidden className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-foreground-muted" fill="currentColor">
            <path d="M9 3.5a5.5 5.5 0 1 0 3.4 9.83l3.13 3.14a1 1 0 0 0 1.42-1.42l-3.14-3.13A5.5 5.5 0 0 0 9 3.5Zm0 2a3.5 3.5 0 1 1 0 7 3.5 3.5 0 0 1 0-7Z" />
          </svg>
          <input
            type="search"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Search missions by name or objective"
            className="w-full rounded-lg border border-border bg-surface py-2 pl-9 pr-3 text-sm outline-none transition focus:border-signal focus:ring-2 focus:ring-signal/30"
          />
        </label>
        <div className="flex flex-wrap gap-1.5">
          {chips.map((c) => (
            <button
              key={c.id}
              type="button"
              onClick={() => setFilter(c.id)}
              className={`rounded-full border px-3 py-1 text-xs font-medium transition ${
                filter === c.id ? "border-signal/40 bg-signal/10 text-signal" : "border-border text-foreground-muted hover:text-foreground"
              }`}
            >
              {c.label} {c.n}
            </button>
          ))}
        </div>
        <select
          value={sort}
          onChange={(event) => setSort(event.target.value as Sort)}
          aria-label="Sort missions"
          className="rounded-lg border border-border bg-surface px-3 py-2 text-xs"
        >
          <option value="newest">Newest first</option>
          <option value="oldest">Oldest first</option>
          <option value="name">Name A–Z</option>
        </select>
      </div>

      {matches.length === 0 ? (
        <p className="rounded-xl border border-dashed border-border px-5 py-8 text-center text-sm text-foreground-muted">
          No missions match this filter.
        </p>
      ) : (
        <ul className="overflow-hidden rounded-xl border border-border bg-surface">
          {matches.map((t) => (
            <li key={t.name} className="border-b border-border last:border-0">
              <Link
                href={`/workspace/missions/${encodeURIComponent(t.name)}`}
                className="flex items-center justify-between gap-4 px-5 py-3.5 transition hover:bg-surface-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal focus-visible:ring-inset"
              >
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium">{t.display_name ?? t.objective}</p>
                  <p className="mt-0.5 line-clamp-1 text-xs text-foreground-muted">
                    Tier {t.tier} · {TIER_LABELS[t.tier] ?? "?"}
                    {t.created_at ? ` · Started: ${new Date(t.created_at).toLocaleString()}` : ""}
                  </p>
                </div>
                <MissionStatusBadge status={statusOf(t)} />
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
