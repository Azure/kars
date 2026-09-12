import Link from "next/link";
import type { ReactNode } from "react";
import type { TeamDetail } from "@/lib/types";


type TeamServerTab = {
  id: string;
  label: string;
  badge?: number | string | null;
  node: ReactNode;
  live?: boolean;
};

export function TeamServerTabs({
  tabs,
  active,
  basePath,
}: {
  tabs: TeamServerTab[];
  active?: string;
  basePath: string;
}) {
  const current = tabs.find((tab) => tab.id === active) ?? tabs[0];
  return (
    <div>
      <div
        role="tablist"
        aria-label="Team sections"
        className="sticky top-[57px] z-10 -mx-1 mb-5 flex gap-1 overflow-x-auto rounded-xl border border-border bg-surface/80 p-1 backdrop-blur supports-[backdrop-filter]:bg-surface/70"
      >
        {tabs.map((tab) => {
          const selected = tab.id === current.id;
          return (
            <Link
              key={tab.id}
              href={`${basePath}?tab=${encodeURIComponent(tab.id)}`}
              role="tab"
              aria-selected={selected}
              className={`relative flex shrink-0 items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-sm font-medium transition ${
                selected
                  ? "bg-signal/10 text-foreground"
                  : "text-foreground-muted hover:bg-surface-muted hover:text-foreground"
              }`}
            >
              {tab.live && <span className="h-1.5 w-1.5 rounded-full bg-signal kb-pulse" />}
              {tab.label}
              {tab.badge != null && tab.badge !== 0 && (
                <span className={`rounded-full px-1.5 text-[11px] tabular-nums ${
                  selected
                    ? "bg-signal/20 text-signal"
                    : "bg-surface-muted text-foreground-muted"
                }`}>
                  {tab.badge}
                </span>
              )}
            </Link>
          );
        })}
      </div>
      <div role="tabpanel" className="kb-rise space-y-6">
        {current.node}
      </div>
    </div>
  );
}

export function NowHero({
  teamName,
  health,
  active,
  runRunning,
  runInFlight,
  everyMinutes,
  commonsEntries,
  nextRunAt,
  lastRunAt,
  delivered,
  generated,
  latestRun,
  latestOutcome,
  lifecycleMode,
  runtimeState,
  idleDeadlineAt,
}: {
  teamName: string;
  health: string | null;
  active: boolean;
  runRunning: boolean;
  runInFlight: boolean;
  everyMinutes: number | null;
  commonsEntries: number;
  nextRunAt: string | null;
  lastRunAt: string | null;
  delivered: number;
  generated: number;
  latestRun: string | null;
  latestOutcome: "paused" | "running" | "delivered" | "delivered_with_issues" | "incomplete" | "failed" | null;
  lifecycleMode: TeamDetail["lifecycle_mode"];
  runtimeState: TeamDetail["runtime_state"];
  idleDeadlineAt: string | null;
}) {
  const tone: Record<string, string> = {
    Healthy: "border-emerald-500/30 bg-emerald-500/5",
    Watching: "border-sky-500/30 bg-sky-500/5",
    AwaitingReview: "border-amber-500/30 bg-amber-500/5",
    Unproductive: "border-amber-500/30 bg-amber-500/5",
    Stalled: "border-rose-500/30 bg-rose-500/5",
    Hibernating: "border-border bg-surface-muted/40",
  };
  const cls = tone[health ?? ""] ?? "border-border bg-surface";
  const headline = !active
    ? "Hibernating — no runs are being generated"
    : health === "AwaitingReview"
      ? "Awaiting your review — no further assignment or memory promotion will proceed"
      : health === "Stalled"
      ? "On watch, but recent runs aren't delivering — needs a look"
      : health === "Unproductive"
        ? "On watch — runs are costly relative to outcomes"
        : everyMinutes
          ? "On watch — generating governed runs on cadence"
          : "On watch — waiting for queued work or Run now";

  const mode: { label: string; dot: string; note: string } = !active
    ? {
        label: "Hibernating",
        dot: "bg-foreground-muted",
        note: "Paused — no sandbox is running and no runs are minted until you resume.",
      }
    : health === "AwaitingReview"
      ? {
          label: "Waiting on your decision",
          dot: "bg-amber-500",
          note:
            "The latest governed outcome is retained in Inbox. The team will not promote it to shared memory or start dependent work until you approve or deny it.",
        }
    : runRunning
      ? {
          label: "Working now",
          dot: "bg-signal",
          note: "A run sandbox is live and executing the charter right now.",
        }
      : runInFlight
        ? {
            label: "Starting — run in flight",
            dot: "bg-amber-500",
            note:
              "A run has been launched and is materializing (or recovering). If it never reaches Working, check the latest run below for a materialization or gateway error — it is NOT idle.",
          }
        : lifecycleMode === "persistent"
          ? {
              label: "Online — waiting for work",
              dot: "bg-sky-500",
              note:
                "The stable principal stays online between assignments. No assignment is active right now; the next queued task reuses this same principal and its approved memory.",
            }
          : lifecycleMode === "resourceOptimized" && runtimeState === "Hibernating"
            ? {
                label: "Hibernating — resumes on demand",
                dot: "bg-foreground-muted",
                note:
                  "The stable principal is suspended to save resources. The next queued task resumes the same principal identity with approved memory intact.",
              }
            : lifecycleMode === "resourceOptimized"
              ? {
                  label: "Warm — no assignment active",
                  dot: "bg-sky-500",
                  note:
                    `The stable principal is retained between assignments${
                      idleDeadlineAt ? ` until ${new Date(idleDeadlineAt).toLocaleTimeString()}` : ""
                    }, then hibernates. The next task reuses the same identity and approved memory.`,
                }
        : {
          label: "Idle — spins up on demand",
          dot: "bg-sky-500",
          note:
            `Ephemeral mode starts a fresh governed sandbox on the next ${everyMinutes ? "cadence tick" : "task or Run now"}. ` +
            `It rehydrates ${commonsEntries} approved ${commonsEntries === 1 ? "memory" : "memories"} and tears the sandbox down after delivery.`,
        };

  return (
    <section className={`kb-rise rounded-xl border p-5 ${cls}`}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Right now</p>
          <p className="mt-0.5 text-sm font-medium">{headline}</p>
        </div>
        <div className="flex flex-wrap items-center gap-x-6 gap-y-1 text-sm">
          <span className="inline-flex items-center gap-1.5 rounded-full border border-border bg-surface px-2.5 py-1">
            <span className={`inline-block h-2 w-2 rounded-full ${mode.dot} ${runRunning ? "animate-pulse" : ""}`} />
            <span className="text-xs font-medium">{mode.label}</span>
          </span>
          <span className="inline-flex items-baseline gap-1.5">
            <span className="text-xs text-foreground-muted">Delivered</span>
            <span className="font-semibold tabular-nums">
              {delivered}
              <span className="text-foreground-muted">/{generated}</span>
            </span>
          </span>
          {active && nextRunAt && (
            <span className="inline-flex items-baseline gap-1.5">
              <span className="text-xs text-foreground-muted">Next tick</span>
              <span className="font-medium">{new Date(nextRunAt).toLocaleTimeString()}</span>
            </span>
          )}
          {!active && lastRunAt && (
            <span className="inline-flex items-baseline gap-1.5">
              <span className="text-xs text-foreground-muted">Last run</span>
              <span className="font-medium">{new Date(lastRunAt).toLocaleDateString()}</span>
            </span>
          )}
        </div>
      </div>
      <p className="mt-3 flex items-start gap-2 border-t border-border/60 pt-3 text-xs text-foreground-muted">
        <span aria-hidden>♻️</span>
        <span>{mode.note}</span>
      </p>
      {latestRun && (
        <p className="mt-2 flex flex-wrap items-center gap-2 text-xs text-foreground-muted">
          <span>Latest team run</span>
          <Link
            href={`/workspace/teams/${encodeURIComponent(teamName)}/runs/${encodeURIComponent(latestRun)}`}
            className="font-mono text-signal hover:underline"
          >
            {latestRun}
          </Link>
          {latestOutcome && (
            <span className={`rounded-full px-2 py-0.5 text-[10px] font-medium ${
              latestOutcome === "delivered"
                ? "bg-signal/10 text-signal"
                : latestOutcome === "running"
                  ? "bg-sky-500/10 text-sky-600"
                : latestOutcome === "paused"
                  ? "bg-surface-muted text-foreground-muted"
                : latestOutcome === "failed"
                  ? "bg-danger/10 text-danger"
                  : "bg-warning/10 text-warning"
            }`}>
              {latestOutcome === "delivered_with_issues"
                ? "Delivered with issues"
                : latestOutcome.replace(/_/g, " ")}
            </span>
          )}
        </p>
      )}
    </section>
  );
}

export function Access({ label, value, isDefault, href }: { label: string; value: string; isDefault?: boolean; href?: string }) {
  return (
    <div className="rounded-lg border border-border px-3 py-2">
      <dt className="flex items-center gap-1.5 text-[11px] uppercase tracking-wide text-foreground-muted">
        {label}
        {isDefault && (
          <span className="rounded-full bg-surface-muted px-1.5 text-[9px] font-medium normal-case tracking-normal text-foreground-muted" title="Inherited from the cluster — not explicitly set on this team.">
            default
          </span>
        )}
      </dt>
      <dd className="mt-0.5 text-sm">
        {href ? (
          <a href={href} className="text-signal underline-offset-2 hover:underline">
            {value}
          </a>
        ) : (
          value
        )}
      </dd>
    </div>
  );
}
